// `[[ ... ]]` — bash's conditional expression.
//
// The grammar and the semantics follow GNU bash 5.3: `parse.y`'s
// cond_expr / cond_or / cond_and / cond_term and `execute_cmd.c`'s
// execute_cond_node.  The quirks that are reproduced here:
//
//   * `[[ x ]]` is NOT a string comparison.  bash treats a lone word as
//     `[[ -n x ]]` (parse.y's cond_term special case), so `[[ abc==abc ]]`
//     merely asks whether the string `abc==abc` is non-empty.
//   * `=` / `==` / `!=` match the right-hand side as a shell pattern.  The
//     quoted parts of the pattern are literal, the unquoted ones stay glob
//     metacharacters (bash 5.3 semantics).
//   * `=~` matches the right-hand side as a POSIX ERE; the quoted parts are
//     literal.  The regex engine is the platform libc's -- the very same
//     one bash uses -- so the match semantics *and* the error messages are
//     byte-identical.
//   * `<` / `>` compare with strcmp (not strcoll); bash only uses strcoll
//     in POSIX mode or with compatibility level > 40.
//   * The numeric operators evaluate their operands as arithmetic
//     expressions, so `abc` is the (unset) variable `abc` and compares
//     as 0.
//
// Nothing inside `[[ ]]` is word-split or pathname-expanded: the operands
// are expanded as single strings.

use std::cell::RefCell;

use crate::error::{DiagnosticFamily, Result, WashError};
use crate::expand::{self, ExpandCtx};
use crate::lexer::{Token, WordPart};

thread_local! {
    /// Capture groups from the most recent `=~` evaluation.  The expander
    /// only has a read-only view of the shell, so the executor copies these
    /// into `BASH_REMATCH` after `eval_cond` returns (just like bash, which
    /// leaves the array untouched until a `=~` is actually evaluated).
    static LAST_REMATCH: RefCell<Option<Vec<String>>> = RefCell::new(None);
}

/// Take (and clear) the capture groups of the last `=~` evaluation.
pub fn take_rematch() -> Option<Vec<String>> {
    LAST_REMATCH.with(|s| s.borrow_mut().take())
}

/// A conditional expression (`[[ ... ]]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CondExpr {
    /// A word operand.  Only reachable through the `-n` special case
    /// (`[[ x ]]` is `[[ -n x ]]`).
    Word(Vec<WordPart>),
    /// `! expr`
    Not(Box<CondExpr>),
    /// `-OP operand` — `op` is one of bash's unary test operators.
    Unary { op: String, operand: Vec<WordPart> },
    /// `lhs OP rhs`
    Binary {
        op: String,
        lhs: Vec<WordPart>,
        rhs: Vec<WordPart>,
    },
    /// `lhs && rhs`
    And(Box<CondExpr>, Box<CondExpr>),
    /// `lhs || rhs`
    Or(Box<CondExpr>, Box<CondExpr>),
    /// `( expr )`
    Group(Box<CondExpr>),
}

/// A diagnostic of the conditional-expression parser or evaluator.
///
/// `message` is bash's own wording; `near` is the token bash's yacc layer
/// names in the follow-up "syntax error near ..." line.
#[derive(Debug, Clone)]
pub struct CondError {
    pub message: String,
    /// The 1-based line of the `[[` that opened the expression.
    pub line: usize,
    /// The offending token, rendered the way bash renders it.
    pub near: String,
}

impl CondError {
    fn new(message: impl Into<String>, line: usize, near: impl Into<String>) -> Self {
        CondError {
            message: message.into(),
            line,
            near: near.into(),
        }
    }

