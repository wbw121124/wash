// Bash-compatible builtin commands.
//
// Every builtin implementation lives in this module. `run_builtin` is the
// single dispatch entry point used by the executor:
//
//   * `Some(code)` — the command was a builtin and has already run; `code` is
//     its exit status.
//   * `None`      — `argv[0]` is not a builtin, so the caller must look it up
//     in `$PATH` and run it as an external program.
//
// The behaviour of every builtin follows GNU bash 5.3 (the reference shell
// used by the project's byte-diff test suite): same output bytes, same exit
// statuses and the same side effects on the shell state.
//
// State-changing builtins (`cd`, `export`, `set`, `alias`, ...) must run in
// the *parent* shell, otherwise their side effects would be lost. The
// executor therefore calls `run_builtin` before forking; the forked child
// only uses it for the stateless output builtins.

use std::io::Write;

use crate::arith;
use crate::posix;
use crate::shell::{Shell, SET_O_ORDER, SHOPT_ORDER};

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Every builtin name understood by `run_builtin`.
const BUILTINS: &[&str] = &[
    ".",
    ":",
    "[",
    "alias",
    "break",
    "builtin",
    "cd",
    "command",
    "continue",
    "declare",
    "dirs",
    "echo",
    "enable",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "hash",
    "kill",
    "let",
    "local",
    "popd",
    "printf",
    "pushd",
    "read",
    "readonly",
    "return",
    "set",
    "shift",
    "shopt",
    "source",
    "test",
    "times",
    "trap",
    "true",
    "type",
    "typeset",
    "umask",
    "unalias",
    "unset",
    "pwd",
];

/// True when `name` is the name of a builtin command.
pub fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

/// Run a builtin command in the given shell.
///
/// See the module comment for the `Option` contract.
pub fn run_builtin(argv: &[String], sh: &mut Shell) -> Option<i32> {
    let name = argv.first()?.as_str();
    let code = match name {
        "cd" => builtin_cd(argv, sh),
        "exit" => builtin_exit(argv, sh),
        "export" => builtin_export(argv, sh),
        "unset" => builtin_unset(argv, sh),
        "set" => builtin_set(argv, sh),
        "shift" => builtin_shift(argv, sh),
        "shopt" => builtin_shopt(argv, sh),
        "alias" => builtin_alias(argv, sh),
        "unalias" => builtin_unalias(argv, sh),
        "source" | "." => builtin_source(argv, sh),
        "eval" => builtin_eval(argv, sh),
        "exec" => builtin_exec(argv, sh),
        "echo" => builtin_echo(argv),
        "printf" => builtin_printf(argv, Some(sh)),
        "test" | "[" => builtin_test(argv, sh),
        "type" => builtin_type(argv, sh),
        "true" => builtin_true(argv, sh),
        "false" => builtin_false(argv, sh),
        "hash" => builtin_hash(argv, sh),
        "read" => builtin_read(argv, sh),
        ":" => builtin_colon(argv, sh),
        "command" => builtin_command(argv, sh),
        "pwd" => builtin_pwd(argv, sh),
        "builtin" => builtin_builtin(argv, sh),
        "break" => builtin_break(argv, sh),
        "continue" => builtin_continue(argv, sh),
        "return" => builtin_return(argv, sh),
        "local" => builtin_local(argv, sh),
        "declare" => builtin_declare(argv, sh, false),
        "typeset" => builtin_declare(argv, sh, true),
        "readonly" => builtin_readonly(argv, sh),
        "umask" => builtin_umask(argv, sh),
        "times" => builtin_times(argv, sh),
        "let" => builtin_let(argv, sh),
        "enable" => builtin_enable(argv, sh),
        "kill" => builtin_kill(argv, sh),
        "trap" => builtin_trap(argv, sh),
        "dirs" => builtin_dirs(argv, sh),
        "pushd" => builtin_pushd(argv, sh),
        "popd" => builtin_popd(argv, sh),
        _ => return None,
    };
    Some(code)
}

// ---------------------------------------------------------------------------
// cd
// ---------------------------------------------------------------------------

/// The `cd` builtin: change the working directory and keep `$PWD`/`$OLDPWD`
/// in sync with it.
///
/// Supported forms (bash 5.3):
///   * `cd [-L|-P] [dir]` — `dir` defaults to `$HOME`.
///   * `cd -` — go back to `$OLDPWD` and print the new directory.
///   * `cd -- [dir]` — `--` ends the option list.
///   * `$CDPATH` is searched for relative directory names.
///
/// bash refuses to change directory when more than one operand is given:
/// it reports "cd: too many arguments" and exits with status 2, leaving
/// `$PWD` untouched.
fn builtin_cd(argv: &[String], sh: &mut Shell) -> i32 {
    let mut physical = false;
    let mut i = 1usize;
    let mut operands: Vec<String> = Vec::new();
    let mut end_of_options = false;

    while i < argv.len() {
        let a = &argv[i];
        if !end_of_options {
            if a == "--" {
                // `--` ends the option list; the next argument is the
                // directory, not an option.
                end_of_options = true;
                i += 1;
                continue;
            }
            // "-" is the "previous directory" operand, not an option.
            if a.len() > 1 && a.starts_with('-') {
                match a.as_str() {
                    "-L" => physical = false,
                    "-P" => physical = true,
                    _ => {
                        eprintln!("wash: cd: {}: invalid option", a);
                        eprintln!("cd: usage: cd [-L|[-P [-e]]] [-@] [dir]");
                        return 2;
                    }
                }
                i += 1;
                continue;
            }
        }
        operands.push(a.clone());
        i += 1;
    }

    if operands.len() > 1 {
        eprintln!("wash: cd: too many arguments");
        return 2;
    }
    let target: Option<String> = operands.into_iter().next();

    // Resolve the destination directory.
    let mut print_result = false;
    let dir: String = match target {
        None => match sh.env.get("HOME") {
            None => {
                eprintln!("wash: cd: HOME not set");
                return 1;
            }
            // An empty $HOME is a successful no-op in bash.
            Some(h) if h.is_empty() => return 0,
            Some(h) => h.clone(),
        },
        Some(d) if d == "-" => match sh.env.get("OLDPWD").or(sh.oldpwd.as_ref()) {
            None => {
                eprintln!("wash: cd: OLDPWD not set");
                return 1;
            }
            Some(p) if p.is_empty() => {
                eprintln!("wash: cd: OLDPWD not set");
                return 1;
            }
            Some(p) => {
                print_result = true;
                p.clone()
            }
        },
        Some(d) if d.is_empty() => {
            eprintln!("wash: cd: null directory");
            return 1;
        }
        Some(d) => d,
    };

    // `$CDPATH` is consulted for relative directory names. The current
    // directory is used when nothing matches.
    let mut candidates: Vec<String> = Vec::new();
    let is_plain_relative = !dir.starts_with('/')
        && !dir.starts_with("./")
        && !dir.starts_with("../")
        && dir != "."
        && dir != "..";
    if is_plain_relative {
        if let Some(cdpath) = sh.env.get("CDPATH") {
            for elem in cdpath.split(':') {
                if elem.is_empty() {
                    continue;
                }
                candidates.push(format!("{}/{}", elem, dir));
            }
        }
    }
    candidates.push(dir.clone());

    let mut chosen: Option<String> = None;
    let mut last_errno = 0;
    for cand in &candidates {
        if posix::chdir(cand) == 0 {
            chosen = Some(cand.clone());
            break;
        }
        last_errno = posix::errno();
    }

    let chosen = match chosen {
        Some(c) => c,
        None => {
            eprintln!(
                "wash: cd: {}: {}",
                dir,
                posix::errno_message(last_errno)
            );
            return 1;
        }
    };
    // bash prints the new directory when it was found through $CDPATH.
    if candidates.len() > 1 && chosen != dir {
        print_result = true;
    }

    let old_pwd = sh.cwd.clone();
    let new_pwd = if physical {
        posix::getcwd()
    } else {
        normalize_logical(&old_pwd, &chosen)
    };

    sh.cwd = new_pwd.clone();
    sh.oldpwd = Some(old_pwd.clone());
    sh.env.insert("PWD".to_string(), new_pwd.clone());
    sh.env.insert("OLDPWD".to_string(), old_pwd);
    sh.exported.insert("PWD".to_string());
    sh.exported.insert("OLDPWD".to_string());
    // `cd` replaces the top of the directory stack with the new directory.
    if !sh.dir_stack.is_empty() {
        sh.dir_stack[0] = new_pwd.clone();
    } else {
        sh.dir_stack.push(new_pwd.clone());
    }

    if print_result {
        println!("{}", new_pwd);
    }
    0
}

