// wash entry point. Phase 0: supports `-c <command>` (non-interactive, used
// for oracle comparison tests) and a plain interactive REPL.

mod arith;
mod builtins;
mod error;
mod executor;
mod expand;
mod lexer;
mod parser;
mod posix;
mod repl;
mod shell;

use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut sh = shell::Shell::new();
    // `$0` is the path the shell was invoked with, exactly like in bash.
    if let Some(argv0) = args.first() {
        sh.dollar_zero = argv0.clone();
    }

    if args.len() >= 2 && (args[1] == "-c" || args[1] == "--command") {
        if args.len() < 3 {
            eprintln!("wash: -c requires an argument");
            exit(2);
        }
        let code = sh.run_str(&args[2]);
        // Fire the EXIT trap (bash runs it just before the shell exits).
        if let Some(action) = sh.traps.get("EXIT").cloned() {
            if action != "-" {
                sh.run_str(&action);
            }
        }
        exit(code);
    }

    if args.len() >= 2 && (args[1] == "-h" || args[1] == "--help") {
        println!("wash - a bash-compatible shell with fish-like interactive UX");
        println!("usage: wash [-c <command>]");
        exit(0);
    }

    if args.len() >= 2 && args[1] == "--version" {
        println!("wash 0.0.1");
        exit(0);
    }

    sh.run_interactive();
}
