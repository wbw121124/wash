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
use crate::lexer::{HereDocBody, RedirKind, Token, WordPart};

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
        Some(Token::Semicolon | Token::Pipe | Token::AndAnd | Token::OrOr | Token::Background) => {
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
        Some(Token::Semicolon | Token::Background)
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
        WashError::syntax("syntax error: unexpected end of file from `{' command")
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
                "if" | "for" | "while" | "until" | "case" => {
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
        "for" | "while" | "until" => "loop",
        "case" => "case",
        _ => {
            return Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                opener
            )))
        }
    };
    let mut stack: Vec<&str> = vec![kind];
    let mut i = start + 1;
    while i < tokens.len() {
        if let Token::Reserved(name, _) = &tokens[i] {
            match name.as_str() {
                "if" | "for" | "while" | "until" | "case" => {
                    stack.push(match name.as_str() {
                        "if" => "if",
                        "case" => "case",
                        _ => "loop",
                    });
                }
                "fi" => {
                    if *stack.last().unwrap_or(&"") == "if" {
                        stack.pop();
                    }
                }
                "esac" => {
                    if *stack.last().unwrap_or(&"") == "case" {
                        stack.pop();
                    }
                }
                "done" => {
                    if *stack.last().unwrap_or(&"") == "loop" {
                        stack.pop();
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

/// Parse `for NAME [in WORDS] ; do BODY ; done` starting at the `for` token.
fn parse_for_compound(tokens: &[Token], start: usize) -> Result<(Compound, usize)> {
    // NAME is the word right after `for`.
    let name = match tokens.get(start + 1) {
        Some(Token::Word(parts)) if is_plain_ident(parts) => plain_text(parts),
        _ => {
            return Err(WashError::syntax(
                "syntax error near unexpected token (expected a loop variable)",
            ))
        }
    };

    // Expect `in` (a plain word, never a reserved word) after the NAME.
    if !is_word_text(tokens.get(start + 2), "in") {
        return Err(WashError::syntax(
            "syntax error near unexpected token (expected `in')",
        ));
    }

    // Collect iteration words up to the `do` reserved word.  A `;` (or newline,
    // which the lexer also turns into a `;`) between the word list and `do` is
    // optional (`for i in a b c; do ...`).
    let mut words: Vec<Vec<WordPart>> = Vec::new();
    let mut do_index = 0usize;
    let mut i = start + 3; // index just past `in`
    loop {
        if i >= tokens.len() {
            return Err(WashError::syntax(
                "syntax error: unexpected end of file",
            ));
        }
        match &tokens[i] {
            Token::Word(parts) => {
                words.push(parts.clone());
                i += 1;
            }
            Token::Semicolon => {
                // end of the word list; the `do` keyword follows.
                i += 1;
            }
            Token::Reserved(r, _) if r == "do" => {
                do_index = i;
                break;
            }
            _ => {
                return Err(WashError::syntax(
                    "syntax error near unexpected token (expected `do')",
                ))
            }
        }
    }

    let end = find_matching_closer(tokens, start)?; // index just after `done`
    let done_index = end - 1;
    let body = parse_command_list(&tokens[do_index + 1..done_index])?;

    Ok((
        Compound::For {
            var: name,
            words,
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
    let condition = parse_command_list(&tokens[start + 1..do_index])?;
    let body = parse_command_list(&tokens[do_index + 1..done_index])?;
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
                Token::Semicolon
                    if depth == 0
                        && body_end + 1 < tokens.len()
                        && matches!(tokens.get(body_end + 1), Some(Token::Semicolon)) =>
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
            && matches!(tokens.get(body_end), Some(Token::Semicolon))
            && body_end + 1 < tokens.len()
            && matches!(tokens.get(body_end + 1), Some(Token::Semicolon))
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
            Token::Semicolon => {
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

fn is_valid_ident(s: &str) -> bool {
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
    if let WordPart::Text(first, _) = &parts[0] {
        if let Some(eq) = first.find('=') {
            let (pre, valstart) = first.split_at(eq);
            let append = pre.ends_with('+');
            let ident = if append { &pre[..pre.len() - 1] } else { pre };
            if is_valid_ident(ident) {
                let mut value: Vec<WordPart> = vec![WordPart::Text(valstart[1..].to_string(), false)];
                value.extend_from_slice(&parts[1..]);
                return Some((ident.to_string(), value, append));
            }
        }
    }
    None
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

/// The token bash names in `syntax error near unexpected token `X'` when a
/// heredoc/herestring has no delimiter/string word: the token that follows
/// the operator (or `newline` at end of input).
fn unexpected_token_name(tokens: &[Token], i: usize) -> String {
    match tokens.get(i) {
        None => "newline".to_string(),
        Some(Token::Semicolon) => ";".to_string(),
        Some(Token::Pipe) => "|".to_string(),
        Some(Token::AndAnd) => "&&".to_string(),
        Some(Token::OrOr) => "||".to_string(),
        Some(Token::Background) => "&".to_string(),
        Some(Token::Reserved(name, _)) => name.clone(),
        Some(Token::Redirect { kind, .. }) => match kind {
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
        Some(_) => "newline".to_string(),
    }
}

fn parse_simple(tokens: &[Token]) -> Result<SimpleCommand> {    let mut argv: Vec<Vec<WordPart>> = Vec::new();
    let mut redirs = Vec::new();
    let mut assignments = Vec::new();
    let mut array_assignments = Vec::new();
    let mut compound: Option<Compound> = None;
    let mut seen_command = false;
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(parts) => {
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
                seen_command = true;
                argv.push(parts.clone());
            }
            Token::Compound(c) => {
                if !argv.is_empty() {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token `if'",
                    ));
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
            Token::Pipe | Token::Semicolon | Token::Background | Token::AndAnd | Token::OrOr => {
                return Err(WashError::syntax("syntax error: unexpected token in command"));
            }
            Token::FunctionDef(_) => {
                // A function definition inside a simple command is unreachable
                // (it is skipped earlier), but guard against it anyway.
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
