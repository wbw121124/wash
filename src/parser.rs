// Parser: flat token stream -> AST.
//
// The AST mirrors bash's grammar:
//
//   CommandList  := AndOr (';' AndOr)*        -- `;` separated AND-OR lists
//   AndOr        := Pipeline (('&&'|'||') Pipeline)*
//   Pipeline     := SimpleCommand ('|' SimpleCommand)*
//   SimpleCommand:= word* (redirection word*)*
//
// `&&` and `||` have the same precedence and are left-associative.
//
// Compounds (control flow) — `if`/`for`/`while`/`until`/`case` and shell
// function definitions — are extracted from the token stream *before* the
// flat AND-OR / pipeline pass runs, exactly the way `if` already was: every
// compound is replaced by a single `Token::Compound` (or `Token::FunctionDef`)
// that carries the parsed sub-trees, so the flat pass never has to know about
// their internal structure.

use crate::error::{DiagnosticFamily, Result, WashError};
use crate::lexer::{HereDocBody, RedirKind, SemicolonKind, Token, WordPart};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redir {
    In { fd: i32, file: Vec<WordPart> },
    /// `clobber` is `>|`: truncate even under `set -C`.
    Out { fd: i32, file: Vec<WordPart>, append: bool, clobber: bool },
    /// `&>` / `&>>` (stdout + stderr, truncate / append).
    Both { file: Vec<WordPart>, append: bool },
    /// `N>&M` / `N<&M`: duplicate descriptor M onto N (`-` closes N).
    Dup { fd: i32, file: Vec<WordPart> },
    /// `<<[-]DELIM` here-document.  `body` is the raw text the lexer
    /// collected (one line per source line, each terminated by `\n`; for
    /// `<<-` leading tabs are already stripped).  `expand` is false when the
    /// delimiter word was quoted: the body must not be expanded.
    HereDoc { fd: i32, body: String, expand: bool },
    /// `<<<WORD` here-string: one line with WORD expanded (never globbed or
    /// word-split, even unquoted) plus a trailing newline.
    HereString { fd: i32, word: Vec<WordPart> },
}

/// A shell function definition (`NAME() { ... }` or `function NAME { ... }`).
///
/// The body is a fully-parsed command list; nested function definitions are
/// hoisted into `body.func_defs` so the executor can register every function
/// globally (bash functions are global even when defined inside another).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncDef {
    pub name: String,
    pub body: CommandList,
}

/// A compound command: a command that is built from other commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compound {
    /// `if CONDITION; then BODY; [elif CONDITION; then BODY;]... [else BODY;] fi`
    If {
        branches: Vec<(CommandList, CommandList)>,
        else_body: Option<CommandList>,
    },
    /// `for NAME in WORDS; do BODY; done`
    ///
    /// `words` holds the raw word parts of each iteration value (expanded at
    /// execution time); `var` is the loop variable; `body` runs once per value.
    For {
        var: String,
        words: Vec<Vec<WordPart>>,
        body: CommandList,
    },
    /// `select NAME [in WORDS]; do BODY; done` — like `for`, but presents a
    /// numbered menu and reads the choice from stdin into `REPLY`.  `line` is
    /// the source line of the `select` keyword, used for the identifier
    /// validation diagnostic.
    Select {
        var: String,
        words: Vec<Vec<WordPart>>,
        body: CommandList,
        line: usize,
    },
    /// `for (( INIT ; COND ; INCR )); do BODY; done` — the C-style form.
    ///
    /// The three expressions are raw arithmetic source text; they are
    /// evaluated at execution time by `arith::eval` (INIT once, COND before
    /// every iteration, INCR after every iteration including `continue`).
    /// Any of the three may be empty; an empty COND means "always true".
    ArithFor {
        init: String,
        cond: String,
        incr: String,
        body: CommandList,
    },
    /// `while CONDITION; do BODY; done` (`is_until == false`) and
    /// `until CONDITION; do BODY; done` (`is_until == true`).
    While {
        condition: CommandList,
        body: CommandList,
        is_until: bool,
    },
    /// `case WORD in PATTERN) BODY;; ... esac`
    ///
    /// Each `CaseArm` carries the patterns that select it (a `|`-separated
    /// group becomes several single patterns) and the body that runs when one
    /// of them matches.
    Case {
        word: Vec<WordPart>,
        arms: Vec<CaseArm>,
    },
    /// `{ LIST; }` — a brace group: LIST runs in the current shell (no
    /// subshell), so variable assignments and `cd` inside it persist.
    Group { body: CommandList },
    /// `( LIST )` — a subshell: LIST runs in a forked copy of the shell, so
    /// variable assignments, `cd` and file-descriptor changes inside it do not
    /// leak back to the parent process.
    Subshell { body: CommandList },
    /// `[[ EXPR ]]` — a conditional command.  Its exit status is 0 when the
    /// expression is true and 1 when it is false; the expression itself is
    /// evaluated by `crate::cond`.
    Cond {
        expr: crate::cond::CondExpr,
        /// The 1-based line of the `[[`, used for bash's diagnostics.
        line: usize,
    },
    /// `(( EXPR ))` — an arithmetic command.  The body is evaluated at
    /// execution time; status 0 means the expression evaluated non-zero.
    Arith {
        expr: String,
        line: usize,
    },
}

/// One arm of a `case` command: a set of patterns and the body to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseArm {
    pub patterns: Vec<Vec<WordPart>>,
    pub body: CommandList,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleCommand {
    pub argv: Vec<Vec<WordPart>>, // each word is a list of parts (pre-expansion)
    pub redirs: Vec<Redir>,
    /// Variable assignments of the form `name=value` / `name+=value` appearing
    /// before the command word. `bool` is the append (`+=`) flag.
    pub assignments: Vec<(String, Vec<WordPart>, bool)>,
    /// Array assignments of the form `name=(a b c)` appearing before the
    /// command word.  Each value is a list of element words (each word itself a
    /// list of pre-expansion `WordPart`s); the flag marks the append (`+=`)
    /// form.
    pub array_assignments: Vec<(String, Vec<Vec<WordPart>>, bool)>,
    /// The compound command this simple command stands for.  When it is set
    /// the command has no words of its own.
    pub compound: Option<Compound>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub commands: Vec<SimpleCommand>,
    /// True when the pipeline was suffixed with `&` (run in the background).
    pub background: bool,
}

/// The operator that joins two pipelines of an AND-OR list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndOrOp {
    /// `&&` — the next pipeline runs only when the previous one succeeded.
    And,
    /// `||` — the next pipeline runs only when the previous one failed.
    Or,
}

/// One AND-OR list: the pipelines joined by `&&` / `||`.
///
/// `ops[i]` is the operator that precedes `pipelines[i + 1]`, so
/// `ops.len() == pipelines.len() - 1` for every list with at least one
/// pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndOr {
    pub pipelines: Vec<Pipeline>,
    pub ops: Vec<AndOrOp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandList {
    /// The AND-OR lists, separated by `;`.
    pub andors: Vec<AndOr>,
    /// Function definitions found anywhere in this list (including nested
    /// ones inside compound bodies).  The executor registers each of them into
    /// the shell so that functions are callable regardless of where they were
    /// defined.
    pub func_defs: Vec<FuncDef>,
}

impl AndOr {
    /// An AND-OR list that consists of a single pipeline.
    pub fn single(pipeline: Pipeline) -> Self {
        AndOr {
            pipelines: vec![pipeline],
            ops: Vec::new(),
        }
    }
}

