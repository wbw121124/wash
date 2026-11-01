// Parser: flat token stream -> AST.
//
// The AST mirrors bash's grammar:
//
//   CommandList  := AndOr (';' AndOr)*        -- `;` separated AND-OR lists
//   AndOr        := Pipeline (('&&'|'||') Pipeline)*
//   Pipeline     := SimpleCommand ('|' SimpleCommand)*
//   SimpleCommand:= word* (redirection word*)*
//
// `&&` and `||` have the same precedence and are left-associative, which is
// why an AND-OR list keeps the operator that precedes every pipeline after
// the first one (`AndOr::ops`).

use crate::error::{Result, WashError};
use crate::lexer::{RedirKind, Token, WordPart};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Redir {
    In { fd: i32, file: Vec<WordPart> },
    Out { fd: i32, file: Vec<WordPart>, append: bool },
    Both { file: Vec<WordPart> }, // &> (stdout + stderr)
}

/// A compound command: a command that is built from other commands.
///
/// Only the `if` construct is supported so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compound {
    /// `if CONDITION; then BODY; [elif CONDITION; then BODY;]... [else BODY;] fi`
    ///
    /// The branches are tried in order; the first one whose condition
    /// succeeds runs its body.  When no condition succeeds the `else` body
    /// runs (if there is one).
    If {
        branches: Vec<(CommandList, CommandList)>,
        else_body: Option<CommandList>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleCommand {
    pub argv: Vec<Vec<WordPart>>, // each word is a list of parts (pre-expansion)
    pub redirs: Vec<Redir>,
    /// Variable assignments of the form `name=value` / `name+=value` appearing
    /// before the command word. `bool` is the append (`+=`) flag.
    pub assignments: Vec<(String, Vec<WordPart>, bool)>,
    /// The compound command this simple command stands for.  When it is set
    /// the command has no words of its own.
    pub compound: Option<Compound>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipeline {
    pub commands: Vec<SimpleCommand>,
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

pub fn parse_command_list(tokens: &[Token]) -> Result<CommandList> {
    // Compound commands (`if ... fi`) are extracted first: each of them is
    // replaced by a single token that carries the parsed branches.
    let tokens = extract_compounds(tokens)?;
    let mut andors: Vec<AndOr> = Vec::new();
    let mut pipelines: Vec<Pipeline> = Vec::new();
    let mut ops: Vec<AndOrOp> = Vec::new();
    let mut current_cmds: Vec<SimpleCommand> = Vec::new();
    let mut stage: Vec<Token> = Vec::new();

    for t in tokens {
        match t {
            Token::Pipe => {
                current_cmds.push(parse_simple(&stage)?);
                stage.clear();
            }
            Token::AndAnd | Token::OrOr => {
                // The operator closes the current pipeline and starts the
                // next one.
                if !stage.is_empty() {
                    current_cmds.push(parse_simple(&stage)?);
                    stage.clear();
                }
                if !current_cmds.is_empty() {
                    pipelines.push(Pipeline {
                        commands: std::mem::take(&mut current_cmds),
                    });
                }
                ops.push(match t {
                    Token::AndAnd => AndOrOp::And,
                    _ => AndOrOp::Or,
                });
            }
            Token::Semicolon => {
                // An empty command (e.g. two consecutive separators) is not a
                // command at all and must not change `$?`.
                if !stage.is_empty() {
                    current_cmds.push(parse_simple(&stage)?);
                    stage.clear();
                }
                if !current_cmds.is_empty() {
                    pipelines.push(Pipeline {
                        commands: std::mem::take(&mut current_cmds),
                    });
                }
                // `;` terminates the AND-OR list.
                flush_andor(&mut andors, &mut pipelines, &mut ops)?;
            }
            Token::Background => {
                return Err(WashError::syntax("background jobs not supported yet (Phase 2)"));
            }
            _ => stage.push(t.clone()),
        }
    }

    if !stage.is_empty() {
        current_cmds.push(parse_simple(&stage)?);
    }
    if !current_cmds.is_empty() {
        pipelines.push(Pipeline {
            commands: current_cmds,
        });
    }
    flush_andor(&mut andors, &mut pipelines, &mut ops)?;

    Ok(CommandList { andors })
}

/// Replace every compound command (`if ... fi`) in the token stream by a
/// single `Token::Compound` token that carries the parsed branches.
///
/// A reserved word that is not part of an `if` construct is a syntax error,
/// exactly like in bash.
fn extract_compounds(tokens: &[Token]) -> Result<Vec<Token>> {
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0usize;
    while i < tokens.len() {
        if let Token::Reserved(name) = &tokens[i] {
            if name == "if" {
                let (compound, end) = parse_if_compound(tokens, i)?;
                out.push(Token::Compound(compound));
                i = end;
                continue;
            }
            return Err(WashError::syntax(format!(
                "syntax error near unexpected token `{}'",
                name
            )));
        }
        out.push(tokens[i].clone());
        i += 1;
    }
    Ok(out)
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

/// The name of the reserved word at `index`, if the token is one.
fn reserved_name(tokens: &[Token], index: usize) -> Option<String> {
    match tokens.get(index) {
        Some(Token::Reserved(name)) => Some(name.clone()),
        _ => None,
    }
}

/// Collect the tokens from `from` up to the next reserved word whose name is
/// listed in `stops`.
///
/// Returns the collected tokens and the index of the reserved word that
/// stopped the scan (the word itself is not consumed).  A nested `if ... fi`
/// is copied verbatim.
fn split_until_reserved(
    tokens: &[Token],
    from: usize,
    stops: &[&str],
) -> Result<(Vec<Token>, usize)> {
    let mut out: Vec<Token> = Vec::new();
    let mut i = from;
    while i < tokens.len() {
        if let Token::Reserved(name) = &tokens[i] {
            if name == "if" {
                // A nested compound: copy it as a whole.
                let end = find_if_end(tokens, i)?;
                out.extend_from_slice(&tokens[i..end]);
                i = end;
                continue;
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

/// Find the index just after the `fi` that closes the `if` at `start`.
fn find_if_end(tokens: &[Token], start: usize) -> Result<usize> {
    let mut depth = 0usize;
    let mut i = start;
    while i < tokens.len() {
        if let Token::Reserved(name) = &tokens[i] {
            match name.as_str() {
                "if" => depth += 1,
                "fi" => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    Err(WashError::syntax("syntax error: unexpected end of file"))
}

/// Move the collected pipelines into the command list as one AND-OR list.
fn flush_andor(
    andors: &mut Vec<AndOr>,
    pipelines: &mut Vec<Pipeline>,
    ops: &mut Vec<AndOrOp>,
) -> Result<()> {
    if pipelines.is_empty() {
        if !ops.is_empty() {
            // An operator without a command on either side (`a && ; b`).
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
    // Only the first part can begin the `name=` prefix.
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

fn parse_simple(tokens: &[Token]) -> Result<SimpleCommand> {
    let mut argv: Vec<Vec<WordPart>> = Vec::new();
    let mut redirs = Vec::new();
    let mut assignments = Vec::new();
    let mut compound: Option<Compound> = None;
    let mut seen_command = false;
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(parts) => {
                if !seen_command {
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
                // A compound command stands on its own: it cannot be
                // combined with words.
                if !argv.is_empty() {
                    return Err(WashError::syntax(
                        "syntax error near unexpected token `if'",
                    ));
                }
                compound = Some(c.clone());
                seen_command = true;
            }
            Token::Reserved(name) => {
                return Err(WashError::syntax(format!(
                    "syntax error near unexpected token `{}'",
                    name
                )));
            }
            Token::Redirect { kind, fd } => {
                let file = match tokens.get(i + 1) {
                    Some(Token::Word(parts)) => parts.clone(),
                    _ => {
                        return Err(WashError::syntax(
                            "syntax error near unexpected token (expected redirection target)",
                        ))
                    }
                };
                match kind {
                    RedirKind::In => redirs.push(Redir::In {
                        fd: fd.unwrap_or(0),
                        file,
                    }),
                    RedirKind::Out => redirs.push(Redir::Out {
                        fd: fd.unwrap_or(1),
                        file,
                        append: false,
                    }),
                    RedirKind::Append => redirs.push(Redir::Out {
                        fd: fd.unwrap_or(1),
                        file,
                        append: true,
                    }),
                    RedirKind::Both => redirs.push(Redir::Both { file }),
                }
                i += 2;
                continue;
            }
            Token::Pipe | Token::Semicolon | Token::Background | Token::AndAnd | Token::OrOr => {
                return Err(WashError::syntax("syntax error: unexpected token in command"));
            }
        }
        i += 1;
    }

    if argv.is_empty() && !redirs.is_empty() && compound.is_none() {
        return Err(WashError::syntax("syntax error: redirection with no command"));
    }

    Ok(SimpleCommand {
        argv,
        redirs,
        assignments,
        compound,
    })
}
