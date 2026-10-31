// Shell state + REPL driver (Phase 1).
//
// Holds the environment, positional parameters, last exit status, and the
// current directory. Parses `-c` / interactive input and dispatches to the
// executor. The fish-like interactive layer is a later phase; for now the
// REPL is a plain line reader so the executor is verifiable.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use crate::executor;
use crate::lexer;
use crate::parser;

pub struct Shell {
    pub env: HashMap<String, String>,
    pub cwd: String,
    pub positional: Vec<String>,
    pub last_status: i32,
    pub self_pid: i32,
}

impl Shell {
    pub fn new() -> Self {
        let env: HashMap<String, String> = std::env::vars().collect();
        Shell::from_env(env)
    }

    /// Build a shell from a given environment map (used for command
    /// substitution subshells and `-c` invocations).
    pub fn from_env(env: HashMap<String, String>) -> Self {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "/".to_string());
        Shell {
            env,
            cwd,
            positional: Vec::new(),
            last_status: 0,
            self_pid: std::process::id() as i32,
        }
    }

    /// Run a single command string (used by `-c` and the interactive loop).
    pub fn run_str(&mut self, s: &str) -> i32 {
        // Top-level `exit [code]` shortcut (so the process actually exits).
        let trimmed = s.trim();
        if let Some(rest) = trimmed.strip_prefix("exit") {
            let r = rest.trim();
            if r.is_empty() {
                std::process::exit(0);
            }
            if let Ok(code) = r.parse::<i32>() {
                std::process::exit(code);
            }
        }

        let code = match lexer::tokenize(s) {
            Ok(tokens) => match parser::parse_command_list(&tokens) {
                Ok(list) => {
                    let c = executor::execute(&list, self);
                    self.last_status = c;
                    c
                }
                Err(e) => {
                    eprintln!("wash: {}", e);
                    2
                }
            },
            Err(e) => {
                eprintln!("wash: {}", e);
                2
            }
        };
        self.last_status = code;
        code
    }

    pub fn run_interactive(&mut self) {
        let stdin = io::stdin();
        loop {
            print!("wash$ ");
            io::stdout().flush().ok();
            let mut line = String::new();
            match stdin.lock().read_line(&mut line) {
                Ok(0) => {
                    println!();
                    break;
                }
                Ok(_) => {
                    let line = line.trim_end_matches(['\n', '\r']);
                    if line.trim().is_empty() {
                        continue;
                    }
                    self.run_str(line);
                }
                Err(_) => break,
            }
        }
    }
}