/// Replace every compound command (`if ... fi`, `for ... done`, `while ...`,
/// `until ...`, `case ... esac`) and function definition in the token stream
/// by a single token (`Token::Compound` / `Token::FunctionDef`) that carries
/// the parsed sub-trees.
///
/// Returns the flattened token stream and the (possibly nested) function
/// definitions discovered anywhere in it.
fn parse_compounds(tokens: &[Token]) -> Result<(Vec<Token>, Vec<FuncDef>)> {
    let mut out: Vec<Token> = Vec::new();
    let mut funcs: Vec<FuncDef> = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() {
        match &tokens[i] {
            Token::Reserved(name, kw_line) => match name.as_str() {
                "if" => {
                    let (compound, end) = parse_if_compound(tokens, i)
                        .map_err(|e| wrap_compound_eof(e, "if", *kw_line))?;
                    out.push(Token::Compound(compound));
                    i = end;
                }
                "for" => {
                    let (compound, end) = parse_for_compound(tokens, i)
                        .map_err(|e| wrap_compound_eof(e, "for", *kw_line))?;
                    out.push(Token::Compound(compound));
                    i = end;
                }
                "select" => {
                    let (compound, end) = parse_select_compound(tokens, i, *kw_line)
                        .map_err(|e| wrap_compound_eof(e, "select", *kw_line))?;
                    out.push(Token::Compound(compound));
                    i = end;
                }
                "while" => {
                    let (compound, end) = parse_while_until_compound(tokens, i, false)
                        .map_err(|e| wrap_compound_eof(e, "while", *kw_line))?;
                    out.push(Token::Compound(compound));
                    i = end;
                }
                "until" => {
                    let (compound, end) = parse_while_until_compound(tokens, i, true)
                        .map_err(|e| wrap_compound_eof(e, "until", *kw_line))?;
                    out.push(Token::Compound(compound));
                    i = end;
                }
                "case" => {
                    let (compound, end) = parse_case_compound(tokens, i)
                        .map_err(|e| wrap_compound_eof(e, "case", *kw_line))?;
                    out.push(Token::Compound(compound));
                    i = end;
                }
                "function" => {
                    let (fd, end) = parse_function_kw(tokens, i)?;
                    funcs.push(fd.clone());
                    out.push(Token::FunctionDef(fd.name.clone()));
                    i = end;
                }
                other => {
                    return Err(WashError::syntax(format!(
                        "syntax error near unexpected token `{}'",
                        other
                    )))
                }
            },
            Token::ArithCmd(body, line) => {
                out.push(Token::Compound(Compound::Arith {
                    expr: body.clone(),
                    line: *line,
                }));
                i += 1;
            }
            Token::CondStart(line) => {
                // `[[ EXPR ]]` — a conditional command.  The lexer only emits
                // the opener where a command may start, so no command-position
                // test is needed here.
                let (compound, end) = parse_cond_compound(tokens, i, *line)?;
                out.push(Token::Compound(compound));
                i = end;
            }
            Token::Word(_) => {
                // A `NAME() { ... }` / `NAME () { ... }` function definition
                // starts with an ordinary word; detect and extract it.
                if let Some((fd, end)) = try_parse_funcdef(tokens, i)? {
                    funcs.push(fd.clone());
                    out.push(Token::FunctionDef(fd.name.clone()));
                    i = end;
                } else if at_command_position(&out) && is_word_text(tokens.get(i), "{") {
                    // `{ LIST; }` is a reserved word only where a command may
                    // start.  Extract it as a compound so its body is parsed
                    // once here and can still take part in pipelines / AND-OR.
                    let (group, end) = parse_group_compound(tokens, i)?;
                    out.push(Token::Compound(group));
                    i = end;
                } else if at_command_position(&out) && is_word_text(tokens.get(i), "(") {
                    // `( LIST )` — a subshell.  Like the brace group it may
                    // only begin where a command may start; elsewhere `(` stays
                    // an ordinary word (bash then reports a syntax error).
                    let (subshell, end) = parse_subshell_compound(tokens, i)?;
                    out.push(Token::Compound(subshell));
                    i = end;
                } else if at_command_position(&out) && is_word_text(tokens.get(i), "}") {
                    // A stray `}` in command position is a syntax error, not a
                    // command name (bash: `syntax error near unexpected token }').
                    return Err(WashError::syntax(
                        "syntax error near unexpected token `}'",
                    ));
                } else {
                    out.push(tokens[i].clone());
                    i += 1;
                }
            }
            _ => {
                out.push(tokens[i].clone());
                i += 1;
            }
        }
    }
    Ok((out, funcs))
}

/// True when a command may start at the current position of the flattened
/// token stream.  Bash treats `{` / `}` as reserved words *only* there; in
/// `echo {` the brace is an ordinary argument.
fn at_command_position(out: &[Token]) -> bool {
    match out.last() {
        None => true,
        Some(Token::Semicolon(_) | Token::Pipe | Token::AndAnd | Token::OrOr | Token::Background) => {
            true
        }
        Some(Token::Reserved(..)) => true,
        _ => false,
    }
}

/// Whether a standalone `}` at `close` may terminate the brace group that
/// opened at `start`.
///
/// Bash only recognises `}` as a reserved word in command position, i.e. after
/// a `;`, `&` or newline (the lexer turns newlines into `Semicolon`).  So
/// `{ echo a }` does *not* close the group: the brace is an argument to
/// `echo` and bash later reports an unexpected EOF.  An immediately empty
/// group (`{ }`) is a syntax error for the same reason.
fn group_may_close(tokens: &[Token], start: usize, close: usize) -> bool {
    if close <= start + 1 {
        return false;
    }
    matches!(
        tokens.get(close - 1),
        Some(Token::Semicolon(_) | Token::Background)
    )
}

/// Parse a `{ LIST; }` brace group whose opener is at `start`.
///
/// Returns the compound and the index just after the matching `}`.  Nested
/// groups are tracked with a depth counter over standalone `{` / `}` words,
/// exactly like `parse_func_body`.
fn parse_group_compound(tokens: &[Token], start: usize) -> Result<(Compound, usize)> {
    let mut depth = 0i32;
    let mut j = start + 1;
    let mut close = None;
    while j < tokens.len() {
        if is_word_text(tokens.get(j), "{") {
            depth += 1;
        } else if is_word_text(tokens.get(j), "}") {
            if depth > 0 {
                depth -= 1;
            } else if group_may_close(tokens, start, j) {
                close = Some(j);
                break;
            }
            // Otherwise `}` is an ordinary argument here (e.g. `{ echo a }`);
            // keep scanning for a later `}` that really is in command position.
        }
        j += 1;
    }
    let close = close.ok_or_else(|| {
        // `{ }` is not an empty group in bash: `}` is only a reserved word in
        // command position (after `;`), so here it is the offending token of
        // a two-line yacc diagnostic, not an unterminated group.
        if is_word_text(tokens.get(start + 1), "}") {
            WashError::syntax("syntax error near unexpected token `}'")
        } else {
            WashError::syntax("syntax error: unexpected end of file from `{' command")
        }
    })?;
    let body = parse_command_list(&tokens[start + 1..close])?;
    if body.andors.is_empty() {
        return Err(WashError::syntax(
            "syntax error near unexpected token `;'",
        ));
    }
    Ok((Compound::Group { body }, close + 1))
}

/// Parse a `( LIST )` subshell whose opener is at `start`.
///
/// Returns the compound and the index just after the matching `)`.  Nested
/// subshells are tracked with a depth counter over standalone `(` / `)` words
/// (arithmetic / command substitutions never reach this layer as bare tokens —
/// the lexer folds `$(( ))` and `$( )` into `WordPart`s — so they cannot be
/// mistaken for subshell delimiters).  An empty subshell (`( )`) is a syntax
/// error, matching bash (`syntax error near unexpected token ')'`).
fn parse_subshell_compound(tokens: &[Token], start: usize) -> Result<(Compound, usize)> {
    let mut depth = 0i32;
    let mut j = start + 1;
    let mut close = None;
    while j < tokens.len() {
        if is_word_text(tokens.get(j), "(") {
            depth += 1;
        } else if is_word_text(tokens.get(j), ")") {
            if depth > 0 {
                depth -= 1;
            } else {
                // The first `)` at depth 0 closes this subshell.
                close = Some(j);
                break;
            }
        }
        j += 1;
    }
    let close = close.ok_or_else(|| {
        WashError::syntax("syntax error: unexpected end of file from `(' command")
    })?;
    let body = parse_command_list(&tokens[start + 1..close])?;
    if body.andors.is_empty() {
        return Err(WashError::syntax(
            "syntax error near unexpected token `)'",
        ));
    }
    Ok((Compound::Subshell { body }, close + 1))
}

/// The name of the reserved word at `index`, if the token is one.
fn reserved_name(tokens: &[Token], index: usize) -> Option<String> {
    match tokens.get(index) {
        Some(Token::Reserved(name, _)) => Some(name.clone()),
        _ => None,
    }
}

/// The name and 1-based physical line of the reserved word at `index`.
/// The line is what bash's unterminated-compound diagnostic reports as
/// `... from \`<kw>' command on line <N>` (heredoc body lines are counted
/// by the lexer, matching bash -- qa9 A13).
fn reserved_at(tokens: &[Token], index: usize) -> Option<(String, usize)> {
    match tokens.get(index) {
        Some(Token::Reserved(name, line)) => Some((name.clone(), *line)),
        _ => None,
    }
}

/// The exact diagnostic bash prints for an unterminated compound command
/// (qa9 A01/A07-A11): a single line, no offending-line echo, rc=2, reported
/// at `n_lines + 1` with the *innermost* opener's line.
fn compound_eof_message(kw: &str, open_line: usize) -> String {
    format!(
        "syntax error: unexpected end of file from `{}' command on line {}",
        kw, open_line
    )
}

/// Wrap a bare "unexpected end of file" parser error into the unterminated
/// compound diagnostic for `kw` opened on `open_line`.  Errors that are NOT
/// the bare EOF (nested-compound diagnostics, near-token errors) pass
/// through unchanged so the innermost opener always wins (qa9 A12).
fn wrap_compound_eof(e: WashError, kw: &str, open_line: usize) -> WashError {
    if e.message == "syntax error: unexpected end of file" {
        return WashError::syntax_unclosed(
            DiagnosticFamily::CompoundEof,
            0,
            compound_eof_message(kw, open_line),
        );
    }
    e
}

