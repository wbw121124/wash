// Shell state + REPL driver.
//
// Holds the environment, positional parameters, last exit status, and the
// current directory. `run_str` is the single command-execution entry point
// shared by `-c` and the interactive REPL (in `repl.rs`). The interactive
// fish-style line editor lives entirely in `repl.rs`; this module only owns
// the persistent shell state and dispatches to the executor.

use std::collections::HashMap;

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
        crate::repl::run(self);
    }
}
