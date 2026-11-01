// Phase 1 parser: flat token stream -> AST (CommandList of Pipelines of
// SimpleCommands). Words now carry raw `WordPart`s; expansion happens in the
// executor via `expand.rs`.

use crate::error::{Result, WashError};
use crate::lexer::{RedirKind, Token, WordPart};

#[derive(Debug, Clone)]
pub enum Redir {
    In { fd: i32, file: Vec<WordPart> },
    Out { fd: i32, file: Vec<WordPart>, append: bool },
    Both { file: Vec<WordPart> }, // &> (stdout + stderr)
}

#[derive(Debug, Clone)]
pub struct SimpleCommand {
    pub argv: Vec<Vec<WordPart>>, // each word is a list of parts (pre-expansion)
    pub redirs: Vec<Redir>,
    /// Variable assignments of the form `name=value` / `name+=value` appearing
    /// before the command word. `bool` is the append (`+=`) flag.
    pub assignments: Vec<(String, Vec<WordPart>, bool)>,
}

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub commands: Vec<SimpleCommand>,
}

#[derive(Debug, Clone)]
pub struct CommandList {
    pub pipelines: Vec<Pipeline>,
}

pub fn parse_command_list(tokens: &[Token]) -> Result<CommandList> {
    let mut pipelines: Vec<Pipeline> = Vec::new();
    let mut current_cmds: Vec<SimpleCommand> = Vec::new();
    let mut stage: Vec<Token> = Vec::new();

    for t in tokens {
        match t {
            Token::Pipe => {
                current_cmds.push(parse_simple(&stage)?);
                stage.clear();
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

    Ok(CommandList { pipelines })
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
            Token::Pipe | Token::Semicolon | Token::Background => {
                return Err(WashError::syntax("syntax error: unexpected token in command"));
            }
        }
        i += 1;
    }

    if argv.is_empty() && !redirs.is_empty() {
        return Err(WashError::syntax("syntax error: redirection with no command"));
    }

    Ok(SimpleCommand {
        argv,
        redirs,
        assignments,
    })
}