/// Collect the tokens from `from` up to the next reserved word whose name is
/// listed in `stops`, COPYING any nested compound (`if`/`for`/`while`/`until`/
/// `case`) verbatim so that its internal reserved words are not mistaken for
/// terminators.
///
/// Returns the collected tokens and the index of the reserved word that
/// stopped the scan (the word itself is not consumed).  A nested compound's
/// closer is also a reserved word, but it belongs to the nested construct, so
/// it is copied rather than treated as a stop.
fn split_until_reserved(
    tokens: &[Token],
    from: usize,
    stops: &[&str],
) -> Result<(Vec<Token>, usize)> {
    let mut out: Vec<Token> = Vec::new();
    let mut i = from;
    while i < tokens.len() {
        if let Token::Reserved(name, _) = &tokens[i] {
            match name.as_str() {
                "if" | "for" | "select" | "while" | "until" | "case" => {
                    let end = find_matching_closer(tokens, i)?;
                    out.extend_from_slice(&tokens[i..end]);
                    i = end;
                    continue;
                }
                _ => {}
            }
            if stops.contains(&name.as_str()) {
                return Ok((out, i));
            }
            return Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                name
            )));
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    Err(WashError::syntax("syntax error: unexpected end of file"))
}

/// Find the index just after the closing reserved word of the compound that
/// begins at `start` (the opener is the reserved word there).
///
/// Nesting is tracked with a small stack.  `do` is *not* a separate nesting
/// level — a `for`/`while`/`until` and its `do` group share the single
/// `done` that closes them both, so only the loop opener (`for`/`while`/
/// `until`) pairs with `done`; `if` pairs with `fi` and `case` pairs with
/// `esac`.
fn find_matching_closer(tokens: &[Token], start: usize) -> Result<usize> {
    let opener = reserved_name(tokens, start).ok_or_else(|| {
        WashError::syntax("internal error: find_matching_closer on a non-reserved token")
    })?;
    let kind = match opener.as_str() {
        "if" => "if",
        "for" | "select" | "while" | "until" => "loop",
        "case" => "case",
        _ => {
            return Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                opener
            )))
        }
    };
    // A C-style `for (( ... ))` header must be followed by `do`.  bash's
    // grammar rejects the header before the compound is scanned, so the
    // offending token is reported even when the compound appears nested in
    // another command's condition (e.g. `if for ((...)); then ...`).
    let is_cfor = opener == "for" && matches!(tokens.get(start + 1), Some(Token::ArithCmd(..)));
    if is_cfor {
        arith_for_do_index(tokens, start)?;
    }
    // A C-style `for` or a `select` that reaches a closer which does not
    // match the current nesting level is a yacc-layer error in bash: it
    // reports the offending token (e.g. `for ((...)); do ...; fi` -> near
    // `fi`, `select x in a; do ...; fi` -> near `fi`), not the generic
    // "unexpected EOF".  Plain loops keep the historical EOF behaviour.
    let is_select = opener == "select";
    let near_on_mismatch = is_cfor || is_select;
    let cfor_near = |tok: &str| -> WashError {
        WashError::syntax(format!("syntax error near unexpected token `{}'", tok))
    };
    let mut stack: Vec<&str> = vec![kind];
    let mut i = start + 1;
    while i < tokens.len() {
        if let Token::Reserved(name, _) = &tokens[i] {
            match name.as_str() {
                "if" | "for" | "select" | "while" | "until" | "case" => {
                    stack.push(match name.as_str() {
                        "if" => "if",
                        "case" => "case",
                        _ => "loop",
                    });
                }
                "fi" => {
                    if *stack.last().unwrap_or(&"") == "if" {
                        stack.pop();
                    } else if near_on_mismatch {
                        return Err(cfor_near(name));
                    }
                }
                "esac" => {
                    if *stack.last().unwrap_or(&"") == "case" {
                        stack.pop();
                    } else if near_on_mismatch {
                        return Err(cfor_near(name));
                    }
                }
                "done" => {
                    if *stack.last().unwrap_or(&"") == "loop" {
                        stack.pop();
                    } else if near_on_mismatch {
                        return Err(cfor_near(name));
                    }
                }
                _ => {}
            }
            if stack.is_empty() {
                return Ok(i + 1);
            }
        }
        i += 1;
    }
    // The compound opened at `start` was never closed.  This function sits
    // below every nested compound scan, so this error is the *innermost*
    // unterminated opener -- exactly what bash reports (qa9 A12).
    let (kw, open_line) = reserved_at(tokens, start)
        .unwrap_or_else(|| (String::new(), 1));
    Err(WashError::syntax_unclosed(
        DiagnosticFamily::CompoundEof,
        0,
        compound_eof_message(&kw, open_line),
    ))
}

/// Parse the `[[ ... ]]` conditional command that starts at `start` (the
/// `[[` token itself).  Returns the parsed compound and the index just after
/// the closing `]]`.
fn parse_cond_compound(tokens: &[Token], start: usize, line: usize) -> Result<(Compound, usize)> {
    let (expr, end) = crate::cond::parse_cond(tokens, start + 1, line)?;
    Ok((Compound::Cond { expr, line }, end))
}

/// Parse the `if` construct that starts at `start` (the `if` token itself).
///
/// Returns the parsed compound and the index just after the closing `fi`.
fn parse_if_compound(tokens: &[Token], start: usize) -> Result<(Compound, usize)> {
    let mut branches: Vec<(CommandList, CommandList)> = Vec::new();
    let mut else_body: Option<CommandList> = None;
    let mut i = start + 1; // skip the `if`

    loop {
        // The condition list ends at the `then`.
        let (condition, next) = split_until_reserved(tokens, i, &["then"])?;
        // The body list ends at the next `elif`, `else` or `fi`.
        let (body, next) = split_until_reserved(tokens, next + 1, &["elif", "else", "fi"])?;
        branches.push((parse_command_list(&condition)?, parse_command_list(&body)?));

        match reserved_name(tokens, next).as_deref() {
            Some("elif") => {
                i = next + 1;
                continue;
            }
            Some("else") => {
                let (body, next) = split_until_reserved(tokens, next + 1, &["fi"])?;
                else_body = Some(parse_command_list(&body)?);
                i = next + 1;
                break;
            }
            Some("fi") => {
                i = next + 1;
                break;
            }
            _ => return Err(WashError::syntax("syntax error near unexpected token")),
        }
    }

    Ok((Compound::If { branches, else_body }, i))
}

/// Where a loop's body begins after its header.
///
/// bash's grammar accepts two spellings for the body of `for` / `for (( ))`:
/// the usual `do LIST; done`, and a brace group `{ LIST; }` in place of both
/// keywords (`for i in 1 2; { echo "$i"; }`).  The latter is a real,
/// documented boundary that used to be rejected as a syntax error.
enum LoopBodyStart {
    Do(usize),
    Brace(usize),
}

/// Reject an empty loop body.  bash treats `do; done`, `do ;; done` and
/// `do done` as yacc errors, but it names the offending token according to
/// what is actually between `do` and the closer:
///
///   * a typed `;`          -> near `;'`
///   * a `;;` pair          -> near `;;'
///   * only newlines (or nothing) -> near the closer (`done`)
///
/// Only separators may appear between `do` and its closer for the body to
/// count as empty; any other token means a real body.
fn reject_empty_loop_body(body: &[Token], closer: Option<&Token>) -> Result<()> {
    let only_seps = !body.is_empty() && body.iter().all(|t| matches!(t, Token::Semicolon(_)));
    if !body.is_empty() && !only_seps {
        return Ok(());
    }
    let closer_name = || match closer {
        Some(Token::Reserved(n, _)) => n.clone(),
        _ => "done".to_string(),
    };
    let closer_line = match closer {
        Some(Token::Reserved(_, line)) => *line,
        _ => 1,
    };
    // bash reports the physical line of the first offending separator when a
    // typed `;` / `;;` is present (`while true; do; \ndone` -> line 1), and the
    // closer's line when the body holds only newline separators or nothing
    // (`while true; do\ndone` -> line 2).  NearToken makes the shell echo that
    // source line instead of guessing from the compound opener.
    let first_typed = body.iter().find_map(|t| match t {
        Token::Semicolon(o)
            if matches!(o.kind, SemicolonKind::Literal | SemicolonKind::DoubleLead) =>
        {
            Some(o)
        }
        _ => None,
    });
    let has_double = body.iter().any(|t| {
        matches!(t, Token::Semicolon(o) if o.kind == SemicolonKind::DoubleLead)
    });
    let (line, shown) = match first_typed {
        Some(o) => (
            o.line,
            if has_double {
                ";;".to_string()
            } else {
                ";".to_string()
            },
        ),
        None => (closer_line, closer_name()),
    };
    Err(WashError::syntax_unclosed(
        DiagnosticFamily::NearToken,
        line,
        format!("syntax error near unexpected token `{}'", shown),
    ))
}