    /// Turn the diagnostic into the shell's parse error, tagged with the
    /// `CondExpr` family so the shell can print bash's two-line diagnostic
    /// (the conditional message plus the yacc "syntax error near ..." line)
    /// and echo the offending source line.
    pub fn into_wash_error(self) -> WashError {
        let mut err = WashError::syntax(self.message);
        err.family = DiagnosticFamily::CondExpr;
        err.open_line = self.line;
        err.near = self.near;
        err
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse the conditional expression that starts at `tokens[start]`.
///
/// `start` must point just after the `[[` token.  Returns the expression
/// and the index of the token *after* the closing `]]`.
pub fn parse_cond(tokens: &[Token], start: usize, line: usize) -> Result<(CondExpr, usize)> {
    // bash's grammar rejects a bare `[[ ]]` at the yacc level: it prints only
    // "syntax error near <token-after-]]>", with no conditional message line.
    if matches!(tokens.get(start), Some(Token::CondEnd)) {
        let after = tokens
            .get(start + 1)
            .map(render_token_near)
            .unwrap_or_default();
        let mut err = WashError::syntax("");
        err.family = DiagnosticFamily::CondExpr;
        err.open_line = line;
        err.near = after;
        return Err(err);
    }

    let mut p = CondParser {
        tokens,
        pos: start,
        line,
    };
    let expr = match p.parse_expr() {
        Ok(expr) => expr,
        Err(mut ce) => {
            // When the conditional parser rejects at the closing `]]`, bash's
            // yacc layer has already consumed it, so its "syntax error near"
            // line names the token *after* `]]` (usually the following `;`).
            if matches!(p.tokens.get(p.pos), Some(Token::CondEnd)) {
                ce.near = p
                    .tokens
                    .get(p.pos + 1)
                    .map(render_token_near)
                    .unwrap_or_default();
            }
            return Err(ce.into_wash_error());
        }
    };
    // The expression must be closed by `]]`.
    match p.tokens.get(p.pos) {
        Some(Token::CondEnd) => Ok((expr, p.pos + 1)),
        Some(other) => {
            let full = render_token(other);
            let near = render_token_near(other);
            Err(CondError::new(
                format!(
                    "syntax error in conditional expression: unexpected token `{}'",
                    full
                ),
                line,
                near,
            )
            .into_wash_error())
        }
        None => Err(CondError::new(
            "syntax error in conditional expression",
            line,
            "",
        )
        .into_wash_error()),
    }
}

struct CondParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    line: usize,
}

impl<'a> CondParser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    /// bash's `cond_skip_newlines`: the conditional grammar skips a newline
    /// at the start of a term and after a completed term, but *not* where a
    /// binary operator is expected.
    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Some(Token::CondNewline)) {
            self.pos += 1;
        }
    }

    fn parse_expr(&mut self) -> std::result::Result<CondExpr, CondError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> std::result::Result<CondExpr, CondError> {
        let lhs = self.parse_and()?;
        if matches!(self.peek(), Some(Token::OrOr)) {
            self.pos += 1;
            let rhs = self.parse_or()?;
            return Ok(CondExpr::Or(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> std::result::Result<CondExpr, CondError> {
        let lhs = self.parse_term()?;
        if matches!(self.peek(), Some(Token::AndAnd)) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            return Ok(CondExpr::And(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    fn parse_term(&mut self) -> std::result::Result<CondExpr, CondError> {
        self.skip_newlines();
        let tok = match self.peek() {
            Some(t) => t.clone(),
            None => {
                return Err(CondError::new(
                    "syntax error in conditional expression",
                    self.line,
                    "",
                ))
            }
        };
        match &tok {
            Token::CondEnd => Err(CondError::new(
                "syntax error in conditional expression",
                self.line,
                "]]",
            )),
            Token::Word(parts) | Token::CondWord(parts, _) if is_paren_open(parts) => {
                self.pos += 1;
                let inner = self.parse_expr()?;
                self.skip_newlines();
                match self.peek() {
                    Some(Token::Word(p)) | Some(Token::CondWord(p, _)) if is_paren_close(p) => {
                        self.pos += 1;
                        Ok(CondExpr::Group(Box::new(inner)))
                    }
                    other => {
                        let full = other.map(|t| {
                            // Prefer the raw source text for CondWord (preserves quotes),
                            // fall back to render_token for ordinary Word tokens.
                            if matches!(t, Token::CondWord(_, _)) {
                                raw_token(t).to_string()
                            } else {
                                render_token(t)
                            }
                        }).unwrap_or_default();
                        let near = other.map(render_token_near).unwrap_or_default();
                        Err(CondError::new(
                            format!("unexpected token `{}', expected `)'", full),
                            self.line,
                            near,
                        ))
                    }
                }
            }
            Token::Word(parts) | Token::CondWord(parts, _) if is_bang(parts) => {
                self.pos += 1;
                let inner = self.parse_term()?;
                Ok(CondExpr::Not(Box::new(inner)))
            }
            Token::Word(parts) | Token::CondWord(parts, _) if is_unary_op(parts) => {
                let op = render_word(parts);
                self.pos += 1;
                match self.peek() {
                    Some(Token::Word(operand)) | Some(Token::CondWord(operand, _)) => {
                        let operand = operand.clone();
                        self.pos += 1;
                        self.skip_newlines();
                        Ok(CondExpr::Unary { op, operand })
                    }
                    other => {
                        let full = other.map(|t| {
                            // Prefer the raw source text for CondWord (preserves quotes),
                            // fall back to render_token for ordinary Word tokens.
                            if matches!(t, Token::CondWord(_, _)) {
                                raw_token(t).to_string()
                            } else {
                                render_token(t)
                            }
                        }).unwrap_or_default();
                        let near = other.map(render_token_near).unwrap_or_default();
                        Err(CondError::new(
                            format!(
                                "unexpected argument `{}' to conditional unary operator",
                                full
                            ),
                            self.line,
                            near,
                        ))
                    }
                }
            }
            Token::Word(parts) | Token::CondWord(parts, _) => {
                let lhs = parts.clone();
                self.pos += 1;
                // The binary operator.  bash reads the next token *without*
                // skipping newlines here, so a newline in this position is
                // an error.
                let op_tok = self.peek().cloned();
                match op_tok {
                    Some(Token::Word(w)) if is_binop(&w) => {
                        let op = render_word(&w);
                        self.pos += 1;
                        self.binary_rhs(op, lhs)
                    }
                    // An escaped operator (`\=`) is a literal word, not an
                    // operator.  `WordPart` drops the backslash, but the raw
                    // source text still has it.
                    Some(Token::CondWord(w, raw)) if is_binop(&w) && !raw.contains('\\') => {
                        let op = render_word(&w);
                        self.pos += 1;
                        self.binary_rhs(op, lhs)
                    }
                    Some(Token::CondOp(o)) => {
                        self.pos += 1;
                        self.binary_rhs(o, lhs)
                    }
                    // bash's special case: `[[ x ]]` is `[[ -n x ]]`, and so
                    // is `[[ x && expr ]]`, `[[ x || expr ]]` and `[[ (x) ]]`.
                    Some(Token::CondEnd) | Some(Token::AndAnd) | Some(Token::OrOr) => {
                        Ok(CondExpr::Unary {
                            op: "-n".to_string(),
                            operand: lhs,
                        })
                    }
                    Some(Token::Word(w)) | Some(Token::CondWord(w, _)) if is_paren_close(&w) => {
                        Ok(CondExpr::Unary {
                            op: "-n".to_string(),
                            operand: lhs,
                        })
                    }
                    other => {
                        let full = other.as_ref().map(render_token).unwrap_or_default();
                        let near = other.as_ref().map(render_token_near).unwrap_or_default();
                        Err(CondError::new(
                            format!(
                                "unexpected token `{}', conditional binary operator expected",
                                full
                            ),
                            self.line,
                            near,
                        ))
                    }
                }
            }
            other => {
                let full = render_token(other);
                let near = render_token_near(other);
                Err(CondError::new(
                    format!("unexpected token `{}' in conditional command", full),
                    self.line,
                    near,
                ))
            }
        }
    }

    /// Read the right-hand side of a binary operator.
    fn binary_rhs(
        &mut self,
        op: String,
        lhs: Vec<WordPart>,
    ) -> std::result::Result<CondExpr, CondError> {
        match self.peek() {
            Some(Token::Word(rhs)) | Some(Token::CondWord(rhs, _)) => {
                let rhs = rhs.clone();
                self.pos += 1;
                self.skip_newlines();
                Ok(CondExpr::Binary { op, lhs, rhs })
            }
            other => {
                let full = other.map(render_token).unwrap_or_default();
                let near = other.map(render_token_near).unwrap_or_default();
                Err(CondError::new(
                    format!(
                        "unexpected argument `{}' to conditional binary operator",
                        full
                    ),
                    self.line,
                    near,
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Token helpers
// ---------------------------------------------------------------------------

/// The plain text of a word (no expansion).  Used for operators and for the
/// token text bash prints in its diagnostics.
pub fn render_word(parts: &[WordPart]) -> String {
    let mut out = String::new();
    for p in parts {
        match p {
            WordPart::Text(s, _) => out.push_str(s),
            WordPart::Var(name, _) => {
                out.push('$');
                out.push_str(name);
            }
            WordPart::Braced(body, _) => {
                out.push_str("${");
                out.push_str(body);
                out.push('}');
            }
            WordPart::CmdSub(body, _) => {
                out.push_str("$(");
                out.push_str(body);
                out.push(')');
            }
            WordPart::Backtick(body, _) => {
                out.push('`');
                out.push_str(body);
                out.push('`');
            }
            WordPart::Arith(body, _) => {
                out.push_str("$((");
                out.push_str(body);
                out.push_str("))");
            }
            WordPart::Tilde => out.push('~'),
            WordPart::Brace(alts) => {
                out.push('{');
                out.push_str(&alts.join(","));
                out.push('}');
            }
        }
    }
    out
}

/// Render a parsed conditional expression back to shell source.  Used by the
/// `type` builtin when it prints a function body containing `[[ ... ]]`.
pub fn render_cond(expr: &CondExpr) -> String {
    match expr {
        CondExpr::Word(parts) => render_word(parts),
        CondExpr::Not(inner) => format!("! {}", render_cond(inner)),
        CondExpr::Unary { op, operand } => format!("{} {}", op, render_word(operand)),
        CondExpr::Binary { op, lhs, rhs } => {
            format!("{} {} {}", render_word(lhs), op, render_word(rhs))
        }
        CondExpr::And(a, b) => format!("{} && {}", render_cond(a), render_cond(b)),
        CondExpr::Or(a, b) => format!("{} || {}", render_cond(a), render_cond(b)),
        CondExpr::Group(inner) => format!("( {} )", render_cond(inner)),
    }
}

/// The raw source text of a token.  This is what bash's yacc layer uses for
/// `syntax error near ...`: quotes, escapes and the `$` of `$'...'` are all
/// preserved verbatim.
fn raw_token(tok: &Token) -> String {
    match tok {
        Token::Word(parts) => render_word(parts),
        Token::CondWord(_, raw) => raw.clone(),
        Token::CondEnd => "]]".to_string(),
        Token::CondOp(op) => op.clone(),
        Token::CondNumber(n) => n.clone(),
        Token::CondNewline => "newline".to_string(),
        Token::Semicolon(_) => ";".to_string(),
        Token::AndAnd => "&&".to_string(),
        Token::OrOr => "||".to_string(),
        Token::Pipe => "|".to_string(),
        Token::Background(_) => "&".to_string(),
        Token::Reserved(name, _) => name.clone(),
        Token::CondStart(_) => "[[".to_string(),
        Token::ArithCmd(body, _) => format!("(( {} ))", body),
        Token::Compound(_) => "compound".to_string(),
        Token::FunctionDef(name) => name.clone(),
        Token::Redirect { .. } => "redirection".to_string(),
    }
}

/// The token text in the conditional parser's own diagnostic.  It is the raw
/// source text except that bash drops the `$` prefix of an ANSI-C / locale
/// quoted word (`[[ a $'=' a ]]` reports ```'=''`` while its yacc line keeps
/// ``$'='``).
fn render_token(tok: &Token) -> String {
    let raw = raw_token(tok);
    // bash drops the `$` of an ANSI-C / locale quoted word in the
    // conditional parser's own message (`[[ a $'=' a ]]` -> ``'=''').  The
    // word may arrive here as a plain `Word` (when the `$'...'` scanner
    // produced a single quoted Text part) rather than a `CondWord`, so key
    // off the rendered text itself instead of only the token variant.  The
    // yacc-layer `syntax error near` line deliberately keeps the `$` and is
    // rendered by `render_token_near`, which is untouched.
    if raw.starts_with("$'") || raw.starts_with("$\"") {
        return raw[1..].to_string();
    }
    raw
}

/// The token text in bash's yacc-layer `syntax error near ...` line.  bash's
/// `error_token_from_token` returns only the tail after the last space or tab
/// when the token contains one (`[[ a "b c" d ]]` shows `"b c"` in the
/// conditional message but `c"` in the yacc line).
fn render_token_near(tok: &Token) -> String {
    let full = raw_token(tok);
    match full.rfind(|c| c == ' ' || c == '\t') {
        Some(idx) => full[idx + 1..].to_string(),
        None => full,
    }
}

fn is_paren_open(parts: &[WordPart]) -> bool {
    matches!(parts, [WordPart::Text(t, false)] if t == "(")
}

fn is_paren_close(parts: &[WordPart]) -> bool {
    matches!(parts, [WordPart::Text(t, false)] if t == ")")
}

/// The word `!` — bash's negation operator.
fn is_bang(parts: &[WordPart]) -> bool {
    matches!(parts, [WordPart::Text(t, false)] if t == "!")
}

/// bash's `test_unop`: a unary operator is exactly two characters, `-` plus
/// one of the listed letters.
fn is_unary_op(parts: &[WordPart]) -> bool {
    let text = match parts {
        [WordPart::Text(t, false)] => t.as_str(),
        _ => return false,
    };
    let b = text.as_bytes();
    if b.len() != 2 || b[0] != b'-' {
        return false;
    }
    matches!(
        b[1],
        b'a' | b'b' | b'c' | b'd' | b'e' | b'f' | b'g' | b'h' | b'k' | b'n' | b'o' | b'p'
            | b'r' | b's' | b't' | b'u' | b'v' | b'w' | b'x' | b'z' | b'G' | b'L' | b'O' | b'S'
            | b'N' | b'R'
    )
}

/// bash's `test_binop`: the binary operators of a conditional expression.
fn is_binop(parts: &[WordPart]) -> bool {
    let op = match parts {
        [WordPart::Text(t, false)] => t.as_str(),
        _ => return false,
    };
    matches!(
        op,
        // string / pattern / regex
        "=" | "==" | "!=" | "=~" | "<" | ">"
            // arithmetic
            | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"
            // file age / inode
            | "-nt" | "-ot" | "-ef"
    )
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Evaluate a conditional expression.
///
/// Returns whether the expression is true.  The only runtime failure is an
/// invalid regular expression, which bash reports and treats as "no match".
pub fn eval_cond(expr: &CondExpr, ctx: &ExpandCtx) -> bool {
    match expr {
        CondExpr::Word(parts) => !expand::expand_scalar(parts, ctx).is_empty(),
        CondExpr::Not(inner) => !eval_cond(inner, ctx),
        CondExpr::And(lhs, rhs) => eval_cond(lhs, ctx) && eval_cond(rhs, ctx),
        CondExpr::Or(lhs, rhs) => eval_cond(lhs, ctx) || eval_cond(rhs, ctx),
        CondExpr::Group(inner) => eval_cond(inner, ctx),
        CondExpr::Unary { op, operand } => {
            let arg = expand::expand_scalar(operand, ctx);
            unary_test(op, &arg, ctx)
        }
        CondExpr::Binary { op, lhs, rhs } => binary_test(op, lhs, rhs, ctx),
    }
}

/// bash's `unary_test`.
fn unary_test(op: &str, arg: &str, ctx: &ExpandCtx) -> bool {
    match op {
        "-a" | "-e" => crate::posix::stat_path(arg, true).is_some(),
        "-r" => crate::posix::access(arg, 4) == 0,
        "-w" => crate::posix::access(arg, 2) == 0,
        "-x" => crate::posix::access(arg, 1) == 0,
        "-O" => crate::posix::stat_path(arg, true)
            .map(|s| s.uid as u32 == crate::posix::effective_uid())
            .unwrap_or(false),
        "-G" => crate::posix::stat_path(arg, true)
            .map(|s| s.gid as u32 == crate::posix::effective_gid())
            .unwrap_or(false),
        "-N" => match crate::posix::stat_path(arg, true) {
            Some(s) => s.mtime > s.atime,
            None => false,
        },
        "-f" => crate::posix::stat_path(arg, true)
            .map(|s| s.file_type == crate::posix::FT_REG)
            .unwrap_or(false),
        "-d" => crate::posix::stat_path(arg, true)
            .map(|s| s.file_type == crate::posix::FT_DIR)
            .unwrap_or(false),
        "-s" => crate::posix::stat_path(arg, true)
            .map(|s| s.size > 0)
            .unwrap_or(false),
        "-S" => crate::posix::stat_path(arg, true)
            .map(|s| s.file_type == crate::posix::FT_SOCK)
            .unwrap_or(false),
        "-c" => crate::posix::stat_path(arg, true)
            .map(|s| s.file_type == crate::posix::FT_CHR)
            .unwrap_or(false),
        "-b" => crate::posix::stat_path(arg, true)
            .map(|s| s.file_type == crate::posix::FT_BLK)
            .unwrap_or(false),
        "-p" => crate::posix::stat_path(arg, true)
            .map(|s| s.file_type == crate::posix::FT_FIFO)
            .unwrap_or(false),
        "-L" | "-h" => {
            !arg.is_empty()
                && crate::posix::stat_path(arg, false)
                    .map(|s| s.file_type == crate::posix::FT_LNK)
                    .unwrap_or(false)
        }
        "-u" => crate::posix::stat_path(arg, true)
            .map(|s| s.mode & crate::posix::S_ISUID != 0)
            .unwrap_or(false),
        "-g" => crate::posix::stat_path(arg, true)
            .map(|s| s.mode & crate::posix::S_ISGID != 0)
            .unwrap_or(false),
        "-k" => crate::posix::stat_path(arg, true)
            .map(|s| s.mode & crate::posix::S_ISVTX != 0)
            .unwrap_or(false),
        "-t" => match crate::posix::parse_long_long(arg) {
            (fd, true, _) => fd >= 0 && fd <= i32::MAX as i64 && crate::posix::isatty(fd as i32),
            _ => false,
        },
        "-n" => !arg.is_empty(),
        "-z" => arg.is_empty(),
        "-o" => ctx.set_options.get(arg).unwrap_or(false),
        "-v" => variable_exists(arg, ctx),
        "-R" => variable_is_nameref(arg, ctx),
        _ => false,
    }
}

/// `[[ -v name ]]`: the variable (or array element) is set.
fn variable_exists(name: &str, ctx: &ExpandCtx) -> bool {
    if let Some(idx) = array_subscript(name) {
        let (base, sub) = idx;
        if let Some(elems) = ctx.arrays.get(&base) {
            if let Ok(i) = sub.parse::<usize>() {
                return i < elems.len();
            }
        }
        return false;
    }
    ctx.env.contains_key(name)
}

/// `[[ -R name ]]`: the variable is a nameref.  wash has no namerefs, so
/// this is always false — bash reports the same for an ordinary variable.
fn variable_is_nameref(_name: &str, _ctx: &ExpandCtx) -> bool {
    false
}

/// Split `name[index]` into its parts.
fn array_subscript(name: &str) -> Option<(String, String)> {
    let open = name.find('[')?;
    if !name.ends_with(']') {
        return None;
    }
    Some((name[..open].to_string(), name[open + 1..name.len() - 1].to_string()))
}

/// bash's `binary_test` for the conditional expression.
fn binary_test(op: &str, lhs: &[WordPart], rhs: &[WordPart], ctx: &ExpandCtx) -> bool {
    match op {
        "=" | "==" => {
            let text = expand::expand_scalar(lhs, ctx);
            let pattern = expand_pattern(rhs, ctx);
            glob_match(&text, &pattern)
        }
        "!=" => {
            let text = expand::expand_scalar(lhs, ctx);
            let pattern = expand_pattern(rhs, ctx);
            !glob_match(&text, &pattern)
        }
        "<" => expand::expand_scalar(lhs, ctx) < expand::expand_scalar(rhs, ctx),
        ">" => expand::expand_scalar(lhs, ctx) > expand::expand_scalar(rhs, ctx),
        "=~" => {
            let text = expand::expand_scalar(lhs, ctx);
            let pattern = expand_regex(rhs, ctx);
            regex_match(&text, &pattern)
        }
        "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
            let a = arith_value(lhs, ctx);
            let b = arith_value(rhs, ctx);
            match op {
                "-eq" => a == b,
                "-ne" => a != b,
                "-lt" => a < b,
                "-le" => a <= b,
                "-gt" => a > b,
                _ => a >= b,
            }
        }
        "-nt" | "-ot" | "-ef" => {
            let a = expand::expand_scalar(lhs, ctx);
            let b = expand::expand_scalar(rhs, ctx);
            match op {
                "-ef" => file_same(&a, &b),
                "-nt" => file_newer(&a, &b),
                _ => file_newer(&b, &a),
            }
        }
        _ => false,
    }
}

/// The numeric operators evaluate their operands as arithmetic
/// expressions; `abc` is the (unset) variable `abc` and compares as 0.
fn arith_value(parts: &[WordPart], ctx: &ExpandCtx) -> i64 {
    let text = expand::expand_scalar(parts, ctx);
    let trimmed = text.trim();
    match crate::posix::parse_long_long(trimmed) {
        (v, true, _) => v,
        _ => expand::eval_arith(trimmed, ctx).trim().parse::<i64>().unwrap_or(0),
    }
}

fn file_same(a: &str, b: &str) -> bool {
    match (crate::posix::stat_path(a, true), crate::posix::stat_path(b, true)) {
        (Some(x), Some(y)) => x.dev == y.dev && x.ino == y.ino,
        _ => false,
    }
}

fn file_newer(a: &str, b: &str) -> bool {
    match (crate::posix::stat_path(a, true), crate::posix::stat_path(b, true)) {
        (Some(x), Some(y)) => (x.mtime, x.mtime_nsec) > (y.mtime, y.mtime_nsec),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Pattern / regex operands
// ---------------------------------------------------------------------------

/// Expand the right-hand side of `=` / `==` / `!=` into a glob pattern.
///
/// The quoted parts of the pattern are literal; the unquoted ones stay glob
/// metacharacters.  bash does this by keeping a "quoted" marker on every
/// character that came from a quoted context and letting
/// `quote_string_for_globbing` escape only the characters that are special
/// to the matcher.
fn expand_pattern(parts: &[WordPart], ctx: &ExpandCtx) -> Vec<char> {
    let bytes = expand_marked(parts, ctx);
    let mut out = String::new();
    for (b, quoted) in bytes {
        if !quoted {
            out.push(b as char);
            continue;
        }
        if glob_quote_char(b) {
            out.push('\\');
        }
        out.push(b as char);
    }
    out.chars().collect()
}

/// Expand the right-hand side of `=~` into a POSIX ERE.
///
/// The quoted parts are literal; the unquoted ones stay ERE
/// metacharacters.  bash escapes only the characters that are special to
/// the ERE matcher (`ere_char`).
fn expand_regex(parts: &[WordPart], ctx: &ExpandCtx) -> String {
    let bytes = expand_marked(parts, ctx);
    let mut out = String::new();
    for (b, quoted) in bytes {
        if !quoted {
            out.push(b as char);
            continue;
        }
        if ere_char(b) {
            out.push('\\');
        }
        out.push(b as char);
    }
    out
}

/// Expand a word into its bytes, remembering for every byte whether it came
/// from a quoted context.
///
/// This is bash's `cond_expand_word` with the "quoted" markers still in
/// place; the caller turns the markers into the escapes its matcher needs.
fn expand_marked(parts: &[WordPart], ctx: &ExpandCtx) -> Vec<(u8, bool)> {
    let mut out: Vec<(u8, bool)> = Vec::new();
    for part in parts {
        match part {
            WordPart::Text(s, quoted) => {
                for b in s.as_bytes() {
                    out.push((*b, *quoted));
                }
            }
            WordPart::Var(name, _) => {
                let value = expand::expand_scalar(&[WordPart::Var(name.clone(), false)], ctx);
                out.extend(value.as_bytes().iter().map(|b| (*b, false)));
            }
            WordPart::Braced(body, _) => {
                let value = expand::expand_scalar(&[WordPart::Braced(body.clone(), false)], ctx);
                out.extend(value.as_bytes().iter().map(|b| (*b, false)));
            }
            WordPart::CmdSub(body, _) | WordPart::Backtick(body, _) => {
                let value =
                    expand::expand_scalar(&[WordPart::CmdSub(body.clone(), false)], ctx);
                out.extend(value.as_bytes().iter().map(|b| (*b, false)));
            }
            WordPart::Arith(body, _) => {
                let value = expand::expand_scalar(&[WordPart::Arith(body.clone(), false)], ctx);
                out.extend(value.as_bytes().iter().map(|b| (*b, false)));
            }
            WordPart::Tilde => {
                let value = ctx.env.get("HOME").cloned().unwrap_or_default();
                out.extend(value.as_bytes().iter().map(|b| (*b, false)));
            }
            WordPart::Brace(alts) => {
                // bash performs no brace expansion inside `[[ ]]`, so the
                // braces stay literal.
                let text = format!("{{{}}}", alts.join(","));
                out.extend(text.as_bytes().iter().map(|b| (*b, false)));
            }
        }
    }
    out
}

/// bash's `glob_char_p` / `glob_quote_char`: the characters that have to be
/// backslash-quoted inside a glob pattern.
fn glob_quote_char(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'^' | b'-' | b'.' | b':' | b'=' | b'*' | b'[' | b']' | b'?' | b'\\' | b'%'
            | b'#' | b'(' | b')' | b'|'
    )
}

/// bash's `ere_char`: the characters that are special in a POSIX ERE.
fn ere_char(b: u8) -> bool {
    matches!(
        b,
        b'.' | b'[' | b'\\' | b'(' | b')' | b'*' | b'+' | b'?' | b'{' | b'|' | b'^' | b'$'
    )
}

/// Match a string against a glob pattern.
fn glob_match(text: &str, pattern: &[char]) -> bool {
    let s: Vec<char> = text.chars().collect();
    expand::glob_match(&s, pattern)
}

/// Match a string against a POSIX ERE.
///
/// An invalid regular expression is reported the way bash reports it and the
/// comparison simply fails.
fn regex_match(text: &str, pattern: &str) -> bool {
    match crate::posix::regex_match(text, pattern, false) {
        crate::posix::RegexMatch::Match(groups) => {
            let matched: Vec<String> = groups
                .iter()
                .map(|g| match g {
                    Some((start, end)) => text.get(*start..*end).unwrap_or("").to_string(),
                    None => String::new(),
                })
                .collect();
            LAST_REMATCH.with(|s| *s.borrow_mut() = Some(matched));
            true
        }
        crate::posix::RegexMatch::NoMatch => {
            LAST_REMATCH.with(|s| *s.borrow_mut() = Some(Vec::new()));
            false
        }
        crate::posix::RegexMatch::Invalid(msg) => {
            crate::safe_eprintln!("wash: [[: invalid regular expression `{}': {}", pattern, msg);
            false
        }
        crate::posix::RegexMatch::Error => false,
    }
}