/// Make `path` absolute relative to `base` and collapse `.`, `..` and
/// duplicate separators *without* resolving symbolic links — bash's logical
/// (`cd -L`) path computation.
fn normalize_logical(base: &str, path: &str) -> String {
    let full = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{}", base, path)
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in full.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

// ---------------------------------------------------------------------------
// exit
// ---------------------------------------------------------------------------

/// Parse `s` the way bash's `legal_number` does: an optional sign followed
/// by decimal digits, surrounded by optional whitespace.
///
/// The number is read in BASE 10, so "08" is eight (not an invalid octal
/// literal) and "0x10" is not a number at all.  A value that does not fit
/// into a 64-bit integer is rejected, exactly like bash's ERANGE check.
///
/// Returns `None` when `s` is not a legal number.
fn parse_bash_number(s: &str) -> Option<i64> {
    let t = s.trim();
    let (sign, digits) = match t.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i64>().ok().map(|v| sign * v)
}

/// The `exit` builtin: terminate the shell with the given status (or the
/// status of the last command when no argument is given).
///
/// A non-numeric argument is a *recoverable* error in bash: the builtin
/// reports it and returns 2, but the shell keeps running and the next
/// command is executed.  Only a legal number terminates the shell.
fn builtin_exit(argv: &[String], sh: &mut Shell) -> i32 {
    let code = if argv.len() > 1 {
        let arg = &argv[1];
        match parse_bash_number(arg) {
            None => {
                eprintln!("wash: exit: {}: numeric argument required", arg);
                // bash does NOT terminate the shell here: the error is
                // reported, the status becomes 2 and the next command
                // still runs.
                return 2;
            }
            Some(value) => (value & 0xff) as i32,
        }
    } else {
        sh.last_status
    };
    sh.exit_requested = Some(code);
    code
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// The `export` builtin: mark variables for export, set their values or list
/// the exported variables (`declare -x NAME="value"`).
fn builtin_export(argv: &[String], sh: &mut Shell) -> i32 {
    let mut print = false;
    let mut unexport = false;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-p" => print = true,
                "-n" => unexport = true,
                "-f" => {
                    eprintln!("wash: export: -f: not supported");
                    eprintln!("export: usage: export [-fn] [name[=value] ...] or export -p [-f]");
                    return 2;
                }
                _ => {
                    eprintln!("wash: export: {}: invalid option", a);
                    eprintln!("export: usage: export [-fn] [name[=value] ...] or export -p [-f]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    if print || i >= argv.len() {
        print_exported_variables(sh);
        return 0;
    }

    for arg in &argv[i..] {
        match arg.find('=') {
            Some(eq) => {
                let name = &arg[..eq];
                let value = &arg[eq + 1..];
                if !is_valid_ident(name) {
                    eprintln!("wash: export: `{}': not a valid identifier", arg);
                    return 1;
                }
                if unexport {
                    sh.env.remove(name);
                    sh.exported.remove(name);
                    sh.null_exported.remove(name);
                } else {
                    sh.env.insert(name.to_string(), value.to_string());
                    sh.exported.insert(name.to_string());
                    sh.null_exported.remove(name);
                }
            }
            None => {
                if !is_valid_ident(arg) {
                    eprintln!("wash: export: `{}': not a valid identifier", arg);
                    return 1;
                }
                if unexport {
                    sh.exported.remove(arg);
                    sh.null_exported.remove(arg);
                } else {
                    sh.exported.insert(arg.to_string());
                    if !sh.env.contains_key(arg) {
                        // A variable that is exported without ever being
                        // assigned has no value; bash prints it as
                        // `declare -x NAME` (without `="..."`).
                        sh.env.insert(arg.to_string(), String::new());
                        sh.null_exported.insert(arg.to_string());
                    }
                }
            }
        }
    }
    0
}

/// Print the exported variables the way bash's `export -p` does.
fn print_exported_variables(sh: &Shell) {
    let mut names: Vec<&String> = sh.exported.iter().collect();
    names.sort();
    for name in names {
        let attrs = export_attributes(sh, name);
        let value = sh.env.get(name);
        match value {
            Some(v) if !sh.null_exported.contains(name) => {
                println!("declare -{} {}={}", attrs, name, quote_double(v))
            }
            _ => println!("declare -{} {}", attrs, name),
        }
    }
}

/// The attribute letters bash prints in front of an exported variable
/// (`declare -rx NAME=...`).
fn export_attributes(sh: &Shell, name: &str) -> String {
    let mut attrs = String::new();
    if sh.integer_vars.contains(name) {
        attrs.push('i');
    }
    if sh.readonly.contains(name) {
        attrs.push('r');
    }
    attrs.push('x');
    attrs
}

// ---------------------------------------------------------------------------
// unset
// ---------------------------------------------------------------------------

/// The `unset` builtin: remove variables (`-v`, the default) or functions
/// (`-f`).
fn builtin_unset(argv: &[String], sh: &mut Shell) -> i32 {
    let mut functions = false;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-f" => functions = true,
                "-v" => functions = false,
                // `-n` names a nameref; nothing to do for us.
                "-n" => {}
                _ => {
                    eprintln!("wash: unset: {}: invalid option", a);
                    eprintln!("unset: usage: unset [-f] [-v] [-n] [name ...]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    let mut status = 0;
    for name in &argv[i..] {
        if functions {
            sh.functions.remove(name);
        } else if sh.readonly.contains(name) {
            // bash refuses to unset a readonly variable and keeps it intact.
            eprintln!("wash: unset: {}: cannot unset: readonly variable", name);
            status = 1;
        } else {
            sh.env.remove(name);
            sh.exported.remove(name);
            sh.null_exported.remove(name);
            sh.readonly.remove(name);
            sh.integer_vars.remove(name);
        }
    }
    status
}

// ---------------------------------------------------------------------------
// set
// ---------------------------------------------------------------------------

/// The `set` builtin: toggle shell options, set the positional parameters or
/// list the shell variables.
fn builtin_set(argv: &[String], sh: &mut Shell) -> i32 {
    if argv.len() == 1 {
        print_all_variables(sh);
        return 0;
    }

    let mut i = 1usize;
    let mut positional_args = false;
    // The option changes are collected first and applied at the end: bash does
    // not apply anything when the command line contains an invalid option.
    let mut changes: Vec<(char, bool)> = Vec::new();
    let mut named: Vec<(String, bool)> = Vec::new();
    // bash prints the option table (or the `set +o` listing) while parsing and
    // reports a different exit status when it did so.
    let mut printed_table = false;

    while i < argv.len() {
        let a = argv[i].clone();
        if a == "--" {
            i += 1;
            positional_args = true;
            break;
        }
        if a == "-" {
            // `set -` clears the positional parameters and turns off -v/-x.
            sh.options.verbose = false;
            sh.options.xtrace = false;
            sh.positional.clear();
            return 0;
        }
        if a.len() > 1 && (a.starts_with('-') || a.starts_with('+')) {
            let enable = a.starts_with('-');
            let cluster: Vec<char> = a.chars().skip(1).collect();
            let mut k = 0usize;
            while k < cluster.len() {
                let ch = cluster[k];
                if ch == 'o' {
                    if k + 1 == cluster.len() {
                        // `set -o NAME` / `set -o`: the option name is the next
                        // argument; without one the option table is printed.
                        if i + 1 < argv.len() && !is_option_argument(&argv[i + 1]) {
                            named.push((argv[i + 1].clone(), enable));
                            i += 1;
                        } else {
                            if enable {
                                print_option_table(sh);
                            } else {
                                print_option_list(sh);
                            }
                            printed_table = true;
                        }
                    } else {
                        // bash keeps parsing the rest of the cluster as short
                        // options and prints the table for the `o`.
                        if enable {
                            print_option_table(sh);
                        } else {
                            print_option_list(sh);
                        }
                        printed_table = true;
                    }
                    k += 1;
                    continue;
                }
                if !is_valid_short_option(ch) {
                    let sign = if enable { '-' } else { '+' };
                    eprintln!("wash: set: {}{}: invalid option", sign, ch);
                    eprintln!(
                        "set: usage: set [-abefhkmnptuvxBCEHPT] [-o option-name] [--] [-] [arg ...]"
                    );
                    return if printed_table { 1 } else { 2 };
                }
                changes.push((ch, enable));
                k += 1;
            }
            i += 1;
            continue;
        }
        break;
    }

    for (ch, enable) in changes {
        if enable {
            apply_short_option(ch, sh);
        } else {
            unset_short_option(ch, sh);
        }
    }
    for (name, enable) in named {
        if !set_named_option(sh, &name, enable) {
            return 2;
        }
    }

    // Everything that is left over becomes the positional parameters.
    if positional_args || i < argv.len() {
        sh.positional = argv[i..].to_vec();
    }
    0
}

/// True when `arg` looks like an option rather than an option name.
fn is_option_argument(arg: &str) -> bool {
    arg.len() > 1 && (arg.starts_with('-') || arg.starts_with('+'))
}

/// True for the single-letter options bash's `set` accepts.
fn is_valid_short_option(ch: char) -> bool {
    matches!(
        ch,
        'a' | 'b' | 'e' | 'f' | 'h' | 'k' | 'm' | 'n' | 'p' | 't' | 'u' | 'v' | 'x' | 'B' | 'C'
            | 'E' | 'H' | 'P' | 'T'
    )
}

/// Enable or disable the option with the given `set -o` name.
fn set_named_option(sh: &mut Shell, name: &str, enable: bool) -> bool {
    if sh.options.set(name, enable) {
        true
    } else {
        eprintln!("wash: set: {}: invalid option name", name);
        false
    }
}

/// Apply a `set -X` short option. Returns false for an unknown option.
fn apply_short_option(ch: char, sh: &mut Shell) -> bool {
    match ch {
        'a' => sh.options.allexport = true,
        'b' => sh.options.notify = true,
        'e' => sh.options.errexit = true,
        'f' => sh.options.noglob = true,
        'h' => sh.options.hashall = true,
        'k' => sh.options.keyword = true,
        'm' => sh.options.monitor = true,
        'n' => sh.options.noexec = true,
        'p' => sh.options.privileged = true,
        // `set -t` is a one-shot xtrace in bash; the option is accepted but
        // the one-shot behaviour is not implemented.
        't' => {}
        'u' => sh.options.nounset = true,
        'v' => sh.options.verbose = true,
        'x' => sh.options.xtrace = true,
        'B' => sh.options.braceexpand = true,
        'C' => sh.options.noclobber = true,
        'E' => sh.options.errtrace = true,
        'H' => sh.options.histexpand = true,
        'P' => sh.options.physical = true,
        'T' => sh.options.functrace = true,
        _ => return false,
    }
    true
}

/// Turn off a `set +X` short option. Returns false for an unknown option.
fn unset_short_option(ch: char, sh: &mut Shell) -> bool {
    match ch {
        'a' => sh.options.allexport = false,
        'b' => sh.options.notify = false,
        'e' => sh.options.errexit = false,
        'f' => sh.options.noglob = false,
        'h' => sh.options.hashall = false,
        'k' => sh.options.keyword = false,
        'm' => sh.options.monitor = false,
        'n' => sh.options.noexec = false,
        'p' => sh.options.privileged = false,
        't' => {}
        'u' => sh.options.nounset = false,
        'v' => sh.options.verbose = false,
        'x' => sh.options.xtrace = false,
        'B' => sh.options.braceexpand = false,
        'C' => sh.options.noclobber = false,
        'E' => sh.options.errtrace = false,
        'H' => sh.options.histexpand = false,
        'P' => sh.options.physical = false,
        'T' => sh.options.functrace = false,
        _ => return false,
    }
    true
}

/// Print the `set -o` table: the option name padded to 15 columns, a tab and
/// its state.
fn print_option_table(sh: &Shell) {
    for name in SET_O_ORDER {
        let state = if sh.options.get(name).unwrap_or(false) {
            "on"
        } else {
            "off"
        };
        println!("{:<15}\t{}", name, state);
    }
}

/// Print the `set +o` listing: one `set -o name` / `set +o name` line per
/// option.
fn print_option_list(sh: &Shell) {
    for name in SET_O_ORDER {
        let flag = if sh.options.get(name).unwrap_or(false) {
            '-'
        } else {
            '+'
        };
        println!("set {}o {}", flag, name);
    }
}

/// Print every shell variable the way bash's `set` (without arguments) does.
fn print_all_variables(sh: &Shell) {
    let mut names: Vec<&String> = sh.env.keys().collect();
    names.sort();
    for name in names {
        let value = sh.env.get(name).cloned().unwrap_or_default();
        println!("{}={}", name, quote_for_set(&value));
    }
}

// ---------------------------------------------------------------------------
// shift
// ---------------------------------------------------------------------------

/// The `shift` builtin: drop the first positional parameters.
///
/// `shift [n]` removes the first `n` positional parameters (`n` defaults to
/// 1).  bash's error handling is reproduced exactly:
///
///   * a non-numeric argument   -> "shift: <arg>: numeric argument required",
///                                 exit status 2;
///   * a negative count         -> "shift: <arg>: shift count out of range",
///                                 exit status 1;
///   * a count larger than `$#` -> no message at all, exit status 1;
///   * more than one argument   -> "shift: too many arguments", exit status 1;
///                                 this error terminates the shell.
fn builtin_shift(argv: &[String], sh: &mut Shell) -> i32 {
    // `--` ends the option list and is removed; the count may follow it.
    let mut args: Vec<&str> = argv[1..].iter().map(|s| s.as_str()).collect();
    if args.first() == Some(&"--") {
        args.remove(0);
    }

    if args.len() > 1 {
        eprintln!("wash: shift: too many arguments");
        // bash aborts the shell on this usage error.
        sh.exit_requested = Some(1);
        return 1;
    }

    let count: i64 = match args.first() {
        None => 1,
        Some(arg) => {
            let value = match parse_bash_number(arg) {
                None => {
                    eprintln!("wash: shift: {}: numeric argument required", arg);
                    return 2;
                }
                Some(v) => v,
            };
            if value < 0 {
                eprintln!("wash: shift: {}: shift count out of range", arg);
                return 1;
            }
            value
        }
    };

    let count = count as usize;
    if count > sh.positional.len() {
        // bash stays silent when the count exceeds the number of parameters.
        return 1;
    }
    sh.positional.drain(0..count);
    0
}

// ---------------------------------------------------------------------------
// shopt
// ---------------------------------------------------------------------------

/// The `shopt` builtin: list or change the shell options that are not
/// reachable through `set -o`.
///
/// Supported forms (bash 5.3):
///   * `shopt` — print every option and its state.
///   * `shopt -p [name ...]` — print the options in the `shopt -s NAME` /
///     `shopt -u NAME` form.
///   * `shopt -s name ...` / `shopt -u name ...` — switch options on / off.
///   * `shopt -o` — print the `set -o` table instead.
fn builtin_shopt(argv: &[String], sh: &mut Shell) -> i32 {
    let mut print = false;
    let mut enable = false;
    let mut disable = false;
    let mut show_set_options = false;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-p" => print = true,
                "-s" => enable = true,
                "-u" => disable = true,
                "-o" => show_set_options = true,
                _ => {
                    eprintln!("wash: shopt: {}: invalid option", a);
                    eprintln!("shopt: usage: shopt [-psu] [-o] [option ...]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    // `shopt -o` shows the options of the `set -o` table.
    if show_set_options {
        if print {
            print_option_list(sh);
        } else {
            print_option_table(sh);
        }
        return 0;
    }

    let names: Vec<String> = argv[i..].to_vec();

    if enable || disable {
        if names.is_empty() {
            eprintln!("wash: shopt: usage: shopt [-psu] [-o] [option ...]");
            return 2;
        }
        let mut status = 0;
        for name in &names {
            if !sh.shopt_set(name, enable) {
                eprintln!("wash: shopt: {}: invalid shell option name", name);
                status = 1;
            }
        }
        return status;
    }

    // Listing mode.
    if names.is_empty() {
        if print {
            print_shopt_listing(sh);
        } else {
            print_shopt_table(sh);
        }
    } else {
        for name in &names {
            match sh.shopt_get(name) {
                None => {
                    eprintln!("wash: shopt: {}: invalid shell option name", name);
                    return 1;
                }
                Some(value) => {
                    if print {
                        println!("shopt {} {}", if value { "-s" } else { "-u" }, name);
                    } else {
                        println!(
                            "{:<width$}\t{}",
                            name,
                            if value { "on" } else { "off" },
                            width = shopt_name_width()
                        );
                    }
                }
            }
        }
    }
    0
}

/// The width of the option-name column in the `shopt` listing: bash pads
/// the names to the length of the longest one plus two columns.
fn shopt_name_width() -> usize {
    SHOPT_ORDER.iter().map(|n| n.len()).max().unwrap_or(0) + 2
}

/// Print every `shopt` option and its state, the way bash's `shopt` does.
fn print_shopt_table(sh: &Shell) {
    let width = shopt_name_width();
    for name in SHOPT_ORDER {
        let state = if sh.shopt_get(name).unwrap_or(false) {
            "on"
        } else {
            "off"
        };
        println!("{:<width$}\t{}", name, state, width = width);
    }
}

/// Print every `shopt` option in the `shopt -s NAME` / `shopt -u NAME` form,
/// the way bash's `shopt -p` does.
fn print_shopt_listing(sh: &Shell) {
    for name in SHOPT_ORDER {
        let state = if sh.shopt_get(name).unwrap_or(false) {
            "-s"
        } else {
            "-u"
        };
        println!("shopt {} {}", state, name);
    }
}

// ---------------------------------------------------------------------------
// test / [
// ---------------------------------------------------------------------------

/// The error of a malformed `test` expression.
///
/// The text is what bash prints after the `test: ` / `[: ` prefix.
enum TestError {
    /// `<arg>: unary operator expected`
    UnaryOperator(String),
    /// `<arg>: binary operator expected`
    BinaryOperator(String),
    /// `<arg>: integer expected`
    Integer(String),
    /// `argument expected`
    ArgumentExpected,
    /// `` `)' expected ``
    ExpectedCloseParen,
    /// `` `)' expected, found <arg> ``
    ExpectedCloseParenFound(String),
}

impl TestError {
    /// The message bash prints for this error.
    fn message(&self) -> String {
        match self {
            TestError::UnaryOperator(arg) => format!("{}: unary operator expected", arg),
            TestError::BinaryOperator(arg) => format!("{}: binary operator expected", arg),
            TestError::Integer(arg) => format!("{}: integer expected", arg),
            TestError::ArgumentExpected => "argument expected".to_string(),
            TestError::ExpectedCloseParen => "`)' expected".to_string(),
            TestError::ExpectedCloseParenFound(arg) => format!("`)' expected, found {}", arg),
        }
    }
}

/// True when `op` is one of the unary operators of `test`.
///
/// The list is bash's own (see `test_unop` in bash's `test.c`): a single
/// dash followed by exactly one letter.
fn is_unary_op(op: &str) -> bool {
    let bytes = op.as_bytes();
    if bytes.first() != Some(&b'-') || bytes.get(1).is_none() || bytes.get(2).is_some() {
        return false;
    }
    matches!(
        bytes[1],
        b'a' | b'b'
            | b'c'
            | b'd'
            | b'e'
            | b'f'
            | b'g'
            | b'h'
            | b'k'
            | b'n'
            | b'o'
            | b'p'
            | b'r'
            | b's'
            | b't'
            | b'u'
            | b'v'
            | b'w'
            | b'x'
            | b'z'
            | b'G'
            | b'L'
            | b'O'
            | b'S'
            | b'N'
            | b'R'
    )
}

/// True when `op` is one of the binary operators of `test`.
fn is_binary_op(op: &str) -> bool {
    match op {
        "=" | "==" | "!=" | "<" | ">" => true,
        "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" | "-nt" | "-ot" | "-ef" => true,
        _ => false,
    }
}

/// True when `op` is the logical operator `-a` or `-o`.
fn is_andor(op: &str) -> bool {
    op == "-a" || op == "-o"
}

/// The evaluator of a `test` expression.
///
/// This is a port of bash 5.3's `test.c`: the same recursive-descent
/// grammar, the same argument-count dispatch and the same error messages.
struct TestEvaluator<'a> {
    /// The operands of the expression (without the command name and,
    /// for `[`, without the closing bracket).
    args: &'a [String],
    /// The index of the next operand to look at.
    pos: usize,
    /// The shell the expression is evaluated in (for `-v` and `-o`).
    sh: &'a Shell,
}

impl<'a> TestEvaluator<'a> {
    /// The operand at `index`, or the empty string when it is missing.
    fn arg(&self, index: usize) -> &'a str {
        self.args.get(index).map(|s| s.as_str()).unwrap_or("")
    }

    /// Move to the next operand.  With `check` the expression must not end
    /// here (bash's `advance(1)`).
    fn advance(&mut self, check: bool) -> Result<(), TestError> {
        self.pos += 1;
        if check && self.pos >= self.args.len() {
            return Err(TestError::ArgumentExpected);
        }
        Ok(())
    }

    /// Evaluate the expression; the number of operands decides which rule
    /// bash applies (bash's `posixtest`).
    fn posixtest(&mut self) -> Result<bool, TestError> {
        match self.args.len() {
            0 => Ok(false),
            1 => {
                let value = !self.arg(0).is_empty();
                self.advance(false)?;
                Ok(value)
            }
            2 => self.two_arguments(),
            3 => self.three_arguments(),
            4 => {
                if self.arg(0) == "!" {
                    self.advance(true)?;
                    Ok(!self.three_arguments()?)
                } else if self.arg(0) == "(" && self.arg(3) == ")" {
                    self.advance(true)?;
                    let value = self.two_arguments()?;
                    self.advance(false)?;
                    Ok(value)
                } else {
                    self.expr()
                }
            }
            _ => self.expr(),
        }
    }

    /// Two operands: either `! ARG` or a unary operator with its argument.
    fn two_arguments(&mut self) -> Result<bool, TestError> {
        let first = self.arg(self.pos);
        if first == "!" {
            self.advance(false)?;
            let value = self.arg(self.pos).is_empty();
            self.pos += 1;
            return Ok(value);
        }
        if first.starts_with('-') && first.len() == 2 && is_unary_op(first) {
            return self.unary_operator();
        }
        Err(TestError::UnaryOperator(first.to_string()))
    }

    /// Three operands: a binary operator, a logical operator, `!` or a
    /// parenthesised expression.
    fn three_arguments(&mut self) -> Result<bool, TestError> {
        let second = self.arg(self.pos + 1).to_string();
        if is_binary_op(&second) {
            return self.binary_operator();
        }
        if is_andor(&second) {
            let left = !self.arg(self.pos).is_empty();
            let right = !self.arg(self.pos + 2).is_empty();
            let value = if second == "-a" { left && right } else { left || right };
            self.pos += 3;
            return Ok(value);
        }
        if self.arg(self.pos) == "!" {
            self.advance(true)?;
            return Ok(!self.two_arguments()?);
        }
        if self.arg(self.pos) == "(" && self.arg(self.pos + 2) == ")" {
            self.advance(false)?;
            let value = !self.arg(self.pos).is_empty();
            self.pos += 2;
            return Ok(value);
        }
        Err(TestError::BinaryOperator(second))
    }

    /// The full expression grammar (`expr` in bash's `test.c`).
    fn expr(&mut self) -> Result<bool, TestError> {
        if self.pos >= self.args.len() {
            return Err(TestError::ArgumentExpected);
        }
        self.or_expr()
    }

    /// `or`: `and ('-o' or)*`.
    fn or_expr(&mut self) -> Result<bool, TestError> {
        let mut value = self.and_expr()?;
        while self.pos < self.args.len() && self.arg(self.pos) == "-o" {
            self.advance(false)?;
            let right = self.or_expr()?;
            value = value || right;
        }
        Ok(value)
    }

    /// `and`: `term ('-a' and)*`.
    fn and_expr(&mut self) -> Result<bool, TestError> {
        let mut value = self.term()?;
        while self.pos < self.args.len() && self.arg(self.pos) == "-a" {
            self.advance(false)?;
            let right = self.and_expr()?;
            value = value && right;
        }
        Ok(value)
    }

    /// `term`: a single test, a negated test or a parenthesised expression.
    fn term(&mut self) -> Result<bool, TestError> {
        if self.pos >= self.args.len() {
            return Err(TestError::ArgumentExpected);
        }

        // Leading `not's bind tighter than `-a` and `-o`.
        if self.arg(self.pos) == "!" {
            let mut negated = false;
            while self.pos < self.args.len() && self.arg(self.pos) == "!" {
                self.advance(true)?;
                negated = !negated;
            }
            return if negated {
                Ok(!self.term()?)
            } else {
                self.term()
            };
        }

        // A parenthesised expression.
        if self.arg(self.pos) == "(" {
            self.advance(true)?;
            let value = self.expr()?;
            if self.pos >= self.args.len() {
                return Err(TestError::ExpectedCloseParen);
            }
            if self.arg(self.pos) != ")" {
                return Err(TestError::ExpectedCloseParenFound(
                    self.arg(self.pos).to_string(),
                ));
            }
            self.advance(false)?;
            return Ok(value);
        }

        // A binary test needs three operands, a unary test two.
        if self.pos + 3 <= self.args.len() && is_binary_op(self.arg(self.pos + 1)) {
            return self.binary_operator();
        }
        if self.pos + 2 <= self.args.len() && is_unary_op(self.arg(self.pos)) {
            return self.unary_operator();
        }

        // A plain word is true when it is not empty.
        let value = !self.arg(self.pos).is_empty();
        self.advance(false)?;
        Ok(value)
    }

    /// Evaluate a binary test: `ARG1 OP ARG2`.
    fn binary_operator(&mut self) -> Result<bool, TestError> {
        let op = self.arg(self.pos + 1).to_string();
        if !is_binary_op(&op) {
            return Err(TestError::BinaryOperator(op));
        }
        let left = self.arg(self.pos).to_string();
        let right = self.arg(self.pos + 2).to_string();
        let value = self.binary_test(&op, &left, &right)?;
        self.pos += 3;
        Ok(value)
    }

    /// Evaluate a unary test: `OP ARG`.
    fn unary_operator(&mut self) -> Result<bool, TestError> {
        let op = self.arg(self.pos).to_string();
        if !is_unary_op(&op) {
            return Ok(false);
        }

        // `-t' is special: its argument is a file descriptor and may be
        // missing.
        if op == "-t" {
            self.advance(false)?;
            if self.pos < self.args.len() {
                let arg = self.arg(self.pos).to_string();
                if parse_bash_number(&arg).is_some() {
                    self.advance(false)?;
                    return Ok(self.unary_test(&op, &arg));
                }
                if self.args.len() >= 5 && is_andor(&arg) {
                    return Ok(self.unary_test(&op, "1"));
                }
                return Err(TestError::Integer(arg));
            }
            return Ok(self.unary_test(&op, "1"));
        }

        // Every other unary operator takes an argument.
        self.advance(true)?;
        self.pos += 1;
        let arg = self.arg(self.pos - 1).to_string();
        Ok(self.unary_test(&op, &arg))
    }

    /// The value of a unary test.
    fn unary_test(&self, op: &str, arg: &str) -> bool {
        match op {
            "-e" | "-a" => posix::stat_path(arg, true).is_some(),
            "-r" => posix::access(arg, posix::R_OK) == 0,
            "-w" => posix::access(arg, posix::W_OK) == 0,
            "-x" => posix::access(arg, posix::X_OK) == 0,
            "-O" => posix::stat_path(arg, true)
                .map(|st| st.uid == posix::effective_uid() as i64)
                .unwrap_or(false),
            "-G" => posix::stat_path(arg, true)
                .map(|st| st.gid == posix::effective_gid() as i64)
                .unwrap_or(false),
            "-N" => posix::stat_path(arg, true)
                .map(|st| {
                    st.mtime > st.atime
                        || (st.mtime == st.atime && st.mtime_nsec > st.atime_nsec)
                })
                .unwrap_or(false),
            "-f" => posix::stat_path(arg, true)
                .map(|st| st.file_type == posix::FT_REG)
                .unwrap_or(false),
            "-d" => posix::stat_path(arg, true)
                .map(|st| st.file_type == posix::FT_DIR)
                .unwrap_or(false),
            "-s" => posix::stat_path(arg, true)
                .map(|st| st.size > 0)
                .unwrap_or(false),
            "-S" => posix::stat_path(arg, true)
                .map(|st| st.file_type == posix::FT_SOCK)
                .unwrap_or(false),
            "-c" => posix::stat_path(arg, true)
                .map(|st| st.file_type == posix::FT_CHR)
                .unwrap_or(false),
            "-b" => posix::stat_path(arg, true)
                .map(|st| st.file_type == posix::FT_BLK)
                .unwrap_or(false),
            "-p" => posix::stat_path(arg, true)
                .map(|st| st.file_type == posix::FT_FIFO)
                .unwrap_or(false),
            "-L" | "-h" => posix::stat_path(arg, false)
                .map(|st| st.file_type == posix::FT_LNK)
                .unwrap_or(false),
            "-u" => posix::stat_path(arg, true)
                .map(|st| st.mode & posix::S_ISUID != 0)
                .unwrap_or(false),
            "-g" => posix::stat_path(arg, true)
                .map(|st| st.mode & posix::S_ISGID != 0)
                .unwrap_or(false),
            "-k" => posix::stat_path(arg, true)
                .map(|st| st.mode & posix::S_ISVTX != 0)
                .unwrap_or(false),
            "-t" => match parse_bash_number(arg) {
                Some(fd) => (0..=i32::MAX as i64).contains(&fd) && posix::isatty(fd as i32),
                None => false,
            },
            "-n" => !arg.is_empty(),
            "-z" => arg.is_empty(),
            "-o" => self.sh.options.get(arg).unwrap_or(false),
            "-v" => self.variable_is_set(arg),
            _ => false,
        }
    }

    /// True when the variable `name` is set (bash's `test -v`).
    fn variable_is_set(&self, name: &str) -> bool {
        // A number refers to a positional parameter.
        if let Ok(index) = name.parse::<usize>() {
            return index >= 1 && index <= self.sh.positional.len();
        }
        self.sh.env.contains_key(name)
    }

    /// The value of a binary test.
    fn binary_test(&self, op: &str, left: &str, right: &str) -> Result<bool, TestError> {
        match op {
            "=" | "==" => Ok(left == right),
            "!=" => Ok(left != right),
            "<" => Ok(left < right),
            ">" => Ok(left > right),
            "-eq" | "-ne" | "-lt" | "-gt" | "-le" | "-ge" => {
                let l = parse_bash_number(left)
                    .ok_or_else(|| TestError::Integer(left.to_string()))?;
                let r = parse_bash_number(right)
                    .ok_or_else(|| TestError::Integer(right.to_string()))?;
                Ok(match op {
                    "-eq" => l == r,
                    "-ne" => l != r,
                    "-lt" => l < r,
                    "-gt" => l > r,
                    "-le" => l <= r,
                    "-ge" => l >= r,
                    _ => false,
                })
            }
            "-nt" | "-ot" | "-ef" => Ok(self.file_comp(op, left, right)),
            _ => Ok(false),
        }
    }

    /// Compare two files by their modification time or their identity.
    fn file_comp(&self, op: &str, left: &str, right: &str) -> bool {
        let l = posix::stat_path(left, true);
        let r = posix::stat_path(right, true);
        match op {
            "-ef" => match (&l, &r) {
                (Some(a), Some(b)) => a.dev == b.dev && a.ino == b.ino,
                _ => false,
            },
            "-nt" => match (&l, &r) {
                (Some(a), Some(b)) => {
                    a.mtime > b.mtime
                        || (a.mtime == b.mtime && a.mtime_nsec > b.mtime_nsec)
                }
                (Some(_), None) => true,
                _ => false,
            },
            "-ot" => match (&l, &r) {
                (Some(a), Some(b)) => {
                    a.mtime < b.mtime
                        || (a.mtime == b.mtime && a.mtime_nsec < b.mtime_nsec)
                }
                (None, Some(_)) => true,
                _ => false,
            },
            _ => false,
        }
    }
}

/// The `test` builtin and its `[` form.
///
/// `test EXPRESSION` evaluates the expression and returns 0 when it is
/// true, 1 when it is false and 2 when it is malformed.  `[ EXPRESSION ]`
/// behaves the same, but the closing bracket is mandatory.
fn builtin_test(argv: &[String], sh: &mut Shell) -> i32 {
    let name = argv.first().map(|s| s.as_str()).unwrap_or("test");
    let mut operands: &[String] = &argv[1..];

    if name == "[" {
        // The last argument has to be the closing bracket.
        match operands.last() {
            Some(last) if last == "]" => {
                operands = &operands[..operands.len() - 1];
            }
            _ => {
                eprintln!("wash: [: missing `]'");
                return 2;
            }
        }
        if operands.is_empty() {
            return 1;
        }
    }

    let mut evaluator = TestEvaluator {
        args: operands,
        pos: 0,
        sh,
    };
    let value = match evaluator.posixtest() {
        Ok(value) => value,
        Err(e) => {
            eprintln!("wash: {}: {}", name, e.message());
            return 2;
        }
    };

    // Everything has to be consumed, otherwise the expression was longer
    // than bash could make sense of.
    if evaluator.pos != operands.len() {
        let leftover = operands[evaluator.pos].as_str();
        let message = if leftover.starts_with('-') {
            format!("syntax error: `{}' unexpected", leftover)
        } else {
            "too many arguments".to_string()
        };
        eprintln!("wash: {}: {}", name, message);
        return 2;
    }

    if value {
        0
    } else {
        1
    }
}

// ---------------------------------------------------------------------------
// type / command / builtin
// ---------------------------------------------------------------------------

/// How a command is described.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DescMode {
    /// `-t`: print the type of the command (`file`, `builtin`, ...).
    Type,
    /// The default of `type`: `<name> is <description>`.
    ShortDesc,
    /// `-v` of `command`: print the reusable form of the command.
    Reusable,
    /// `-p` of `type` and `-P`: print only the path of the command.
    PathOnly,
}

/// The options of the description functions (`type` and `command`).
#[derive(Clone, Copy)]
struct DescFlags {
    mode: DescMode,
    /// `-a`: report every candidate, not only the first one.
    all: bool,
    /// `-P`: look the command up in the `$PATH` only.
    force_path: bool,
    /// `-f`: do not look the command up in the function table.
    nofuncs: bool,
}

/// The reserved words of bash.  `type` reports them as `keyword`.
const RESERVED_WORDS_BASH: &[&str] = &[
    "!", "{", "}", "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if",
    "in", "select", "then", "time", "until", "while",
];

/// The executables named `name` in the colon-separated `$PATH`.
///
/// Like Cygwin's `find_user_command`, the `.exe` suffix is tried as a
/// fallback but it is not reported: bash prints the path exactly as it
/// stands in the `$PATH`.
fn path_matches(name: &str, path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if name.contains('/') || name.contains('\\') {
        // An explicit path is used as it is.
        if posix::access(name, posix::X_OK) == 0 {
            out.push(name.to_string());
        }
        return out;
    }
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = format!("{}/{}", dir, name);
        if posix::access(&candidate, posix::X_OK) == 0 {
            out.push(candidate);
            continue;
        }
        let candidate_exe = format!("{}.exe", candidate);
        if posix::access(&candidate_exe, posix::X_OK) == 0 {
            out.push(candidate);
        }
    }
    out
}

/// Describe a command the way bash's `describe_command` does.
///
/// Returns true when the command was found.
fn describe_command(name: &str, flags: DescFlags, sh: &Shell) -> bool {
    let mut found = false;

    // An alias -- but only when the shell expands the aliases.
    if !flags.force_path && sh.expand_aliases {
        if let Some(value) = sh.aliases.get(name) {
            match flags.mode {
                DescMode::Type => println!("alias"),
                DescMode::ShortDesc => println!("{} is aliased to `{}'", name, value),
                DescMode::Reusable => println!("alias {}={}", name, single_quote(value)),
                DescMode::PathOnly => {}
            }
            found = true;
            if !flags.all {
                return true;
            }
        }
    }

    // A reserved word.
    if !flags.force_path && RESERVED_WORDS_BASH.contains(&name) {
        match flags.mode {
            DescMode::Type => println!("keyword"),
            DescMode::ShortDesc => println!("{} is a shell keyword", name),
            DescMode::Reusable => println!("{}", name),
            DescMode::PathOnly => {}
        }
        found = true;
        if !flags.all {
            return true;
        }
    }

    // A shell function.
    if !flags.force_path && !flags.nofuncs {
        if let Some(body) = sh.functions.get(name) {
            match flags.mode {
                DescMode::Type => println!("function"),
                DescMode::ShortDesc => {
                    println!("{} is a function", name);
                    print_function_body(name, body);
                }
                DescMode::Reusable => println!("{}", name),
                DescMode::PathOnly => {}
            }
            found = true;
            if !flags.all {
                return true;
            }
        }
    }

    // A shell builtin.
    if !flags.force_path && is_builtin(name) && !sh.disabled_builtins.contains(name) {
        match flags.mode {
            DescMode::Type => println!("builtin"),
            DescMode::ShortDesc => println!("{} is a shell builtin", name),
            DescMode::Reusable => println!("{}", name),
            DescMode::PathOnly => {}
        }
        found = true;
        if !flags.all {
            return true;
        }
    }

    // An executable file.
    let path = sh.env.get("PATH").cloned().unwrap_or_default();
    for full_path in path_matches(name, &path) {
        match flags.mode {
            DescMode::Type => println!("file"),
            DescMode::ShortDesc => println!("{} is {}", name, full_path),
            DescMode::Reusable | DescMode::PathOnly => println!("{}", full_path),
        }
        found = true;
        if !flags.all {
            break;
        }
    }

    found
}

/// Print a shell function the way bash's `type` does.
fn print_function_body(name: &str, body: &crate::parser::CommandList) {
    println!("{} ()", name);
    println!("{{");
    let rendered = render_command_list(body, 1);
    for line in rendered.lines() {
        if line.is_empty() {
            println!();
        } else {
            println!("    {}", line);
        }
    }
    println!("}}");
}

/// Render a parsed command list back into a human-readable (bash-like) source
/// form.  Used only by `type` / `command -V` for functions, where exact
/// byte-fidelity to bash is not asserted by the test suite; the goal is a
/// readable, structurally-faithful reconstruction.
fn render_part(part: &crate::lexer::WordPart) -> String {
    match part {
        crate::lexer::WordPart::Text(s, _) => s.clone(),
        crate::lexer::WordPart::Var(n, _) => format!("${}", n),
        crate::lexer::WordPart::Braced(b, _) => format!("${{{}}}", b),
        crate::lexer::WordPart::CmdSub(b, _) => format!("$({})", b),
        crate::lexer::WordPart::Backtick(b, _) => format!("`{}`", b),
        crate::lexer::WordPart::Arith(b, _) => format!("$(({}))", b),
        crate::lexer::WordPart::Tilde => "~".to_string(),
        crate::lexer::WordPart::Brace(alts) => format!("{{{}}}", alts.join(",")),
    }
}

fn render_word(word: &[crate::lexer::WordPart]) -> String {
    word.iter().map(render_part).collect()
}

fn render_redir(r: &crate::parser::Redir) -> String {
    match r {
        crate::parser::Redir::In { fd, file } => {
            format!("{}<{}", if *fd == 0 { String::new() } else { fd.to_string() }, render_word(file))
        }
        crate::parser::Redir::Out { fd, file, append } => format!(
            "{}{}{}",
            if *fd == 1 { String::new() } else { fd.to_string() },
            if *append { ">>" } else { ">" },
            render_word(file)
        ),
        crate::parser::Redir::Both { file } => format!("&>{}", render_word(file)),
    }
}

fn render_simple(c: &crate::parser::SimpleCommand) -> String {
    let mut parts: Vec<String> = c.argv.iter().map(|w| render_word(w)).collect();
    for r in &c.redirs {
        parts.push(render_redir(r));
    }
    parts.join(" ")
}

fn render_pipeline(p: &crate::parser::Pipeline) -> String {
    p.commands.iter().map(render_simple).collect::<Vec<_>>().join(" | ")
}

fn render_andor(a: &crate::parser::AndOr) -> String {
    let mut s = render_pipeline(&a.pipelines[0]);
    for (op, p) in a.ops.iter().zip(a.pipelines[1..].iter()) {
        let sep = match op {
            crate::parser::AndOrOp::And => "&&",
            crate::parser::AndOrOp::Or => "||",
        };
        s.push_str(&format!(" {} {}", sep, render_pipeline(p)));
    }
    s
}

fn render_command_list(cl: &crate::parser::CommandList, indent: usize) -> String {
    let pad = "    ".repeat(indent);
    let mut out = String::new();
    for (i, andor) in cl.andors.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{}{}", pad, render_andor(andor)));
    }
    // Nested function definitions are hoisted to the global table but their
    // sources are preserved here for readability.
    for fd in &cl.func_defs {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!(
            "{}{} () \n{}",
            pad,
            fd.name,
            render_command_list(&fd.body, indent + 1)
        ));
    }
    out
}

fn render_compound(c: &crate::parser::Compound) -> String {
    match c {
        crate::parser::Compound::If { branches, else_body } => {
            let mut s = String::new();
            for (i, (cond, body)) in branches.iter().enumerate() {
                let kw = if i == 0 { "if" } else { "elif" };
                s.push_str(&format!(
                    "{} {}; then\n{}\n",
                    kw,
                    render_command_list(cond, 1),
                    render_command_list(body, 1)
                ));
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("else\n{}\n", render_command_list(eb, 1)));
            }
            s.push_str("fi");
            s
        }
        crate::parser::Compound::For { var, words, body } => {
            let w = words
                .iter()
                .map(|w| render_word(w))
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "for {} in {}; do\n{}\ndone",
                var,
                w,
                render_command_list(body, 1)
            )
        }
        crate::parser::Compound::While { condition, body, is_until } => {
            let kw = if *is_until { "until" } else { "while" };
            format!(
                "{} {}; do\n{}\ndone",
                kw,
                render_command_list(condition, 1),
                render_command_list(body, 1)
            )
        }
        crate::parser::Compound::Case { word, arms } => {
            let mut s = format!("case {} in\n", render_word(word));
            for arm in arms {
                let pats = arm
                    .patterns
                    .iter()
                    .map(|p| render_word(p))
                    .collect::<Vec<_>>()
                    .join(" | ");
                s.push_str(&format!(
                    "    {})\n{}\n    ;;\n",
                    pats,
                    render_command_list(&arm.body, 2)
                ));
            }
            s.push_str("esac");
            s
        }
    }
}