/// Return the parsed body and the index just after its closer.
///
/// Keeping this in one place makes the `do ... done` and `{ ... }` forms share
/// the same downstream execution model: the brace group's command list becomes
/// the loop body verbatim (a brace group runs in the current shell anyway).
fn finish_loop_body(
    tokens: &[Token],
    start: usize,
    body_start: LoopBodyStart,
) -> Result<(CommandList, usize)> {
    match body_start {
        LoopBodyStart::Do(do_index) => {
            let end = find_matching_closer(tokens, start)?; // just after `done`
            let done_index = end - 1;
            let body_tokens = &tokens[do_index + 1..done_index];
            // bash rejects an empty loop body (`do; done` / `do done`) as a
            // yacc error near the offending token.  Applies to every loop
            // form: for / select / while / until.
            reject_empty_loop_body(body_tokens, tokens.get(done_index))?;
            Ok((parse_command_list(body_tokens)?, end))
        }
        LoopBodyStart::Brace(brace_start) => {
            let (group, end) = parse_group_compound(tokens, brace_start)?;
            match group {
                Compound::Group { body } => Ok((body, end)),
                _ => unreachable!("parse_group_compound always returns a Group"),
            }
        }
    }
}

/// Parse `for NAME [in WORDS] ; do BODY ; done` starting at the `for` token.
fn parse_for_compound(tokens: &[Token], start: usize) -> Result<(Compound, usize)> {
    // The C-style form begins with a single `(( ... ))` token emitted by the
    // lexer (it consumes the three `;`-separated clauses verbatim).
    if let Some(Token::ArithCmd(expr, _)) = tokens.get(start + 1) {
        return parse_arith_for_compound(tokens, start, expr);
    }
    // NAME is the word right after `for`.
    let name = match tokens.get(start + 1) {
        Some(Token::Word(parts)) if is_plain_ident(parts) => plain_text(parts),
        // bash's yacc layer names the actual token where the loop variable
        // should be (`for; do ...` -> `;`, not a fixed "expected" message).
        Some(tok) if !matches!(tok, Token::Word(_)) => {
            return Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                token_near_text(tok)
            )))
        }
        _ => {
            return Err(WashError::syntax(
                "syntax error near unexpected token (expected a loop variable)",
            ))
        }
    };

    // `for NAME [in WORDS] ; BODY` — `in WORDS` is optional; without it bash
    // iterates the positional parameters (`for i; do ...; done`).  Both body
    // spellings are handled below.
    let mut words: Vec<Vec<WordPart>> = Vec::new();
    let mut i = start + 2;
    let body_start;
    if is_word_text(tokens.get(i), "in") {
        i += 1;
        // Collect iteration words up to `do` or a brace group.  A `;` (or a
        // newline, which the lexer also turns into `;`) between the word list
        // and the body is optional (`for i in a b c; do ...`).
        loop {
            if i >= tokens.len() {
                return Err(WashError::syntax(
                    "syntax error: unexpected end of file",
                ));
            }
            match &tokens[i] {
                Token::Word(parts) if is_plain_text(parts) && plain_text(parts) == "}" => {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token `}'",
                    ));
                }
                Token::Word(parts) => {
                    words.push(parts.clone());
                    i += 1;
                }
                Token::Semicolon(_) => {
                    // end of the word list; `do` or `{` follows.
                    i += 1;
                    if is_word_text(tokens.get(i), "{") {
                        body_start = LoopBodyStart::Brace(i);
                        break;
                    }
                }
                Token::Reserved(r, _) if r == "do" => {
                    body_start = LoopBodyStart::Do(i);
                    break;
                }
                _ => {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token (expected `do')",
                    ))
                }
            }
        }
    } else {
        // No `in`: `do` may follow NAME directly, but a brace group still
        // needs the separating `;` (`for i { ...; }` is a yacc error).
        if matches!(tokens.get(i), Some(Token::Semicolon(_))) {
            i += 1;
        } else if is_word_text(tokens.get(i), "{") {
            return Err(WashError::syntax(
                "syntax error near unexpected token `{'",
            ));
        }
        if is_word_text(tokens.get(i), "{") {
            body_start = LoopBodyStart::Brace(i);
        } else if matches!(tokens.get(i), Some(Token::Reserved(r, _)) if r == "do") {
            body_start = LoopBodyStart::Do(i);
        } else {
            return Err(WashError::syntax(
                "syntax error near unexpected token (expected `do')",
            ));
        }
        // Without `in WORDS`, bash iterates the positional parameters as if
        // `"$@"` had been written (each parameter stays one field, even when
        // it contains IFS whitespace).  A quoted `$@` word has that exact
        // expansion semantics in `expand_word`.
        words.push(vec![WordPart::Var("@".to_string(), true)]);
    }

    let (body, end) = finish_loop_body(tokens, start, body_start)?;
    Ok((
        Compound::For {
            var: name,
            words,
            body,
        },
        end,
    ))
}

/// Parse `select NAME [in WORDS] ; do BODY ; done` starting at the `select`
/// token.  Unlike `for`, bash accepts *any* word as NAME here and defers the
/// identifier check to execution time (so `select 1 in a; do ...; done` parses
/// and then reports `` `1': not a valid identifier'') — that is why the
/// executor carries `line` and calls `is_valid_ident` itself.
fn parse_select_compound(tokens: &[Token], start: usize, line: usize) -> Result<(Compound, usize)> {
    // NAME is the word right after `select`; keep its raw source text so the
    // execution-time diagnostic renders it the way bash does.
    let var = match tokens.get(start + 1) {
        Some(Token::Word(parts)) => word_source_text(parts),
        // `select; do ...` is a yacc error naming the offending token,
        // e.g. `select; do echo hi; done` -> near `;`.
        Some(tok) if !matches!(tok, Token::Word(_)) => {
            return Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                token_near_text(tok)
            )))
        }
        _ => {
            return Err(WashError::syntax(
                "syntax error near unexpected token (expected a loop variable)",
            ))
        }
    };

    let mut words: Vec<Vec<WordPart>> = Vec::new();
    let mut i = start + 2;
    let body_start;
    if is_word_text(tokens.get(i), "in") {
        i += 1;
        loop {
            if i >= tokens.len() {
                return Err(WashError::syntax(
                    "syntax error: unexpected end of file",
                ));
            }
            match &tokens[i] {
                Token::Word(parts) if is_plain_text(parts) && plain_text(parts) == "}" => {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token `}'",
                    ));
                }
                Token::Word(parts) => {
                    words.push(parts.clone());
                    i += 1;
                }
                Token::Semicolon(_) => {
                    i += 1;
                    if is_word_text(tokens.get(i), "{") {
                        body_start = LoopBodyStart::Brace(i);
                        break;
                    }
                    if matches!(tokens.get(i), Some(Token::Reserved(r, _)) if r == "do") {
                        body_start = LoopBodyStart::Do(i);
                        break;
                    }
                    // After the word list's `;`, bash requires `do` or `{`.
                    // Anything else is a yacc error near that token.
                    let shown = match tokens.get(i) {
                        Some(Token::Word(parts)) => plain_text(parts),
                        Some(Token::Reserved(n, _)) => n.clone(),
                        Some(Token::Semicolon(_)) => ";".to_string(),
                        _ => return Err(WashError::syntax("syntax error: unexpected end of file")),
                    };
                    return Err(WashError::syntax(format!(
                        "syntax error near unexpected token `{}'",
                        shown
                    )));
                }
                Token::Reserved(r, _) if r == "do" => {
                    body_start = LoopBodyStart::Do(i);
                    break;
                }
                _ => {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token (expected `do')",
                    ))
                }
            }
        }
    } else {
        // `select NAME` with no `in`: a following bare word is a yacc error
        // (bash: `select x y in ...` -> near `y`); select does not treat extra
        // words as an implicit list the way a malformed `for` might.
        if let Some(shown) = match tokens.get(i) {
            Some(Token::Word(parts)) => Some(plain_text(parts)),
            Some(Token::Reserved(n, _)) => Some(n.clone()),
            _ => None,
        } {
            return Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                shown
            )));
        }
        if matches!(tokens.get(i), Some(Token::Semicolon(_))) {
            i += 1;
        }
        if is_word_text(tokens.get(i), "{") {
            body_start = LoopBodyStart::Brace(i);
        } else if matches!(tokens.get(i), Some(Token::Reserved(r, _)) if r == "do") {
            body_start = LoopBodyStart::Do(i);
        } else {
            return Err(WashError::syntax(
                "syntax error near unexpected token (expected `do')",
            ));
        }
        // Without `in`, select iterates the positional parameters as `"$@"`.
        words.push(vec![WordPart::Var("@".to_string(), true)]);
    }

    let (body, end) = finish_loop_body(tokens, start, body_start)?;
    Ok((
        Compound::Select {
            var,
            words,
            body,
            line,
        },
        end,
    ))
}

