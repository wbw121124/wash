// Phase 0 lexer: turn a command line into a flat token stream.
//
// Operators produced:
//   Pipe `|`, Semicolon `;`, Background `&` (unsupported in Phase 0)
//   Redirect { kind, fd } for < > >> &>  with optional fd prefix (e.g. `2>`).
//
// Bash distinguishes `2>file` (fd-2 redirect) from `2 > file` (arg "2" then
// stdout redirect) purely by adjacency. We preserve that by checking the
// CURRENT un-flushed word buffer: if it is a bare digit run with no space
// before the operator, it is an fd prefix and gets consumed; otherwise the
// digits stay as a normal word.
//
// No expansion yet (Phase 1). Inside double quotes `$` is literal for now.

use crate::error::{Result, WashError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirKind {
    In,
    Out,
    Append,
    Both, // &>
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(String),
    Pipe,
    Semicolon,
    Background,
    Redirect { kind: RedirKind, fd: Option<i32> },
}

/// If `word` is a non-empty run of ASCII digits, consume it as an fd prefix
/// and return the number; otherwise return None (and leave `word` intact).
fn take_digit_prefix(word: &mut String) -> Option<i32> {
    if !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()) {
        let fd = word.parse::<i32>().ok();
        word.clear();
        fd
    } else {
        None
    }
}

pub fn tokenize(input: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let n = chars.len();

    let mut word = String::new();
    let mut in_single = false;
    let mut in_double = false;

    let flush_word = |word: &mut String, tokens: &mut Vec<Token>| {
        if !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(word)));
        }
    };

    while i < n {
        let c = chars[i];

        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                word.push(c);
            }
            i += 1;
            continue;
        }
        if in_double {
            if c == '"' {
                in_double = false;
            } else {
                word.push(c);
            }
            i += 1;
            continue;
        }

        match c {
            '\'' => {
                in_single = true;
                i += 1;
            }
            '"' => {
                in_double = true;
                i += 1;
            }
            '\\' => {
                if i + 1 < n {
                    word.push(chars[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            ' ' | '\t' => {
                flush_word(&mut word, &mut tokens);
                i += 1;
            }
            '|' => {
                flush_word(&mut word, &mut tokens);
                tokens.push(Token::Pipe);
                i += 1;
            }
            ';' => {
                flush_word(&mut word, &mut tokens);
                tokens.push(Token::Semicolon);
                i += 1;
            }
            '<' => {
                flush_word(&mut word, &mut tokens);
                let fd = take_digit_prefix(&mut word);
                tokens.push(Token::Redirect {
                    kind: RedirKind::In,
                    fd,
                });
                i += 1;
            }
            '>' => {
                flush_word(&mut word, &mut tokens);
                let is_append = i + 1 < n && chars[i + 1] == '>';
                let fd = take_digit_prefix(&mut word);
                let kind = if is_append {
                    RedirKind::Append
                } else {
                    RedirKind::Out
                };
                tokens.push(Token::Redirect { kind, fd });
                i += if is_append { 2 } else { 1 };
            }
            '&' => {
                flush_word(&mut word, &mut tokens);
                if i + 1 < n && chars[i + 1] == '>' {
                    tokens.push(Token::Redirect {
                        kind: RedirKind::Both,
                        fd: None,
                    });
                    i += 2;
                } else {
                    tokens.push(Token::Background);
                    i += 1;
                }
            }
            _ => {
                word.push(c);
                i += 1;
            }
        }
    }

    flush_word(&mut word, &mut tokens);

    if in_single || in_double {
        return Err(WashError::syntax("unexpected EOF while looking for matching quote"));
    }

    for t in &tokens {
        if let Token::Background = t {
            return Err(WashError::syntax(
                "background jobs (`&`) are not supported yet (Phase 2)",
            ));
        }
    }

    Ok(tokens)
}
