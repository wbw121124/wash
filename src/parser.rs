// Phase 0 parser: flat token stream -> AST (CommandList of Pipelines of
// SimpleCommands). Only the subset needed for Phase 0 is handled; unsupported
// constructs surface a friendly syntax error.

use crate::error::{Result, WashError};
use crate::lexer::{RedirKind, Token};

#[derive(Debug, Clone)]
pub enum Redir {
    In { fd: i32, file: String },
    Out { fd: i32, file: String, append: bool },
    Both { file: String }, // &> (stdout + stderr)
}

#[derive(Debug, Clone)]
pub struct SimpleCommand {
    pub argv: Vec<String>,
    pub redirs: Vec<Redir>,
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
                current_cmds.push(parse_simple(&stage)?);
                stage.clear();
                pipelines.push(Pipeline {
                    commands: std::mem::take(&mut current_cmds),
                });
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

fn parse_simple(tokens: &[Token]) -> Result<SimpleCommand> {
    let mut argv = Vec::new();
    let mut redirs = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(w) => argv.push(w.clone()),
            Token::Redirect { kind, fd } => {
                let file = match tokens.get(i + 1) {
                    Some(Token::Word(f)) => f.clone(),
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

    Ok(SimpleCommand { argv, redirs })
}