/// Split the body of `(( INIT ; COND ; INCR ))` into its clauses.
///
/// Semicolons inside quotes or nested brackets belong to the expression and
/// are not separators.  bash counts the clauses the same way (`nsemi` in
/// `make_arith_for_command`): the number of `;` plus one.  A well-formed
/// header has exactly three; any other count is a syntax error.
fn split_arith_clauses(expr: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::with_capacity(3);
    let mut cur = String::new();
    let mut depth: i32 = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in expr.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        if let Some(q) = quote {
            cur.push(ch);
            if ch == q {
                quote = None;
            } else if q == '"' && ch == '\\' {
                escaped = true;
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                cur.push(ch);
            }
            '(' | '[' => {
                depth += 1;
                cur.push(ch);
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(ch);
            }
            ';' if depth == 0 => {
                // bash strips leading whitespace from each clause but keeps
                // trailing whitespace (it echoes the raw text in diagnostics).
                parts.push(cur.trim_start().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    parts.push(cur.trim_start().to_string());
    parts
}

/// Locate the `do` that opens the body of a C-style `for (( ... ))`.
///
/// bash's grammar expects `do` right after the header, so a header followed
/// by anything else is a *grammar* error ("syntax error near unexpected
/// token `X'"), which is reported before the header's clause count is ever
/// examined.  Returns the index of the `do` token.
fn arith_for_do_index(tokens: &[Token], start: usize) -> Result<usize> {
    let mut i = start + 2;
    if matches!(tokens.get(i), Some(Token::Semicolon(_))) {
        i += 1;
    }
    match tokens.get(i) {
        None => {
            // End of input inside the header: bash reports the unterminated
            // compound, naming the innermost opener.
            let (kw, open_line) = reserved_at(tokens, start).unwrap_or_else(|| (String::new(), 1));
            Err(WashError::syntax_unclosed(
                DiagnosticFamily::CompoundEof,
                0,
                compound_eof_message(&kw, open_line),
            ))
        }
        Some(Token::Reserved(r, _)) if r == "do" => Ok(i),
        // bash's grammar recognises `do` by its syntactic position, not only
        // where the lexer happened to mark a reserved word.  After a C-style
        // header the `))` is an unambiguous terminator, so bash accepts `do`
        // even with no separating `;`.  In that case the lexer did not see a
        // command position and emitted `do` as an ordinary word.  Accept it
        // by literal text (but never a quoted `do`, which is not a keyword).
        Some(tok) if is_word_text(Some(tok), "do") => Ok(i),
        Some(tok) => {
            // bash names the token it could not use there.
            Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                token_near_text(tok)
            )))
        }
    }
}

/// Locate the body opener of a C-style `for`.  bash accepts both `do ... done`
/// and a brace group `{ ... }` after the header; the latter has no `done`.
fn arith_for_body_index(tokens: &[Token], start: usize) -> Result<LoopBodyStart> {
    let mut i = start + 2;
    if matches!(tokens.get(i), Some(Token::Semicolon(_))) {
        i += 1;
    }
    if is_word_text(tokens.get(i), "{") {
        return Ok(LoopBodyStart::Brace(i));
    }
    arith_for_do_index(tokens, start).map(LoopBodyStart::Do)
}

/// Parse `for (( INIT ; COND ; INCR )); do BODY; done` starting at `for`.
fn parse_arith_for_compound(
    tokens: &[Token],
    start: usize,
    expr: &str,
) -> Result<(Compound, usize)> {
    // Grammar level: the header must be followed by `do` or `{`.
    let body_start = arith_for_body_index(tokens, start)?;

    // Action level: bash's `make_arith_for_command` then checks that the
    // header holds exactly three clauses and reports the raw `((...))`.
    let clauses = split_arith_clauses(expr);
    if clauses.len() != 3 {
        // bash: "syntax error: arithmetic expression required" for too few
        // clauses, "syntax error: `;' unexpected" for too many, then the raw
        // `((...))` fragment on a second line.
        let open_line = match tokens.get(start) {
            Some(Token::Reserved(_, line)) => *line,
            _ => 1,
        };
        return Err(WashError::syntax_arith_for_header(
            open_line,
            expr.to_string(),
            clauses.len() > 3,
        ));
    }
    let mut clauses = clauses.into_iter();
    let init = clauses.next().unwrap_or_default();
    let cond = clauses.next().unwrap_or_default();
    let incr = clauses.next().unwrap_or_default();

    let (body, end) = finish_loop_body(tokens, start, body_start)?;
    Ok((
        Compound::ArithFor {
            init,
            cond,
            incr,
            body,
        },
        end,
    ))
}