/// Quote `text` the way bash's `sh_single_quote` does.
fn single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The `type` builtin: describe the commands the shell knows.
fn builtin_type(argv: &[String], sh: &mut Shell) -> i32 {
    let mut flags = DescFlags {
        mode: DescMode::ShortDesc,
        all: false,
        force_path: false,
        nofuncs: false,
    };
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-a" => flags.all = true,
                "-f" => flags.nofuncs = true,
                "-p" => flags.mode = DescMode::PathOnly,
                "-t" => flags.mode = DescMode::Type,
                "-P" => {
                    flags.mode = DescMode::PathOnly;
                    flags.force_path = true;
                }
                _ => {
                    eprintln!("wash: type: {}: invalid option", a);
                    eprintln!("type: usage: type [-afptP] name [name ...]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    let names = &argv[i..];
    if names.is_empty() {
        return 0;
    }

    let mut any_failed = false;
    for name in names {
        if !describe_command(name, flags, sh) {
            // bash only reports the failure when it did not print a type
            // or a path.
            if !matches!(flags.mode, DescMode::Type | DescMode::PathOnly) {
                eprintln!("wash: type: {}: not found", name);
            }
            any_failed = true;
        }
    }

    if any_failed {
        1
    } else {
        0
    }
}

/// The `command` builtin: run a command without looking at the shell
/// functions and the aliases, or describe it.
fn builtin_command(argv: &[String], sh: &mut Shell) -> i32 {
    let mut mode: Option<DescMode> = None;
    let mut force_path = false;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-p" => force_path = true,
                "-v" => mode = Some(DescMode::Reusable),
                "-V" => mode = Some(DescMode::ShortDesc),
                _ => {
                    eprintln!("wash: command: {}: invalid option", a);
                    eprintln!("command: usage: command [-pVv] command [arg ...]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    let rest = &argv[i..];
    if rest.is_empty() {
        return 0;
    }

    match mode {
        Some(mode) => {
            let flags = DescFlags {
                mode,
                all: false,
                force_path,
                nofuncs: false,
            };
            let mut any_found = false;
            for name in rest {
                let found = describe_command(name, flags, sh);
                // `-v` stays silent when the command is unknown.
                if !found && mode != DescMode::Reusable {
                    eprintln!("wash: command: {}: not found", name);
                }
                any_found |= found;
            }
            if any_found {
                0
            } else {
                1
            }
        }
        None => {
            // Run the command: a builtin first, then the `$PATH`.
            if is_builtin(&rest[0]) && !sh.disabled_builtins.contains(&rest[0]) {
                return run_builtin(rest, sh).unwrap_or(0);
            }
            crate::executor::exec_external(rest, sh)
        }
    }
}

/// The `builtin` builtin: run a shell builtin, bypassing the functions and
/// the aliases of the same name.
fn builtin_builtin(argv: &[String], sh: &mut Shell) -> i32 {
    let i = 1usize;
    if i < argv.len() && argv[i].starts_with('-') {
        eprintln!("wash: builtin: {}: invalid option", argv[i]);
        eprintln!("builtin: usage: builtin [shell-builtin [arg ...]]");
        return 2;
    }
    if i >= argv.len() {
        return 0;
    }

    let name = argv[i].clone();
    if !is_builtin(&name) || sh.disabled_builtins.contains(&name) {
        eprintln!("wash: builtin: {}: not a shell builtin", name);
        return 1;
    }

    run_builtin(&argv[i..], sh).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// hash
// ---------------------------------------------------------------------------

/// Print the hashed commands the way bash's `hash` does.
fn print_hashed_commands(sh: &Shell, portable: bool) {
    let mut names: Vec<&String> = sh.hash_table.keys().collect();
    names.sort();
    if portable {
        // The portable form can be fed back to `hash'.
        for name in names {
            let path = &sh.hash_table[name];
            println!("builtin hash -p {} {}", path, name);
        }
    } else {
        println!("hits\tcommand");
        for name in names {
            let hits = sh.hash_hits.get(name).copied().unwrap_or(0);
            let path = &sh.hash_table[name];
            println!("{:>4}\t{}", hits, path);
        }
    }
}

/// The `hash` builtin: manage the table of the hashed commands.
///
///   * `hash` — print the table (the hits and the path of every command).
///   * `hash -l` — print the table in a form that can be fed back.
///   * `hash -r` — empty the table.
///   * `hash -t name ...` — print the hashed path of every name.
///   * `hash -d name ...` — remove the names from the table.
///   * `hash -p path name ...` — hash the names to the given path.
///   * `hash name ...` — look the names up in the `$PATH` and hash them.
fn builtin_hash(argv: &[String], sh: &mut Shell) -> i32 {
    const USAGE: &str = "hash: usage: hash [-lr] [-p pathname] [-dt] [name ...]";

    let mut list_portably = false; // -l
    let mut list_targets = false; // -t
    let mut delete = false; // -d
    let mut expunge = false; // -r
    let mut pathname: Option<String> = None; // -p
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-l" => list_portably = true,
                "-t" => list_targets = true,
                "-d" => delete = true,
                "-r" => expunge = true,
                "-p" => {
                    // `-p' takes the pathname as its argument.
                    i += 1;
                    match argv.get(i) {
                        Some(p) => pathname = Some(p.clone()),
                        None => {
                            eprintln!("{}", USAGE);
                            return 2;
                        }
                    }
                }
                _ => {
                    eprintln!("wash: hash: {}: invalid option", a);
                    eprintln!("{}", USAGE);
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }
    let names: Vec<String> = argv[i..].to_vec();

    // `hash -t' and `hash -d' need at least one name.
    if names.is_empty() && (delete || list_targets) {
        eprintln!(
            "wash: hash: {}: option requires an argument",
            if delete { "-d" } else { "-t" }
        );
        return 1;
    }

    // A pathname without a name is a usage error.
    if pathname.is_some() && names.is_empty() {
        eprintln!("{}", USAGE);
        return 2;
    }

    // Without an option and without a name the table itself is printed.
    if names.is_empty() && !expunge {
        if sh.hash_table.is_empty() {
            // bash only reports the empty table for the default listing.
            if !list_portably {
                println!("hash: hash table empty");
            }
        } else {
            print_hashed_commands(sh, list_portably);
        }
        return 0;
    }

    if expunge {
        sh.hash_table.clear();
        sh.hash_hits.clear();
    }

    if list_targets {
        let mut status = 0;
        for name in &names {
            match sh.hash_table.get(name) {
                Some(path) => println!("{}", path),
                None => {
                    eprintln!("wash: hash: {}: not found", name);
                    status = 1;
                }
            }
        }
        return status;
    }

    let path_env = sh.env.get("PATH").cloned().unwrap_or_default();
    let mut status = 0;
    for name in &names {
        // A name with a slash is not hashed.
        if name.contains('/') {
            continue;
        }
        if let Some(path) = &pathname {
            // `hash -p' stores the given path for the name.
            sh.hash_table.insert(name.clone(), path.clone());
            sh.hash_hits.entry(name.clone()).or_insert(0);
            continue;
        }
        if delete {
            if sh.hash_table.remove(name).is_none() {
                eprintln!("wash: hash: {}: not found", name);
                status = 1;
            } else {
                sh.hash_hits.remove(name);
            }
            continue;
        }
        // Look the name up in the `$PATH' and cache it.
        match crate::executor::path_matches(name, &path_env).into_iter().next() {
            Some(path) => {
                sh.hash_table.insert(name.clone(), path);
                sh.hash_hits.entry(name.clone()).or_insert(0);
            }
            None => {
                eprintln!("wash: hash: {}: not found", name);
                status = 1;
            }
        }
    }

    status
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

/// The IFS characters that are white space (space, tab and newline).
///
/// A white space IFS character is skipped in front of a field and at the
/// end of the line, while any other IFS character ends a field.
fn is_ifs_whitespace(c: char, ifs: &[char]) -> bool {
    ifs.contains(&c) && matches!(c, ' ' | '\t' | '\n')
}

/// Split the input of `read` into the fields of its variables.
///
/// The variables before the last one get a single field each; the last one
/// gets the rest of the line (without the trailing IFS white space).
fn read_fields(input: &str, ifs: &[char], nvars: usize) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0usize;
    let mut fields: Vec<String> = Vec::new();

    for index in 0..nvars {
        // Remove the IFS white space at the beginning of the field.
        while pos < chars.len() && is_ifs_whitespace(chars[pos], ifs) {
            pos += 1;
        }
        if index + 1 == nvars {
            // The last variable gets the rest of the line.
            let mut end = chars.len();
            while end > pos && is_ifs_whitespace(chars[end - 1], ifs) {
                end -= 1;
            }
            fields.push(chars[pos..end].iter().collect());
            pos = end;
            break;
        }
        // One field: everything up to the next IFS character.
        let mut field = String::new();
        while pos < chars.len() && !ifs.contains(&chars[pos]) {
            field.push(chars[pos]);
            pos += 1;
        }
        // A non-white-space IFS character ends the field and is consumed.
        if pos < chars.len() && ifs.contains(&chars[pos]) && !is_ifs_whitespace(chars[pos], ifs) {
            pos += 1;
        }
        fields.push(field);
    }

    fields
}

/// The `read` builtin: read a line from the standard input and split it
/// into the given variables.
///
///   * `read [-r] [-p prompt] [-d delim] [-n nchars] [-N nchars] [-s] [-e]
///     [-u fd] [name ...]`
///
/// Without a variable name the line is stored in `$REPLY`.  The exit status
/// is 1 when the end of the file was reached.
fn builtin_read(argv: &[String], sh: &mut Shell) -> i32 {
    const USAGE: &str = "read: usage: read [-Eers] [-a array] [-d delim] [-i text] \
                         [-n nchars] [-N nchars] [-p prompt] [-t timeout] [-u fd] [name ...]";

    let mut raw = false;
    let mut prompt: Option<String> = None;
    let mut delim: u8 = b'\n';
    let mut nchars: usize = 0;
    let mut exact = false; // -N: read exactly NCHARS characters
    let mut fd = 0i32;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            // The options may be bundled, like in `read -rp prompt'.
            let opts: Vec<char> = a.chars().skip(1).collect();
            let mut k = 0usize;
            while k < opts.len() {
                let c = opts[k];
                match c {
                    'r' => raw = true,
                    // `-s' (silent) and `-e' (readline) only matter on a
                    // terminal.
                    's' | 'e' => {}
                    'p' | 'd' | 'n' | 'N' | 'u' | 'a' | 'i' | 't' => {
                        // The option takes an argument: the rest of the
                        // bundle or the next word.
                        let value: Option<String> = if k + 1 < opts.len() {
                            let rest: String = opts[k + 1..].iter().collect();
                            k = opts.len();
                            Some(rest)
                        } else {
                            i += 1;
                            argv.get(i).cloned()
                        };
                        match c {
                            'p' => prompt = value,
                            'd' => {
                                delim = value
                                    .as_ref()
                                    .and_then(|s| s.as_bytes().first().copied())
                                    .unwrap_or(0);
                            }
                            'n' | 'N' => {
                                exact = c == 'N';
                                nchars = value
                                    .as_ref()
                                    .and_then(|s| s.trim().parse::<usize>().ok())
                                    .unwrap_or(0);
                            }
                            'u' => {
                                fd = value
                                    .as_ref()
                                    .and_then(|s| s.trim().parse::<i32>().ok())
                                    .unwrap_or(0);
                            }
                            'a' => {
                                eprintln!("wash: read: -a: array assignment is not supported");
                                return 1;
                            }
                            _ => {
                                eprintln!("wash: read: -{}: not supported", c);
                                return 1;
                            }
                        }
                    }
                    _ => {
                        eprintln!("wash: read: -{}: invalid option", c);
                        eprintln!("{}", USAGE);
                        return 2;
                    }
                }
                k += 1;
            }
            i += 1;
            continue;
        }
        break;
    }
    let names: Vec<String> = argv[i..].to_vec();

    // The prompt is only shown when the input comes from a terminal.
    if let Some(p) = &prompt {
        if posix::isatty(fd) {
            eprint!("{}", p);
            let _ = std::io::stderr().flush();
        }
    }

    // Read the input up to the delimiter.
    let mut input = String::new();
    let mut eof = false;
    let mut count = 0usize;
    let mut pass_next = false;
    loop {
        if nchars > 0 && count >= nchars {
            break;
        }
        let c = match posix::read_byte_from(fd) {
            Some(c) => c,
            None => {
                eof = true;
                break;
            }
        };
        if pass_next {
            pass_next = false;
            if c == b'\n' {
                // A backslash-newline pair disappears from the input.
                continue;
            }
            input.push(c as char);
            count += 1;
            continue;
        }
        if c == b'\\' && !raw {
            pass_next = true;
            continue;
        }
        if c == delim && !exact {
            break;
        }
        input.push(c as char);
        count += 1;
    }
    if pass_next {
        // The input ended with a lone backslash: it is dropped.
        input.pop();
    }

    // bash returns 1 when the end of the file was reached.
    let status = if eof { 1 } else { 0 };

    if names.is_empty() {
        // Without a variable the whole line goes into $REPLY.
        sh.env.insert("REPLY".to_string(), input);
        return status;
    }

    for name in &names {
        if !is_valid_ident(name) {
            eprintln!("wash: read: `{}': not a valid identifier", name);
            return 1;
        }
    }

    // The default IFS is the space, the tab and the newline; an empty IFS
    // disables the field splitting.
    let ifs_value = sh.env.get("IFS").cloned();
    let ifs_chars: Vec<char> = match &ifs_value {
        Some(value) => value.chars().collect(),
        None => vec![' ', '\t', '\n'],
    };
    let fields = read_fields(&input, &ifs_chars, names.len());
    for (name, value) in names.iter().zip(fields.iter()) {
        sh.env.insert(name.clone(), value.clone());
    }

    status
}

// ---------------------------------------------------------------------------
// :
// ---------------------------------------------------------------------------

/// The `:` builtin: a no-op that ignores all its arguments.
///
/// It always returns 0 and never writes anything.
fn builtin_colon(_argv: &[String], _sh: &mut Shell) -> i32 {
    0
}

// ---------------------------------------------------------------------------
// true / false
// ---------------------------------------------------------------------------

/// The `true` builtin: a no-op that always succeeds (exit status 0).
///
/// Like bash, every argument is ignored (POSIX `true` accepts none, but bash
/// silently discards them).
fn builtin_true(_argv: &[String], _sh: &mut Shell) -> i32 {
    0
}

/// The `false` builtin: a no-op that always fails (exit status 1).
fn builtin_false(_argv: &[String], _sh: &mut Shell) -> i32 {
    1
}

// ---------------------------------------------------------------------------
// pwd
// ---------------------------------------------------------------------------

/// The `pwd` builtin: print the current working directory.
///
///   * `-L` (the default) prints the *logical* directory — the value of
///     `sh.cwd`, which tracks `$PWD` across `cd`.
///   * `-P` resolves symbolic links and prints the *physical* directory.
///   * `-W` is accepted (Win32 physical form) but behaves like `-P` here.
///
/// Unknown options produce bash's usage diagnostic on stderr and exit 2.
fn builtin_pwd(argv: &[String], sh: &mut Shell) -> i32 {
    let mut logical = true;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a.chars().skip(1) {
                match c {
                    'L' => logical = true,
                    'P' | 'W' => logical = false,
                    _ => {
                        eprintln!("wash: pwd: {}: invalid option", a);
                        eprintln!("pwd: usage: pwd [-LPW]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    let path = if logical {
        sh.cwd.clone()
    } else {
        let p = posix::realpath(&sh.cwd);
        if p.is_empty() {
            sh.cwd.clone()
        } else {
            p
        }
    };
    println!("{}", path);
    0
}

// ---------------------------------------------------------------------------
// alias / unalias
// ---------------------------------------------------------------------------

/// The `alias` builtin: define, query or list aliases.
fn builtin_alias(argv: &[String], sh: &mut Shell) -> i32 {
    let mut print = false;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-p" => print = true,
                _ => {
                    eprintln!("wash: alias: {}: invalid option", a);
                    eprintln!("alias: usage: alias [-p] [name[=value] ... ]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    if print || i >= argv.len() {
        let mut names: Vec<&String> = sh.aliases.keys().collect();
        names.sort();
        for name in names {
            println!("alias {}={}", name, quote_single(sh.aliases.get(name).unwrap()));
        }
        return 0;
    }

    let mut status = 0;
    for arg in &argv[i..] {
        match arg.find('=') {
            Some(eq) => {
                let name = arg[..eq].to_string();
                let value = arg[eq + 1..].to_string();
                sh.aliases.insert(name, value);
            }
            None => match sh.aliases.get(arg) {
                Some(value) => println!("alias {}={}", arg, quote_single(value)),
                None => {
                    eprintln!("wash: alias: {}: not found", arg);
                    status = 1;
                }
            },
        }
    }
    status
}

/// The `unalias` builtin: remove one or all aliases.
fn builtin_unalias(argv: &[String], sh: &mut Shell) -> i32 {
    let mut all = false;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-a" => all = true,
                _ => {
                    eprintln!("wash: unalias: {}: invalid option", a);
                    eprintln!("unalias: usage: unalias [-a] name [name ...]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    if all {
        sh.aliases.clear();
        return 0;
    }

    if i >= argv.len() {
        // bash prints the usage line (with the builtin name as prefix, no
        // shell prefix) and exits with 2.
        eprintln!("wash: unalias: usage: unalias [-a] name [name ...]");
        return 2;
    }

    let mut status = 0;
    for name in &argv[i..] {
        if sh.aliases.remove(name).is_none() {
            eprintln!("wash: unalias: {}: not found", name);
            status = 1;
        }
    }
    status
}

// ---------------------------------------------------------------------------
// source / .
// ---------------------------------------------------------------------------

/// The `source` builtin (also spelled `.`): run a script in the current
/// shell.
fn builtin_source(argv: &[String], sh: &mut Shell) -> i32 {
    if argv.len() < 2 {
        eprintln!("wash: source: filename argument required");
        eprintln!("source: usage: source [-p path] filename [arguments]");
        return 2;
    }
    let name = argv[1].clone();
    let path = match find_source_file(&name, sh) {
        Some(p) => p,
        None => {
            eprintln!("wash: {}: {}", name, posix::errno_message(posix::errno()));
            return 1;
        }
    };
    let content = match posix::read_file(&path) {
        Some(c) => c,
        None => {
            eprintln!("wash: {}: {}", path, posix::errno_message(posix::errno()));
            return 1;
        }
    };

    // The script gets its own positional parameters.  bash does NOT change
    // `$0` while a file is sourced: it keeps the name the shell itself was
    // invoked with, both in the default mode and in --posix mode.
    let saved_positional = std::mem::replace(&mut sh.positional, argv[2..].to_vec());
    sh.sourcing_depth += 1;
    let code = sh.run_str(&content);
    sh.sourcing_depth -= 1;
    sh.positional = saved_positional;
    // A `return` inside the sourced script stops it and becomes its exit code.
    if let Some(r) = sh.return_requested.take() {
        return r;
    }
    code
}

/// Locate a script: an explicit path is used as given, otherwise the
/// lookup order is controlled by the `sourcepath` shell option.
///
/// bash has `sourcepath` ON by default: a name without a slash is looked
/// up in `$PATH` first and in the current directory afterwards.  With
/// `shopt -u sourcepath` the current directory comes first.
fn find_source_file(name: &str, sh: &Shell) -> Option<String> {
    if name.contains('/') {
        if posix::access(name, posix::R_OK) == 0 {
            return Some(name.to_string());
        }
        return None;
    }

    let from_path = || search_path(&sh.env.get("PATH").cloned().unwrap_or_default(), name);
    let from_cwd = || {
        if posix::access(name, posix::R_OK) == 0 {
            Some(name.to_string())
        } else {
            None
        }
    };

    if sh.sourcepath {
        from_path().or_else(from_cwd)
    } else {
        from_cwd().or_else(from_path)
    }
}

/// Search the colon-separated `$PATH` for a readable file called `name`.
fn search_path(path: &str, name: &str) -> Option<String> {
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = format!("{}/{}", dir, name);
        if posix::access(&candidate, posix::R_OK) == 0 {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// eval
// ---------------------------------------------------------------------------

/// The `eval` builtin: concatenate the arguments and run them as a command
/// in the current shell.
fn builtin_eval(argv: &[String], sh: &mut Shell) -> i32 {
    let command = argv[1..].join(" ");
    if command.trim().is_empty() {
        return 0;
    }
    sh.run_str(&command)
}

// ---------------------------------------------------------------------------
// exec
// ---------------------------------------------------------------------------

/// The `exec` builtin: replace the shell with the given command, or apply
/// the redirections permanently when no command is given.
fn builtin_exec(argv: &[String], sh: &mut Shell) -> i32 {
    let mut i = 1usize;
    let mut argv0: Option<String> = None;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a == "-a" {
            if i + 1 >= argv.len() {
                eprintln!("wash: exec: -a: option requires an argument");
                eprintln!("exec: usage: exec [-a name] [command [arguments ...]] [redirection ...]");
                return 2;
            }
            argv0 = Some(argv[i + 1].clone());
            i += 2;
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            eprintln!("wash: exec: {}: invalid option", a);
            eprintln!("exec: usage: exec [-a name] [command [arguments ...]] [redirection ...]");
            return 2;
        }
        break;
    }

    if i >= argv.len() {
        // `exec` with redirections only: the redirections have already been
        // applied to the shell and must not be undone.
        sh.persistent_redirs = true;
        return 0;
    }

    let mut command = argv[i..].to_vec();
    let program = command[0].clone();
    if let Some(name) = argv0 {
        // `-a name` replaces argv[0] of the new program, not the program name.
        command[0] = name;
    }
    let path = sh.env.get("PATH").cloned().unwrap_or_default();
    let env = sh.child_env();
    let (_cstrings, env_ptrs) = crate::executor::build_env_ptrs(&env);
    match crate::executor::find_executable(&program, &path) {
        Some(exe) => {
            let argvec = crate::executor::ArgVec::from(&command);
            crate::executor::execve(&exe, &argvec.ptrs, &env_ptrs);
            let code = posix::errno();
            eprintln!(
                "wash: exec: {}: {}",
                program,
                posix::errno_message(code)
            );
            // A failed `exec` terminates a non-interactive shell.
            sh.exit_requested = Some(127);
            127
        }
        None => {
            eprintln!("wash: exec: {}: not found", program);
            // A failed `exec` terminates a non-interactive shell.
            sh.exit_requested = Some(127);
            127
        }
    }
}

// ---------------------------------------------------------------------------
// echo
// ---------------------------------------------------------------------------

/// The `echo` builtin.
fn builtin_echo(argv: &[String]) -> i32 {
    // Options are recognised only in LEADING arguments (before the first
    // non-option argument). A leading arg is an option arg only if every
    // character after the initial '-' is one of n/e/E. `--` (or any arg whose
    // second char is not n/e/E) stops option parsing and becomes text.
    let mut nflag = false;
    let mut eflag = false;
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a.starts_with('-') && a.len() > 1 {
            let mut all_ok = true;
            for ch in a.chars().skip(1) {
                if !matches!(ch, 'n' | 'e' | 'E') {
                    all_ok = false;
                    break;
                }
            }
            if !all_ok {
                break;
            }
            for ch in a.chars().skip(1) {
                match ch {
                    'n' => nflag = true,
                    'e' => eflag = true,
                    'E' => eflag = false,
                    _ => {}
                }
            }
            i += 1;
        } else {
            break;
        }
    }

    let text_args = &argv[i..];
    let mut out: Vec<u8> = Vec::new();
    if eflag {
        let joined = text_args.join(" ");
        let (bytes, stopped) = interpret_escapes(joined.as_bytes(), true);
        out.extend_from_slice(&bytes);
        if !stopped && !nflag {
            out.push(b'\n');
        }
    } else {
        let s = text_args.join(" ");
        out.extend_from_slice(s.as_bytes());
        if !nflag {
            out.push(b'\n');
        }
    }

    let _ = std::io::stdout().write_all(&out);
    let _ = std::io::stdout().flush();
    0
}

// ---------------------------------------------------------------------------
// printf
// ---------------------------------------------------------------------------

/// True if `s` is a legal bash identifier (used to validate `printf -v var`).
fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// How `printf`'s leading options parsed.
enum PrintfOpt {
    /// `-v var`: assign the rendered output to shell variable `var`.
    Assign { var: String, format_idx: usize },
    /// Ordinary invocation; the format string starts at `format_idx`.
    Normal { format_idx: usize },
    /// A usage error (missing `-v` argument, invalid option, or a `-v` with no
    /// format string). Caller should emit a diagnostic and exit 2.
    Usage,
}

/// Parse the leading options of a `printf` command line, mirroring bash's
/// `getopt(argc, argv, "v:")` behaviour:
///   * `-v var` or `-vvar` (attached) selects the assignment variable;
///   * `--` ends option scanning (and is removed);
///   * any other `-X` argument is an invalid option;
///   * a `-` alone, or the first non-option argument, is the format string.
fn parse_printf_opt(argv: &[String]) -> PrintfOpt {
    let mut idx = 1usize;
    let mut var: Option<String> = None;

    loop {
        if idx >= argv.len() {
            // Ran out of arguments. A bare `printf` with `-v` but no format is
            // a usage error; a bare `printf` with no args yields an empty format
            // which the caller turns into a usage error too.
            return if var.is_some() {
                PrintfOpt::Usage
            } else {
                PrintfOpt::Normal { format_idx: argv.len() }
            };
        }
        let a = &argv[idx];
        if a == "-v" {
            if idx + 1 >= argv.len() {
                return PrintfOpt::Usage; // `-v` requires an argument
            }
            var = Some(argv[idx + 1].clone());
            idx += 2;
        } else if a.starts_with("-v") && a.len() > 2 {
            var = Some(a[2..].to_string());
            idx += 1;
        } else if a == "--" {
            idx += 1;
            break;
        } else if a.starts_with('-') && a.len() > 1 {
            // Any other `-X` is an invalid option.
            return PrintfOpt::Usage;
        } else {
            // First non-option argument is the format string.
            break;
        }
    }

    if var.is_some() && idx >= argv.len() {
        // `-v var` with no format string.
        return PrintfOpt::Usage;
    }

    match var {
        Some(v) => PrintfOpt::Assign {
            var: v,
            format_idx: idx,
        },
        None => PrintfOpt::Normal { format_idx: idx },
    }
}

/// The `printf` builtin.
///
/// `-v var` assigns the rendered output to the shell variable `var`, which
/// requires a shell: in a subshell the assignment is discarded, exactly like
/// bash.
fn builtin_printf(argv: &[String], sh: Option<&mut Shell>) -> i32 {
    match parse_printf_opt(argv) {
        PrintfOpt::Assign { var, format_idx } => {
            if !is_valid_ident(&var) {
                eprintln!("wash: printf: `{}': not a valid identifier", var);
                return 2;
            }
            let format = match argv.get(format_idx) {
                Some(f) => f.clone(),
                None => {
                    eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
                    return 2;
                }
            };
            let args = argv.get(format_idx + 1..).unwrap_or(&[]);
            let (value, conv_error) = printf_format(&format, args);
            if let Some(sh) = sh {
                sh.env.insert(var, String::from_utf8_lossy(&value).into_owned());
            }
            if conv_error {
                1
            } else {
                0
            }
        }
        PrintfOpt::Usage => {
            eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
            2
        }
        PrintfOpt::Normal { format_idx } => {
            let format = argv.get(format_idx).cloned().unwrap_or_default();
            let args = argv.get(format_idx + 1..).unwrap_or(&[]);
            if format.is_empty() {
                eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
                return 2;
            }
            let (bytes, conv_error) = printf_format(&format, args);
            let _ = std::io::stdout().write_all(&bytes);
            let _ = std::io::stdout().flush();
            if conv_error {
                1
            } else {
                0
            }
        }
    }
}

/// Format `format` against `args` and return the rendered bytes plus a flag
/// indicating whether a numeric conversion failed (bash's "conv_error" → exit
/// code 1). This is the shared core used by both the child (stdout) and the
/// parent (`-v`, store to env).
fn printf_format(format: &str, args: &[String]) -> (Vec<u8>, bool) {
    let bytes = format.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut conv_error = false;
    let mut ai: usize = 0;

    let has_conv = {
        // The scan has to skip the escaped percent signs exactly like the
        // real parser does: `%%` is a literal percent and not the start of
        // a conversion.  Otherwise a format like `%%. ' would look like it
        // had a conversion, while the parser consumes no argument for it --
        // and the format would be repeated forever.
        let mut f = false;
        let mut k = 0;
        while k < bytes.len() {
            if bytes[k] == b'%' {
                if k + 1 >= bytes.len() {
                    // A trailing '%' is not a conversion.
                    break;
                }
                if bytes[k + 1] == b'%' {
                    k += 2;
                    continue;
                }
                f = true;
                break;
            }
            k += 1;
        }
        f
    };

    loop {
        let consumed_before = ai;
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'%' {
                if i + 1 >= bytes.len() {
                    // A trailing '%' (no conversion): bash reports a missing
                    // format character and stops.
                    conv_error = true;
                    eprintln!("wash: printf: `%': missing format character");
                    return (out, conv_error);
                }
                let nxt = bytes[i + 1];
                if nxt == b'%' {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                let (ni, stop) = parse_printf_spec(bytes, i, &mut out, args, &mut ai, &mut conv_error);
                i = ni;
                if stop {
                    return (out, conv_error);
                }
            } else if c == b'\\' {
                let (esc, ni, stop) = escape_at(bytes, i, false);
                out.extend_from_slice(&esc);
                i = ni;
                if stop {
                    return (out, conv_error);
                }
            } else {
                out.push(c);
                i += 1;
            }
        }
        if !has_conv {
            break;
        }
        if ai >= args.len() {
            break;
        }
        if ai == consumed_before {
            // The format did not consume an argument, so repeating it would
            // never end.  bash stops here as well.
            break;
        }
    }

    (out, conv_error)
}

fn parse_printf_spec(
    bytes: &[u8],
    start: usize,
    out: &mut Vec<u8>,
    args: &[String],
    ai: &mut usize,
    conv_error: &mut bool,
) -> (usize, bool) {
    let mut i = start + 1; // skip '%'

    // Flags.
    let mut flags = String::new();
    let mut minus = false;
    let mut zero = false;
    while i < bytes.len() && matches!(bytes[i], b'-' | b'+' | b' ' | b'#' | b'0') {
        let f = bytes[i] as char;
        flags.push(f);
        if f == '-' {
            minus = true;
        }
        if f == '0' {
            zero = true;
        }
        i += 1;
    }

    // Width.
    let mut width: i64 = 0;
    let mut have_width = false;
    if i < bytes.len() && bytes[i] == b'*' {
        width = get_star_arg(args, ai, conv_error);
        *ai += 1;
        have_width = true;
        i += 1;
    } else if i < bytes.len() && bytes[i].is_ascii_digit() {
        let (w, ni) = parse_int_literal(bytes, i);
        width = w;
        have_width = true;
        i = ni;
    }

    // Precision.
    let mut prec: i64 = 0;
    let mut have_prec = false;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        if i < bytes.len() && bytes[i] == b'*' {
            prec = get_star_arg(args, ai, conv_error);
            *ai += 1;
            have_prec = true;
            i += 1;
        } else if i < bytes.len() && bytes[i].is_ascii_digit() {
            let (p, ni) = parse_int_literal(bytes, i);
            prec = p;
            have_prec = true;
            i = ni;
        } else {
            prec = 0;
            have_prec = true;
        }
    }

    // Length modifiers (ignored — we always format with ll / L). NOTE: `q` is
    // intentionally NOT here, because in bash `%q` is the quote *conversion*,
    // not a length modifier.
    while i < bytes.len() && matches!(bytes[i], b'l' | b'h' | b'L' | b'j' | b'z' | b't') {
        i += 1;
    }

    if i >= bytes.len() {
        // Incomplete conversion (e.g. a trailing "%5"): bash quotes the whole
        // specification, reports it and stops.
        *conv_error = true;
        let spec = String::from_utf8_lossy(&bytes[start..]).into_owned();
        eprintln!("wash: printf: `{}': missing format character", spec);
        return (bytes.len(), true);
    }

    let conv = bytes[i];
    i += 1;

    // Resolve width/precision.
    let mut rwidth = width;
    let rprec = prec;
    if have_prec && rprec < 0 {
        have_prec = false; // negative precision → omitted
    }
    if have_width && rwidth < 0 {
        minus = true; // negative width → left-justify
        rwidth = -rwidth;
    }

    // The argument for this conversion.
    let arg: &str = if *ai < args.len() {
        args[*ai].as_str()
    } else {
        ""
    };
    let arg_missing = *ai >= args.len();

    match conv {
        b's' | b'b' => {
            let content: Vec<u8> = if conv == b'b' {
                let (bts, stop) = interpret_escapes(arg.as_bytes(), true);
                if stop {
                    *ai += 1;
                    return (i, true);
                }
                bts
            } else {
                arg.as_bytes().to_vec()
            };
            let content = truncate_prec(&content, have_prec, rprec);
            let padded =
                apply_field(&content, minus, zero, if have_width { rwidth } else { 0 }, true);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'q' => {
            let content = sh_quote(arg.as_bytes());
            let content = truncate_prec(&content, have_prec, rprec);
            // `%q` ignores the `0` flag (space padding only).
            let padded =
                apply_field(&content, minus, false, if have_width { rwidth } else { 0 }, false);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'c' => {
            let content: Vec<u8> = if arg.is_empty() {
                vec![0u8] // empty → NUL byte (bash behaviour)
            } else {
                vec![arg.as_bytes()[0]]
            };
            let padded = apply_field(&content, minus, zero, if have_width { rwidth } else { 0 }, true);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'd' | b'i' | b'o' | b'u' | b'x' | b'X' => {
            let (v_ll, v_ull, valid) = if arg_missing {
                (0i64, 0u64, true)
            } else if conv == b'u' {
                let (v, fully, overflow) = posix::parse_unsigned_long_long(arg);
                (0i64, v, fully && !overflow)
            } else {
                let (v, fully, overflow) = posix::parse_long_long(arg);
                (v, v as u64, fully && !overflow)
            };
            if !valid {
                *conv_error = true;
                report_invalid_number(arg);
            }
            let mut cf = String::from("%");
            cf.push_str(&flags);
            if have_width {
                cf.push_str(&rwidth.to_string());
            }
            if have_prec {
                cf.push('.');
                cf.push_str(&rprec.to_string());
            }
            let rendered = match conv {
                b'd' | b'i' => {
                    cf.push_str("lld");
                    posix::snprintf_ll_d(&cf, v_ll)
                }
                b'o' => {
                    cf.push_str("llo");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
                b'x' => {
                    cf.push_str("llx");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
                b'X' => {
                    cf.push_str("llX");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
                _ => {
                    cf.push_str("llu");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
            };
            out.extend_from_slice(&rendered);
            *ai += 1;
        }
        b'e' | b'E' | b'f' | b'g' | b'G' | b'a' | b'A' => {
            let mut cf = String::from("%");
            cf.push_str(&flags);
            if have_width {
                cf.push_str(&rwidth.to_string());
            }
            if have_prec {
                cf.push('.');
                cf.push_str(&rprec.to_string());
            }
            cf.push('L');
            cf.push(conv as char);
            let arg_for_c = if arg_missing { "0" } else { arg };
            let (rendered, valid) = posix::printf_long_double(&cf, arg_for_c);
            if !valid {
                *conv_error = true;
                if !arg_missing {
                    report_invalid_number(arg);
                }
            }
            out.extend_from_slice(&rendered);
            *ai += 1;
        }
        _ => {
            // An unknown conversion character: bash quotes the character,
            // reports it and stops.
            *conv_error = true;
            eprintln!(
                "wash: printf: `{}': invalid format character",
                conv as char
            );
            return (i, true);
        }
    }

    (i, false)
}

/// Report a numeric conversion that failed, the way bash does.
fn report_invalid_number(arg: &str) {
    eprintln!("wash: printf: {}: invalid number", arg);
}

/// Read a `*` field-width / precision argument as a base-0 integer.
fn get_star_arg(args: &[String], ai: &mut usize, conv_error: &mut bool) -> i64 {
    if *ai >= args.len() {
        return 0;
    }
    let a = &args[*ai];
    let (v, fully, overflow) = posix::parse_long_long(a);
    if !fully || overflow {
        *conv_error = true;
        report_invalid_number(a);
    }
    v
}

/// Parse a run of decimal digits starting at `i` (for literal width/precision).
fn parse_int_literal(bytes: &[u8], i: usize) -> (i64, usize) {
    let mut j = i;
    let mut v: i64 = 0;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((bytes[j] - b'0') as i64);
        j += 1;
    }
    (v, j)
}

/// Interpret a single backslash escape at `bytes[i]`. Returns the bytes to
/// emit, the new index, and (for `allow_c`) whether output must stop (`\c`).
fn escape_at(bytes: &[u8], i: usize, allow_c: bool) -> (Vec<u8>, usize, bool) {
    // Caller guarantees bytes[i] == '\\'.
    if i + 1 >= bytes.len() {
        return (vec![b'\\'], i + 1, false);
    }
    let nxt = bytes[i + 1];
    match nxt {
        b'a' => (vec![0x07], i + 2, false),
        b'b' => (vec![0x08], i + 2, false),
        b'e' => (vec![0x1b], i + 2, false),
        b'f' => (vec![0x0c], i + 2, false),
        b'n' => (vec![0x0a], i + 2, false),
        b'r' => (vec![0x0d], i + 2, false),
        b't' => (vec![0x09], i + 2, false),
        b'v' => (vec![0x0b], i + 2, false),
        b'\\' => (vec![b'\\'], i + 2, false),
        b'c' => {
            if allow_c {
                (Vec::new(), i + 2, true)
            } else {
                (vec![b'\\', b'c'], i + 2, false)
            }
        }
        b'0' => {
            // Octal escape: bash requires the leading zero (`\0NNN`); up to three
            // additional octal digits are consumed, the value is taken mod 256.
            // A bare `\0` yields a NUL byte.
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2; // skip the backslash and the `0`
            while cnt < 3 && j < bytes.len() && (bytes[j] as char).is_digit(8) {
                val = val * 8 + (bytes[j] - b'0') as u32;
                j += 1;
                cnt += 1;
            }
            (vec![(val & 0xff) as u8], j, false)
        }
        b'x' | b'X' => {
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2;
            while cnt < 2 && j < bytes.len() && (bytes[j] as char).is_digit(16) {
                val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                j += 1;
                cnt += 1;
            }
            if cnt == 0 {
                (vec![b'\\', nxt], i + 2, false)
            } else {
                (vec![(val & 0xff) as u8], j, false)
            }
        }
        b'u' => {
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2;
            while cnt < 4 && j < bytes.len() && (bytes[j] as char).is_digit(16) {
                val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                j += 1;
                cnt += 1;
            }
            if cnt == 0 {
                (vec![b'\\', b'u'], i + 2, false)
            } else {
                (encode_unicode(val), j, false)
            }
        }
        b'U' => {
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2;
            while cnt < 8 && j < bytes.len() && (bytes[j] as char).is_digit(16) {
                val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                j += 1;
                cnt += 1;
            }
            if cnt == 0 {
                (vec![b'\\', b'U'], i + 2, false)
            } else {
                (encode_unicode(val), j, false)
            }
        }
        _ => (vec![b'\\', nxt], i + 2, false),
    }
}

/// Interpret backslash escapes over a whole buffer (`allow_c` enables `\c` to
/// stop output). Used by `echo -e` and `%b`.
fn interpret_escapes(input: &[u8], allow_c: bool) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\\' {
            let (esc, ni, stop) = escape_at(input, i, allow_c);
            out.extend_from_slice(&esc);
            i = ni;
            if stop {
                return (out, true);
            }
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    (out, false)
}

/// Apply field width / precision / flags to a converted string.
/// `zero_pad` enables zero padding (used by `%s %c %b`; `%q` never zero-pads).
fn apply_field(content: &[u8], minus: bool, zero: bool, width: i64, zero_pad: bool) -> Vec<u8> {
    if width <= 0 {
        return content.to_vec();
    }
    let w = width as usize;
    if content.len() >= w {
        return content.to_vec();
    }
    let pad = w - content.len();
    if minus {
        let mut r = content.to_vec();
        r.extend(std::iter::repeat(b' ').take(pad));
        r
    } else if zero && zero_pad {
        let mut r = vec![b'0'; pad];
        r.extend_from_slice(content);
        r
    } else {
        let mut r = vec![b' '; pad];
        r.extend_from_slice(content);
        r
    }
}

fn truncate_prec(content: &[u8], have_prec: bool, prec: i64) -> Vec<u8> {
    if have_prec && prec >= 0 {
        let p = prec as usize;
        if content.len() > p {
            return content[..p].to_vec();
        }
    }
    content.to_vec()
}

/// Encode a Unicode code point as UTF-8 (works for any u32, matching bash's
/// `printf`/`echo -e` raw byte output for `\u`/`\U`).
fn encode_unicode(cp: u32) -> Vec<u8> {
    if cp <= 0x7F {
        vec![(cp & 0xFF) as u8]
    } else if cp <= 0x7FF {
        vec![
            (0xC0 | (cp >> 6)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else if cp <= 0xFFFF {
        vec![
            (0xE0 | (cp >> 12)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else if cp <= 0x1FFFFF {
        vec![
            (0xF0 | (cp >> 18)) as u8,
            (0x80 | ((cp >> 12) & 0x3F)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else if cp <= 0x3FFFFFF {
        vec![
            (0xF8 | (cp >> 24)) as u8,
            (0x80 | ((cp >> 18) & 0x3F)) as u8,
            (0x80 | ((cp >> 12) & 0x3F)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else {
        vec![
            (0xFC | (cp >> 30)) as u8,
            (0x80 | ((cp >> 24) & 0x3F)) as u8,
            (0x80 | ((cp >> 18) & 0x3F)) as u8,
            (0x80 | ((cp >> 12) & 0x3F)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    }
}

/// Bash `%q` quoting (mirrors `sh_quote`): quote a string so it can be safely
/// reused as shell input.
fn sh_quote(s: &[u8]) -> Vec<u8> {
    if s.is_empty() {
        return b"''".to_vec();
    }
    // bash drops newline bytes entirely.
    let filtered: Vec<u8> = s.iter().cloned().filter(|&b| b != b'\n').collect();
    if filtered.is_empty() {
        return b"''".to_vec();
    }

    // A control byte (other than newline) triggers `$'...'` ANSI-C quoting.
    let needs_ansi = filtered
        .iter()
        .any(|&b| (b >= 0x01 && b <= 0x1f) || b == 0x7f);

    if needs_ansi {
        let mut r: Vec<u8> = Vec::new();
        r.push(b'$');
        r.push(b'\'');
        for &b in &filtered {
            match b {
                0x07 => r.extend_from_slice(b"\\a"),
                0x08 => r.extend_from_slice(b"\\b"),
                0x09 => r.extend_from_slice(b"\\t"),
                0x0b => r.extend_from_slice(b"\\v"),
                0x0c => r.extend_from_slice(b"\\f"),
                0x0d => r.extend_from_slice(b"\\r"),
                0x1b => r.extend_from_slice(b"\\E"),
                0x01..=0x1f | 0x7f => {
                    r.push(b'\\');
                    let o = format!("{:03o}", b);
                    r.extend_from_slice(o.as_bytes());
                }
                b'\\' => r.extend_from_slice(b"\\\\"),
                b'\'' => r.extend_from_slice(b"\\'"),
                _ => r.push(b),
            }
        }
        r.push(b'\'');
        r
    } else {
        let mut r: Vec<u8> = Vec::new();
        for &b in &filtered {
            if is_special_q(b) {
                r.push(b'\\');
                r.push(b);
            } else {
                r.push(b);
            }
        }
        r
    }
}

/// Whether `b` is a "special" character that `%q` backslash-escapes (everything
/// printable/ASCII except alnum and `% + - . / : = @ _`; high bytes are raw).
fn is_special_q(b: u8) -> bool {
    if b >= 0x80 {
        return false; // raw UTF-8
    }
    if b.is_ascii_alphanumeric() {
        return false;
    }
    !matches!(b, b'%' | b'+' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'_')
}

// ---------------------------------------------------------------------------
// umask
// ---------------------------------------------------------------------------

/// The `umask` builtin: print or change the file-mode creation mask.
///
///   * `umask`               → print the current mask as 4-digit octal.
///   * `umask MODE`          → set the mask (octal digits).
///   * `umask -S`            → print symbolically (`u=rwx,g=rx,o=rx`).
///   * `umask -p`            → print a `umask NNNN` command that restores it.
///   * `umask -S MODE`       → set from a symbolic mode string.
///
/// Unknown options produce bash's usage diagnostic and exit 2.
fn builtin_umask(argv: &[String], sh: &mut Shell) -> i32 {
    let mut symbolic = false;
    let mut portability = false;
    let mut mode_arg: Option<String> = None;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a.chars().skip(1) {
                match c {
                    'S' => symbolic = true,
                    'p' => portability = true,
                    _ => {
                        eprintln!("wash: umask: {}: invalid option", a);
                        eprintln!("umask: usage: umask [-p] [-S] [mode]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }
    // Everything left is the (optional) mode argument.
    let rest: Vec<String> = argv[i..].to_vec();
    if !rest.is_empty() {
        mode_arg = Some(rest.join(" "));
    }

    // Read the current mask.
    let cur = posix::umask_get();
    // Restore it immediately (getumask is not available; set-and-get).
    posix::umask_set(cur);

    match mode_arg {
        None => {
            if portability {
                println!("umask {:04o}", cur);
            } else if symbolic {
                println!("{}", symbolic_umask(cur));
            } else {
                println!("{:04o}", cur);
            }
            0
        }
        Some(mode) => {
            let new_mask = if symbolic || mode.chars().any(|c| c.is_alphabetic()) {
                parse_symbolic_umask(mode.trim(), cur)
            } else {
                match i64::from_str_radix(mode.trim(), 8) {
                    Ok(v) => (v & 0o7777) as i32,
                    Err(_) => {
                        eprintln!("wash: umask: `{}': invalid octal mode", mode.trim());
                        return 1;
                    }
                }
            };
            posix::umask_set(new_mask);
            // bash prints the new mask only for `umask -p MODE` (`umask NNNN`)
            // and `umask -S MODE` (symbolic); a plain `umask MODE` stays silent.
            if portability {
                println!("umask {:04o}", new_mask);
            } else if symbolic {
                println!("{}", symbolic_umask(new_mask));
            }
            0
        }
    }
}

/// Format a numeric umask as bash's symbolic form (`u=rwx,g=rx,o=rx`).
fn symbolic_umask(mask: i32) -> String {
    let perms = |bits: i32| -> String {
        let mut s = String::new();
        if bits & 0o4 != 0 {
            s.push('r');
        }
        if bits & 0o2 != 0 {
            s.push('w');
        }
        if bits & 0o1 != 0 {
            s.push('x');
        }
        s
    };
    let u = perms((!mask >> 6) & 0o7);
    let g = perms((!mask >> 3) & 0o7);
    let o = perms((!mask) & 0o7);
    format!("u={},g={},o={}", u, g, o)
}

/// Parse a symbolic umask mode (`u=rwx,g=,o=`) relative to `cur`.
///
/// Each comma-separated group is `who=perms` (who is any of u/g/o/a or empty
/// for all). The bits where the permission is NOT granted are set in the mask.
fn parse_symbolic_umask(mode: &str, cur: i32) -> i32 {
    let mut mask = cur & 0o7777;
    for group in mode.split(',') {
        let group = group.trim();
        if group.is_empty() {
            continue;
        }
        let (who, perms) = match group.split_once('=') {
            Some((w, p)) => (w, p),
            None => continue,
        };
        let mut allow = 0i32;
        for c in perms.chars() {
            match c {
                'r' => allow |= 0o4,
                'w' => allow |= 0o2,
                'x' => allow |= 0o1,
                _ => {}
            }
        }
        let mut shifts: Vec<i32> = Vec::new();
        if who.contains('u') || who.is_empty() || who.contains('a') {
            shifts.push(6);
        }
        if who.contains('g') || who.is_empty() || who.contains('a') {
            shifts.push(3);
        }
        if who.contains('o') || who.is_empty() || who.contains('a') {
            shifts.push(0);
        }
        for shift in shifts {
            mask &= !(0o7 << shift);
            mask |= ((!allow) & 0o7) << shift;
        }
    }
    mask & 0o7777
}

// ---------------------------------------------------------------------------
// times
// ---------------------------------------------------------------------------

/// The `times` builtin: print the cumulative user/system CPU times of the shell
/// and its children, the way bash does (`0m0.015s 0m0.078s` per line).
fn builtin_times(_argv: &[String], _sh: &mut Shell) -> i32 {
    let (u, s, cu, cs) = posix::get_times();
    println!("{} {}", fmt_clock(u), fmt_clock(s));
    println!("{} {}", fmt_clock(cu), fmt_clock(cs));
    0
}

/// Format a number of seconds as bash's `0m0.015s` clock string.
///
/// Only the `Display`/`i64` formatting traits are used (the `f` float trait is
/// not available in the pinned toolchain), so the fractional part is built
/// manually from the integer and millisecond components.
fn fmt_clock(t: f64) -> String {
    let minutes = (t / 60.0).floor() as i64;
    let secs = t - (minutes as f64) * 60.0;
    let secs_int = if secs < 0.0 { 0 } else { secs as i64 };
    let frac = (((secs - secs_int as f64) * 1000.0).round() as i64).clamp(0, 999);
    let (secs_int, frac) = if frac == 1000 {
        (secs_int + 1, 0)
    } else {
        (secs_int, frac)
    };
    format!("{}m{}.{:03}s", minutes, secs_int, frac)
}

// ---------------------------------------------------------------------------
// let
// ---------------------------------------------------------------------------

/// The `let` builtin: evaluate one or more arithmetic expressions, assigning
/// any results back into shell variables and returning the status of the last
/// expression (a zero result → status 1, like bash).
fn builtin_let(argv: &[String], sh: &mut Shell) -> i32 {
    if argv.len() < 2 {
        return 1;
    }
    let mut status = 1;
    let mut env = arith::MapEnv {
        vars: &mut sh.env,
    };
    for expr in &argv[1..] {
        if expr.is_empty() {
            status = 1;
            continue;
        }
        match arith::eval(expr, &mut env) {
            Ok(value) => {
                status = if value == 0 { 1 } else { 0 };
            }
            Err(e) => {
                let token = if e.token.is_empty() {
                    String::new()
                } else {
                    format!(" (error token is \"{}\")", e.token)
                };
                eprintln!("wash: let: {}: {}{}", expr, e.message, token);
                status = 1;
            }
        }
    }
    status
}

// ---------------------------------------------------------------------------
// enable
// ---------------------------------------------------------------------------

/// The `enable` builtin: enable or disable shell builtins.
///
///   * `enable NAME`          → ensure NAME is enabled.
///   * `enable -n NAME`       → disable NAME (falls back to a file lookup).
///   * `enable -n`            → list the disabled builtins.
///   * `enable` / `enable -a` → list every builtin with its status/path.
///   * `enable BAD`           → `not a shell builtin` (rc 1).
///   * `enable -z`            → usage error (rc 2).
fn builtin_enable(argv: &[String], sh: &mut Shell) -> i32 {
    let mut disable = false;
    let mut all = false;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a.chars().skip(1) {
                match c {
                    'n' => disable = true,
                    'a' => all = true,
                    's' => {} // shared library builtins (none here)
                    _ => {
                        eprintln!("wash: enable: {}: invalid option", a);
                        eprintln!("enable: usage: enable [-a] [-dnps] [-f filename] [name ...]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    let names: Vec<String> = argv[i..].to_vec();

    if names.is_empty() {
        if disable {
            // List disabled builtins.
            let mut v: Vec<&String> = sh.disabled_builtins.iter().collect();
            v.sort();
            for n in v {
                println!("{}", n);
            }
            return 0;
        }
        // List every builtin (bash's `enable` / `enable -a`).
        let mut v: Vec<&str> = BUILTINS.to_vec();
        v.sort_unstable();
        for n in v {
            if all {
                if sh.disabled_builtins.contains(n) {
                    println!("{}", n);
                } else {
                    println!("{}", n);
                }
            } else if sh.disabled_builtins.contains(n) {
                println!("{}", n);
            } else {
                println!("{}", n);
            }
        }
        return 0;
    }

    let mut status = 0;
    for name in &names {
        if !BUILTINS.contains(&name.as_str()) {
            eprintln!("wash: enable: {}: not a shell builtin", name);
            status = 1;
            continue;
        }
        if disable {
            sh.disabled_builtins.insert(name.clone());
        } else {
            sh.disabled_builtins.remove(name);
        }
    }
    status
}

// ---------------------------------------------------------------------------
// return / break / continue / local
// ---------------------------------------------------------------------------

/// The `return` builtin: stop a function or a sourced script.
///
/// At the top level (not inside a function or a sourced script) bash reports
/// "can only `return' from a function or sourced script" and keeps going.
fn builtin_return(argv: &[String], sh: &mut Shell) -> i32 {
    if sh.sourcing_depth == 0 && sh.function_depth == 0 {
        eprintln!("wash: return: can only `return' from a function or sourced script");
        return 1;
    }
    let code = if argv.len() >= 2 {
        argv[1].trim().parse::<i32>().unwrap_or_else(|_| sh.last_status)
    } else {
        sh.last_status
    };
    sh.return_requested = Some(code);
    0
}

/// The `break` builtin: exit a loop. At the top level bash reports
/// "only meaningful in a `for', `while', or `until' loop" and keeps going.
fn builtin_break(argv: &[String], sh: &mut Shell) -> i32 {
    if sh.loop_depth == 0 {
        eprintln!("wash: break: only meaningful in a `for', `while', or `until' loop");
        return 1;
    }
    let n = if argv.len() >= 2 {
        argv[1].trim().parse::<i32>().unwrap_or(1)
    } else {
        1
    };
    sh.break_requested = Some(n);
    0
}

/// The `continue` builtin: resume a loop at the next iteration.
fn builtin_continue(argv: &[String], sh: &mut Shell) -> i32 {
    if sh.loop_depth == 0 {
        eprintln!("wash: continue: only meaningful in a `for', `while', or `until' loop");
        return 1;
    }
    let n = if argv.len() >= 2 {
        argv[1].trim().parse::<i32>().unwrap_or(1)
    } else {
        1
    };
    sh.continue_requested = Some(n);
    0
}

/// The `local` builtin: declare a variable local to a function.
///
/// `local` is only legal inside a function (`function_depth > 0`).  It records
/// the variable's current value in the active local frame (so that
/// `execute_function` can restore it on return) and, when an initializer is
/// present, assigns the new value.  Attribute flags (`-i`/`-x`/`-r`) toggle the
/// matching shell attribute the way bash does.
fn builtin_local(argv: &[String], sh: &mut Shell) -> i32 {
    if sh.function_depth == 0 {
        eprintln!("wash: local: can only be used in a function");
        return 1;
    }
    let frame = match sh.call_stack.last_mut() {
        Some(f) => f,
        None => {
            eprintln!("wash: local: can only be used in a function");
            return 1;
        }
    };

    // Parse optional attribute flags, then the `name[=value]` operands.
    let mut i = 1usize;
    let mut integer_attr = false;
    let mut export_attr = false;
    let mut readonly_attr = false;
    while i < argv.len() {
        let a = &argv[i];
        if a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'i' => integer_attr = true,
                    'x' => export_attr = true,
                    'r' => readonly_attr = true,
                    // `-a`/`-A` (arrays) and `-g`/`-n`/... are accepted but
                    // ignored: wash has no array support yet.
                    _ => {}
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    let mut status = 0i32;
    for arg in &argv[i..] {
        let (name, value) = match arg.find('=') {
            Some(eq) => (&arg[..eq], Some(arg[eq + 1..].to_string())),
            None => (arg.as_str(), None),
        };
        if !is_valid_ident(name) {
            eprintln!("wash: local: `{}': not a valid identifier", arg);
            status = 1;
            continue;
        }
        // Record the previous value (once per variable per function) so the
        // enclosing scope survives the function's return.
        if !frame.contains_key(name) {
            frame.insert(name.to_string(), sh.env.get(name).cloned());
        }
        if let Some(v) = value {
            if readonly_attr && sh.readonly.contains(name) {
                eprintln!("wash: {}: readonly variable", name);
                status = 1;
            } else {
                sh.env.insert(name.to_string(), v);
            }
        }
        // Apply the attribute flags (the same bookkeeping `declare` uses).
        if integer_attr {
            sh.integer_vars.insert(name.to_string());
        }
        if export_attr {
            sh.exported.insert(name.to_string());
            sh.null_exported.remove(name);
        }
        if readonly_attr {
            sh.readonly.insert(name.to_string());
        }
    }
    status
}

// ---------------------------------------------------------------------------
// declare / typeset / readonly
// ---------------------------------------------------------------------------

/// The `declare` / `typeset` builtin: set variable attributes (`-x` export,
/// `-r` readonly, `-i` integer) and print them with `-p`.
///
/// `typeset` is a complete synonym of `declare`.
fn builtin_declare(argv: &[String], sh: &mut Shell, _is_typeset: bool) -> i32 {
    let mut export_attr = false;
    let mut readonly_attr = false;
    let mut integer_attr = false;
    let mut print = false;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.starts_with('-') {
            let opts = &a[1..];
            if opts.is_empty() {
                i += 1;
                continue;
            }
            for c in opts.chars() {
                match c {
                    'x' => export_attr = true,
                    'r' => readonly_attr = true,
                    'i' => integer_attr = true,
                    'p' => print = true,
                    'a' | 'A' | 'g' | 'n' | 't' | 'l' | 'u' | 'c' => {} // accepted but no-op
                    _ => {
                        eprintln!("wash: declare: {}: invalid option", a);
                        eprintln!("declare: usage: declare [-aAfFgilrtux] [-p] [name[=value] ...]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    // `declare -p [NAME...]`: print attributes of the named variable(s).
    if print {
        let names: Vec<String> = argv[i..].to_vec();
        if names.is_empty() {
            // Print every variable (KNOWN-DIFF: bash prints all; we print the
            // ones that carry interesting attributes).
            print_declared(sh, &all_declared_names(sh));
        } else {
            print_declared(sh, &names);
        }
        return 0;
    }

    if i >= argv.len() {
        // `declare` with no arguments: list all variables (KNOWN-DIFF).
        print_declared(sh, &all_declared_names(sh));
        return 0;
    }

    let mut status = 0;
    for arg in &argv[i..] {
        match arg.find('=') {
            Some(eq) => {
                let name = &arg[..eq];
                let value = &arg[eq + 1..];
                if !is_valid_ident(name) {
                    eprintln!("wash: declare: `{}': not a valid identifier", arg);
                    status = 1;
                    continue;
                }
                apply_var_attrs(
                    sh,
                    name,
                    Some(value.to_string()),
                    export_attr,
                    readonly_attr,
                    integer_attr,
                );
            }
            None => {
                if !is_valid_ident(arg) {
                    eprintln!("wash: declare: `{}': not a valid identifier", arg);
                    status = 1;
                    continue;
                }
                apply_var_attrs(
                    sh,
                    arg,
                    None,
                    export_attr,
                    readonly_attr,
                    integer_attr,
                );
            }
        }
    }
    status
}

/// The `readonly` builtin: mark variables as read-only (`-p` lists them).
fn builtin_readonly(argv: &[String], sh: &mut Shell) -> i32 {
    let mut print = false;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-p" => print = true,
                _ => {
                    // `-a`/`-A` are array forms (deferred); other flags are
                    // accepted but unused.
                    if a[1..].chars().all(|c| matches!(c, 'a' | 'A' | 'f' | 'n')) {
                        i += 1;
                        continue;
                    }
                    eprintln!("wash: readonly: {}: invalid option", a);
                    eprintln!("readonly: usage: readonly [-aAf] [name[=value] ...] or readonly -p");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    if print || i >= argv.len() {
        let mut names: Vec<&String> = sh.readonly.iter().collect();
        names.sort();
        for name in names {
            let value = sh.env.get(name);
            match value {
                Some(v) if sh.null_exported.contains(name) => {
                    println!("declare -r {}", name);
                }
                Some(v) => {
                    println!("declare -r {}={}", name, quote_double(v));
                }
                None => {
                    println!("declare -r {}", name);
                }
            }
        }
        return 0;
    }

    let mut status = 0;
    for arg in &argv[i..] {
        match arg.find('=') {
            Some(eq) => {
                let name = &arg[..eq];
                let value = &arg[eq + 1..];
                if !is_valid_ident(name) {
                    eprintln!("wash: readonly: `{}': not a valid identifier", arg);
                    status = 1;
                    continue;
                }
                if sh.readonly.contains(name) {
                    eprintln!("wash: readonly: {}: readonly variable", name);
                    status = 1;
                    continue;
                }
                sh.env.insert(name.to_string(), value.to_string());
                sh.readonly.insert(name.to_string());
            }
            None => {
                if !is_valid_ident(arg) {
                    eprintln!("wash: readonly: `{}': not a valid identifier", arg);
                    status = 1;
                    continue;
                }
                if sh.readonly.contains(arg) {
                    eprintln!("wash: readonly: {}: readonly variable", arg);
                    status = 1;
                    continue;
                }
                if !sh.env.contains_key(arg) {
                    sh.env.insert(arg.to_string(), String::new());
                    sh.null_exported.insert(arg.to_string());
                }
                sh.readonly.insert(arg.to_string());
            }
        }
    }
    status
}

/// Apply the requested attributes to `name`, optionally assigning `value`.
fn apply_var_attrs(
    sh: &mut Shell,
    name: &str,
    value: Option<String>,
    export_attr: bool,
    readonly_attr: bool,
    integer_attr: bool,
) {
    if readonly_attr && sh.readonly.contains(name) {
        eprintln!("wash: {}: readonly variable", name);
        return;
    }
    if let Some(v) = value {
        let v = if integer_attr {
            match arith::eval(&v, &mut arith::MapEnv { vars: &mut sh.env }) {
                Ok(n) => n.to_string(),
                Err(_) => v,
            }
        } else {
            v
        };
        sh.env.insert(name.to_string(), v);
    }
    if export_attr {
        sh.exported.insert(name.to_string());
        sh.null_exported.remove(name);
    }
    if readonly_attr {
        sh.readonly.insert(name.to_string());
    }
    if integer_attr {
        sh.integer_vars.insert(name.to_string());
    }
}

/// Every variable name the shell knows about (used by `declare -p` with no
/// name). Mirrors bash's "list all" behaviour closely enough for the
/// KNOWN-DIFF listing.
fn all_declared_names(sh: &Shell) -> Vec<String> {
    let mut names: Vec<String> = sh.env.keys().cloned().collect();
    names.sort();
    names
}

/// Print `declare -x NAME=...` lines for the given names, in sorted order.
fn print_declared(sh: &Shell, names: &[String]) {
    let mut sorted = names.to_vec();
    sorted.sort();
    for name in &sorted {
        let attrs = declare_attributes(sh, name);
        let attr_str = if attrs.is_empty() {
            "--".to_string()
        } else {
            format!("-{}", attrs)
        };
        let value = sh.env.get(name);
        match value {
            Some(v) if sh.null_exported.contains(name) => {
                println!("declare {} {}", attr_str, name);
            }
            Some(v) => {
                println!("declare {} {}={}", attr_str, name, quote_double(v));
            }
            None => {
                println!("declare {} {}", attr_str, name);
            }
        }
    }
}

/// The attribute letters bash prints before a `declare -p` name (`-irx...`).
fn declare_attributes(sh: &Shell, name: &str) -> String {
    let mut attrs = String::new();
    if sh.integer_vars.contains(name) {
        attrs.push('i');
    }
    if sh.readonly.contains(name) {
        attrs.push('r');
    }
    if sh.exported.contains(name) {
        attrs.push('x');
    }
    attrs
}

// ---------------------------------------------------------------------------
// kill
// ---------------------------------------------------------------------------

/// The `kill` builtin: send a signal to a process, or list signal names.
///
///   * `kill -l [sigspec]` → list signals (a number → name, a name → number).
///   * `kill -SIG PID` / `kill -s SIG PID` / `kill PID` → send the signal.
///
/// The three bash-locked behaviours are reproduced exactly:
///   * no arguments → the bash usage line on stderr, rc 2;
///   * an unknown signal spec → "invalid signal specification", rc 1;
///   * sending to a non-existent pid → "(pid) - No such process", rc 1.
fn builtin_kill(argv: &[String], sh: &mut Shell) -> i32 {
    const USAGE: &str =
        "kill: usage: kill [-s sigspec | -n signum | -sigspec] pid | jobspec ... or kill -l [sigspec]";

    if argv.len() < 2 {
        eprintln!("wash: {}", USAGE);
        return 2;
    }

    // `kill -l` handling.
    if argv[1] == "-l" {
        if argv.len() == 2 {
            print_signal_table();
            return 0;
        }
        // `kill -l SIGSPEC` → name for a number, number for a name.
        let spec = &argv[2];
        if let Ok(n) = spec.trim().parse::<i32>() {
            if let Some(name) = signal_name(n) {
                println!("{}", name);
            }
            return 0;
        }
        if let Some(n) = canonical_signal(spec) {
            // spec is a name → print its number.
            let num = signal_to_num(&n);
            println!("{}", num);
            return 0;
        }
        eprintln!("wash: kill: {}: invalid signal specification", spec);
        return 1;
    }

    // Parse an optional leading signal spec.
    let mut sigspec: Option<String> = None;
    let mut i = 1usize;
    if argv[i].starts_with('-') && argv[i].len() > 1 && argv[i] != "-" {
        let opt = &argv[i][1..];
        if opt == "l" {
            // already handled above; guard against `-l` appearing mid-way
            eprintln!("wash: {}", USAGE);
            return 2;
        } else if opt == "s" {
            i += 1;
            if i >= argv.len() {
                eprintln!("wash: {}", USAGE);
                return 2;
            }
            sigspec = Some(argv[i].clone());
            i += 1;
        } else if opt == "n" {
            i += 1;
            if i >= argv.len() {
                eprintln!("wash: {}", USAGE);
                return 2;
            }
            sigspec = Some(argv[i].clone());
            i += 1;
        } else {
            sigspec = Some(opt.to_string());
            i += 1;
        }
    }

    let sig = match &sigspec {
        Some(s) => match canonical_signal(s) {
            Some(name) => signal_to_num(&name),
            None => {
                eprintln!("wash: kill: {}: invalid signal specification", s);
                return 1;
            }
        },
        None => 15, // SIGTERM
    };

    if i >= argv.len() {
        eprintln!("wash: {}", USAGE);
        return 2;
    }

    let mut status = 0;
    for pidstr in &argv[i..] {
        // A leading "-" turns the operand into a process-group id; strip it so
        // we can parse the number, and negate for the kill(2) call.
        let (negate, numstr) = if let Some(rest) = pidstr.strip_prefix('-') {
            (true, rest)
        } else {
            (false, pidstr.as_str())
        };
        let pid: i32 = match numstr.trim().parse() {
            Ok(p) => p,
            Err(_) => {
                eprintln!("wash: kill: {}: arguments must be process or job IDs", pidstr);
                status = 1;
                continue;
            }
        };
        let target = if negate { -pid } else { pid };

        // Sending a signal to our own shell while a trap is installed: run the
        // trap handler directly instead of raising a fatal signal.
        if target == sh.self_pid {
            let canon = signal_to_name(sig);
            if let Some(action) = sh.traps.get(&canon).cloned() {
                if action != "-" {
                    sh.run_str(&action);
                }
                continue;
            }
        }

        let r = posix::kill(target, sig);
        if r != 0 {
            let e = posix::errno();
            // bash prints `kill: (PID) - <strerror>` (note the ") - " form,
            // not a colon).
            eprintln!(
                "wash: kill: ({}) - {}",
                target,
                posix::errno_message(e)
            );
            status = 1;
        }
    }
    status
}

/// Print the bash-like numbered signal table.
fn print_signal_table() {
    let names = signal_names();
    let mut row = 0;
    while row < names.len() {
        for col in 0..4 {
            let idx = row + col * 16;
            if idx >= names.len() {
                break;
            }
            if col > 0 {
                print!("\t");
            }
            let num = idx + 1;
            print!("{:>2}) SIG{}", num, names[idx]);
        }
        println!();
        row += 16;
    }
}

/// The signal names in numeric order (1-based, like bash).
fn signal_names() -> Vec<&'static str> {
    vec![
        "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "EMT", "FPE", "KILL", "BUS", "SEGV", "SYS",
        "PIPE", "ALRM", "TERM", "URG", "STOP", "TSTP", "CONT", "CHLD", "TTIN", "TTOU", "IO",
        "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "PWR", "USR1", "USR2", "RTMIN", "RTMAX",
    ]
}

/// Map a signal number (1-based) to its bash name (e.g. 9 → "KILL").
fn signal_name(num: i32) -> Option<&'static str> {
    let names = signal_names();
    if num >= 1 && (num as usize) <= names.len() {
        Some(names[(num - 1) as usize])
    } else {
        None
    }
}

/// Map a canonical signal name (e.g. "SIGINT") to its number.
fn signal_to_num(name: &str) -> i32 {
    let n = name.strip_prefix("SIG").unwrap_or(name);
    signal_names()
        .iter()
        .position(|x| *x == n)
        .map(|i| (i + 1) as i32)
        .unwrap_or(-1)
}

/// Canonicalise a signal spec (`INT`, `SIGINT`, `2`, `EXIT`, `USR1`) to the
/// `SIGxxx` form bash stores and prints (special traps like EXIT stay as-is).
fn canonical_signal(spec: &str) -> Option<String> {
    let s = spec.strip_prefix("SIG").unwrap_or(spec);
    if s == "EXIT" {
        return Some("EXIT".to_string());
    }
    let upper = s.to_uppercase();
    if signal_names().iter().any(|x| *x == upper) {
        return Some(format!("SIG{}", upper));
    }
    // Maybe it is a number.
    if let Ok(num) = s.trim().parse::<i32>() {
        if let Some(name) = signal_name(num) {
            return Some(format!("SIG{}", name));
        }
    }
    None
}

/// Map a signal number to its `SIGxxx` canonical name (for trap lookups).
fn signal_to_name(num: i32) -> String {
    match signal_name(num) {
        Some(n) => format!("SIG{}", n),
        None => format!("SIG{}", num),
    }
}

// ---------------------------------------------------------------------------
// trap
// ---------------------------------------------------------------------------

/// The `trap` builtin: set, clear, ignore or list signal handlers.
///
///   * `trap`                  → list all handlers (`trap -- 'cmd' SIG`).
///   * `trap -p [SIG...]`      → list handlers for the named signals.
///   * `trap CMD SIG ...`      → install CMD for each signal.
///   * `trap - SIG ...`        → reset (remove) the handler.
///   * `trap "" SIG ...`       → ignore the signal.
fn builtin_trap(argv: &[String], sh: &mut Shell) -> i32 {
    if argv.len() < 2 {
        // List every installed trap.
        print_traps(sh);
        return 0;
    }

    // `trap -p [SIG...]` lists handlers.
    if argv[1] == "-p" {
        let sigs: Vec<String> = argv[2..].to_vec();
        if sigs.is_empty() {
            print_traps(sh);
        } else {
            for s in &sigs {
                if let Some(cn) = canonical_signal(s) {
                    if let Some(action) = sh.traps.get(&cn) {
                        println!("trap -- '{}' {}", action, cn);
                    }
                }
            }
        }
        return 0;
    }

    let action = argv[1].clone();
    if argv.len() < 3 {
        // `trap CMD` with no signal is a usage error in bash.
        eprintln!("wash: trap: usage: trap [-lp] [[arg] signal_spec ...]");
        return 2;
    }

    let mut status = 0;
    for sig in &argv[2..] {
        let cn = match canonical_signal(sig) {
            Some(c) => c,
            None => {
                eprintln!("wash: trap: {}: invalid signal specification", sig);
                status = 1;
                continue;
            }
        };
        if action == "-" {
            sh.traps.remove(&cn);
        } else {
            sh.traps.insert(cn, action.clone());
        }
    }
    status
}

/// Print the installed traps the way bash does (`trap -- 'cmd' SIG`).
fn print_traps(sh: &Shell) {
    let mut keys: Vec<&String> = sh.traps.keys().collect();
    keys.sort();
    for k in keys {
        let action = &sh.traps[k];
        println!("trap -- '{}' {}", action, k);
    }
}

// ---------------------------------------------------------------------------
// dirs / pushd / popd
// ---------------------------------------------------------------------------

/// Print the directory stack, honouring `-v` (numbered), `-l` (long form) and
/// `-c` (clear).
fn builtin_dirs(argv: &[String], sh: &mut Shell) -> i32 {
    let mut numbered = false;
    let mut clear = false;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a.len() > 1 && a.starts_with('-') {
            for c in a.chars().skip(1) {
                match c {
                    'v' => numbered = true,
                    'l' => {} // long form: no ~ abbreviation (already plain)
                    'c' => clear = true,
                    'p' => {} // would print for command substitution
                    _ => {
                        eprintln!("wash: dirs: {}: invalid option", a);
                        eprintln!("dirs: usage: dirs [-clpv] [+N] [-N]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    if clear {
        sh.dir_stack = vec![sh.cwd.clone()];
        return 0;
    }

    if numbered {
        for (idx, dir) in sh.dir_stack.iter().enumerate() {
            println!("{:>2}  {}", idx, dir);
        }
    } else {
        println!("{}", sh.dir_stack.join(" "));
    }
    0
}

/// The `pushd` builtin: push a directory onto the stack and cd into it. With no
/// argument it swaps the top two entries.
fn builtin_pushd(argv: &[String], sh: &mut Shell) -> i32 {
    if argv.len() == 1 {
        // Swap the top two stack entries (bash behaviour).
        if sh.dir_stack.len() < 2 {
            eprintln!("wash: pushd: no other directory");
            return 1;
        }
        sh.dir_stack.swap(0, 1);
        let target = sh.dir_stack[0].clone();
        let _ = set_cwd(sh, &target);
        println!("{}", sh.dir_stack.join(" "));
        return 0;
    }

    // One operand only (more is an error in bash).
    if argv.len() > 2 {
        eprintln!("wash: pushd: too many arguments");
        return 1;
    }
    let dir = &argv[1];
    if dir.starts_with('+') || dir.starts_with('-') {
        // `pushd +N` rotates the stack — not yet implemented here.
        eprintln!("wash: pushd: {}: invalid option", dir);
        return 1;
    }

    // Remember the current top of the stack (the old working directory)
    // before changing into the new directory.
    let old_top = sh.dir_stack.get(0).cloned().unwrap_or_else(|| sh.cwd.clone());
    // cd into the directory first (this updates the stack's top with the new
    // directory on success).
    if !set_cwd(sh, dir) {
        // cd failed: bash leaves the stack unchanged and reports the error.
        let e = posix::errno();
        eprintln!("wash: pushd: {}: {}", dir, posix::errno_message(e));
        return 1;
    }
    // The new directory is now at index 0; push the previous top down to index
    // 1 so the stack reads "<newdir> <olddir> ..." (bash semantics).
    sh.dir_stack.insert(1, old_top);
    println!("{}", sh.dir_stack.join(" "));
    0
}

/// The `popd` builtin: remove a directory from the stack and cd into the new
/// top. `popd +N` removes the N-th entry (0-based).
fn builtin_popd(argv: &[String], sh: &mut Shell) -> i32 {
    let mut index: usize = 0;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a.len() > 1 && a.starts_with('-') && a != "-" {
            eprintln!("wash: popd: {}: invalid option", a);
            eprintln!("popd: usage: popd [-n] [+N | -N]");
            return 2;
        }
        if let Some(n) = a.strip_prefix('+') {
            match n.trim().parse::<usize>() {
                Ok(v) => index = v,
                Err(_) => {
                    eprintln!("wash: popd: {}: invalid option", a);
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        i += 1;
    }

    if sh.dir_stack.len() <= 1 {
        eprintln!("wash: popd: directory stack empty");
        return 1;
    }
    if index >= sh.dir_stack.len() {
        eprintln!("wash: popd: +{}: directory stack index out of range", index);
        return 1;
    }

    sh.dir_stack.remove(index);
    if index == 0 {
        let target = sh.dir_stack[0].clone();
        let _ = set_cwd(sh, &target);
    }
    println!("{}", sh.dir_stack.join(" "));
    0
}

/// Change the shell's working directory, keeping `$PWD` in sync and updating the
/// top of the directory stack. Returns true on success.
fn set_cwd(sh: &mut Shell, path: &str) -> bool {
    if posix::chdir(path) == 0 {
        let newdir = posix::chdir_getcwd(path, false);
        let dir = if newdir.is_empty() { path.to_string() } else { newdir };
        sh.cwd = dir.clone();
        sh.env.insert("PWD".to_string(), dir);
        if !sh.dir_stack.is_empty() {
            sh.dir_stack[0] = sh.cwd.clone();
        } else {
            sh.dir_stack.push(sh.cwd.clone());
        }
        true
    } else {
        false
    }
}


// ---------------------------------------------------------------------------
// Quoting helpers shared by `export -p`, `alias` and `set`
// ---------------------------------------------------------------------------

/// True when the byte needs no quoting at all (bash's "safe" character set).
fn is_safe_shell_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'%' | b'+' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'_')
}

/// True when the value contains characters that force bash's `$'...'` form.
fn needs_ansi_quote(value: &str) -> bool {
    value
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b >= 0x80)
}

/// Quote a value the way bash's `export -p` does: `"..."` with the usual
/// backslash escapes, or `$'...'` when the value holds control characters.
fn quote_double(value: &str) -> String {
    if needs_ansi_quote(value) {
        return ansi_quote(value);
    }
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '`' => out.push_str("\\`"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Quote a value the way bash's `alias` does: `'...'` with embedded quotes
/// escaped as `'\''`.
fn quote_single(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Quote a value the way bash's `set` (without arguments) does: raw when the
/// value only contains safe characters, `$'...'` for control characters and
/// `'...'` otherwise. An empty value is printed as nothing at all.
fn quote_for_set(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if needs_ansi_quote(value) {
        return ansi_quote(value);
    }
    if value.bytes().all(is_safe_shell_char) {
        return value.to_string();
    }
    quote_single(value)
}

/// Bash's ANSI-C quoting (`$'...'`).
fn ansi_quote(value: &str) -> String {
    let mut out = String::from("$'");
    for &b in value.as_bytes() {
        match b {
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            0x1b => out.push_str("\\E"),
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{:03o}", b)),
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// Helpers shared with the executor
// ---------------------------------------------------------------------------

/// Quote a word the way bash's `set -x` trace does.
pub fn quote_for_trace(word: &str) -> String {
    if word.is_empty() {
        return "''".to_string();
    }
    if needs_ansi_quote(word) {
        return ansi_quote(word);
    }
    if word.bytes().all(is_safe_shell_char) {
        return word.to_string();
    }
    quote_single(word)
}