/// Parse `while CONDITION ; do BODY ; done` (`is_until == false`) or
/// `until CONDITION ; do BODY ; done` (`is_until == true`) from the opener.
fn parse_while_until_compound(
    tokens: &[Token],
    start: usize,
    is_until: bool,
) -> Result<(Compound, usize)> {
    let do_index = {
        let mut i = start + 1;
        loop {
            if i >= tokens.len() {
                return Err(WashError::syntax(format!(
                    "syntax error: unexpected end of file",
                )));
            }
            match &tokens[i] {
                Token::Reserved(r, _) if r == "do" => break i,
                Token::Reserved(..) => {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token (expected `do')",
                    ))
                }
                _ => i += 1,
            }
        }
    };
    let end = find_matching_closer(tokens, start)?; // just after `done`
    let done_index = end - 1;
    let body_tokens = &tokens[do_index + 1..done_index];
    // `while`/`until` need the same empty-body rejection as `for`/`select`:
    // without it `while true; do ; ; done` parses as an endless no-op loop
    // instead of yacc-failing near the stray separator / closer.
    reject_empty_loop_body(body_tokens, tokens.get(done_index))?;
    let condition = parse_command_list(&tokens[start + 1..do_index])?;
    let body = parse_command_list(body_tokens)?;
    Ok((
        Compound::While {
            condition,
            body,
            is_until,
        },
        end,
    ))
}

/// Parse `case WORD in PATTERN) BODY ;; ... esac` from the `case` token.
fn parse_case_compound(tokens: &[Token], start: usize) -> Result<(Compound, usize)> {
    let word = match tokens.get(start + 1) {
        Some(Token::Word(parts)) => parts.clone(),
        _ => {
            return Err(WashError::syntax(
                "syntax error near unexpected token (expected a case word)",
            ))
        }
    };

    // Locate the `in` keyword.
    let mut in_index = start + 2;
    while in_index < tokens.len() {
        if is_word_text(tokens.get(in_index), "in") {
            break;
        }
        in_index += 1;
    }
    if !is_word_text(tokens.get(in_index), "in") {
        return Err(WashError::syntax(
            "syntax error near unexpected token (expected `in')",
        ));
    }

    let mut arms: Vec<CaseArm> = Vec::new();
    let mut i = in_index + 1;
    loop {
        // End of the case: a top-level `esac`.
        if matches!(reserved_name(tokens, i).as_deref(), Some("esac")) {
            break;
        }
        // Collect patterns until the `)` delimiter (emitted as its own word by
        // the lexer).  A `|` between patterns is tokenized as `Token::Pipe` and
        // acts as a separator, so `a|b|c)` becomes the three patterns `a`, `b`,
        // `c`.  A `|` *inside* a single word (e.g. a quoted `a|b`) is also a
        // separator in bash, so we split each word on `|` as well.
        let mut patterns: Vec<Vec<WordPart>> = Vec::new();
        let mut current: Vec<WordPart> = Vec::new();
        let mut saw_pipe = false;
        loop {
            if i >= tokens.len() {
                return Err(WashError::syntax(
                    "syntax error: unexpected end of file",
                ));
            }
            match &tokens[i] {
                Token::Word(parts) if is_plain_text(parts) => {
                    let text = plain_text(parts);
                    // The arm head ends at the `)` delimiter.
                    if text == ")" {
                        if !current.is_empty() || saw_pipe {
                            patterns.push(std::mem::take(&mut current));
                        } else {
                            return Err(WashError::syntax(
                                "syntax error near unexpected token (expected a pattern)",
                            ));
                        }
                        i += 1;
                        break;
                    }
                    // A `)` suffix (leftover from a non-delimited tokenizer) is
                    // also treated as the terminator; any other text is appended
                    // to the current pattern, split on `|`.
                    let head = text.strip_suffix(')').unwrap_or(&text);
                    for seg in head.split('|') {
                        current.push(WordPart::Text(seg.to_string(), false));
                    }
                    i += 1;
                }
                Token::Pipe => {
                    // Flush the current segment as a completed pattern.
                    patterns.push(std::mem::take(&mut current));
                    saw_pipe = true;
                    i += 1;
                }
                _ => {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token (expected `)')",
                    ))
                }
            }
        }
        // The body runs until a top-level `;;` (two semicolons) or `esac`.
        let body_start = i;
        let mut body_end = i;
        let mut depth = 0i32;
        while body_end < tokens.len() {
            match &tokens[body_end] {
                Token::Reserved(r, _) if matches!(r.as_str(), "if" | "for" | "while" | "until" | "case") => {
                    depth += 1;
                }
                Token::Reserved(r, _)
                    if matches!(r.as_str(), "fi" | "esac" | "done") && depth > 0 =>
                {
                    depth -= 1;
                }
                Token::Semicolon(_)
                    if depth == 0
                        && body_end + 1 < tokens.len()
                        && matches!(tokens.get(body_end + 1), Some(Token::Semicolon(_))) =>
                {
                    break;
                }
                Token::Reserved(r, _) if r == "esac" && depth == 0 => break,
                _ => {}
            }
            body_end += 1;
        }
        let body = parse_command_list(&tokens[body_start..body_end])?;
        arms.push(CaseArm { patterns, body });

        // Advance past the `;;` terminator if present.
        if body_end < tokens.len()
            && matches!(tokens.get(body_end), Some(Token::Semicolon(_)))
            && body_end + 1 < tokens.len()
            && matches!(tokens.get(body_end + 1), Some(Token::Semicolon(_)))
        {
            i = body_end + 2;
        } else {
            i = body_end;
        }
    }

    let end = find_matching_closer(tokens, start)?; // just after `esac`
    Ok((Compound::Case { word, arms }, end))
}

/// Parse a `function NAME { BODY }` (or `function NAME () { BODY }`) definition
/// starting at the `function` reserved word.
fn parse_function_kw(tokens: &[Token], start: usize) -> Result<(FuncDef, usize)> {
    let name = match tokens.get(start + 1) {
        Some(Token::Word(parts)) if is_plain_ident(parts) => plain_text(parts),
        _ => {
            return Err(WashError::syntax(
                "syntax error near unexpected token (expected a function name)",
            ))
        }
    };
    let mut i = start + 2;
    // Optional `()` after the name (bash allows `function NAME () { ... }`).
    if matches!(tokens.get(i), Some(Token::Word(parts)) if is_plain_text(parts) && plain_text(parts) == "()")
    {
        i += 1;
    }
    // Skip the `{` that opens the function body so `parse_func_body` scans the
    // body (it expects `open_index` to be just past the opener).
    if matches!(tokens.get(i), Some(Token::Word(parts)) if is_plain_text(parts) && plain_text(parts) == "{")
    {
        i += 1;
    }
    let (body, end) = parse_func_body(tokens, i)?;
    Ok((FuncDef { name, body }, end))
}

/// Detect and parse a `NAME() { BODY }` or `NAME () { BODY }` definition at
/// index `i` (a plain word).  Returns the definition and the index just after
/// the closing `}` when one is found, otherwise `None`.
fn try_parse_funcdef(tokens: &[Token], i: usize) -> Result<Option<(FuncDef, usize)>> {
    // Compact form: `NAME()` directly followed by `{`.
    if let Some(Token::Word(parts)) = tokens.get(i) {
        if is_plain_text(parts) {
            let text = plain_text(parts);
            if let Some(name) = ident_in_parens(&text) {
                if matches!(tokens.get(i + 1), Some(Token::Word(p)) if is_plain_text(p) && plain_text(p) == "{")
                {
                    let (body, end) = parse_func_body(tokens, i + 2)?;
                    return Ok(Some((FuncDef { name, body }, end)));
                }
            }
        }
    }
    // Spaced form: `NAME` `(` `)` `{`.
    if let (
        Some(Token::Word(a)),
        Some(Token::Word(b)),
        Some(Token::Word(c)),
        Some(Token::Word(d)),
    ) = (tokens.get(i), tokens.get(i + 1), tokens.get(i + 2), tokens.get(i + 3))
    {
        if is_plain_ident(a)
            && is_plain_text(b)
            && plain_text(b) == "("
            && is_plain_text(c)
            && plain_text(c) == ")"
            && is_plain_text(d)
            && plain_text(d) == "{"
        {
            let (body, end) = parse_func_body(tokens, i + 4)?;
            return Ok(Some((FuncDef {
                name: plain_text(a),
                body,
            }, end)));
        }
    }
    Ok(None)
}

/// Parse a function body delimited by `{ ... }` whose opening brace is at
/// `open_index` (the token after `{`).  Returns the parsed body and the index
/// just after the matching `}`.
fn parse_func_body(tokens: &[Token], open_index: usize) -> Result<(CommandList, usize)> {
    // Find the matching `}` by counting standalone `{` / `}` words.
    let mut depth = 0i32;
    let mut j = open_index;
    let mut found = false;
    while j < tokens.len() {
        if let Some(Token::Word(parts)) = tokens.get(j) {
            if is_plain_text(parts) {
                let t = plain_text(parts);
                if t == "{" {
                    depth += 1;
                } else if t == "}" {
                    if depth == 0 {
                        found = true;
                        break;
                    }
                    depth -= 1;
                }
            }
        }
        j += 1;
    }
    if !found {
        return Err(WashError::syntax("syntax error: unexpected end of file (expected `}')"));
    }
    let body = parse_command_list(&tokens[open_index..j])?;
    Ok((body, j + 1))
}

/// True when a word is a single, unquoted identifier-looking text part (no
/// expansions), e.g. a loop variable or function name.
fn is_plain_ident(parts: &[WordPart]) -> bool {
    match parts {
        [WordPart::Text(t, false)] => is_valid_ident(t),
        _ => false,
    }
}

/// True when a word is a single, unquoted text part (no expansions).
fn is_plain_text(parts: &[WordPart]) -> bool {
    matches!(parts, [WordPart::Text(_, false)])
}

/// True when `tok` is a single, unquoted word whose text equals `text`.  Used
/// to recognise positional keywords such as `in` that are NOT reserved words
/// (they only ever appear inside `for` / `case` bodies, so the lexer leaves
/// them as ordinary words and the parser recognises them by their text).
fn is_word_text(tok: Option<&Token>, text: &str) -> bool {
    match tok {
        Some(Token::Word(parts)) if is_plain_text(parts) => plain_text(parts) == text,
        _ => false,
    }
}

/// Concatenate a word's plain text parts (used for reserved-keyword words such
/// as `f()`, `(`, `)`, `{`, `}`).
fn plain_text(parts: &[WordPart]) -> String {
    let mut s = String::new();
    for p in parts {
        if let WordPart::Text(t, _) = p {
            s.push_str(t);
        }
    }
    s
}

/// Reconstruct a word's source spelling for diagnostics (e.g. the NAME of a
/// `select` that fails identifier validation).  Quoted parts are re-wrapped so
/// `select "x" in ...` renders as `"x"`.  This is intentionally best-effort:
/// single-quoted forms collapse to the double-quoted spelling, a documented
/// narrow edge (see docs/compatibility.md).
fn word_source_text(parts: &[WordPart]) -> String {
    let mut s = String::new();
    for p in parts {
        match p {
            WordPart::Text(t, true) => {
                s.push('"');
                s.push_str(t);
                s.push('"');
            }
            WordPart::Text(t, false) => s.push_str(t),
            WordPart::Var(n, _) => {
                s.push('$');
                s.push_str(n);
            }
            WordPart::Braced(b, _) => {
                s.push_str("${");
                s.push_str(b);
                s.push('}');
            }
            WordPart::CmdSub(b, _) => {
                s.push_str("$(");
                s.push_str(b);
                s.push(')');
            }
            WordPart::Backtick(b, _) => {
                s.push('`');
                s.push_str(b);
                s.push('`');
            }
            WordPart::Arith(b, _) => {
                s.push_str("((");
                s.push_str(b);
                s.push_str("))");
            }
            WordPart::Tilde => s.push('~'),
            WordPart::Brace(alts) => {
                s.push('{');
                s.push_str(&alts.join(","));
                s.push('}');
            }
        }
    }
    s
}

/// If `text` is `IDENT(...)` (an identifier followed by balanced parens and
/// nothing else), return the identifier.  `f` and `f()` both yield `f`.
fn ident_in_parens(text: &str) -> Option<String> {
    let open = text.find('(')?;
    if text.ends_with(')') {
        let pre = &text[..open];
        // Allow an optional closing paren only; reject extra characters.
        let post = &text[open + 1..text.len() - 1];
        if post.is_empty() && is_valid_ident(pre) {
            return Some(pre.to_string());
        }
    }
    None
}

/// True when `stage` is exactly the bare word `coproc` (a reserved word that
/// requires a command to follow it; bash rejects `coproc ;` and a trailing
/// `coproc` with a syntax error).
fn is_bare_coproc(stage: &[Token]) -> bool {
    if stage.len() != 1 {
        return false;
    }
    match &stage[0] {
        Token::Word(parts) => is_plain_text(parts) && plain_text(parts) == "coproc",
        _ => false,
    }
}

pub fn parse_command_list(tokens: &[Token]) -> Result<CommandList> {
    // A trailing `|`, `&&` or `||` has no command after it.  bash reports a
    // single-line EOF error (no offending-line echo); silently dropping the
    // operator would execute only the left-hand side with rc=0.
    if matches!(
        tokens.last(),
        Some(Token::Pipe) | Some(Token::AndAnd) | Some(Token::OrOr)
    ) {
        return Err(WashError::syntax("syntax error: unexpected end of file"));
    }
    // Extract compounds + function definitions first: each becomes a single
    // token that carries the parsed branches.
    let (tokens, funcs) = parse_compounds(tokens)?;
    let mut andors: Vec<AndOr> = Vec::new();
    let mut pipelines: Vec<Pipeline> = Vec::new();
    let mut ops: Vec<AndOrOp> = Vec::new();
    let mut current_cmds: Vec<SimpleCommand> = Vec::new();
    let mut stage: Vec<Token> = Vec::new();

    for t in &tokens {
        match t {
            Token::Pipe => {
                current_cmds.push(parse_simple(&stage)?);
                stage.clear();
            }
            Token::AndAnd | Token::OrOr => {
                if !stage.is_empty() {
                    current_cmds.push(parse_simple(&stage)?);
                    stage.clear();
                }
                if !current_cmds.is_empty() {
                    pipelines.push(Pipeline {
                        commands: std::mem::take(&mut current_cmds),
                        background: false,
                    });
                }
                ops.push(match t {
                    Token::AndAnd => AndOrOp::And,
                    _ => AndOrOp::Or,
                });
            }
            Token::Semicolon(_) => {
                if !stage.is_empty() {
                    if is_bare_coproc(&stage) {
                        return Err(WashError::syntax(
                            "syntax error near unexpected token `;'",
                        ));
                    }
                    current_cmds.push(parse_simple(&stage)?);
                    stage.clear();
                }
                if !current_cmds.is_empty() {
                    pipelines.push(Pipeline {
                        commands: std::mem::take(&mut current_cmds),
                        background: false,
                    });
                }
                flush_andor(&mut andors, &mut pipelines, &mut ops)?;
            }
            Token::Background => {
                if !stage.is_empty() {
                    if is_bare_coproc(&stage) {
                        return Err(WashError::syntax(
                            "syntax error near unexpected token `&'",
                        ));
                    }
                    current_cmds.push(parse_simple(&stage)?);
                    stage.clear();
                }
                if !current_cmds.is_empty() {
                    pipelines.push(Pipeline {
                        commands: std::mem::take(&mut current_cmds),
                        background: true,
                    });
                }
                flush_andor(&mut andors, &mut pipelines, &mut ops)?;
            }
            // A function definition is a whole command that registers itself;
            // it produces no executable, so it is skipped here.
            Token::FunctionDef(_) => {
                if !stage.is_empty() {
                    current_cmds.push(parse_simple(&stage)?);
                    stage.clear();
                }
                if !current_cmds.is_empty() {
                    pipelines.push(Pipeline {
                        commands: std::mem::take(&mut current_cmds),
                        background: false,
                    });
                }
                flush_andor(&mut andors, &mut pipelines, &mut ops)?;
            }
            _ => stage.push(t.clone()),
        }
    }

    if !stage.is_empty() {
        if is_bare_coproc(&stage) {
            return Err(WashError::syntax(
                "syntax error near unexpected token `newline'",
            ));
        }
        current_cmds.push(parse_simple(&stage)?);
    }
    if !current_cmds.is_empty() {
        pipelines.push(Pipeline {
            commands: current_cmds,
            background: false,
        });
    }
    flush_andor(&mut andors, &mut pipelines, &mut ops)?;

    Ok(CommandList {
        andors,
        func_defs: funcs,
    })
}

/// Move the collected pipelines into the command list as one AND-OR list.
fn flush_andor(
    andors: &mut Vec<AndOr>,
    pipelines: &mut Vec<Pipeline>,
    ops: &mut Vec<AndOrOp>,
) -> Result<()> {
    if pipelines.is_empty() {
        if !ops.is_empty() {
            return Err(WashError::syntax("syntax error near unexpected token"));
        }
        return Ok(());
    }
    if ops.len() + 1 != pipelines.len() {
        return Err(WashError::syntax("syntax error near unexpected token"));
    }
    andors.push(AndOr {
        pipelines: std::mem::take(pipelines),
        ops: std::mem::take(ops),
    });
    Ok(())
}

pub fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// If `parts` is a bare variable assignment (`name=value` or `name+=value`)
/// at the start of a command, return (name, value_parts, append).
fn as_assignment(parts: &[WordPart]) -> Option<(String, Vec<WordPart>, bool)> {
    if parts.is_empty() {
        return None;
    }
    // Find the `=` that separates the lhs from the value.  A subscript may
    // contain expansions (`m[$k]=v`), so scan across parts while inside
    // `[...]`.  The returned name keeps its raw subscript (`m[$k]`); the
    // executor expands it and decides between indexed/associative storage.
    let mut eq_at: Option<(usize, usize)> = None;
    let mut in_subscript = false;
    for (i, p) in parts.iter().enumerate() {
        match p {
            WordPart::Text(s, _) => {
                if let Some(pos) = s.find('=') {
                    eq_at = Some((i, pos));
                    break;
                }
                for ch in s.chars() {
                    if ch == '[' {
                        in_subscript = true;
                    } else if ch == ']' {
                        in_subscript = false;
                    }
                }
            }
            _ if in_subscript => {}
            _ => return None,
        }
    }
    let (eq_part, eq_pos) = eq_at?;
    let mut lhs = String::new();
    for p in &parts[..eq_part] {
        lhs.push_str(&word_part_source(p)?);
    }
    match &parts[eq_part] {
        WordPart::Text(s, _) => lhs.push_str(&s[..eq_pos]),
        _ => return None,
    }
    let append = lhs.ends_with('+');
    let lhs = if append { &lhs[..lhs.len() - 1] } else { &lhs };
    let valid = if let Some(open) = lhs.find('[') {
        lhs.ends_with(']')
            && open + 1 < lhs.len() - 1
            && is_valid_ident(&lhs[..open])
    } else {
        is_valid_ident(lhs)
    };
    if !valid {
        return None;
    }
    let mut value = Vec::new();
    if let WordPart::Text(s, _) = &parts[eq_part] {
        value.push(WordPart::Text(s[eq_pos + 1..].to_string(), false));
    }
    value.extend_from_slice(&parts[eq_part + 1..]);
    Some((lhs.to_string(), value, append))
}

/// Reconstruct the source spelling of a word part so a subscript containing
/// an expansion (`m[$k]`, `m[${k}]`) can be expanded again by the executor.
fn word_part_source(p: &WordPart) -> Option<String> {
    Some(match p {
        WordPart::Text(s, _) => s.clone(),
        WordPart::Var(n, _) => format!("${}", n),
        WordPart::Braced(n, _) => format!("${{{}}}", n),
        WordPart::CmdSub(n, _) => format!("$({})", n),
        WordPart::Backtick(n, _) => format!("`{}`", n),
        WordPart::Arith(n, _) => format!("$(({}))", n),
        _ => return None,
    })
}

/// True when `parts` is the head of an array assignment: a `name=` word with an
/// empty RHS (the lexer split the following `(` into its own token).  `i` is
/// the index of `parts` in `tokens`; the token right after it must be `(`.
fn is_array_assignment_head(parts: &[WordPart], tokens: &[Token], i: usize) -> bool {
    if let Some((name, value, _append)) = as_assignment(parts) {
        if !is_valid_ident(&name) {
            return false;
        }
        // `name=` with nothing after the `=` — the value `as_assignment`
        // produced is a single empty `Text` part.
        let empty = value.len() == 1
            && matches!(value[0], WordPart::Text(ref s, _) if s.is_empty());
        if empty {
            if let Some(Token::Word(p)) = tokens.get(i + 1) {
                return is_plain_text(p) && plain_text(p) == "(";
            }
        }
    }
    false
}

/// Parse an array assignment `name=( ... )` whose `name=` token is at index
/// `*i`. Advances `*i` to the closing `)` and returns the array name together
/// with its element words (each element is itself a list of `WordPart`s, so it
/// can carry expansions / globs) and the append (`+=`) flag.  Returns `None` on
/// an unterminated body.
fn parse_array_assignment(
    tokens: &[Token],
    i: &mut usize,
) -> Option<(String, Vec<Vec<WordPart>>, bool)> {
    let head = match &tokens[*i] {
        Token::Word(p) => p,
        _ => return None,
    };
    let (name, _value, append) = as_assignment(head)?;
    // The `(` that opens the array body.
    match tokens.get(*i + 1) {
        Some(Token::Word(p)) if is_plain_text(p) && plain_text(p) == "(" => {}
        _ => return None,
    }
    let mut j = *i + 2; // skip `name=` and `(`
    let mut elems: Vec<Vec<WordPart>> = Vec::new();
    let mut depth: i32 = 1;
    while j < tokens.len() {
        match &tokens[j] {
            Token::Word(p) if is_plain_text(p) && plain_text(p) == "(" => {
                depth += 1;
                elems.push(p.clone());
            }
            Token::Word(p) if is_plain_text(p) && plain_text(p) == ")" => {
                depth -= 1;
                if depth == 0 {
                    *i = j;
                    return Some((name, elems, append));
                }
                elems.push(p.clone());
            }
            Token::Word(p) => elems.push(p.clone()),
            // Reserved words / compounds / operators are not valid array
            // elements; skip them so the scan keeps looking for the closing `)`.
            _ => {}
        }
        j += 1;
    }
    None
}

/// The token bash names in `syntax error near unexpected token `X'`.
///
/// `Token::Word` renders the word's plain text; every other token kind has a
/// fixed spelling.  Anything bash has no name for falls back to "newline",
/// the same way the heredoc/herestring diagnostic does.
fn token_near_text(tok: &Token) -> String {
    match tok {
        Token::Reserved(name, _) => name.clone(),
        Token::Word(parts) => plain_text(parts),
        Token::Semicolon(_) => ";".to_string(),
        Token::Pipe => "|".to_string(),
        Token::AndAnd => "&&".to_string(),
        Token::OrOr => "||".to_string(),
        Token::Background => "&".to_string(),
        Token::ArithCmd(_, _) => "(( ))".to_string(),
        Token::Redirect { kind, .. } => match kind {
            RedirKind::In => "<".to_string(),
            RedirKind::Out => ">".to_string(),
            RedirKind::Clobber => ">|".to_string(),
            RedirKind::Append => ">>".to_string(),
            RedirKind::Both => "&>".to_string(),
            RedirKind::BothAppend => "&>>".to_string(),
            RedirKind::Dup => "<&".to_string(),
            RedirKind::HereDoc | RedirKind::HereDocDash => "<<".to_string(),
            RedirKind::HereString => "<<<".to_string(),
        },
        _ => "newline".to_string(),
    }
}

/// The token bash names in `syntax error near unexpected token `X'` when a
/// heredoc/herestring has no delimiter/string word: the token that follows
/// the operator (or `newline` at end of input).
fn unexpected_token_name(tokens: &[Token], i: usize) -> String {
    match tokens.get(i) {
        None => "newline".to_string(),
        // A heredoc/herestring operator is completed by the end of the line,
        // so a word there still counts as "newline" for bash.
        Some(Token::Word(_)) => "newline".to_string(),
        Some(tok) => token_near_text(tok),
    }
}

fn parse_simple(tokens: &[Token]) -> Result<SimpleCommand> {
    let mut argv: Vec<Vec<WordPart>> = Vec::new();
    let mut redirs = Vec::new();
    let mut assignments = Vec::new();
    let mut array_assignments = Vec::new();
    let mut compound: Option<Compound> = None;
    let mut seen_command = false;
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            // `ArithCmd` is already turned into a Compound by
            // `parse_compounds`; reaching it here means it appeared in an
            // unexpected command tail, which bash rejects as a syntax error.
            Token::ArithCmd(_, _) => {
                return Err(WashError::syntax("syntax error near unexpected token `('"));
            }
            // `CondWord` only occurs inside `[[ ... ]]` and is consumed by
            // `parse_cond_compound`; treat it as an ordinary word here so an
            // unexpected one degrades gracefully instead of panicking.
            Token::Word(parts) | Token::CondWord(parts, _) => {
                if !seen_command {
                    if is_array_assignment_head(parts, tokens, i) {
                        match parse_array_assignment(tokens, &mut i) {
                            Some((name, elems, append)) => {
                                array_assignments.push((name, elems, append));
                                i += 1;
                                continue;
                            }
                            None => {
                                return Err(WashError::syntax(
                                    "syntax error near unexpected token (expected `)')",
                                ))
                            }
                        }
                    }
                    if let Some((name, value, append)) = as_assignment(parts) {
                        assignments.push((name, value, append));
                        i += 1;
                        continue;
                    }
                }
                // A compound command (`for ...; done`, `if ...; fi`, ...) is a
                // complete command: bash rejects a following bare word as a
                // yacc-layer syntax error (`... done extra` -> near `extra`)
                // instead of treating it as an argument or a new command.
                // Redirections and `&`/`&&`/`||` are handled by their own arms
                // and remain legal after the closer.
                if compound.is_some() {
                    return Err(WashError::syntax(format!(
                        "syntax error near unexpected token `{}'",
                        plain_text(parts)
                    )));
                }
                seen_command = true;
                argv.push(parts.clone());
            }
            Token::Compound(c) => {
                if !argv.is_empty() || compound.is_some() {
                    return Err(WashError::syntax(format!(
                        "syntax error near unexpected token `{}'",
                        match c {
                            Compound::If { .. } => "if",
                            Compound::For { .. } | Compound::ArithFor { .. } => "for",
                            Compound::Select { .. } => "select",
                            Compound::While { .. } => "while",
                            Compound::Case { .. } => "case",
                            Compound::Group { .. } => "{",
                            Compound::Subshell { .. } => "(",
                            Compound::Cond { .. } => "[[",
                            Compound::Arith { .. } => "((",
                        }
                    )));
                }
                compound = Some(c.clone());
                seen_command = true;
            }
            Token::Reserved(name, _) => {
                return Err(WashError::syntax(format!(
                    "syntax error near unexpected token `{}'",
                    name
                )));
            }
            Token::Redirect { kind, fd, heredoc } => {
                // Every redirection operator consumes a word.  A missing one
                // is reported against the token that actually follows the
                // operator (`newline` at end of input, `;`, `|`, ...), the
                // way bash does.  Here-docs / here-strings share this path.
                let file = match tokens.get(i + 1) {
                    Some(Token::Word(parts)) => parts.clone(),
                    _ => {
                        return Err(WashError::syntax(format!(
                            "syntax error near unexpected token `{}'",
                            unexpected_token_name(tokens, i + 1)
                        )));
                    }
                };
                match kind {
                    RedirKind::HereDoc | RedirKind::HereDocDash => {
                        // The lexer always fills the body in before parsing;
                        // the default only guards non-heredoc call sites.
                        let hd = heredoc.clone().unwrap_or(HereDocBody {
                            text: String::new(),
                            delim: String::new(),
                            expand: false,
                            line: 1,
                            eof: false,
                        });
                        redirs.push(Redir::HereDoc {
                            fd: fd.unwrap_or(0),
                            body: hd.text,
                            expand: hd.expand,
                        });
                    }
                    RedirKind::HereString => {
                        redirs.push(Redir::HereString {
                            fd: fd.unwrap_or(0),
                            word: file,
                        });
                    }
                    RedirKind::In => redirs.push(Redir::In {
                        fd: fd.unwrap_or(0),
                        file,
                    }),
                    RedirKind::Out | RedirKind::Clobber => redirs.push(Redir::Out {
                        fd: fd.unwrap_or(1),
                        file,
                        append: false,
                        clobber: matches!(kind, RedirKind::Clobber),
                    }),
                    RedirKind::Append => redirs.push(Redir::Out {
                        fd: fd.unwrap_or(1),
                        file,
                        append: true,
                        clobber: false,
                    }),
                    RedirKind::Both | RedirKind::BothAppend => redirs.push(Redir::Both {
                        file,
                        append: matches!(kind, RedirKind::BothAppend),
                    }),
                    RedirKind::Dup => redirs.push(Redir::Dup {
                        fd: fd.unwrap_or(1),
                        file,
                    }),
                }
                i += 2;
                continue;
            }
            Token::Pipe | Token::Semicolon(_) | Token::Background | Token::AndAnd | Token::OrOr => {
                return Err(WashError::syntax("syntax error: unexpected token in command"));
            }
            Token::FunctionDef(_) => {
                // A function definition inside a simple command is unreachable
                // (it is skipped earlier), but guard against it anyway.
                return Err(WashError::syntax("syntax error near unexpected token"));
            }
            // Conditional-expression tokens are consumed by `parse_cond_compound`
            // before `parse_simple` is reached.  Seeing one here means the
            // input is malformed; report it like any other stray token.
            Token::CondStart(_) | Token::CondEnd | Token::CondOp(_) | Token::CondNumber(_)
            | Token::CondNewline => {
                return Err(WashError::syntax("syntax error near unexpected token"));
            }
        }
        i += 1;
    }

    // A command made only of redirections (`> file`, `2>&1`) is legal: bash
    // applies them for the duration of that command and restores the
    // descriptors afterwards.  `run_standalone_exp` already implements this
    // (apply_redirs_in_parent -> restore_fds -> 0), so it must reach the
    // executor rather than being rejected here.

    Ok(SimpleCommand {
        argv,
        redirs,
        assignments,
        array_assignments,
        compound,
    })
}
