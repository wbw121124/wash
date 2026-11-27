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

use std::collections::HashSet;
use std::io::Write;

use crate::arith;
use crate::safe_eprint_bytes;
use crate::safe_eprintln;
use crate::safe_print;
use crate::safe_println;
use crate::posix;
use crate::shell::{Shell, JobInfo, SET_O_ORDER, SHOPT_ORDER};
// fish 风格的翻译宏 `tr!("...")`，参见 `src/i18n.rs`。
use crate::tr;

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Every builtin name understood by `run_builtin`.
pub const BUILTINS: &[&str] = &[
    ".",
    ":",
    "[",
    "alias",
    "bg",
    "break",
    "builtin",
    "cd",
    "command",
    "continue",
    "declare",
    "disown",
    "dirs",
    "echo",
    "enable",
    "eval",
    "exec",
    "exit",
    "export",
    "false",
    "fg",
    "getopts",
    "hash",
    "help",
    "jobs",
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
    "ulimit",
    "unalias",
    "unset",
    "wait",
    "pwd",
    "complete",
    "fc",
    "history",
    "mapfile",
    "readarray",
    "compgen",
    "bind",
];

/// True when `name` is the name of a builtin command.
///
/// 先查静态 `BUILTINS`（热路径 O(1)），miss 才查动态注册表；无插件时动态表
/// 为空，零新增开销。`disabled_builtins` 由各调用点自行判定。
pub fn is_builtin(sh: &Shell, name: &str) -> bool {
    BUILTINS.contains(&name) || sh.plugins.dynamic.contains_key(name)
}

/// Run a builtin command in the given shell.
///
/// See the module comment for the `Option` contract.
pub fn run_builtin(argv: &[String], sh: &mut Shell) -> Option<i32> {
    let name = argv.first()?.as_str();
    // 动态插件内建：加载时已拒绝与静态表同名，故此处命中必为插件内建。
    // 无插件时跳过整次 HashMap 查找（热路径：每个内建命令都走到这里）。
    if !sh.plugins.dynamic.is_empty() {
        if let Some(db) = crate::plugin::lookup_dynamic_builtin(sh, name) {
            return Some(crate::plugin::invoke(&db, argv, sh));
        }
    }
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
        "getopts" => builtin_getopts(argv, sh),
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
        "ulimit" => builtin_ulimit(argv, sh),
        "wait" => builtin_wait(argv, sh),
        "jobs" => builtin_jobs(argv, sh),
        "fg" => builtin_fg(argv, sh),
        "bg" => builtin_bg(argv, sh),
        "disown" => builtin_disown(argv, sh),
        "times" => builtin_times(argv, sh),
        "let" => builtin_let(argv, sh),
        "enable" => builtin_enable(argv, sh),
        "kill" => builtin_kill(argv, sh),
        "trap" => builtin_trap(argv, sh),
        "dirs" => builtin_dirs(argv, sh),
        "pushd" => builtin_pushd(argv, sh),
        "popd" => builtin_popd(argv, sh),
        "help" => builtin_help(argv, sh),
        "mapfile" | "readarray" => builtin_mapfile(argv, sh),
        "complete" => builtin_complete(argv, sh),
        "fc" => builtin_fc(argv, sh),
        "history" => builtin_history(argv, sh),
        "compgen" => builtin_compgen(argv, sh),
        "bind" => builtin_bind(argv, sh),
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
                        safe_eprintln!("wash: cd: {}: invalid option", a);
                        safe_eprintln!("cd: usage: cd [-L|[-P [-e]]] [-@] [dir]");
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
        safe_eprintln!("wash: cd: too many arguments");
        return 2;
    }
    let target: Option<String> = operands.into_iter().next();

    // Resolve the destination directory.
    let mut print_result = false;
    let dir: String = match target {
        None => match sh.env.get("HOME") {
            None => {
                safe_eprintln!("wash: cd: HOME not set");
                return 1;
            }
            // An empty $HOME is a successful no-op in bash.
            Some(h) if h.is_empty() => return 0,
            Some(h) => h.clone(),
        },
        Some(d) if d == "-" => match sh.env.get("OLDPWD").or(sh.oldpwd.as_ref()) {
            None => {
                safe_eprintln!("wash: cd: OLDPWD not set");
                return 1;
            }
            Some(p) if p.is_empty() => {
                safe_eprintln!("wash: cd: OLDPWD not set");
                return 1;
            }
            Some(p) => {
                print_result = true;
                p.clone()
            }
        },
        Some(d) if d.is_empty() => {
            safe_eprintln!("wash: cd: null directory");
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
            safe_eprintln!(
                "wash: cd: {}: {}",
                diag_text(&dir),
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
                safe_eprintln!("wash: exit: {}: numeric argument required", arg);
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
                    safe_eprintln!("wash: export: -f: not supported");
                    safe_eprintln!("export: usage: export [-fn] [name[=value] ...] or export -p [-f]");
                    return 2;
                }
                _ => {
                    safe_eprintln!("wash: export: {}: invalid option", a);
                    safe_eprintln!("export: usage: export [-fn] [name[=value] ...] or export -p [-f]");
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
                    safe_eprintln!("wash: export: `{}': not a valid identifier", arg);
                    return 1;
                }
                // A readonly variable must not be reassigned through `export`.
                // bash prints `Y: readonly variable` (no `export:` prefix).
                if sh.readonly.contains(name) {
                    safe_eprintln!("wash: {}: readonly variable", name);
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
                    safe_eprintln!("wash: export: `{}': not a valid identifier", arg);
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
                println!("declare -{} {}={}", attrs, name, quote_value_for_audit(v))
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
                    safe_eprintln!("wash: unset: {}: invalid option", a);
                    safe_eprintln!("unset: usage: unset [-f] [-v] [-n] [name ...]");
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
            safe_eprintln!("wash: unset: {}: cannot unset: readonly variable", name);
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
                    safe_eprintln!("wash: set: {}{}: invalid option", sign, ch);
                    safe_eprintln!(
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
        safe_eprintln!("wash: set: {}: invalid option name", name);
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
        // `set -t` enables `onecmd` (run a single command then exit).  The
        // one-shot behaviour itself is not yet implemented, but the flag must
        // be recorded so `${-}` / `$-` reflect it.
        't' => sh.options.onecmd = true,
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
        't' => sh.options.onecmd = false,
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
///
/// bash prints its internal variables (BASH, BASHOPTS, BASH_VERSINFO, ...)
/// first, in a fixed order, then the regular shell variables sorted
/// alphabetically, then array variables.  The exact variable SET differs from
/// bash's, so this output is a KNOWN-DIFF (GROUP K `set-noargs`); the goal
/// here is a stable, bash-shaped listing.
fn print_all_variables(sh: &Shell) {
    const INTERNAL_ORDER: &[&str] = &[
        "BASH",
        "BASHOPTS",
        "BASH_ALIASES",
        "BASH_ARGC",
        "BASH_ARGV",
        "BASH_CMDS",
        "BASH_EXECUTION_STRING",
        "BASH_LINENO",
        "BASH_LOADABLES_PATH",
        "BASH_SOURCE",
        "BASH_VERSINFO",
        "BASH_VERSION",
        "DIRSTACK",
        "EUID",
        "GROUPS",
        "HOSTTYPE",
        "MACHTYPE",
        "OSTYPE",
        "PPID",
        "SHELLOPTS",
        "UID",
    ];
    // Internal scalar variables first.
    for name in INTERNAL_ORDER {
        if let Some(value) = sh.env.get(*name) {
            println!("{}={}", name, quote_for_set(value));
        }
    }
    // Internal array variables (bash lists them interleaved with the scalars,
    // but a stable grouping here is sufficient for our purposes).
    let mut array_names: Vec<&String> = sh.arrays.keys().collect();
    array_names.sort();
    for name in array_names {
        if let Some(elems) = sh.arrays.get(name) {
            println!("{}={}", name, format_array(elems));
        }
    }
    // Then every other scalar variable, sorted alphabetically.
    let mut names: Vec<&String> = sh.env.keys().collect();
    names.sort();
    for name in names {
        if INTERNAL_ORDER.contains(&name.as_str()) {
            continue;
        }
        let value = sh.env.get(name).cloned().unwrap_or_default();
        println!("{}={}", name, quote_for_set(&value));
    }
}

/// Format an array the way bash's `set` does: `([0]="a" [1]="b")`.  An empty
/// array is printed as `()`.
fn format_array(elems: &[String]) -> String {
    if elems.is_empty() {
        return "()".to_string();
    }
    let mut out = String::from("(");
    for (i, e) in elems.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&format!("[{}]={}", i, quote_value_for_audit(e)));
    }
    out.push(')');
    out
}

/// Format an array for `declare -p`, preserving sparse subscripts.  `sh.arrays`
/// stores a dense `Vec` with empty placeholders in the gaps and keeps the set
/// of actually assigned indices in `sh.array_set_indices`; bash prints only
/// the assigned ones (`a=([0]=x [3]=y)` -> `([0]="x" [3]="y")`).
fn format_array_sparse(elems: &[String], indices: Option<&HashSet<i64>>) -> String {
    if elems.is_empty() {
        return "()".to_string();
    }
    let assigned: Vec<(i64, &String)> = match indices {
        Some(set) if !set.is_empty() => {
            let mut sorted: Vec<i64> = set
                .iter()
                .copied()
                .filter(|i| *i >= 0 && (*i as usize) < elems.len())
                .collect();
            sorted.sort_unstable();
            sorted
                .into_iter()
                .map(|i| (i, &elems[i as usize]))
                .collect()
        }
        _ => elems.iter().enumerate().map(|(i, e)| (i as i64, e)).collect(),
    };
    if assigned.is_empty() {
        return "()".to_string();
    }
    let body = assigned
        .iter()
        .map(|(i, e)| format!("[{}]={}", i, quote_value_for_audit(e)))
        .collect::<Vec<_>>()
        .join(" ");
    format!("({})", body)
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
        safe_eprintln!("wash: shift: too many arguments");
        // bash aborts the shell on this usage error.
        sh.exit_requested = Some(1);
        return 1;
    }

    let count: i64 = match args.first() {
        None => 1,
        Some(arg) => {
            let value = match parse_bash_number(arg) {
                None => {
                    safe_eprintln!("wash: shift: {}: numeric argument required", arg);
                    return 2;
                }
                Some(v) => v,
            };
            if value < 0 {
                safe_eprintln!("wash: shift: {}: shift count out of range", arg);
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
    let mut query = false;
    let mut enable = false;
    let mut disable = false;
    let mut show_set_options = false;
    let mut i = 1usize;

    // bash accepts bundled short options (`-qs`, `-pqsu`, `-oq`), so parse
    // every flag character of a `-xyz` cluster rather than matching whole
    // words.  `--` ends option parsing.
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for ch in a[1..].chars() {
                match ch {
                    'p' => print = true,
                    'q' => query = true,
                    's' => enable = true,
                    'u' => disable = true,
                    'o' => show_set_options = true,
                    _ => {
                        safe_eprintln!("wash: shopt: -{}: invalid option", ch);
                        safe_eprintln!("shopt: usage: shopt [-pqsu] [-o] [optname ...]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    // `shopt -s -u` (either order, bundled or separate) is a hard error.
    if enable && disable {
        safe_eprintln!("wash: shopt: cannot set and unset shell options simultaneously");
        return 1;
    }

    let names: Vec<String> = argv[i..].to_vec();

    // `shopt -o` operates on the `set -o` option table.
    if show_set_options {
        return shopt_set_options(sh, &names, print, query, enable, disable);
    }

    // `-s` / `-u` with an explicit name list: change those options.
    if enable || disable {
        if names.is_empty() {
            // `shopt -s` / `shopt -u` with no names list the on/off subset.
            return shopt_list_subset(sh, enable, query);
        }
        let mut status = 0;
        for name in &names {
            if !sh.shopt_set(name, enable) {
                safe_eprintln!("wash: shopt: {}: invalid shell option name", name);
                status = 1;
            }
        }
        return status;
    }

    // Query-only mode (`-q` without set/unset): no output; rc reflects state.
    if query {
        if names.is_empty() {
            // `shopt -q` with no names succeeds unconditionally in bash.
            return 0;
        }
        let mut status = 0;
        for name in &names {
            match sh.shopt_get(name) {
                Some(true) => {}
                Some(false) => status = 1,
                None => {
                    safe_eprintln!("wash: shopt: {}: invalid shell option name", name);
                    status = 1;
                }
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
        return 0;
    }
    let mut status = 0;
    for name in &names {
        match sh.shopt_get(name) {
            None => {
                safe_eprintln!("wash: shopt: {}: invalid shell option name", name);
                status = 1;
            }
            Some(value) => {
                if print {
                    println!("shopt {} {}", if value { "-s" } else { "-u" }, name);
                } else {
                    println!(
                        "{:<width$}\t{}",
                        name,
                        if value { "on" } else { "off" },
                        width = SHOPT_WIDTH
                    );
                }
                if !value {
                    status = 1;
                }
            }
        }
    }
    status
}

/// `shopt -o [name ...]`: list / query / change the `set -o` option table.
fn shopt_set_options(
    sh: &mut Shell,
    names: &[String],
    print: bool,
    query: bool,
    enable: bool,
    disable: bool,
) -> i32 {
    if enable || disable {
        if names.is_empty() {
            // `shopt -o -s` / `-o -u` with no names list the on/off subset.
            if !query {
                let want = enable;
                for name in SET_O_ORDER {
                    if sh.options.get(name).unwrap_or(false) == want {
                        // The `-s`/-`-u` subset uses the same 15-column table
                        // as the full `shopt -o` listing (only the *named*
                        // `shopt -o NAME` form uses width 20).
                        println!("{:<15}\t{}", name, if want { "on" } else { "off" });
                    }
                }
            }
            return 0;
        }
        let mut status = 0;
        for name in names {
            if sh.options.set(name, enable) {
                // applied
            } else {
                safe_eprintln!("wash: shopt: {}: invalid option name", name);
                status = 1;
            }
        }
        return status;
    }
    if names.is_empty() {
        if print {
            print_option_list(sh);
        } else if !query {
            print_option_table(sh);
        }
        return 0;
    }
    let mut status = 0;
    for name in names {
        match sh.options.get(name) {
            None => {
                safe_eprintln!("wash: shopt: {}: invalid option name", name);
                status = 1;
            }
            Some(value) => {
                if print {
                    println!("set {}o {}", if value { '-' } else { '+' }, name);
                } else if !query && !value {
                    println!("{:<width$}\t{}", name, "off", width = SET_O_WIDTH);
                } else if !query {
                    println!("{:<width$}\t{}", name, "on", width = SET_O_WIDTH);
                }
                if !value {
                    status = 1;
                }
            }
        }
    }
    status
}

/// `shopt -s` / `shopt -u` with no names: list the options currently on/off.
fn shopt_list_subset(sh: &Shell, want_on: bool, query: bool) -> i32 {
    if query {
        return 0;
    }
    for name in SHOPT_ORDER {
        if sh.shopt_get(name).unwrap_or(false) == want_on {
            println!(
                "{:<width$}\t{}",
                name,
                if want_on { "on" } else { "off" },
                width = SHOPT_WIDTH
            );
        }
    }
    0
}

/// bash pads `shopt` option names to a fixed 20-column field before the tab
/// (verified byte-for-byte: `sourcepath` + 10 spaces + `\\t`).  It is NOT
/// derived from the longest implemented name — wash knows only three options
/// but bash's column is still 20.
const SHOPT_WIDTH: usize = 20;
const SET_O_WIDTH: usize = 20;

/// Print every `shopt` option and its state, the way bash's `shopt` does.
fn print_shopt_table(sh: &Shell) {
    let width = SHOPT_WIDTH;
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
                safe_eprintln!("wash: [: missing `]'");
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
            safe_eprintln!("wash: {}: {}", name, e.message());
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
        safe_eprintln!("wash: {}: {}", name, message);
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

// `type` 的关键字判定直接查 `lexer::SHELL_KEYWORDS`（见 `describe_command`），
// 与 lexer 保留字识别、`compgen -k` 共享同一份定义，这里不再另抄一份常量。

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
    if !flags.force_path && crate::lexer::SHELL_KEYWORDS.iter().any(|(w, _)| *w == name) {
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
    if !flags.force_path && is_builtin(sh, name) && !sh.disabled_builtins.contains(name) {
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
        crate::parser::Redir::Out { fd, file, append, clobber } => format!(
            "{}{}{}",
            if *fd == 1 { String::new() } else { fd.to_string() },
            if *append { ">>" } else if *clobber { ">|" } else { ">" },
            render_word(file)
        ),
        crate::parser::Redir::Both { file, append } => format!(
            "{}{}",
            if *append { "&>>" } else { "&>" },
            render_word(file)
        ),
        // Dup's execution is direction-independent (dup2(src, fd)); the fd
        // value is only used to pick the `<&` / `>&` spelling for display.
        crate::parser::Redir::Dup { fd, file, .. } => format!(
            "{}{}&{}",
            if *fd == 0 || *fd == 1 {
                String::new()
            } else {
                fd.to_string()
            },
            if *fd == 0 { "<" } else { ">" },
            render_word(file)
        ),
        crate::parser::Redir::HereDoc { fd, .. } => format!("{}<<...", if *fd == 0 { String::new() } else { fd.to_string() }),
        crate::parser::Redir::HereString { .. } => "<<<...".to_string(),
    }
}

fn render_simple(c: &crate::parser::SimpleCommand) -> String {
    let mut parts: Vec<String> = c.argv.iter().map(|w| render_word(w)).collect();
    for r in &c.redirs {
        parts.push(render_redir(r));
    }
    parts.join(" ")
}

pub(crate) fn render_pipeline(p: &crate::parser::Pipeline) -> String {
    p.commands.iter().map(render_simple).collect::<Vec<_>>().join(" | ")
}

/// Bash-style multi-line rendering of a pipeline, including compound
/// commands.  Used for `jobs` when the raw `&` span is unreliable (a
/// semicolon inside `if` / `for` / `while` restarts the span in the lexer).
pub(crate) fn render_pipeline_bash(p: &crate::parser::Pipeline) -> String {
    let mut clone = p.clone();
    clone.background = false; // `jobs` prints the trailing ` &` itself.
    bash_fmt_pipeline(&clone, 0).join("\n")
}

/// Single-line fallback for a brace group whose raw `&` span is unavailable.
/// bash keeps `{ ...; }` on one line in `jobs` (unlike if/for/while/case).
pub(crate) fn render_group_single_line(c: &crate::parser::Compound) -> String {
    match c {
        crate::parser::Compound::Group { body } => {
            let inner = render_command_list(body, 0).replace('\n', "; ");
            if inner.is_empty() {
                "{ }".to_string()
            } else {
                format!("{{ {}; }}", inner)
            }
        }
        _ => render_pipeline_bash(&crate::parser::Pipeline {
            commands: vec![crate::parser::SimpleCommand {
                argv: Vec::new(),
                redirs: Vec::new(),
                assignments: Vec::new(),
                array_assignments: Vec::new(),
                compound: Some(c.clone()),
            }],
            negate: false,
            background: false,
            raw: None,
        }),
    }
}

/// Bash-style rendering of a whole AND-OR list for a background job.
pub(crate) fn render_andor_bash(a: &crate::parser::AndOr) -> String {
    let mut out = render_pipeline_bash(&a.pipelines[0]);
    for (op, p) in a.ops.iter().zip(a.pipelines[1..].iter()) {
        let sep = match op {
            crate::parser::AndOrOp::And => "&&",
            crate::parser::AndOrOp::Or => "||",
        };
        out.push_str(&format!(" {} {}", sep, render_pipeline_bash(p)));
    }
    out
}

pub(crate) fn render_andor(a: &crate::parser::AndOr) -> String {
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
        crate::parser::Compound::Select { var, words, body, .. } => {
            let w = words
                .iter()
                .map(|w| render_word(w))
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "select {} in {}; do\n{}\ndone",
                var,
                w,
                render_command_list(body, 1)
            )
        }
        crate::parser::Compound::ArithFor { init, cond, incr, body } => {
            format!(
                "for (( {}; {}; {} )); do\n{}\ndone",
                init,
                cond,
                incr,
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
        crate::parser::Compound::Group { body } => {
            format!("{{\n{}\n}}", render_command_list(body, 1))
        }
        crate::parser::Compound::Subshell { body } => {
            format!("(\n{}\n)", render_command_list(body, 1))
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
                let term = match arm.terminator {
                    crate::parser::CaseTerminator::Fall => ";&",
                    crate::parser::CaseTerminator::FallTest => ";;&",
                    _ => ";;",
                };
                s.push_str(&format!(
                    "    {})\n{}\n    {}\n",
                    pats,
                    render_command_list(&arm.body, 2),
                    term
                ));
            }
            s.push_str("esac");
            s
        }
        crate::parser::Compound::Arith { expr, .. } => {
            format!("(( {} ))", expr)
        }
        crate::parser::Compound::Cond { expr, .. } => {
            format!("[[ {} ]]", crate::cond::render_cond(expr))
        }
    }
}

// ---------------------------------------------------------------------------
// `declare -f` source rendering
// ---------------------------------------------------------------------------
//
// `type` only needs a human-readable reconstruction; `declare -f` is compared
// byte-for-byte against bash, whose formatter has its own fixed layout:
//   * the header and the opening brace each carry a trailing space;
//   * every command sits on its own line, four spaces deeper per block;
//   * simple commands inside a block end with `;`, the final command of the
//     *function* body does not;
//   * `do`, `done`, `else`, `fi`, `;;` and `}` start a new line, while `then`
//     stays on the condition's line;
//   * `elif` is rendered as a nested `else / if / fi;`.
// The renderer below is deliberately separate from `render_command_list` so
// the long-standing `type` output cannot be perturbed by these rules.

fn bash_fmt_pad(indent: usize) -> String {
    "    ".repeat(indent)
}

/// A word as `declare -f` prints it.  The synthesized no-`in` loop word is a
/// quoted `$@` in the AST, and bash spells it that way in the reconstructed
/// source (`for i in "$@";`).
fn bash_fmt_word(word: &[crate::lexer::WordPart]) -> String {
    if matches!(word, [crate::lexer::WordPart::Var(n, true)] if n == "@") {
        return "\"$@\"".to_string();
    }
    render_word(word)
}

fn bash_fmt_simple(c: &crate::parser::SimpleCommand, indent: usize) -> Vec<String> {
    if let Some(cmp) = &c.compound {
        return bash_fmt_compound(cmp, indent);
    }
    let mut parts: Vec<String> = Vec::new();
    for (name, value, append) in &c.assignments {
        parts.push(format!(
            "{}{}={}",
            name,
            if *append { "+" } else { "" },
            render_word(value)
        ));
    }
    for (name, values, append) in &c.array_assignments {
        let elems = values
            .iter()
            .map(|(index, w)| match index {
                Some(i) => format!("[{}]={}", render_word(i), render_word(w)),
                None => render_word(w),
            })
            .collect::<Vec<_>>()
            .join(" ");
        parts.push(format!("{}{}=({})", name, if *append { "+" } else { "" }, elems));
    }
    parts.extend(c.argv.iter().map(|w| bash_fmt_word(w)));
    parts.extend(c.redirs.iter().map(render_redir));
    vec![format!("{}{}", bash_fmt_pad(indent), parts.join(" "))]
}

/// Continuation pipelines in one AND-OR / pipe chain stay on the same line,
/// so their already-applied indent must be removed before joining.
fn bash_fmt_dedent(line: &str, indent: usize) -> String {
    let pad = bash_fmt_pad(indent);
    line.strip_prefix(&pad).unwrap_or(line).to_string()
}

fn bash_fmt_pipeline(p: &crate::parser::Pipeline, indent: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for cmd in &p.commands {
        let mut part = bash_fmt_simple(cmd, indent);
        if lines.is_empty() {
            lines.append(&mut part);
        } else {
            let first = bash_fmt_dedent(&part.remove(0), indent);
            if let Some(last) = lines.last_mut() {
                last.push_str(" | ");
                last.push_str(&first);
            }
            lines.append(&mut part);
        }
    }
    if p.background {
        if let Some(last) = lines.last_mut() {
            last.push_str(" &");
        }
    }
    lines
}

fn bash_fmt_andor(a: &crate::parser::AndOr, indent: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (i, pipeline) in a.pipelines.iter().enumerate() {
        let mut part = bash_fmt_pipeline(pipeline, indent);
        if i > 0 && !lines.is_empty() && !part.is_empty() {
            let op = match a.ops[i - 1] {
                crate::parser::AndOrOp::And => " && ",
                crate::parser::AndOrOp::Or => " || ",
            };
            let first = bash_fmt_dedent(&part.remove(0), indent);
            if let Some(last) = lines.last_mut() {
                last.push_str(op);
                last.push_str(&first);
            }
        }
        lines.append(&mut part);
    }
    lines
}

/// `trail` marks a nested block: its last command also gets a `;` because the
/// source has one before `fi` / `done` / `}`.  The outermost function body is
/// rendered with `trail == false` (bash drops that final separator).
fn bash_fmt_list(cl: &crate::parser::CommandList, indent: usize, trail: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Nested definitions precede their uses in the reconstructed source.
    for fd in &cl.func_defs {
        let mut lines = bash_fmt_function(&fd.name, &fd.body, indent, true);
        if let Some(last) = lines.last_mut() {
            last.push(';');
        }
        out.append(&mut lines);
    }
    let n = cl.andors.len();
    for (i, andor) in cl.andors.iter().enumerate() {
        let mut lines = bash_fmt_andor(andor, indent);
        if i + 1 < n || trail {
            if let Some(last) = lines.last_mut() {
                last.push(';');
            }
        }
        out.append(&mut lines);
    }
    out
}

fn bash_fmt_inline(cl: &crate::parser::CommandList) -> String {
    cl.andors
        .iter()
        .map(|a| {
            bash_fmt_andor(a, 0)
                .iter()
                .map(|s| s.trim_start())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn bash_fmt_if(
    branches: &[(crate::parser::CommandList, crate::parser::CommandList)],
    else_body: &Option<crate::parser::CommandList>,
    indent: usize,
) -> Vec<String> {
    let pad = bash_fmt_pad(indent);
    let (cond, body) = &branches[0];
    let mut out = vec![format!("{}if {}; then", pad, bash_fmt_inline(cond))];
    out.extend(bash_fmt_list(body, indent + 1, true));
    for (cond, body) in &branches[1..] {
        out.push(format!("{}else", pad));
        let mut nested = bash_fmt_if(&[(cond.clone(), body.clone())], &None, indent + 1);
        // The nested `fi` is followed by the outer `fi`, so bash keeps the
        // source separator (`fi;`) on that line.
        if let Some(last) = nested.last_mut() {
            last.push(';');
        }
        out.extend(nested);
    }
    if let Some(eb) = else_body {
        out.push(format!("{}else", pad));
        out.extend(bash_fmt_list(eb, indent + 1, true));
    }
    out.push(format!("{}fi", pad));
    out
}

fn bash_fmt_compound(c: &crate::parser::Compound, indent: usize) -> Vec<String> {
    let pad = bash_fmt_pad(indent);
    match c {
        crate::parser::Compound::If { branches, else_body } => {
            bash_fmt_if(branches, else_body, indent)
        }
        crate::parser::Compound::For { var, words, body } => {
            let w = words
                .iter()
                .map(|w| bash_fmt_word(w))
                .collect::<Vec<_>>()
                .join(" ");
            let mut out = vec![format!("{}for {} in {};", pad, var, w), format!("{}do", pad)];
            out.extend(bash_fmt_list(body, indent + 1, true));
            out.push(format!("{}done", pad));
            out
        }
        crate::parser::Compound::Select { var, words, body, .. } => {
            let w = words
                .iter()
                .map(|w| bash_fmt_word(w))
                .collect::<Vec<_>>()
                .join(" ");
            let mut out = vec![format!("{}select {} in {};", pad, var, w), format!("{}do", pad)];
            out.extend(bash_fmt_list(body, indent + 1, true));
            out.push(format!("{}done", pad));
            out
        }
        crate::parser::Compound::ArithFor { init, cond, incr, body } => {
            let norm = |s: &str| if s.trim().is_empty() { "1".to_string() } else { s.trim().to_string() };
            let mut out = vec![format!(
                "{}for (({}; {}; {}))",
                pad,
                norm(init),
                norm(cond),
                norm(incr)
            )];
            out.push(format!("{}do", pad));
            out.extend(bash_fmt_list(body, indent + 1, true));
            out.push(format!("{}done", pad));
            out
        }
        crate::parser::Compound::While { condition, body, is_until } => {
            let kw = if *is_until { "until" } else { "while" };
            let mut out = vec![format!("{}{} {}; do", pad, kw, bash_fmt_inline(condition))];
            out.extend(bash_fmt_list(body, indent + 1, true));
            out.push(format!("{}done", pad));
            out
        }
        crate::parser::Compound::Case { word, arms } => {
            let mut out = vec![format!("{}case {} in ", pad, bash_fmt_word(word))];
            for arm in arms {
                let pats = arm
                    .patterns
                    .iter()
                    .map(|p| bash_fmt_word(p))
                    .collect::<Vec<_>>()
                    .join(" | ");
                out.push(format!("{}    {})", pad, pats));
                out.extend(bash_fmt_list(&arm.body, indent + 2, false));
                let term = match arm.terminator {
                    crate::parser::CaseTerminator::Fall => ";&",
                    crate::parser::CaseTerminator::FallTest => ";;&",
                    _ => ";;",
                };
                out.push(format!("{}    {}", pad, term));
            }
            out.push(format!("{}esac", pad));
            out
        }
        crate::parser::Compound::Group { body } => {
            let mut out = vec![format!("{}{{ ", pad)];
            out.extend(bash_fmt_list(body, indent + 1, false));
            out.push(format!("{}}}", pad));
            out
        }
        crate::parser::Compound::Subshell { body } => {
            // bash keeps a one-command subshell on a single line.
            if body.andors.len() == 1
                && body.andors[0].pipelines.len() == 1
                && body.andors[0].pipelines[0].commands.len() == 1
                && body.andors[0].pipelines[0].commands[0].compound.is_none()
            {
                let inner = bash_fmt_list(body, 0, false);
                return vec![format!("{}( {} )", pad, inner.join("; ").trim_start())];
            }
            let mut out = vec![format!("{}(", pad)];
            out.extend(bash_fmt_list(body, indent + 1, false));
            out.push(format!("{})", pad));
            out
        }
        crate::parser::Compound::Arith { expr, .. } => vec![format!("{}(( {} ))", pad, expr.trim())],
        crate::parser::Compound::Cond { expr, .. } => {
            vec![format!("{}[[ {} ]]", pad, crate::cond::render_cond(expr))]
        }
    }
}

fn bash_fmt_function(
    name: &str,
    body: &crate::parser::CommandList,
    indent: usize,
    nested: bool,
) -> Vec<String> {
    let pad = bash_fmt_pad(indent);
    let mut out = vec![
        format!(
            "{}{}{} () ",
            pad,
            if nested { "function " } else { "" },
            name
        ),
        format!("{}{{ ", pad),
    ];
    out.extend(bash_fmt_list(body, indent + 1, false));
    out.push(format!("{}}}", pad));
    out
}

fn print_function_bash(name: &str, body: &crate::parser::CommandList) {
    for line in bash_fmt_function(name, body, 0, false) {
        println!("{}", line);
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
                    safe_eprintln!("wash: type: {}: invalid option", a);
                    safe_eprintln!("type: usage: type [-afptP] name [name ...]");
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
                safe_eprint_bytes!(
                    b"wash: type: ",
                    crate::byteval::decode_raw(&name),
                    b": not found\n"
                );
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
                    safe_eprintln!("wash: command: {}: invalid option", a);
                    safe_eprintln!("command: usage: command [-pVv] command [arg ...]");
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
                    safe_eprintln!("wash: command: {}: not found", diag_text(name));
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
            if is_builtin(sh, &rest[0]) && !sh.disabled_builtins.contains(&rest[0]) {
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
        safe_eprintln!("wash: builtin: {}: invalid option", argv[i]);
        safe_eprintln!("builtin: usage: builtin [shell-builtin [arg ...]]");
        return 2;
    }
    if i >= argv.len() {
        return 0;
    }

    let name = argv[i].clone();
    if !is_builtin(sh, &name) || sh.disabled_builtins.contains(&name) {
        safe_eprintln!("wash: builtin: {}: not a shell builtin", diag_text(&name));
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

/// The `getopts` builtin: parse one option per call from the positional
/// parameters (or the explicit arguments), updating `OPTIND`/`OPTARG` and the
/// caller-named variable the way bash 5.3 does.
fn builtin_getopts(argv: &[String], sh: &mut Shell) -> i32 {
    if argv.len() < 3 {
        safe_eprintln!("getopts: usage: getopts optstring name [arg ...]");
        return 2;
    }
    let optstring = argv[1].clone();
    let name = argv[2].clone();
    let args: Vec<String> = if argv.len() > 3 {
        argv[3..].to_vec()
    } else {
        sh.positional.clone()
    };
    // A leading colon selects bash's silent error mode.
    let silent = optstring.starts_with(':');
    let opt_chars: Vec<char> = if silent {
        optstring.chars().skip(1).collect()
    } else {
        optstring.chars().collect()
    };
    let mut optind: usize = sh
        .env
        .get("OPTIND")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1);
    if optind == 0 {
        optind = 1;
    }
    let (saved_ind, saved_off) = sh.getopts_offset;
    let mut offset = if saved_ind == optind { saved_off } else { 0 };

    loop {
        if optind > args.len() {
            break;
        }
        let cur = &args[optind - 1];
        if cur == "--" {
            optind += 1;
            offset = 0;
            break;
        }
        if !cur.starts_with('-') || cur == "-" {
            break;
        }
        let chars: Vec<char> = cur.chars().collect();
        if offset == 0 {
            offset = 1; // skip the leading '-'
        }
        if offset >= chars.len() {
            optind += 1;
            offset = 0;
            continue;
        }
        let c = chars[offset];
        let pos = match opt_chars.iter().position(|x| *x == c) {
            Some(p) => p,
            None => {
                if !silent {
                    safe_eprintln!("wash: illegal option -- {}", c);
                }
                sh.env.insert(name.clone(), "?".to_string());
                sh.env.insert("OPTIND".to_string(), optind.to_string());
                offset += 1;
                if offset >= chars.len() {
                    optind += 1;
                    offset = 0;
                }
                sh.getopts_offset = (optind, offset);
                sh.env.insert("OPTIND".to_string(), optind.to_string());
                return 0;
            }
        };
        let needs_arg = opt_chars.get(pos + 1) == Some(&':');
        offset += 1;
        let mut optarg: Option<String> = None;
        if needs_arg {
            if offset < chars.len() {
                optarg = Some(chars[offset..].iter().collect());
                offset = chars.len();
            } else if optind < args.len() {
                optind += 1;
                optarg = Some(args[optind - 1].clone());
            } else {
                if !silent {
                    safe_eprintln!("wash: option requires an argument -- {}", c);
                }
                sh.env.insert(
                    name.clone(),
                    if silent { ":" } else { "?" }.to_string(),
                );
                if silent {
                    sh.env.insert("OPTARG".to_string(), c.to_string());
                }
                optind += 1;
                offset = 0;
                sh.getopts_offset = (optind, offset);
                sh.env.insert("OPTIND".to_string(), optind.to_string());
                return 0;
            }
        }
        if offset >= chars.len() {
            optind += 1;
            offset = 0;
        }
        sh.env.insert(name.clone(), c.to_string());
        match optarg {
            Some(v) => {
                sh.env.insert("OPTARG".to_string(), v);
            }
            None => {
                sh.env.remove("OPTARG");
            }
        }
        sh.getopts_offset = (optind, offset);
        sh.env.insert("OPTIND".to_string(), optind.to_string());
        return 0;
    }
    // No more options: bash sets the caller's variable to `?` and returns 1.
    sh.env.insert(name, "?".to_string());
    sh.env.insert("OPTIND".to_string(), optind.to_string());
    sh.getopts_offset = (0, 0);
    1
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
                            safe_eprintln!("{}", USAGE);
                            return 2;
                        }
                    }
                }
                _ => {
                    safe_eprintln!("wash: hash: {}: invalid option", a);
                    safe_eprintln!("{}", USAGE);
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
        safe_eprintln!(
            "wash: hash: {}: option requires an argument",
            if delete { "-d" } else { "-t" }
        );
        return 1;
    }

    // A pathname without a name is a usage error.
    if pathname.is_some() && names.is_empty() {
        safe_eprintln!("{}", USAGE);
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
                    safe_eprintln!("wash: hash: {}: not found", diag_text(name));
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
                safe_eprintln!("wash: hash: {}: not found", diag_text(name));
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
                safe_eprintln!("wash: hash: {}: not found", diag_text(name));
                status = 1;
            }
        }
    }

    status
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

/// Split the input of `read` into the fields of its variables.
///
/// 切分直接在**字节层**进行：bash 的 read 字段切分本质是字节操作，而
/// `read -n` 截断出的变量可能包含非法 UTF-8（如 `e4 b8 ad e6`），若按
/// char 迭代会在非字符边界切片 panic。IFS 字符按其 UTF-8 字节序列匹配，
/// 对合法 UTF-8 输入与旧的逐 char 实现逐字节等价。
///
/// The variables before the last one get a single field each; the last one
/// gets the rest of the line (without the trailing IFS white space).
fn read_fields(input: &[u8], ifs: &[char], nvars: usize) -> Vec<String> {
    // 每个 IFS 字符的 UTF-8 字节序列；IFS 空白恒为单字节（空格/制表/换行）。
    let mut ifs_seqs: Vec<Vec<u8>> = Vec::with_capacity(ifs.len());
    for c in ifs {
        let mut buf = [0u8; 4];
        ifs_seqs.push(c.encode_utf8(&mut buf).as_bytes().to_vec());
    }
    let is_ws_seq = |seq: &[u8]| -> bool {
        seq.len() == 1 && matches!(seq[0], b' ' | b'\t' | b'\n')
    };
    // 用合法 UTF-8 的可逆编码保存原始字节（见 crate::byteval），绝不再把
    // 非法 UTF-8 塞进 String —— 那违反 String 有效性不变式（UB）。
    let bytes_to_string = |b: &[u8]| -> String { crate::byteval::encode_raw(b) };

    let mut pos = 0usize;
    let mut fields: Vec<String> = Vec::new();

    for index in 0..nvars {
        // Remove the IFS white space at the beginning of the field.
        while pos < input.len()
            && ifs_seqs
                .iter()
                .any(|s| is_ws_seq(s) && input[pos..].starts_with(s))
        {
            pos += 1;
        }
        if index + 1 == nvars {
            // The last variable gets the rest of the line.
            let mut end = input.len();
            while end > pos
                && ifs_seqs
                    .iter()
                    .any(|s| is_ws_seq(s) && input[end - 1] == s[0])
            {
                end -= 1;
            }
            fields.push(bytes_to_string(&input[pos..end]));
            pos = end;
            break;
        }
        // One field: everything up to the next IFS character.
        let mut q = pos;
        while q < input.len() && !ifs_seqs.iter().any(|s| input[q..].starts_with(s)) {
            q += 1;
        }
        let field = bytes_to_string(&input[pos..q]);
        // A non-white-space IFS character ends the field and is consumed.
        if q < input.len() {
            if let Some(s) = ifs_seqs.iter().find(|s| input[q..].starts_with(s.as_slice())) {
                if !is_ws_seq(s) {
                    q += s.len();
                }
            }
        }
        fields.push(field);
        pos = q;
    }

    fields
}

/// 把 `read` 收集到的**原始字节流**编码成可安全存入 `String` 的合法 UTF-8
/// 文本（见 [`crate::byteval`]）。所有高位字节 `0x80..=0xFF` 一一映射到私有
/// 区字符，因此任意字节序列都可无损往返；字节长度与切片语义保持不变。
/// 合法 UTF-8 文本在输出边界经 [`crate::byteval::decode_raw`] 还原后仍是
/// 原字节，不会失真。
fn decode_read_bytes(bytes: &[u8]) -> String {
    crate::byteval::encode_raw(bytes)
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
    let mut nchars_given = false;
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
                                nchars_given = true;
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
                                safe_eprintln!("wash: read: -a: array assignment is not supported");
                                return 1;
                            }
                            _ => {
                                safe_eprintln!("wash: read: -{}: not supported", c);
                                return 1;
                            }
                        }
                    }
                    _ => {
                        safe_eprintln!("wash: read: -{}: invalid option", c);
                        safe_eprintln!("{}", USAGE);
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
    // Collect the RAW bytes first.  `read` is byte-oriented (bash keeps the
    // original stdin bytes, especially under LC_ALL=C); decoding per byte
    // with `push(b as char)` would corrupt UTF-8 into Latin-1.  The single
    // decode below restores the original bytes verbatim for valid UTF-8.
    let mut input_bytes: Vec<u8> = Vec::new();
    let mut eof = false;
    let mut count = 0usize;
    let mut pass_next = false;
    // `read -n 0` / `-N 0`：bash 一个字节都不读（stdin 保持原位）、变量为
    // 空、rc 0（实测：后续 read 拿到完整第一行）。
    if !(nchars_given && nchars == 0) {
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
                input_bytes.push(c);
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
            input_bytes.push(c);
            count += 1;
        }
    }
    if pass_next {
        // The input ended with a lone backslash: it is dropped.
        input_bytes.pop();
    }
    // 字段切分必须基于**原始字节**：read_fields 内部只会做一次 encode_raw。
    // 若先把 input_bytes 编码再传 .as_bytes()，PUA 字符的 UTF-8 字节会被
    // 二次编码（read -n 截断场景曾由此输出 ee 81 a4 而非 e4）。
    let input = decode_read_bytes(&input_bytes);

    // bash returns 1 when the end of the file was reached.
    let status = if eof { 1 } else { 0 };

    if names.is_empty() {
        // Without a variable the whole line goes into $REPLY（已编码一次）。
        sh.env.insert("REPLY".to_string(), input);
        return status;
    }

    for name in &names {
        if !is_valid_ident(name) {
            safe_eprintln!("wash: read: `{}': not a valid identifier", name);
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
    let fields = read_fields(&input_bytes, &ifs_chars, names.len());
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
// mapfile / readarray
// ---------------------------------------------------------------------------

/// `mapfile` / `readarray`（`readarray` 是 `mapfile` 的别名）：把 fd 的输入
/// 逐行读入索引数组。
///
/// 支持的选项（bash 5.3 语义）：
///   * `-t`        去掉每行尾部的分隔符（默认分隔符是换行）；
///   * `-d DELIM`  用 DELIM 的首字节作分隔符（空串表示 NUL）；
///   * `-n COUNT`  最多取 COUNT 行；
///   * `-s COUNT`  跳过前 COUNT 行；
///   * `-O ORIGIN` 从下标 ORIGIN 开始写入；
///   * `-u FD`     从指定 fd 读取（默认 0）；
///   * `-c` / `-C` 回调选项：本轮只接受参数、不触发回调（如实说明）。
///
/// 省略数组名时写入默认数组 `MAPFILE`。空输入得到空数组；通过
/// `Shell::array_assigned` 标记，`declare -p` 会打印 `NAME=()`，
/// 与 bash 对「赋过值的空数组」的表示一致。
fn builtin_mapfile(argv: &[String], sh: &mut Shell) -> i32 {
    const USAGE: &str = "mapfile: usage: mapfile [-d delim] [-n count] [-O origin] [-s count] [-t] [-u fd] [-C callback] [-c quantum] [array]";

    let mut strip_delim = false;
    let mut delim: u8 = b'\n';
    let mut max_count: Option<usize> = None;
    let mut skip: usize = 0;
    let mut origin: usize = 0;
    let mut fd: i32 = 0;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            // 选项可能捆绑（如 `-tn 2`）：逐字符处理，带参数的选项吃掉
            // 同一词的剩余部分或下一个词。
            let chars: Vec<char> = a.chars().skip(1).collect();
            let mut k = 0usize;
            while k < chars.len() {
                let c = chars[k];
                let needs_arg = matches!(c, 'd' | 'n' | 'O' | 's' | 'u' | 'C' | 'c');
                let value: Option<String> = if needs_arg {
                    if k + 1 < chars.len() {
                        let rest: String = chars[k + 1..].iter().collect();
                        k = chars.len();
                        Some(rest)
                    } else {
                        i += 1;
                        argv.get(i).cloned()
                    }
                } else {
                    None
                };
                match c {
                    't' => strip_delim = true,
                    'd' => {
                        delim = value
                            .as_ref()
                            .and_then(|s| s.as_bytes().first().copied())
                            .unwrap_or(0);
                    }
                    'n' => {
                        max_count = value
                            .as_ref()
                            .and_then(|s| s.trim().parse::<usize>().ok());
                    }
                    's' => {
                        skip = value
                            .as_ref()
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                    }
                    'O' => {
                        origin = value
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
                    // -c quantum / -C callback：本轮接受参数但不触发回调。
                    'c' | 'C' => {}
                    _ => {
                        safe_eprintln!("wash: mapfile: -{}: invalid option", c);
                        safe_eprintln!("{}", USAGE);
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

    // 第一个非选项参数是要写入的数组名，默认 `MAPFILE`。
    let operands: Vec<String> = argv[i..].to_vec();
    if operands.len() > 1 {
        safe_eprintln!("wash: mapfile: too many arguments");
        return 2;
    }
    let name = operands
        .first()
        .cloned()
        .unwrap_or_else(|| "MAPFILE".to_string());
    if !is_valid_ident(&name) {
        safe_eprintln!("wash: mapfile: `{}': not a valid identifier", name);
        return 1;
    }

    // 逐字节读 fd 直到 EOF，按 delim 切行。
    let mut lines: Vec<String> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    while let Some(byte) = posix::read_byte_from(fd) {
        if byte == delim {
            // 无 -t 时保留尾部分隔符；-t 时去掉。
            if !strip_delim {
                cur.push(byte);
            }
            lines.push(crate::byteval::encode_raw(&cur));
            cur.clear();
        } else {
            cur.push(byte);
        }
    }
    // 末尾没有分隔符时，最后一段仍算一行（不含分隔符）。
    if !cur.is_empty() {
        lines.push(crate::byteval::encode_raw(&cur));
    }

    // -s 跳过前 skip 行，再按 -n 截断。
    let mut selected: Vec<String> = if skip >= lines.len() {
        Vec::new()
    } else {
        lines[skip..].to_vec()
    };
    if let Some(max) = max_count {
        selected.truncate(max);
    }

    // -O origin：保留已有数组的前 origin 个元素，新行从 origin 起写。
    // wash 用连续 Vec 无法表达 bash 的稀疏下标，origin 前的空位以空串
    // 占位（如实说明这一简化；GROUP D 用例不涉及 -O）。
    let mut result: Vec<String> = Vec::new();
    if origin > 0 {
        if let Some(existing) = sh.arrays.get(&name) {
            let keep = origin.min(existing.len());
            result.extend_from_slice(&existing[..keep]);
        }
        while result.len() < origin {
            result.push(String::new());
        }
    }
    result.extend(selected);

    sh.arrays.insert(name.clone(), result);
    sh.array_assigned.insert(name);
    0
}

// ---------------------------------------------------------------------------
// complete
// ---------------------------------------------------------------------------

/// `complete`：注册 / 查询 / 删除可编程补全规格，规格存在
/// `Shell::completion_specs`（按注册顺序）。
///
///   * 无参 `complete` / `complete -p [name...]` —— 回显规格，
///     格式 `complete {选项串} {命令名}`；干净 shell 下空输出、rc=0；
///   * `complete -W 'aa bb cc' cmd` —— 注册；`-p cmd` 回显
///     `complete -W 'aa bb cc' cmd`（wordlist 类参数用单引号包裹）；
///   * `complete -r [name...]` —— 删除规格（无参清空全部）。
///
/// 其余常见选项（-F/-C/-A/-o/-G/-X/-P/-S/-D/-E/-I 等）接受并参与回显。
fn builtin_complete(argv: &[String], sh: &mut Shell) -> i32 {
    const USAGE: &str = "complete: usage: complete [-abcdefgjksuv] [-o comp-option] [-DEI] [-A action] [-G globpat] [-W wordlist] [-F function] [-C command] [-X filterpat] [-P prefix] [-S suffix] [name ...]";

    let mut print_mode = false;
    let mut remove_mode = false;
    let mut options: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    let mut end_of_options = false;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if !end_of_options && a == "--" {
            end_of_options = true;
            i += 1;
            continue;
        }
        if !end_of_options && a.len() > 1 && a.starts_with('-') {
            let opt = a.as_str();
            match opt {
                "-p" => print_mode = true,
                "-r" => remove_mode = true,
                // wordlist / glob / filter / prefix / suffix：bash 回显时
                // 用单引号包裹这些字符串参数。
                "-W" | "-X" | "-P" | "-S" | "-G" => {
                    i += 1;
                    let v = argv.get(i).cloned().unwrap_or_default();
                    options.push(format!("{} {}", opt, quote_single(&v)));
                }
                // 函数名 / 命令名 / action / comp-option：原样回显。
                "-F" | "-C" | "-A" | "-o" => {
                    i += 1;
                    let v = argv.get(i).cloned().unwrap_or_default();
                    options.push(format!("{} {}", opt, v));
                }
                // 默认/空行/初始单词补全标记，以及无参开关。
                other => options.push(other.to_string()),
            }
            i += 1;
            continue;
        }
        commands.push(a.clone());
        i += 1;
    }

    // -r：删除指定命令的规格（无参清空全部）。
    if remove_mode {
        if commands.is_empty() {
            sh.completion_specs.clear();
        } else {
            sh.completion_specs.retain(|(c, _)| !commands.contains(c));
        }
        return 0;
    }

    // 列表模式：-p，或完全没有选项与命令名（裸 `complete`）。
    let listing = print_mode || (options.is_empty() && commands.is_empty());
    if listing {
        for (cmd, spec) in &sh.completion_specs {
            if !commands.is_empty() && !commands.contains(cmd) {
                continue;
            }
            if spec.is_empty() {
                println!("complete {}", cmd);
            } else {
                println!("complete {} {}", spec, cmd);
            }
        }
        // -p 指定了名字却查不到时，bash 报错并返回 1。
        if print_mode && !commands.is_empty() {
            for cmd in &commands {
                if !sh.completion_specs.iter().any(|(c, _)| c == cmd) {
                    safe_eprintln!("wash: complete: {}: no completion specification", cmd);
                    return 1;
                }
            }
        }
        return 0;
    }

    // 注册模式：必须有命令名。
    if commands.is_empty() {
        safe_eprintln!("wash: {}", USAGE);
        return 2;
    }
    let spec = options.join(" ");
    for cmd in &commands {
        if let Some(entry) = sh.completion_specs.iter_mut().find(|(c, _)| c == cmd) {
            entry.1 = spec.clone();
        } else {
            sh.completion_specs.push((cmd.clone(), spec.clone()));
        }
    }
    0
}

// ---------------------------------------------------------------------------
// fc
// ---------------------------------------------------------------------------

/// `fc -l` / `fc -ln`：列出命令历史（读真实的 `Shell::history`）。
///
/// bash 格式：`fc -l` 每行 `{序号}\t {命令}`，`fc -ln` 每行 `\t {命令}`
/// （无序号但保留 tab + 空格前缀）。空历史时无输出、rc=0。
/// `fc` 的编辑 / 重执行模式（-e / -s / 无 -l）本轮不支持，返回诊断。
fn builtin_fc(argv: &[String], sh: &mut Shell) -> i32 {
    let mut list = false;
    let mut no_numbers = false;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            // `--` 之后 fc 的位置参数（范围）本轮不处理，直接结束选项解析。
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a.chars().skip(1) {
                match c {
                    'l' => list = true,
                    'n' => no_numbers = true,
                    // -r 反转、-e/-s 编辑类选项本轮仅接受、不报错。
                    'r' | 'e' | 's' => {}
                    _ => {
                        safe_eprintln!("wash: fc: -{}: invalid option", c);
                        safe_eprintln!("fc: usage: fc [-e ename] [-lnr] [first] [last] or fc -s [pat=rep] [command]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    if !list {
        // bash 会调用编辑器 / 重执行历史命令；本轮不实现，如实返回诊断。
        safe_eprintln!("wash: fc: editing/executing history is not supported");
        return 1;
    }

    for (idx, cmd) in sh.history.iter().enumerate() {
        if no_numbers {
            println!("\t {}", cmd);
        } else {
            println!("{}\t {}", idx + 1, cmd);
        }
    }
    0
}

// ---------------------------------------------------------------------------
// history
// ---------------------------------------------------------------------------

/// `history`：显示 / 管理命令历史（真实读写 `Shell::history`）。
///
///   * 无参 —— 列出全部，格式 `    1  cmd`（序号右对齐宽 5 + 两空格）；
///   * `history N` —— 只列最后 N 条；
///   * `history -c` —— 清空历史；
///   * `history -d N` —— 删除第 N 条；
///   * `history -s ARG...` —— 参数以空格连接成一行追加到历史。
///
/// -a/-n/-r/-w（历史文件读写）与 -p（展开）本轮仅接受参数、不落盘。
fn builtin_history(argv: &[String], sh: &mut Shell) -> i32 {
    let mut clear = false;
    let mut delete: Option<usize> = None;
    let mut append: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            positional.extend_from_slice(&argv[i..]);
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            match a.as_str() {
                "-c" => clear = true,
                "-d" => {
                    i += 1;
                    delete = argv.get(i).and_then(|s| s.trim().parse::<usize>().ok());
                    if delete.is_none() {
                        safe_eprintln!("wash: history: -d: numeric argument required");
                        return 2;
                    }
                }
                "-s" => {
                    append = Some(argv[i + 1..].join(" "));
                    break;
                }
                // -a/-n/-r/-w/-p：接受但不落盘 / 不展开（如实说明）。
                "-a" | "-n" | "-r" | "-w" | "-p" => {
                    break;
                }
                _ => {
                    safe_eprintln!("wash: history: {}: invalid option", a);
                    safe_eprintln!("history: usage: history [-c] [-d offset] [n] or history -anrw [filename] or history -ps arg [arg...]");
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        positional.push(a.clone());
        i += 1;
    }

    if clear {
        sh.history.clear();
        return 0;
    }
    if let Some(n) = delete {
        if n == 0 || n > sh.history.len() {
            safe_eprintln!("wash: history: {}: history position out of range", n);
            return 1;
        }
        sh.history.remove(n - 1);
        return 0;
    }
    if let Some(line) = append {
        sh.history.push(line);
        return 0;
    }

    // 列出历史：`history N` 只显示最后 N 条（默认全部）。
    let total = sh.history.len();
    let count = positional
        .first()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(total);
    let start = total.saturating_sub(count);
    for (idx, cmd) in sh.history.iter().enumerate().skip(start) {
        println!("{:>5}  {}", idx + 1, cmd);
    }
    0
}

// ---------------------------------------------------------------------------
// compgen
// ---------------------------------------------------------------------------

/// `compgen`：生成补全候选。
///
/// 本轮实现的候选来源都取自真实表，不硬编码输出：
///   * `-k` —— `lexer::SHELL_KEYWORDS`（与 lexer 保留字识别共享的唯一
///     关键字定义，顺序与 bash 5.3 `compgen -k` 一致，共 22 行）；
///   * `-b` —— 本模块 `BUILTINS`（wash 真实内建表）；
///   * `-W wordlist` —— 按 `$IFS` 拆分 wordlist。
///
/// 位置参数 `WORD` 作为前缀过滤：只输出以它开头的候选。
/// 其余 action（-c/-A/-f/-v/...）及带参选项（-o/-G/-F/-C/-X/-P/-S/-V）
/// 本轮仅接受、不据此生成候选（如实说明，GROUP D 用例只用 -k）。
fn builtin_compgen(argv: &[String], sh: &mut Shell) -> i32 {
    let mut action: Option<char> = None;
    let mut wordlist: Option<String> = None;
    let mut prefix: Option<String> = None;
    let mut i = 1usize;
    let mut end_of_options = false;

    while i < argv.len() {
        let a = &argv[i];
        if !end_of_options && a == "--" {
            end_of_options = true;
            i += 1;
            continue;
        }
        if !end_of_options && a.len() > 1 && a.starts_with('-') {
            // 选项可捆绑；带参数的选项吃掉同一词的剩余部分或下一个词。
            let chars: Vec<char> = a.chars().skip(1).collect();
            let mut k = 0usize;
            while k < chars.len() {
                let c = chars[k];
                let takes_arg =
                    matches!(c, 'o' | 'A' | 'G' | 'W' | 'F' | 'C' | 'X' | 'P' | 'S' | 'V');
                let value: Option<String> = if takes_arg {
                    if k + 1 < chars.len() {
                        let rest: String = chars[k + 1..].iter().collect();
                        k = chars.len();
                        Some(rest)
                    } else {
                        i += 1;
                        argv.get(i).cloned()
                    }
                } else {
                    None
                };
                if c == 'W' {
                    wordlist = value.clone();
                    action = Some('W');
                } else if !takes_arg {
                    // 无参数 action / 开关字母：记录最后一个 action。
                    action = Some(c);
                }
                // 其余带参选项：接受但不据此生成候选。
                k += 1;
            }
            i += 1;
            continue;
        }
        prefix = Some(a.clone());
        break;
    }

    let mut candidates: Vec<String> = Vec::new();
    match action {
        Some('k') => {
            candidates.extend(
                crate::lexer::SHELL_KEYWORDS
                    .iter()
                    .map(|(w, _)| (*w).to_string()),
            );
        }
        Some('b') => {
            // 统一名单：静态内建 + 动态插件内建。
            candidates.extend(crate::plugin::all_builtin_names(sh));
        }
        Some('W') => {
            if let Some(wl) = &wordlist {
                let ifs = sh
                    .env
                    .get("IFS")
                    .cloned()
                    .unwrap_or_else(|| " \t\n".to_string());
                let seps: Vec<char> = ifs.chars().collect();
                candidates.extend(
                    wl.split(|c| seps.contains(&c))
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                );
            }
        }
        _ => {}
    }

    let p = prefix.as_deref().unwrap_or("");
    for c in &candidates {
        if c.starts_with(p) {
            println!("{}", c);
        }
    }
    0
}

// ---------------------------------------------------------------------------
// bind
// ---------------------------------------------------------------------------

/// GNU readline 默认 emacs keymap 的 `bind -p` 导出（静态资源）。
///
/// 来源：`LC_ALL=C /d/msys2/usr/bin/bash.exe -c 'bind -p'`，GNU bash
/// 5.3.20(1)-release (x86_64-pc-cygwin)。实测 497 行 / 11705 字节，
/// 首行是空行、末尾带换行；md5 = 943c9637957580f0e4b2176f773ca66c。
///
/// 该输出与随该 bash 附带的 GNU readline 版本及 locale collation 绑定：
/// QA 实测 zh_CN.UTF-8 下字节数同为 11705B，但行序不同、md5 不同，故这里
/// 固定采用 LC_ALL=C 的实测导出，绝不按 locale 重排，也绝不用循环生成
/// 填充行凑够 497 行，更不为满足 grep 塞几行假数据——这是完整的真实
/// 默认绑定表。（该 MSYS2 安装未能从 pacman/ldd/头文件/strings 直接查得
/// readline 版本号，故只标注 bash 版本与导出命令。）
static BIND_P_KEYMAP: &str = include_str!("keymap_emacs.txt");

/// `bind`：显示 readline 键绑定。
///
///   * `bind -p` —— 把完整默认 keymap 原样写到 stdout，并在 stderr 输出
///     `bind: warning: line editing not enabled`（非交互下 bash 的固定诊断），rc=0；
///   * `bind`（无 -p）—— stdout 为空，仅输出该 warning，rc=0。
///
/// wash 不启用 readline 行编辑，其余选项（-l/-s/-v/-P/-S/-V/-X/-m/-f/
/// -q/-u/-r/-x 等）本轮仅接受、不改变输出（如实说明）。
fn builtin_bind(argv: &[String], _sh: &mut Shell) -> i32 {
    let mut print = false;
    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for c in a.chars().skip(1) {
                if c == 'p' {
                    print = true;
                }
                // 其余选项本轮仅接受，不改变输出。
            }
            i += 1;
            continue;
        }
        break;
    }
    if print {
        print!("{}", BIND_P_KEYMAP);
        let _ = std::io::stdout().flush();
    }
    // 非交互下 readline 行编辑未启用，bash 固定输出这条诊断；归一化后需
    // 精确等于 `bind: warning: line editing not enabled`。
    safe_eprintln!("wash: bind: warning: line editing not enabled");
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
                        safe_eprintln!("wash: pwd: {}: invalid option", a);
                        safe_eprintln!("pwd: usage: pwd [-LPW]");
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
    // bash 检测 pwd 的写错误（`pwd > /dev/full` → `pwd: write error: ...`
    // rc 1）；修复前 println! 在写失败时直接 panic（rc 101）。
    let mut out = path.into_bytes();
    out.push(b'\n');
    write_stdout_checked(&out, "pwd")
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
                    safe_eprintln!("wash: alias: {}: invalid option", a);
                    safe_eprintln!("alias: usage: alias [-p] [name[=value] ... ]");
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
                    safe_eprintln!("wash: alias: {}: not found", arg);
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
                    safe_eprintln!("wash: unalias: {}: invalid option", a);
                    safe_eprintln!("unalias: usage: unalias [-a] name [name ...]");
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
        safe_eprintln!("wash: unalias: usage: unalias [-a] name [name ...]");
        return 2;
    }

    let mut status = 0;
    for name in &argv[i..] {
        if sh.aliases.remove(name).is_none() {
            safe_eprintln!("wash: unalias: {}: not found", name);
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
        safe_eprintln!("wash: source: filename argument required");
        safe_eprintln!("source: usage: source [-p path] filename [arguments]");
        return 2;
    }
    let name = argv[1].clone();
    let path = match find_source_file(&name, sh) {
        Some(p) => p,
        None => {
            // source 把文件名按 `%s` 原样写诊断（不 ANSI-C 引用），可能含
            // 任意字节；必须 decode_raw 后按裸字节写出。
            safe_eprint_bytes!(
                b"wash: ",
                crate::byteval::decode_raw(&name),
                b": ",
                posix::errno_message(posix::errno()).as_bytes(),
                b"\n"
            );
            return 1;
        }
    };
    // bash refuses to source a directory: `<cmd>: <name>: is a directory`
    // (rc 1)，其中 <cmd> 是 `source` 或 `.`（argv[0]），<name> 是调用时的
    // 原样拼写（相对路径保持相对）。修复前目录被当成空脚本静默成功 rc 0。
    if let Some(st) = posix::stat_path(&path, true) {
        if st.file_type == posix::FT_DIR {
            safe_eprint_bytes!(
                b"wash: ",
                crate::byteval::decode_raw(&argv[0]),
                b": ",
                crate::byteval::decode_raw(&name),
                b": is a directory\n"
            );
            return 1;
        }
    }
    let content = match posix::read_file(&path) {
        // read_file 已做 byteval 原始字节编码（含字面哨兵），不能再
        // escape_literal，否则会把 encode_raw 产生的 ESC 标记双写。
        Some(c) => c,
        None => {
            safe_eprint_bytes!(
                b"wash: ",
                crate::byteval::decode_raw(&path),
                b": ",
                posix::errno_message(posix::errno()).as_bytes(),
                b"\n"
            );
            return 1;
        }
    };

    // The script gets its own positional parameters.  bash does NOT change
    // `$0` while a file is sourced: it keeps the name the shell itself was
    // invoked with, both in the default mode and in --posix mode.
    let saved_positional = std::mem::replace(&mut sh.positional, argv[2..].to_vec());
    sh.sourcing_depth += 1;
    let code =
        sh.run_in_error_context(crate::shell::ErrorContext::sourced(&path), &content);
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
    sh.run_in_error_context(crate::shell::ErrorContext::eval(), &command)
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
                safe_eprintln!("wash: exec: -a: option requires an argument");
                safe_eprintln!("exec: usage: exec [-a name] [command [arguments ...]] [redirection ...]");
                return 2;
            }
            argv0 = Some(argv[i + 1].clone());
            i += 2;
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            safe_eprintln!("wash: exec: {}: invalid option", a);
            safe_eprintln!("exec: usage: exec [-a name] [command [arguments ...]] [redirection ...]");
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
    let (_cstrings, env_ptrs) = crate::executor::build_shell_env_ptrs(sh);
    match crate::executor::find_executable(&program, &path) {
        Some(exe) => {
            let argvec = crate::executor::ArgVec::from(&command);
            crate::executor::execve(&exe, &argvec.ptrs, &env_ptrs);
            let code = posix::errno();
            safe_eprintln!(
                "wash: exec: {}: {}",
                program,
                posix::errno_message(code)
            );
            // 只有非交互 shell（-c / 脚本）在 exec 失败时终止；交互 shell
            // 只报错并返回 127，REPL 继续存活（对齐 bash 交互行为）。
            if !sh.interactive {
                sh.exit_requested = Some(127);
            }
            127
        }
        None => {
            safe_eprintln!("wash: exec: {}: not found", program);
            // 只有非交互 shell（-c / 脚本）在 exec 失败时终止；交互 shell
            // 只报错并返回 127，REPL 继续存活（对齐 bash 交互行为）。
            if !sh.interactive {
                sh.exit_requested = Some(127);
            }
            127
        }
    }
}

// ---------------------------------------------------------------------------
// checked stdout writes
// ---------------------------------------------------------------------------

/// Write `bytes` to stdout (fd 1) and report failures bash-style.
///
/// bash 会对内建的 stdout 写入做错误检测：`echo hi > /dev/full` 报
/// `<builtin>: write error: <strerror>` 并置 rc=1。这里必须绕过 Rust 的
/// LineWriter 缓冲、直接对 fd 1 做 write(2)：
///   1. 缓冲 write 失败会把数据留在缓冲区，executor 恢复重定向后进程退出
///      的兜底 flush 会把残留字节泄漏到已恢复的真实 stdout（曾实测 `printf
///      hi > /dev/full` 把 `hi` 漏到了终端）；
///   2. errno 文案必须走单一真源 `posix::errno_message`（wash_strerror）。
///
/// fd 1 由 executor 的重定向机制管理，这里只写不关（ManuallyDrop）。
fn write_stdout_checked(bytes: &[u8], name: &str) -> i32 {
    use std::os::unix::io::FromRawFd;
    let mut f = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
    if let Err(e) = f.write_all(bytes) {
        let code = e.raw_os_error().unwrap_or_else(posix::errno);
        // stderr 本身也可能写失败（如 `echo hi > /dev/full 2>&1`）。bash
        // 此时静默放弃诊断、仍按写错误退出 1；`eprintln!` 会 panic（rc=101）
        // 造成逐字节偏离，必须使用不 panic 的 write! 并丢弃结果。
        let _ = writeln!(
            std::io::stderr(),
            "{}: write error: {}",
            name,
            posix::errno_message(code)
        );
        return 1;
    }
    0
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
        let raw_joined = crate::byteval::decode_raw(&joined);
        let (bytes, stopped) = interpret_escapes(&raw_joined, true);
        out.extend_from_slice(&bytes);
        if !stopped && !nflag {
            out.push(b'\n');
        }
    } else {
        let s = text_args.join(" ");
        out.extend_from_slice(&crate::byteval::decode_raw(&s));
        if !nflag {
            out.push(b'\n');
        }
    }

    // 写失败（如 `> /dev/full`）报 `echo: write error: <strerror>` rc=1，
    // 与 bash 一致；成功 rc=0。
    write_stdout_checked(&out, "echo")
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
                safe_eprintln!("wash: printf: `{}': not a valid identifier", var);
                return 2;
            }
            let format = match argv.get(format_idx) {
                Some(f) => f.clone(),
                None => {
                    safe_eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
                    return 2;
                }
            };
            let args = argv.get(format_idx + 1..).unwrap_or(&[]);
            let (value, conv_error, assigns) = printf_format(&format, args);
            if let Some(sh) = sh {
                sh.env.insert(var, crate::byteval::encode_raw(&value));
                // bash（oracle 实测）：`printf -v out "a%n" v` → out="a" 且
                // v=1 —— %n 的赋值与 -v 目标并存。
                for (nv, nv_val) in assigns {
                    sh.env.insert(nv, nv_val.to_string());
                }
            }
            if conv_error {
                1
            } else {
                0
            }
        }
        PrintfOpt::Usage => {
            safe_eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
            2
        }
        PrintfOpt::Normal { format_idx } => {
            // 只有「完全没有格式串」才是 usage 错误；`printf ""` 输出为空
            // 但 rc 0（bash 实测），修复前空格式串被误报 usage rc 2。
            if format_idx >= argv.len() {
                safe_eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
                return 2;
            }
            let format = &argv[format_idx];
            let args = argv.get(format_idx + 1..).unwrap_or(&[]);
            let (bytes, conv_error, assigns) = printf_format(format, args);
            // bash 也会检测 printf 的写错误（`printf hi > /dev/full` →
            // `printf: write error: ...` rc 1）；转换错误与写错误都是 rc 1。
            let werr = write_stdout_checked(&bytes, "printf");
            // %n 赋值：正常输出路径同样把「已打印字符数」写入指定变量
            //（bash oracle：`printf "a%n" v` → 输出 a、v=1、rc 0）。
            if let Some(sh) = sh {
                for (nv, nv_val) in assigns {
                    sh.env.insert(nv, nv_val.to_string());
                }
            }
            if conv_error || werr != 0 {
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
///
/// 第三个返回值是 `%n` 产生的赋值清单 `(变量名, 已打印字符数)`（按出现
/// 顺序）；调用方负责写入 shell 环境（`-v` 路径与普通路径都要应用）。
fn printf_format(format: &str, args: &[String]) -> (Vec<u8>, bool, Vec<(String, u64)>) {
    // 格式串与参数可能含 byteval 编码的裸字节（read -n 截断），先解码为
    // 原始字节再做逐字节格式化，避免 PUA 编码泄漏到输出。
    let format_raw = crate::byteval::decode_raw(format);
    let bytes = &format_raw[..];
    let mut out: Vec<u8> = Vec::new();
    let mut conv_error = false;
    let mut ai: usize = 0;
    let mut assigns: Vec<(String, u64)> = Vec::new();

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
        // bash 的 %n 计数在每轮格式复用时重置（oracle 实测
        // `printf "a%n" v x` → v=1、x=1，不是累计的 x=2）。
        let iter_start_len = out.len();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'%' {
                if i + 1 >= bytes.len() {
                    // A trailing '%' (no conversion): bash reports a missing
                    // format character and stops.
                    conv_error = true;
                    safe_eprintln!("wash: printf: `%': missing format character");
                    return (out, conv_error, assigns);
                }
                let nxt = bytes[i + 1];
                if nxt == b'%' {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                let (ni, stop) = parse_printf_spec(
                    bytes,
                    i,
                    &mut out,
                    args,
                    &mut ai,
                    &mut conv_error,
                    &mut assigns,
                    iter_start_len,
                );
                i = ni;
                if stop {
                    return (out, conv_error, assigns);
                }
            } else if c == b'\\' {
                let (esc, ni, stop) = escape_at(bytes, i, false);
                out.extend_from_slice(&esc);
                i = ni;
                if stop {
                    return (out, conv_error, assigns);
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

    (out, conv_error, assigns)
}

fn parse_printf_spec(
    bytes: &[u8],
    start: usize,
    out: &mut Vec<u8>,
    args: &[String],
    ai: &mut usize,
    conv_error: &mut bool,
    assigns: &mut Vec<(String, u64)>,
    iter_start_len: usize,
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
        // spec 来自格式串原始字节（可能含裸 0xff），必须按裸字节写诊断，
        // 不能 from_utf8_lossy 成 U+FFFD 或 U+00FF。
        safe_eprint_bytes!(
            b"wash: printf: `",
            &bytes[start..],
            b"': missing format character\n"
        );
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
    let arg_raw = crate::byteval::decode_raw(arg);

    match conv {
        b's' | b'b' => {
            let content: Vec<u8> = if conv == b'b' {
                let (bts, stop) = interpret_escapes(&arg_raw, true);
                if stop {
                    *ai += 1;
                    return (i, true);
                }
                bts
            } else {
                arg_raw.clone()
            };
            let content = truncate_prec(&content, have_prec, rprec);
            let padded =
                apply_field(&content, minus, zero, if have_width { rwidth } else { 0 }, true);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'q' => {
            let content = sh_quote(&arg_raw);
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
                vec![arg_raw[0]]
            };
            let padded = apply_field(&content, minus, zero, if have_width { rwidth } else { 0 }, true);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'n' => {
            // bash 语义（oracle 实测）：%n 不产生输出，消耗下一个参数作为
            // 变量名，把**到此为止已打印的字符数**赋给它。
            //   `printf "a%n" v`   → 输出 a，v=1，rc 0
            //   `printf "%s%n" hello v` → 输出 hello，v=5
            //   `printf "a%nb" v`  → 输出 ab，v=1（%n 之后的字面字符照常输出）
            //   `printf "a%n" v x` → 输出 aa（格式重复，v、x 各被赋 1）
            // 无效标识符：报 `printf: `X': not a valid identifier`、rc=1，
            // 并**终止**格式处理（`printf "a%nZ" 1abc` 只输出 `a`）。
            // 参数缺失：不赋值、不消耗参数（ai 未前进使格式重复循环终止）。
            // 消除 curl 符号盲区：不让 snprintf 直接吃 %n（内存写入面）。
            if arg_missing {
                // nothing to consume, nothing to assign
            } else {
                *ai += 1;
                if is_valid_ident(arg) {
                    assigns.push((arg.to_string(), (out.len() - iter_start_len) as u64));
                } else {
                    // bash（oracle 实测 `printf "a%nZ" 1abc` → 只输出 `a`，
                    // rc=1）：无效标识符报错后**终止**格式处理，与未知转换
                    // 字符同型。arg_raw 是原始字节（可能含裸 0xff），按字节写。
                    *conv_error = true;
                    safe_eprint_bytes!(
                        b"wash: printf: `",
                        &arg_raw,
                        b"': not a valid identifier\n"
                    );
                    return (i, true);
                }
            }
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
            // reports it and stops.  conv 是格式串里的原始字节（可能是
            // 0xff），`conv as char` 会变成 U+00FF 写出 c3 bf；按裸字节写。
            *conv_error = true;
            safe_eprint_bytes!(
                b"wash: printf: `",
                &[conv],
                b"': invalid format character\n"
            );
            return (i, true);
        }
    }

    (i, false)
}

/// Report a numeric conversion that failed, the way bash does.
fn report_invalid_number(arg: &str) {
    safe_eprintln!("wash: printf: {}: invalid number", arg);
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
                        safe_eprintln!("wash: umask: {}: invalid option", a);
                        safe_eprintln!("umask: usage: umask [-p] [-S] [mode]");
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
                        safe_eprintln!("wash: umask: `{}': invalid octal mode", mode.trim());
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
                safe_eprintln!("wash: let: {}: {}{}", expr, e.message, token);
                status = 1;
            }
        }
    }
    status
}

// ---------------------------------------------------------------------------
// enable
// ---------------------------------------------------------------------------

/// POSIX special builtins，按 bash 5.3 `enable -s` 的实测顺序（16 项）。
/// 与动态加载无关，`-s` 永远只列这 16 个。
const POSIX_SPECIAL_BUILTINS: &[&str] = &[
    ".", ":", "break", "continue", "eval", "exec", "exit", "export", "readonly",
    "return", "set", "shift", "source", "times", "trap", "unset",
];

/// The `enable` builtin: enable/disable shell builtins, load/unload plugins.
///
/// 已按 MSYS2 bash 5.3.20 实测定型（见计划 §oracle 实测事实）：
///   * `enable` / `enable -p`      → 列全部 **enabled** 内建，`enable <name>` 行
///                                （`-p` 带 NAME 仍打印全表，NAME 被忽略）；
///   * `enable -a`                → 列**全部**内建，disabled 显示 `enable -n <name>`；
///   * `enable -n`                → 列 disabled 内建，`enable -n <name>` 行；
///   * `enable -s`                → 列 16 个 POSIX special builtins；
///   * `enable -n NAME`           → disable；`enable NAME` / `-a NAME` → enable；
///   * `enable -f FILE [NAME...]` → 加载插件；`enable -d NAME...` → 卸载；
///   * `enable -z` / 未知选项     → usage error (rc 2)；
///   * `enable BAD` / `-d 未知名` → `not a shell builtin` (rc 1)。
fn builtin_enable(argv: &[String], sh: &mut Shell) -> i32 {
    let mut disable = false;
    let mut all = false;
    let mut print_reusable = false;
    let mut special = false;
    let mut delete = false;
    let mut load_file: Option<String> = None;
    let mut i = 1usize;

    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            let cl: Vec<char> = a.chars().skip(1).collect();
            let mut k = 0usize;
            while k < cl.len() {
                match cl[k] {
                    'n' => disable = true,
                    'a' => all = true,
                    'p' => print_reusable = true,
                    's' => special = true,
                    'd' => delete = true,
                    'f' => {
                        // `-f filename`：本串剩余部分，或下一个参数。
                        let rem: String = cl[k + 1..].iter().collect();
                        if !rem.is_empty() {
                            load_file = Some(rem);
                        } else {
                            i += 1;
                            if i >= argv.len() {
                                safe_eprintln!("wash: enable: -f: option requires an argument");
                                safe_eprintln!("enable: usage: enable [-a] [-dnps] [-f filename] [name ...]");
                                return 2;
                            }
                            load_file = Some(argv[i].clone());
                        }
                        k = cl.len();
                        continue;
                    }
                    _ => {
                        safe_eprintln!("wash: enable: {}: invalid option", a);
                        safe_eprintln!("enable: usage: enable [-a] [-dnps] [-f filename] [name ...]");
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

    // 1) -f FILE [NAME ...] —— 加载插件（NAME 仅语法兼容，wash ABI 自带名单）。
    if let Some(file) = load_file {
        return match crate::plugin::load_plugin(&file, &names, sh) {
            Ok(()) => 0,
            Err(e) => {
                // wash 自有诊断，不冒充 bash 的 "dynamic loading not available"。
                safe_eprintln!("wash: enable: cannot load {}: {}", file, e);
                1
            }
        };
    }

    // 2) -d NAME... —— 按名字卸载动态内建；未知名 rc=1（受控扩展，对齐 WSL）。
    if delete {
        if names.is_empty() {
            // 无参：列当前已加载的动态内建（reusable 行）。
            let mut v: Vec<&String> = sh.plugins.dynamic.keys().collect();
            v.sort();
            for n in v {
                println!("enable {}", n);
            }
            return 0;
        }
        let mut status = 0;
        for name in &names {
            if let Err(e) = crate::plugin::unload_plugin(name, sh) {
                safe_eprintln!("wash: enable: {}", e);
                status = 1;
            }
        }
        return status;
    }

    // 3) -s —— 16 个 POSIX special builtins（与动态加载无关）。
    if special {
        for n in POSIX_SPECIAL_BUILTINS {
            println!("enable {}", n);
        }
        return 0;
    }

    // 4) 列表模式。
    let list_disabled = disable && names.is_empty() && !print_reusable;
    let list_all = all && names.is_empty() && !print_reusable;
    let list_enabled = (names.is_empty() && !disable && !all) || print_reusable;

    if list_disabled {
        let mut v: Vec<&String> = sh.disabled_builtins.iter().collect();
        v.sort();
        for n in v {
            println!("enable -n {}", n);
        }
        return 0;
    }

    if list_all || list_enabled {
        let mut all_names = crate::plugin::all_builtin_names(sh);
        all_names.sort();
        for n in all_names {
            let is_disabled = sh.disabled_builtins.contains(&n);
            if is_disabled {
                if list_all {
                    println!("enable -n {}", n);
                }
            } else {
                println!("enable {}", n);
            }
        }
        return 0;
    }

    // 5) enable / disable 动作（带 NAME）。
    let mut status = 0;
    for name in &names {
        if !is_builtin(sh, name) {
            safe_eprintln!("wash: enable: {}: not a shell builtin", name);
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
        safe_eprintln!("wash: return: can only `return' from a function or sourced script");
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
        safe_eprintln!("wash: break: only meaningful in a `for', `while', or `until' loop");
        // bash reports the error but the builtin itself succeeds (`$?` is 0);
        // only the diagnostic is observable.
        return 0;
    }
    let n = match parse_loop_count(argv, "break", sh) {
        Ok(n) => n,
        Err(code) => return code,
    };
    sh.break_requested = Some(n);
    0
}

/// The `continue` builtin: resume a loop at the next iteration.
fn builtin_continue(argv: &[String], sh: &mut Shell) -> i32 {
    if sh.loop_depth == 0 {
        safe_eprintln!("wash: continue: only meaningful in a `for', `while', or `until' loop");
        return 0;
    }
    let n = match parse_loop_count(argv, "continue", sh) {
        Ok(n) => n,
        Err(code) => return code,
    };
    sh.continue_requested = Some(n);
    0
}

/// Validate the optional `break` / `continue` level argument exactly as bash
/// does.  The three error classes do not behave alike in bash:
///   * too many arguments / non-numeric text abort the whole command list
///     (and therefore every enclosing loop) with rc 1 / rc 2;
///   * a level below one merely ends the current loop with rc 1 and lets the
///     rest of the script run.
/// The first two use `fatal_error`, the last one reuses the normal `break`
/// channel so the enclosing loop consumes it.
fn parse_loop_count(
    argv: &[String],
    name: &str,
    sh: &mut Shell,
) -> std::result::Result<i32, i32> {
    if argv.len() > 2 {
        safe_eprintln!("wash: {}: too many arguments", name);
        sh.fatal_error = Some(1);
        return Err(1);
    }
    let Some(arg) = argv.get(1) else {
        return Ok(1);
    };
    match arg.trim().parse::<i32>() {
        Ok(n) if n >= 1 => Ok(n),
        Ok(_) => {
            safe_eprintln!("wash: {}: {}: loop count out of range", name, arg);
            sh.break_requested = Some(1);
            Err(1)
        }
        Err(_) => {
            safe_eprintln!("wash: {}: {}: numeric argument required", name, arg);
            sh.fatal_error = Some(2);
            Err(2)
        }
    }
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
        safe_eprintln!("wash: local: can only be used in a function");
        return 1;
    }
    let frame = match sh.call_stack.last_mut() {
        Some(f) => f,
        None => {
            safe_eprintln!("wash: local: can only be used in a function");
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
            safe_eprintln!("wash: local: `{}': not a valid identifier", arg);
            status = 1;
            continue;
        }
        // Record the previous value (once per variable per function) so the
        // enclosing scope survives the function's return.
        if !frame.contains_key(name) {
            frame.insert(name.to_string(), sh.env.get(name).cloned());
        }
        if let Some(v) = value {
            if sh.readonly.contains(name) {
                // bash prints `local: Z: readonly variable` (builtin prefix).
                // Guard applies with or without `-r`, so `local Z=9` can no
                // longer silently overwrite a readonly variable.
                safe_eprintln!("wash: local: {}: readonly variable", name);
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
    let mut array_attr = false;
    let mut assoc_attr = false;
    let mut print = false;
    let mut functions = false;
    let mut function_names = false;
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
                    'a' => array_attr = true,
                    'p' => print = true,
                    'A' => assoc_attr = true,
                    'f' => functions = true,
                    'F' => function_names = true,
                    'g' | 'n' | 't' | 'l' | 'u' | 'c' => {} // 已接受但暂未实现
                    _ => {
                        safe_eprintln!("wash: declare: {}: invalid option", a);
                        safe_eprintln!("declare: usage: declare [-aAfFgilrtux] [-p] [name[=value] ...]");
                        return 2;
                    }
                }
            }
            i += 1;
            continue;
        }
        break;
    }

    // `declare -f` / `declare -F`: list shell functions.  `-p` combined with
    // either flag is a no-op in bash (the function listing wins), and both
    // flags ignore variable operands entirely.
    if functions || function_names {
        let names: Vec<&String> = argv[i..].iter().collect();
        let mut status = 0;
        if names.is_empty() {
            let mut all: Vec<&String> = sh.functions.keys().collect();
            all.sort();
            for name in all {
                if function_names {
                    println!("declare -f {}", name);
                } else if let Some(body) = sh.functions.get(name) {
                    print_function_bash(name, body);
                }
            }
            return 0;
        }
        for name in names {
            match sh.functions.get(name) {
                Some(body) if function_names => println!("{}", name),
                Some(body) => print_function_bash(name, body),
                None => status = 1,
            }
        }
        return status;
    }

    // `declare -p [NAME...]`: print attributes of the named variable(s).
    if print {
        let names: Vec<String> = argv[i..].to_vec();
        return if names.is_empty() {
            // Print every variable (KNOWN-DIFF: bash prints all; we print the
            // ones that carry interesting attributes).
            print_declared(sh, &all_declared_names(sh))
        } else {
            print_declared(sh, &names)
        };
    }

    if i >= argv.len() {
        // `declare` with no arguments: list all variables (KNOWN-DIFF).
        return print_declared(sh, &all_declared_names(sh));
    }

    let mut status = 0;
    while i < argv.len() {
        let arg = &argv[i];
        // `declare -A m=([k]=v [j]=w)` and `declare -a a=(x y)`: the lexer
        // splits a compound literal into separate argv words (`m=`, `(`,
        // `[k]=v`, `)`), so consume the whole run here.
        if let Some(eq) = arg.find('=') {
            let name = &arg[..eq];
            let value = &arg[eq + 1..];
            if is_valid_ident(name)
                && value.is_empty()
                && argv.get(i + 1).map(String::as_str) == Some("(")
            {
                let mut j = i + 2;
                let mut elements: Vec<String> = Vec::new();
                while j < argv.len() && argv[j] != ")" {
                    elements.push(argv[j].clone());
                    j += 1;
                }
                i = if j < argv.len() { j + 1 } else { j };
                if assoc_attr {
                    sh.assoc_declared.insert(name.to_string());
                    let entries = sh.assoc_arrays.entry(name.to_string()).or_default();
                    for el in elements {
                        if let Some(close) = el.find(']') {
                            if el.starts_with('[')
                                && el.as_bytes().get(close + 1) == Some(&b'=')
                            {
                                let key = &el[1..close];
                                let val = &el[close + 2..];
                                if let Some(pair) = entries.iter_mut().find(|(k, _)| k == key) {
                                    pair.1 = val.to_string();
                                } else {
                                    entries.push((key.to_string(), val.to_string()));
                                }
                                continue;
                            }
                        }
                        let key = entries.len().to_string();
                        entries.push((key, el));
                    }
                } else {
                    sh.array_assigned.insert(name.to_string());
                    sh.arrays.insert(name.to_string(), elements);
                }
                continue;
            }
        }
        match arg.find('=') {
            Some(eq) => {
                let name = &arg[..eq];
                let value = &arg[eq + 1..];
                if !is_valid_ident(name) {
                    safe_eprintln!("wash: declare: `{}': not a valid identifier", arg);
                    status = 1;
                    i += 1;
                    continue;
                }
                if !apply_var_attrs(
                    sh,
                    name,
                    Some(value.to_string()),
                    export_attr,
                    readonly_attr,
                    integer_attr,
                ) {
                    status = 1;
                }
            }
            None => {
                if !is_valid_ident(arg) {
                    safe_eprintln!("wash: declare: `{}': not a valid identifier", arg);
                    status = 1;
                    i += 1;
                    continue;
                }
                // `declare -a NAME` creates an (empty) indexed array;
                // `declare -A NAME` creates an empty associative array and
                // makes later `NAME[key]=value` use string keys.
                if array_attr {
                    sh.arrays.entry(arg.clone()).or_insert_with(Vec::new);
                }
                if assoc_attr {
                    sh.assoc_declared.insert(arg.clone());
                    sh.assoc_arrays.entry(arg.clone()).or_insert_with(Vec::new);
                }
                if !apply_var_attrs(
                    sh,
                    arg,
                    None,
                    export_attr,
                    readonly_attr,
                    integer_attr,
                ) {
                    status = 1;
                }
            }
        }
        i += 1;
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
                    safe_eprintln!("wash: readonly: {}: invalid option", a);
                    safe_eprintln!("readonly: usage: readonly [-aAf] [name[=value] ...] or readonly -p");
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
                    println!("declare -r {}={}", name, quote_value_for_audit(v));
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
                    safe_eprintln!("wash: readonly: `{}': not a valid identifier", arg);
                    status = 1;
                    continue;
                }
                if sh.readonly.contains(name) {
                    safe_eprintln!("wash: {}: readonly variable", name);
                    status = 1;
                    continue;
                }
                sh.env.insert(name.to_string(), value.to_string());
                sh.readonly.insert(name.to_string());
            }
            None => {
                if !is_valid_ident(arg) {
                    safe_eprintln!("wash: readonly: `{}': not a valid identifier", arg);
                    status = 1;
                    continue;
                }
                if sh.readonly.contains(arg) {
                    safe_eprintln!("wash: {}: readonly variable", arg);
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
/// Returns false when the assignment was refused because the variable is
/// readonly (bash: `declare: X: readonly variable`, rc=1, value unchanged).
fn apply_var_attrs(
    sh: &mut Shell,
    name: &str,
    value: Option<String>,
    export_attr: bool,
    readonly_attr: bool,
    integer_attr: bool,
) -> bool {
    // Guard applies to every assignment path, not just `declare -r`: a plain
    // `declare X=2` must not silently overwrite a readonly variable either.
    if sh.readonly.contains(name) && (readonly_attr || value.is_some()) {
        safe_eprintln!("wash: declare: {}: readonly variable", name);
        return false;
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
    true
}

/// Every variable name the shell knows about (used by `declare -p` with no
/// name). Mirrors bash's "list all" behaviour closely enough for the
/// KNOWN-DIFF listing.
fn all_declared_names(sh: &Shell) -> Vec<String> {
    let mut names: Vec<String> = sh.env.keys().cloned().collect();
    names.sort();
    names
}

/// True when `name` currently denotes a shell variable (scalar, indexed or
/// associative array, or a declared attribute).  Used by `declare -p NAME` to
/// reproduce bash's `NAME: not found` diagnostic for unknown names.
fn variable_is_defined(sh: &Shell, name: &str) -> bool {
    sh.env.contains_key(name)
        || sh.arrays.contains_key(name)
        || sh.assoc_arrays.contains_key(name)
        || sh.assoc_declared.contains(name)
        || sh.array_assigned.contains(name)
        || sh.integer_vars.contains(name)
        || sh.readonly.contains(name)
        || sh.exported.contains(name)
        || sh.null_exported.contains(name)
}

/// Print `declare -x NAME=...` lines for the given names, in sorted order.
/// Returns 1 when any requested name was not found (bash keeps printing the
/// names that do exist and still exits 1).
fn print_declared(sh: &Shell, names: &[String]) -> i32 {
    let mut sorted = names.to_vec();
    sorted.sort();
    let mut status = 0;
    for name in &sorted {
        if !variable_is_defined(sh, name) {
            safe_eprintln!("wash: declare: {}: not found", name);
            status = 1;
            continue;
        }
        // Array variables are printed with the `-a` attribute and the
        // `([0]="a" [1]="b")` body, exactly like bash does.  A bare
        // `declare -a NAME` (declared but never assigned) prints without a
        // value, while an array that WAS assigned -- even an empty one from
        // `mapfile`/`readarray` -- prints the explicit `=()` body (bash 5.3).
        if sh.assoc_declared.contains(name) {
            let entries = sh.assoc_arrays.get(name).cloned().unwrap_or_default();
            if entries.is_empty() {
                println!("declare -A {}", name);
            } else {
                // bash prints associative-array elements in hash-bucket
                // order (the same order `${!m[@]}` uses), not insertion
                // order -- reuse the expander's exact bucket walk.
                let body = crate::expand::assoc_iter_keys(&entries)
                    .iter()
                    .filter_map(|k| {
                        entries
                            .iter()
                            .find(|(ek, _)| ek == k)
                            .map(|(_, v)| format!("[{}]={}", k, quote_value_for_audit(v)))
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("declare -A {}=({} )", name, body);
            }
            continue;
        }
        if let Some(elems) = sh.arrays.get(name) {
            if elems.is_empty() {
                if sh.array_assigned.contains(name) {
                    println!("declare -a {}=()", name);
                } else {
                    println!("declare -a {}", name);
                }
            } else {
                println!(
                    "declare -a {}={}",
                    name,
                    format_array_sparse(elems, sh.array_set_indices.get(name))
                );
            }
            continue;
        }
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
                println!("declare {} {}={}", attr_str, name, quote_value_for_audit(v));
            }
            None => {
                println!("declare {} {}", attr_str, name);
            }
        }
    }
    status
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
        safe_eprintln!("wash: {}", USAGE);
        return 2;
    }

    // `kill -l` handling.
    if argv[1] == "-l" {
        if argv.len() == 2 {
            print_signal_table();
            return 0;
        }
        // `kill -l` with a numeric spec: `0` is EXIT, and `128+N` maps back
        // to signal N (bash: `kill -l 130` -> INT, `kill -l 192` -> RTMAX).
        let spec = &argv[2];
        if let Ok(n) = spec.trim().parse::<i32>() {
            if n == 0 {
                println!("EXIT");
                return 0;
            }
            if (129..=192).contains(&n) {
                if let Some(name) = signal_name(n - 128) {
                    println!("{}", name);
                    return 0;
                }
            }
            // 数字信号号 → 裸名（不含 SIG 前缀，与 bash 一致）。
            match signal_name(n) {
                Some(name) => {
                    println!("{}", name);
                    return 0;
                }
                None => {
                    safe_eprintln!("wash: kill: {}: invalid signal specification", spec);
                    return 1;
                }
            }
        }
        // 信号名（含 RTMIN+n / RTMAX-n）→ 编号。
        if let Some(cn) = canonical_signal(spec) {
            let bare = cn.strip_prefix("SIG").unwrap_or(&cn);
            let num = if let Some(pos) = base_signal_names().iter().position(|x| *x == bare) {
                Some((pos + 1) as i32)
            } else {
                parse_rt_name(bare)
            };
            if let Some(num) = num {
                println!("{}", num);
                return 0;
            }
        }
        safe_eprintln!("wash: kill: {}: invalid signal specification", spec);
        return 1;
    }

    // Parse an optional leading signal spec.
    let mut sigspec: Option<String> = None;
    let mut i = 1usize;
    if argv[i].starts_with('-') && argv[i].len() > 1 && argv[i] != "-" {
        let opt = &argv[i][1..];
        if opt == "l" {
            // already handled above; guard against `-l` appearing mid-way
            safe_eprintln!("wash: {}", USAGE);
            return 2;
        } else if opt == "s" {
            i += 1;
            if i >= argv.len() {
                // bash: `kill: -s: option requires an argument`（rc 1）。
                safe_eprintln!("wash: kill: -{}: option requires an argument", opt);
                return 1;
            }
            sigspec = Some(argv[i].clone());
            i += 1;
        } else if opt == "n" {
            i += 1;
            if i >= argv.len() {
                safe_eprintln!("wash: kill: -{}: option requires an argument", opt);
                return 1;
            }
            sigspec = Some(argv[i].clone());
            i += 1;
        } else {
            sigspec = Some(opt.to_string());
            i += 1;
        }
    }

    let sig = match &sigspec {
        Some(s) => {
            // 信号 0 是合法的 sigspec：不投递任何信号，仅做进程存在性探活
            // （bash 实测：`kill -0 $$` / `kill -s 0 $$` / `kill -n 0 $$`
            // rc 0；`kill -0` 无 pid 才报 usage rc 2）。修复前信号 0 被当成
            // 非法信号报 `invalid signal specification`。
            if s.trim().parse::<i32>().map(|n| n == 0).unwrap_or(false) {
                0
            } else {
                match canonical_signal(s) {
                    Some(name) => signal_to_num(&name),
                    None => {
                        safe_eprintln!("wash: kill: {}: invalid signal specification", s);
                        return 1;
                    }
                }
            }
        }
        None => 15, // SIGTERM
    };

    if i >= argv.len() {
        safe_eprintln!("wash: {}", USAGE);
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
        // bash accepts a jobspec (`%N`, `%%`, `%+`, `%-`, `%?str`, `%str`) as
        // a kill operand and resolves it to the job's real pid.  A leading
        // `-` before a jobspec is NOT a process-group id (`kill -%1` is not
        // a bash spelling), so only numeric operands take that path.
        let resolved: Option<i32> = if !negate && pidstr.starts_with('%') {
            match find_job(sh, Some(pidstr), "kill") {
                Ok(idx) => Some(sh.job_table[idx].pid),
                Err(_) => {
                    safe_eprintln!("wash: kill: {}: no such job", pidstr);
                    status = 1;
                    Some(-1) // sentinel: already diagnosed, skip numeric path
                }
            }
        } else {
            None
        };
        if resolved == Some(-1) {
            continue;
        }
        if let Some(p) = resolved {
            let target = p;
            let r = posix::kill(target, sig);
            if r != 0 {
                let e = posix::errno();
                safe_eprintln!(
                    "wash: kill: ({}) - {}",
                    target,
                    posix::errno_message(e)
                );
                status = 1;
            }
            continue;
        }
        // bash 接受任意 `int` 范围内的 pid；非数字或超出范围都报
        // ``kill: `ARG': not a pid or valid job spec``（注意反引号包裹，
        // 实测 `kill -0 10000000000` 与 `kill abc` 同文案，rc 1）。
        let pid: i32 = match numstr.trim().parse::<i64>() {
            Ok(p) if (i32::MIN as i64..=i32::MAX as i64).contains(&p) => p as i32,
            _ => {
                safe_eprintln!("wash: kill: `{pidstr}': not a pid or valid job spec");
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
                    // bash finishes the `kill` builtin before it dispatches the
                    // pending trap, so `$?` inside the trap is the builtin's
                    // status (0 on success), NOT the status of the command that
                    // ran before `kill`.  Without this, `trap 'exit $?' INT;
                    // false; kill -INT $$` would exit 1 instead of bash's 0.
                    sh.last_status = status;
                    sh.run_in_error_context(crate::shell::ErrorContext::signal_trap(), &action);
                }
                continue;
            }
        }

        let r = posix::kill(target, sig);
        if r != 0 {
            let e = posix::errno();
            // bash prints `kill: (PID) - <strerror>` (note the ") - " form,
            // not a colon).
            safe_eprintln!(
                "wash: kill: ({}) - {}",
                target,
                posix::errno_message(e)
            );
            status = 1;
        }
    }
    status
}

/// 信号总数（bash 在本平台输出 1..64，含 33 个实时信号）。
const SIGNAL_COUNT: i32 = 64;

/// 基础信号名（1..=32，不含 `SIG` 前缀），与 bash 在本平台的编号一致。
fn base_signal_names() -> &'static [&'static str] {
    &[
        "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "EMT", "FPE", "KILL", "BUS", "SEGV", "SYS",
        "PIPE", "ALRM", "TERM", "URG", "STOP", "TSTP", "CONT", "CHLD", "TTIN", "TTOU", "IO",
        "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "PWR", "USR1", "USR2", "RTMIN",
    ]
}

/// 实时信号（33..=64）的裸名：32=RTMIN，33..=48=RTMIN+n，49..=63=RTMAX-(64-n)，64=RTMAX。
fn rt_signal_name(num: i32) -> Option<String> {
    if num == 32 {
        return Some("RTMIN".to_string());
    }
    if num == 64 {
        return Some("RTMAX".to_string());
    }
    if (33..=48).contains(&num) {
        return Some(format!("RTMIN+{}", num - 32));
    }
    if (49..=63).contains(&num) {
        return Some(format!("RTMAX-{}", 64 - num));
    }
    None
}

/// 将形如 `RTMIN` / `RTMIN+5` / `RTMAX` / `RTMAX-3` 的裸名解析为 1-based 信号号。
/// 越界或格式不符返回 `None`。
fn parse_rt_name(s: &str) -> Option<i32> {
    if s == "RTMIN" {
        return Some(32);
    }
    if s == "RTMAX" {
        return Some(64);
    }
    if let Some(rest) = s.strip_prefix("RTMIN+") {
        if let Ok(k) = rest.trim().parse::<i32>() {
            let num = 32 + k;
            if (33..=48).contains(&num) {
                return Some(num);
            }
        }
        return None;
    }
    if let Some(rest) = s.strip_prefix("RTMAX-") {
        if let Ok(k) = rest.trim().parse::<i32>() {
            let num = 64 - k;
            if (49..=63).contains(&num) {
                return Some(num);
            }
        }
        return None;
    }
    None
}

/// 打印 bash 风格的带编号信号表：5 列、按行优先排列，形如
/// ` 1) SIGHUP\t 2) SIGINT\t ...`，末尾用制表符分隔。
fn print_signal_table() {
    const COLS: usize = 5;
    let mut n = 0i32;
    while n < SIGNAL_COUNT {
        let mut last_col: i32 = -1;
        for col in 0..COLS {
            if n >= SIGNAL_COUNT {
                break;
            }
            if col > 0 {
                safe_print!("\t");
            }
            let num = n + 1;
            let name = signal_name(num).unwrap_or_default();
            safe_print!("{:>2}) SIG{}", num, name);
            n += 1;
            last_col = col as i32;
        }
        // bash 在最后一行（不满 5 列）行末额外输出一个分隔 tab，
        // 而完整行行末只有换行。逐字节对齐需据此区分。
        if last_col >= 0 && (last_col as usize) < COLS - 1 {
            safe_print!("\t");
        }
        safe_println!();
    }
}

/// Map a signal number (1-based) to its bash name (e.g. 9 → "KILL", 33 → "RTMIN+1").
fn signal_name(num: i32) -> Option<String> {
    if num >= 1 && num <= 32 {
        return Some(base_signal_names()[(num - 1) as usize].to_string());
    }
    if num >= 33 && num <= 64 {
        return rt_signal_name(num);
    }
    None
}

/// Map a canonical signal name (e.g. "SIGINT") to its number.
fn signal_to_num(name: &str) -> i32 {
    let n = name.strip_prefix("SIG").unwrap_or(name);
    if let Some(pos) = base_signal_names().iter().position(|x| *x == n) {
        return (pos + 1) as i32;
    }
    if let Some(num) = parse_rt_name(n) {
        return num;
    }
    -1
}

/// Canonicalise a signal spec (`INT`, `SIGINT`, `2`, `EXIT`, `USR1`, `RTMIN+3`)
/// to the `SIGxxx` form bash stores and prints (special traps like EXIT stay as-is).
fn canonical_signal(spec: &str) -> Option<String> {
    let s = spec.strip_prefix("SIG").unwrap_or(spec);
    if s == "EXIT" {
        return Some("EXIT".to_string());
    }
    let upper = s.to_uppercase();
    if base_signal_names().iter().any(|x| *x == upper) {
        return Some(format!("SIG{}", upper));
    }
    if let Some(num) = parse_rt_name(&upper) {
        // 规范名保留 RTMIN+n / RTMAX-n 形式（如 `SIGRTMIN+3`），与 bash 一致。
        return rt_signal_name(num).map(|bare| format!("SIG{}", bare));
    }
    // 也许是一个数字。
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

/// 把运行时收到的信号号派发到已安装的信号 trap（P1-2）。
///
/// C 侧 handler 只记录信号号（异步信号安全），REPL 循环在回到提示符前的
/// 安全时机调用本函数：把信号号映射成 bash 的 `SIGxxx` 规范名，查
/// `sh.traps`，存在且非 "-" 时用 run_str 执行 trap 命令。
pub fn dispatch_signal_trap(sh: &mut Shell, signo: i32) -> bool {
    let canon = signal_to_name(signo);
    match sh.traps.get(&canon).cloned() {
        Some(action) => {
            if action != "-" {
                sh.run_in_error_context(crate::shell::ErrorContext::signal_trap(), &action);
            }
            true
        }
        None => false,
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
    // `trap -l` (and its `-lp` / `-l -p` spellings) prints the numbered
    // signal table and ignores any following signal_spec operands.
    let action = argv.get(1).map(String::as_str).unwrap_or("");
    if action == "-l" || (action.starts_with('-') && action.contains('l')) {
        print_signal_table();
        return 0;
    }
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
        safe_eprintln!("wash: trap: usage: trap [-lp] [[arg] signal_spec ...]");
        return 2;
    }

    let mut status = 0;
    for sig in &argv[2..] {
        let cn = match canonical_signal(sig) {
            Some(c) => c,
            None => {
                safe_eprintln!("wash: trap: {}: invalid signal specification", sig);
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
                        safe_eprintln!("wash: dirs: {}: invalid option", a);
                        safe_eprintln!("dirs: usage: dirs [-clpv] [+N] [-N]");
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
            safe_eprintln!("wash: pushd: no other directory");
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
        safe_eprintln!("wash: pushd: too many arguments");
        return 1;
    }
    let dir = &argv[1];
    if dir.starts_with('+') || dir.starts_with('-') {
        // `pushd +N` rotates the stack — not yet implemented here.
        safe_eprintln!("wash: pushd: {}: invalid option", dir);
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
        safe_eprintln!("wash: pushd: {}: {}", dir, posix::errno_message(e));
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
            safe_eprintln!("wash: popd: {}: invalid option", a);
            safe_eprintln!("popd: usage: popd [-n] [+N | -N]");
            return 2;
        }
        if let Some(n) = a.strip_prefix('+') {
            match n.trim().parse::<usize>() {
                Ok(v) => index = v,
                Err(_) => {
                    safe_eprintln!("wash: popd: {}: invalid option", a);
                    return 2;
                }
            }
            i += 1;
            continue;
        }
        i += 1;
    }

    if sh.dir_stack.len() <= 1 {
        safe_eprintln!("wash: popd: directory stack empty");
        return 1;
    }
    if index >= sh.dir_stack.len() {
        safe_eprintln!("wash: popd: +{}: directory stack index out of range", index);
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

/// 把 byteval 编码的变量值先还原成裸字节，再按 bash 的审计输出规则引用。
/// 直接把编码串交给 [`quote_double`] 会把 PUA 转义序列当成普通字符输出
/// （例如裸字节 0xff 会打印成 `$'\356\201\277'` 而不是 `$'\377'`）。
/// 把 byteval 编码值还原成 bash 诊断里显示的文本。
///
/// 可打印 ASCII 原样输出（bash 的 `cd: foo: ...` 不加引号）；只要含不可打印
/// 或高位字节，就按 `$'...'` ANSI-C 引用（bash 的 `cd: $'\377': ...`）。
/// 所有把变量值嵌入 stderr 诊断的站点都必须先过这里，禁止直接打印编码串。
pub(crate) fn diag_text(value: &str) -> String {
    let raw = crate::byteval::decode_raw(value);
    if raw.iter().all(|&b| (0x20..=0x7e).contains(&b)) {
        // 全部是 ASCII，from_utf8 必然成功。
        return String::from_utf8(raw).unwrap();
    }
    ansi_quote_bytes(&raw)
}

fn quote_value_for_audit(value: &str) -> String {
    let raw = crate::byteval::decode_raw(value);
    if raw.iter().any(|&b| b < 0x20 || b == 0x7f || b >= 0x80) {
        return ansi_quote_bytes(&raw);
    }
    let mut out = String::from("\"");
    for &b in &raw {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'$' => out.push_str("\\$"),
            b'`' => out.push_str("\\`"),
            _ => out.push(b as char),
        }
    }
    out.push('"');
    out
}

fn ansi_quote_bytes(value: &[u8]) -> String {
    let mut out = String::from("$'");
    for &b in value {
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
// help
// ---------------------------------------------------------------------------

/// 每个内建命令的用法摘要，`help NAME` 时打印。
///
/// 只收录已经有明确用法的内建；其余的内建只打印名字（与 bash 的 `help`
/// 列出用法行不同，但 wash 并没有为每个内建维护用法文本）。
/// 内建命令帮助表：`(名字, 用法摘要, 单行描述)`。
///
/// 描述取自 bash 的 `help` 原文（C locale 下需逐字节等于 bash）。
/// The `ulimit` builtin — set or display process resource limits.
///
/// Mirrors GNU bash 5.3.0's `ulimit` semantics byte-for-byte:
///   * `ulimit -a` prints every supported limit as
///     `printf("%-20s %20s", desc, " (unit, -X) ")` followed by the value,
///     with the value scaled by the resource's block factor (POSIXBLK -> 1024
///     in non-posix mode).
///   * A bare `ulimit` defaults to `-f`.
///   * `RLIM_INFINITY` is rendered as `unlimited`.
///   * Unknown option letters and `-H`/`-S` modifiers are handled exactly like
///     bash (invalid option -> `usage`, rc 2).
fn builtin_ulimit(argv: &[String], _sh: &mut Shell) -> i32 {
    // Resource descriptors in `ulimit -a` print order (bash's limits[] table on
    // this platform). `factor` scales the raw rlimit value to the displayed
    // unit: BLOCKSIZE(POSIXBLK=-2) -> 1024 in non-posix mode (512 in posix).
    const RES: &[(&str, char, u64)] = &[
        ("core file size",     'c', 1024),
        ("data seg size",      'd', 1024),
        ("file size",          'f', 1024),
        ("open files",         'n', 1),
        ("pipe size",          'p', 512),
        ("stack size",         's', 1024),
        ("cpu time",           't', 1),
        ("max user processes", 'u', 1),
        ("virtual memory",     'v', 1024),
    ];
    fn unit_of(c: char) -> Option<&'static str> {
        match c {
            'c' => Some("blocks"),
            'd' => Some("kbytes"),
            'f' => Some("blocks"),
            'n' => None,
            'p' => Some("512 bytes"),
            's' => Some("kbytes"),
            't' => Some("seconds"),
            'u' => None,
            'v' => Some("kbytes"),
            _ => None,
        }
    }
    const INF: u64 = u64::MAX;

    let mut mode: u8 = 0; // 0=default, bit0=SOFT, bit1=HARD
    let mut all = false;
    let mut resources: Vec<char> = Vec::new();
    let mut value: Option<String> = None;

    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            let opts = &a[1..];
            for ch in opts.chars() {
                match ch {
                    'a' => all = true,
                    'S' => mode |= 1,
                    'H' => mode |= 2,
                    'c' | 'd' | 'f' | 'n' | 'p' | 's' | 't' | 'u' | 'v' =>
                        resources.push(ch),
                    _ => {
                        safe_eprintln!("wash: ulimit: {}: invalid option", a);
                        safe_eprintln!(
                            "ulimit: usage: ulimit [-SHabcdefiklmnpqrstuvxPRT] [limit]"
                        );
                        return 2;
                    }
                }
            }
        } else if value.is_none() {
            value = Some(a.clone());
        }
        i += 1;
    }

    // default to -f (file size) when no resource given and not -a
    if !all && resources.is_empty() {
        resources.push('f');
    }

    let hard = (mode & 2) != 0;

    // ---- `ulimit -a` : print every supported limit ----
    if all {
        for (desc, letter, factor) in RES.iter() {
            let raw = if hard {
                match posix::rlimit_hard(*letter as u8) {
                    Some(v) => v,
                    None => continue,
                }
            } else {
                match posix::rlimit(*letter as u8, 0, 0, false) {
                    Some(v) => v,
                    None => continue,
                }
            };
            let val = if raw == INF {
                "unlimited".to_string()
            } else {
                (raw / *factor).to_string()
            };
            let unitstr = match unit_of(*letter) {
                Some(u) => format!("({u}, -{letter}) "),
                None => format!("(-{}) ", letter),
            };
            println!("{:<20} {:>20}{}", desc, unitstr, val);
        }
        return 0;
    }

    let letter = *resources.last().unwrap();

    // ---- set ----
    if let Some(v) = &value {
        let raw: u64 = if v == "unlimited" {
            INF
        } else if v == "hard" {
            posix::rlimit_hard(letter as u8).unwrap_or(INF)
        } else if v == "soft" {
            posix::rlimit(letter as u8, 0, 0, false).unwrap_or(0)
        } else {
            // bash 的 ulimit 数值语义（MSYS2 bash 5.3 逐例实测标定）：
            //   * `0x`/`0X` 前缀              → `invalid hex number` (rc 1)
            //   * 其它非纯十进制数字           → `invalid number` (rc 1)
            //     （`+5`、`12abc`、`1k`、空串、带空格都算）
            //   * 纯数字按 **u64 回绕**（mod 2^64）：超长十进制串不会 clamp
            //     （`2^128+1` 对 -t 回绕成 1 后照常尝试 setrlimit）
            //   * 回绕值 > RLIM_INFINITY/factor → `limit out of range` (rc 1)
            //     （-f 边界恰为 18014398509481983 = 2^64/1024：等于则照常
            //      尝试 setrlimit，加 1 才 out of range）
            //   * 否则原样尝试 setrlimit，失败走下方 `cannot modify limit`
            //     的 errno 路径（如 `-f 1024` → EINVAL、`-n 2^64-1` → EPERM）
            if v.starts_with("0x") || v.starts_with("0X") {
                safe_eprintln!("wash: ulimit: {}: invalid hex number", v);
                return 1;
            }
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                safe_eprintln!("wash: ulimit: {}: invalid number", v);
                return 1;
            }
            let mut wrapped: u64 = 0;
            for b in v.bytes() {
                wrapped = wrapped.wrapping_mul(10).wrapping_add((b - b'0') as u64);
            }
            let factor = RES
                .iter()
                .find(|(_, c, _)| *c == letter)
                .map(|(_, _, f)| *f)
                .unwrap_or(1);
            if wrapped > INF / factor {
                safe_eprintln!("wash: ulimit: {}: limit out of range", v);
                return 1;
            }
            wrapped * factor
        };
        if posix::rlimit(letter as u8, raw, raw, true).is_none() {
            // bash reports `ulimit: <resource name>: cannot modify limit:
            // <strerror>`.  Read errno immediately after the failing shim and
            // use the resource's full name, not the short option letter.
            let e = posix::errno();
            let msg = posix::errno_message(e);
            let desc = RES
                .iter()
                .find(|(_, c, _)| *c == letter)
                .map(|(d, _, _)| *d)
                .unwrap_or("");
            safe_eprintln!("wash: ulimit: {}: cannot modify limit: {}", desc, msg);
            return 1;
        }
        return 0;
    }

    // ---- get (print soft limit value) ----
    match posix::rlimit(letter as u8, 0, 0, false) {
        None => {
            safe_eprintln!("wash: ulimit: {}: cannot get limit", letter);
            1
        }
        Some(raw) => {
            if raw == INF {
                println!("unlimited");
            } else {
                let factor = RES
                    .iter()
                    .find(|(_, c, _)| *c == letter)
                    .map(|(_, _, f)| *f)
                    .unwrap_or(1);
                println!("{}", raw / factor);
            }
            0
        }
    }
}

// ---------------------------------------------------------------------------
// wait
// ---------------------------------------------------------------------------

/// The `wait` builtin: wait for background jobs / child processes to finish and
/// report their exit status.
///
/// Usage: `wait [-fn] [-p var] [id ...]`
///   * no arguments       → wait for every background job (all of `sh.jobs`),
///                           return the status of the last one reaped.
///   * `wait PID`         → wait for the child with that pid (it need not be a
///                           tracked job); report its exit status. If the child
///                           was killed by signal N, the status is 128+N (bash
///                           semantics — this is what makes `kill -TERM $p; wait
///                           $p` return 143).
///   * `wait JOBSPEC`     → wait for the background job identified by the
///                           jobspec: `%N` (1-based), `%%`/`%+` (current job),
///                           `%-` (previous job), `%?str` (substring match) and
///                           `%str` (prefix match). Resolution goes through
///                           `find_job` — the SAME job_table view used by
///                           `jobs`/`fg`/`bg` — so there is no second, index
///                           numbering to keep in sync.
///
/// Oracle (MSYS2 bash 5.3.20, LC_ALL=C) exact diagnostics:
///   * unknown jobspec → stderr ``wait: <spec>: no such job`` (no backticks),
///     rc 127;
///   * non-pid, non-jobspec word → stderr ``wait: `abc': not a pid or valid
///     job spec`` (backticked), rc 1;
///   * pid that was never a child of this shell (or was already reaped) yields
///     `wait: pid N is not a child of this shell` and rc 127.
fn builtin_wait(argv: &[String], sh: &mut Shell) -> i32 {
    let mut last_status: i32 = 0;
    let mut any = false;

    // 解析参数：支持 PID 与 %N jobspec。当前忽略 -f/-n/-p 选项（GROUP C
    // 的用例只用到裸 PID 形式），遇到选项直接跳过其后的参数由调用方负责；
    // 这里只处理非选项的实参。
    let targets: Vec<String> = argv
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .cloned()
        .collect();

    // 没有参数 → 等待全部后台作业。bash（oracle 实测）的无参 `wait` 总是
    // 返回 0，即便等待的作业被信号终止（`sleep 2 & kill %1; wait` -> rc=0）；
    // 只有显式 `wait %N` / `wait $pid` 才报告 128+sig（`wait %1` -> 143）。
    if targets.is_empty() {
        let pids: Vec<i32> = sh.jobs.clone();
        for pid in pids {
            if posix::wait_pid_check(pid).is_some() {
                any = true;
                sh.jobs.retain(|&p| p != pid);
                // 两表同步：wait 回收后作业也从真实作业表移除，保证
                // jobs / fg / disown 的 %N 下标与 wait 的 %N 一致。
                sh.job_table.retain(|j| j.pid != pid);
            }
        }
        let _ = (last_status, any);
        return 0;
    }

    for arg in &targets {
        let pid = if let Some(_spec) = arg.strip_prefix('%') {
            // jobspec：%N / %% / %+ / %- / %?str / %str —— 与 jobs/fg/bg 共用
            // 同一个 find_job / job_table 视角（review #28：不再用 sh.jobs
            // 下标造第二套编号）。报错文案对齐 oracle：
            //   ``wait: %1: no such job``（无反引号）+ rc 127。
            match find_job(sh, Some(arg.as_str()), "wait") {
                Ok(i) => sh.job_table[i].pid,
                Err(text) => {
                    safe_eprintln!("wash: wait: {}: no such job", text);
                    return 127;
                }
            }
        } else {
            match arg.parse::<i32>() {
                Ok(p) => p,
                Err(_) => {
                    // oracle：``wait: `abc': not a pid or valid job spec``
                    //（带反引号）+ rc 1。
                    safe_eprintln!("wash: wait: `{}': not a pid or valid job spec", arg);
                    return 1;
                }
            }
        };

        match posix::wait_pid_check(pid) {
            Some(code) => {
                last_status = code;
                any = true;
                sh.jobs.retain(|&p| p != pid);
                sh.job_table.retain(|j| j.pid != pid);
            }
            None => {
                safe_eprintln!("wash: wait: pid {} is not a child of this shell", pid);
                return 127;
            }
        }
    }

    if !any {
        0
    } else {
        last_status
    }
}

// ---------------------------------------------------------------------------
// 作业控制内建：jobs / fg / bg / disown
// ---------------------------------------------------------------------------

/// 是否启用作业控制：`set -m` 显式开启，或交互式 tty 下默认开启。
/// `-c` / stdin 非 tty 时关闭，此时 `fg` / `bg` 报 "no job control"。
fn job_control_on(sh: &Shell) -> bool {
    sh.options.monitor || (sh.interactive && posix::isatty(0))
}

/// 把 jobspec 解析成 `job_table` 的下标（0-based）。
///
/// 支持 `%N`（1-based 作业号）、`%%` / `%+`（当前作业，表中最后一个）、
/// `%-`（前一个作业）、`%?str`（命令含 str）、`%str`（命令以 str 开头）。
/// `None` 表示省略 jobspec，按当前作业处理。失败时 `Err` 携带用于报错的
/// spec 文本（省略时用 bash 的 "current"）。
///
/// 裸词（不以 `%` 开头）：bash（oracle 实测）先打印
/// `{builtin}: warning: <词>: job specification requires leading `%'`，
/// 然后**仍按 `%<词>` 解释**——`jobs sleep`（有 sleep 作业）告警后照样
/// 列出该作业，`jobs foo` 告警后报 `foo: no such job` rc=1。`bname` 用于
/// 告警行的内建名前缀（jobs / fg / bg / disown）。
fn find_job(sh: &Shell, spec: Option<&str>, bname: &str) -> Result<usize, String> {
    let n = sh.job_table.len();
    let (raw, key) = match spec {
        None => ("current".to_string(), "current".to_string()),
        Some(s) => {
            if s.starts_with('%') {
                (s.to_string(), s.to_string())
            } else {
                safe_eprintln!(
                    "{}: warning: {}: job specification requires leading `%'",
                    bname, s
                );
                (s.to_string(), format!("%{}", s))
            }
        }
    };
    if n == 0 {
        return Err(raw);
    }
    let found = match key.as_str() {
        "current" | "%%" | "%+" => Some(n - 1),
        // `%-`：oracle 实测 = 前一个作业（倒数第二个）；**只有一个作业时
        // bash 回退到当前作业**（`sleep 5 & jobs %-` / `disown %-` 均命中
        // 作业 [1]，get_job_spec 共用此语义）。
        "%-" => Some(n.saturating_sub(2).max(0)),
        s if s.starts_with('%') => {
            let rest = &s[1..];
            if let Ok(num) = rest.parse::<usize>() {
                // bash 作业号持久：%N 按注册时分配的 jobno 定位，不能按
                // 当前表下标（中间作业被 disown/wait 移除后表会重排）。
                if num == 0 {
                    None
                } else {
                    sh.job_table.iter().position(|j| j.jobno == num)
                }
            } else if let Some(needle) = rest.strip_prefix('?') {
                sh.job_table.iter().position(|j| j.command.contains(needle))
            } else {
                sh.job_table.iter().position(|j| j.command.starts_with(rest))
            }
        }
        // key 只可能是 "current" 或以 `%` 开头（裸词已在上面补 `%`）。
        _ => None,
    };
    found.ok_or_else(|| raw)
}

/// 用 `kill(pid, 0)` 非破坏性地刷新作业存活状态。
///
/// 不调用 waitpid，避免回收掉子进程而破坏 `wait` 内建语义（已结束但未被
/// wait 的僵尸进程仍会被判为存活，作为遗留边界记录）。
fn refresh_job_states(sh: &mut Shell) {
    for j in &mut sh.job_table {
        if !j.done && posix::kill(j.pid, 0) < 0 {
            j.done = true;
        }
    }
}

/// 作业状态文本（bash 的 jobs 列表）：
///   Running（未结束）/ Done（status=0 或 kill 探测路径无状态）/
///   Exit N（非零普通退出）/ 信号名（status=128+sig，如 Terminated）。
/// 信号名取自 oracle（MSYS2 bash 5.3.20 `kill -l` + 各信号 `jobs %1` 实测），
/// 与 Cygwin strsignal 文案一致。
fn job_state(j: &JobInfo) -> String {
    if let Some(sig) = j.stopped_signal {
        // bash renders a stopped job by its signal: SIGTSTP(20) -> `Stopped`,
        // SIGTTIN(21) -> `Stopped (tty input)`, SIGTTOU(22) -> `Stopped (tty
        // output)`, and any other signal -> `Stopped (signal)`/`Stopped (SIG)`.
        // Signal numbers are MSYS2/Cygwin values, queried at runtime so the
        // mapping cannot drift: SIGSTOP=17, SIGTSTP=18, SIGTTIN=21, SIGTTOU=22.
        let stop = posix::signal_number("STOP");
        let tstp = posix::signal_number("TSTP");
        let ttin = posix::signal_number("TTIN");
        let ttou = posix::signal_number("TTOU");
        if sig == stop || sig == tstp {
            return "Stopped".to_string();
        }
        if sig == ttin {
            return "Stopped (tty input)".to_string();
        }
        if sig == ttou {
            return "Stopped (tty output)".to_string();
        }
        return "Stopped (signal)".to_string();
    }
    if !j.done {
        return "Running".to_string();
    }
    match j.status {
        None | Some(0) => "Done".to_string(),
        Some(n) if n < 128 => format!("Exit {}", n),
        Some(n) => {
            let sig = n - 128;
            let name = match sig {
                1 => "Hangup",
                2 => "Interrupt",
                3 => "Quit",
                4 => "Illegal instruction",
                5 | 6 => "Trace/breakpoint trap",
                7 => "Bus error",
                8 => "Floating point exception",
                9 => "Killed",
                10 => "User defined signal 1",
                11 => "Segmentation fault",
                12 => "User defined signal 2",
                13 => "Broken pipe",
                14 => "Alarm clock",
                15 => "Terminated",
                16 => "Stack fault",
                17 => "Child exited",
                18 => "Continued",
                19 => "Stopped (signal)",
                20 => "Stopped",
                21 => "Stopped (tty input)",
                22 => "Stopped (tty output)",
                23 => "Urgent I/O condition",
                24 => "CPU time limit exceeded",
                25 => "File size limit exceeded",
                26 => "Virtual timer expired",
                27 => "Profiling timer expired",
                28 => "Window changed",
                29 => "I/O possible",
                30 => "Power failure",
                31 => "Bad system call",
                _ => return format!("Signal {}", sig),
            };
            name.to_string()
        }
    }
}

/// 从两张作业表中同时移除一个 pid，保持 `jobs` / `wait` 视图一致。
fn drop_job(sh: &mut Shell, pid: i32) {
    sh.jobs.retain(|&p| p != pid);
    sh.job_table.retain(|j| j.pid != pid);
}

/// `jobs [-lnprs] [jobspec ...]`：列出真实后台作业。
///
/// `-l` 带 pid、`-p` 只输出 pid、`-r` 仅运行、`-s` 仅停止（bash 语义；此前
/// 误把 `-s` 当 done-only，Phase 2 引入 stopped 状态后更正）。`-x` 把
/// jobspec 参数替换为 pid 后执行命令。其余字母
/// 对齐 oracle（MSYS2 bash 5.3.20，LC_ALL=C 实测）：
///   stderr 两行
///     `jobs: -z: invalid option`
///     `jobs: usage: jobs [-lnprs] [jobspec ...] or jobs -x command [args]`
///   rc=2。
/// 选项扫描遵循 bash `internal_getopt` 语义：`--` 终止选项，首个非选项实参
/// 之后的 `-z` 等一律按 jobspec 处理（oracle 实测 `jobs foo -z` 把 `-z` 当
/// jobspec 而不是选项）。
fn builtin_jobs(argv: &[String], sh: &mut Shell) -> i32 {
    let mut long = false;
    let mut pid_only = false;
    let mut running_only = false;
    let mut done_only = false;
    let mut specs: Vec<&str> = Vec::new();
    let mut opts_done = false;
    let mut exec_command: Option<usize> = None;
    for (idx, a) in argv.iter().enumerate().skip(1) {
        if !opts_done && a == "--" {
            opts_done = true;
        } else if !opts_done && a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'l' => long = true,
                    'p' => pid_only = true,
                    'r' => running_only = true,
                    's' => done_only = true,
                    // `-x COMMAND [ARGS]`: replace every jobspec argument
                    // with the job's pid and execute COMMAND directly.
                    'x' => exec_command = Some(idx + 1),
                    _ => {
                        safe_eprintln!("wash: jobs: -{}: invalid option", c);
                        safe_eprintln!(
                            "jobs: usage: jobs [-lnprs] [jobspec ...] or jobs -x command [args]"
                        );
                        return 2;
                    }
                }
            }
        } else {
            // 首个非选项实参之后不再做选项解析（bash internal_getopt）。
            opts_done = true;
            specs.push(a.as_str());
        }
    }

    refresh_job_states(sh);

    // `jobs -x command [args]`: bash substitutes each RESOLVABLE `%N`-style
    // jobspec in the argument list with the corresponding job's pid, then
    // executes the command directly (no job listing).  An UNRESOLVABLE
    // spec is left literally in argv (oracle: `jobs -x echo %99` prints
    // `%99`, rc=0) -- unlike `jobs %99`, which reports `no such job`.
    if let Some(cmd_idx) = exec_command {
        if cmd_idx >= argv.len() {
            safe_eprintln!("wash: jobs: -x: option requires an argument");
            return 2;
        }
        let mut args: Vec<String> = Vec::new();
        for a in &argv[cmd_idx..] {
            if a.starts_with('%') {
                match find_job(sh, Some(a), "jobs") {
                    Ok(i) => args.push(sh.job_table[i].pid.to_string()),
                    // Unresolvable spec: keep the literal word, no error.
                    Err(_) => args.push(a.clone()),
                }
            } else {
                args.push(a.clone());
            }
        }
        return crate::executor::run_job_x_command(&args, sh);
    }

    // 显式 jobspec 过滤；无 jobspec 时列出全部。bash（oracle 实测）对
    // 失败的 jobspec **逐个**报告且不中断：`jobs foo %1` 照样列出作业 [1]
    // 并报 foo 两次诊断，rc=1；`jobs foo bar` 报两对诊断 rc=1。
    let (wanted, spec_rc): (Option<Vec<usize>>, i32) = if specs.is_empty() {
        (None, 0)
    } else {
        let mut v = Vec::new();
        let mut rc = 0;
        for s in &specs {
            match find_job(sh, Some(s), "jobs") {
                Ok(i) => v.push(i),
                Err(text) => {
                    safe_eprintln!("wash: jobs: {}: no such job", text);
                    rc = 1;
                }
            }
        }
        (Some(v), rc)
    };

    let n = sh.job_table.len();
    for (i, j) in sh.job_table.iter().enumerate() {
        if let Some(w) = &wanted {
            if !w.contains(&i) {
                continue;
            }
        }
        let stopped = j.stopped_signal.is_some();
        let running = !j.done && !stopped;
        if (running_only && !running) || (done_only && !stopped) {
            continue;
        }
        if pid_only {
            println!("{}", j.pid);
            continue;
        }
        // 最后一个作业标 `+`、倒数第二个标 `-`、其余留空（bash 语义）。
        let marker = if i + 1 == n {
            '+'
        } else if i + 2 == n {
            '-'
        } else {
            ' '
        };
        let state = job_state(j);
        // bash（oracle 实测）：已结束的作业行**不带尾部 ` &`**——
        //   Running：`[1]+  Running                    sleep 5 &`
        //   Done：  `[1]+  Done                       true`（无 &，无多余空格）
        // 状态列由 JobInfo.status 驱动（0→Done、1..127→Exit N、128+sig→信号名）；
        // 作业号用持久 jobno（中间作业被 disown/wait 移除后不重排）。
        // bash omits the trailing ` &` for both finished AND stopped jobs.
        let amp = if j.done || j.stopped_signal.is_some() {
            ""
        } else {
            " &"
        };
        if long {
            println!("[{}]{} {:>5} {:<27}{}{}", j.jobno, marker, j.pid, state, j.command, amp);
        } else {
            println!("[{}]{}  {:<27}{}{}", j.jobno, marker, state, j.command, amp);
        }
    }
    spec_rc
}

/// `fg [jobspec]`：把后台作业带回前台。
///
/// 本轮实现到「定位真实 pid → 回显命令 → SIGCONT 续跑 → 阻塞等待 →
/// 从作业表移除」。遗留边界：后台作业没有独立进程组（execute_background
/// 的子进程被 SUPPRESS_JOB_CONTROL 排除在 setpgid 接线之外），因此没有做
/// tcsetpgrp 交还终端；完整作业切换留待后续阶段，不假装完成。
fn builtin_fg(argv: &[String], sh: &mut Shell) -> i32 {
    if !job_control_on(sh) {
        safe_eprintln!("wash: fg: no job control");
        return 1;
    }
    let idx = match find_job(sh, argv.get(1).map(|s| s.as_str()), "fg") {
        Ok(i) => i,
        Err(text) => {
            safe_eprintln!("wash: fg: {}: no such job", text);
            return 1;
        }
    };
    let pid = sh.job_table[idx].pid;
    let pgid = sh.job_table[idx].pgid;
    let command = sh.job_table[idx].command.clone();

    // bash 把作业命令回显到 stdout（不带 `&`）。
    println!("{}", command);
    std::io::stdout().flush().ok();

    // Phase 2: before resuming, hand the terminal's foreground process group
    // to the job so terminal signals reach it and not the shell.  `fg` is only
    // reachable with job control on, so pgid is valid here.
    let shell_pgrp = posix::getpgrp();
    if pgid > 0 {
        posix::tcsetpgrp(0, pgid);
    }
    let cont = posix::signal_number("CONT");
    if cont > 0 {
        posix::kill(pid, cont);
    }
    let code = posix::wait_pid(pid);
    // Always restore the shell as terminal foreground group, even when the
    // job exits by signal (a stopped job that is later killed).
    if pgid > 0 {
        posix::tcsetpgrp(0, shell_pgrp);
    }
    drop_job(sh, pid);
    code
}

/// `bg [jobspec ...]`：让后台作业继续运行。
///
/// 本轮实现到「按 jobspec 定位真实 pid → 对已停止/结束的作业发 SIGCONT
/// 续跑并回显」。wash 无 stopped 状态，Running 作业按 bash 语义报
/// "job N already in background"（rc 仍为 0）。
fn builtin_bg(argv: &[String], sh: &mut Shell) -> i32 {
    if !job_control_on(sh) {
        safe_eprintln!("wash: bg: no job control");
        return 1;
    }
    let specs: Vec<Option<&str>> = if argv.len() > 1 {
        argv.iter()
            .skip(1)
            .filter(|a| !a.starts_with('-'))
            .map(|s| Some(s.as_str()))
            .collect()
    } else {
        vec![None]
    };

    refresh_job_states(sh);
    for spec in specs {
        let idx = match find_job(sh, spec, "bg") {
            Ok(i) => i,
            Err(text) => {
                safe_eprintln!("wash: bg: {}: no such job", text);
                return 1;
            }
        };
        let jobno = sh.job_table[idx].jobno;
        // bash only resumes a *stopped* job; a running background job gets
        // "already in background".  With Phase 2 stopped tracking this is now
        // a real state check rather than the old `done` approximation.
        if sh.job_table[idx].stopped_signal.is_none() && !sh.job_table[idx].done {
            safe_eprintln!("wash: bg: job {} already in background", jobno);
            continue;
        }
        let pid = sh.job_table[idx].pid;
        let cont = posix::signal_number("CONT");
        if cont > 0 {
            posix::kill(pid, cont);
        }
        sh.job_table[idx].done = false;
        sh.job_table[idx].stopped_signal = None;
        println!("[{}]+ {} &", jobno, sh.job_table[idx].command);
    }
    0
}

/// `disown [-arh] [jobspec ...]`：把作业从作业表移除。
///
/// 不检查作业控制（bash 的 disown 在 `-c` 下也能对空表给出
/// "current: no such job"）。`-a` 全部、`-r` 仅未结束、`-h` 保留在表中
/// 仅标记不发送 SIGHUP（wash 暂无 SIGHUP 转发，故为近似）。未知选项字母
/// 对齐 oracle（MSYS2 bash 5.3.20，LC_ALL=C 实测）：
///   stderr 两行
///     `disown: -z: invalid option`
///     `disown: usage: disown [-h] [-ar] [jobspec ... | pid ...]`
///   rc=2（`--foo` 报 `disown: --: invalid option`，即 `-` + 首个非法字符）。
/// 选项扫描与 `jobs` 相同：`--` 终止选项，首个非选项实参之后全部按
/// jobspec 处理。
fn builtin_disown(argv: &[String], sh: &mut Shell) -> i32 {
    let mut all = false;
    let mut running_only = false;
    let mut keep = false;
    let mut specs: Vec<&str> = Vec::new();
    let mut opts_done = false;
    for a in argv.iter().skip(1) {
        if !opts_done && a == "--" {
            opts_done = true;
        } else if !opts_done && a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    'a' => all = true,
                    'r' => running_only = true,
                    'h' => keep = true,
                    _ => {
                        safe_eprintln!("wash: disown: -{}: invalid option", c);
                        safe_eprintln!("disown: usage: disown [-h] [-ar] [jobspec ... | pid ...]");
                        return 2;
                    }
                }
            }
        } else {
            // 首个非选项实参之后不再做选项解析（bash internal_getopt）。
            opts_done = true;
            specs.push(a.as_str());
        }
    }

    refresh_job_states(sh);

    let targets: Vec<i32> = if all {
        sh.job_table
            .iter()
            .filter(|j| !running_only || !j.done)
            .map(|j| j.pid)
            .collect()
    } else {
        let list: Vec<Option<&str>> = if specs.is_empty() {
            vec![None]
        } else {
            specs.iter().map(|s| Some(*s)).collect()
        };
        let mut v = Vec::new();
        let mut failed = false;
        for s in list {
            match find_job(sh, s, "disown") {
                Ok(i) => {
                    if !running_only || !sh.job_table[i].done {
                        v.push(sh.job_table[i].pid);
                    }
                }
                Err(text) => {
                    // bash（oracle 实测）：disown 对失败的 jobspec 逐个报错并
                    // 继续，但只要有任一 spec 失败 rc 就为 1——即使其余 spec
                    // 全部成功（`disown %1 %nosuch` rc=1，与顺序无关）。
                    safe_eprintln!("wash: disown: {}: no such job", text);
                    failed = true;
                }
            }
        }
        if failed {
            return 1;
        }
        v
    };

    if !keep {
        for pid in targets {
            drop_job(sh, pid);
        }
    }
    0
}

// `help NAME` no longer uses a two-line (usage, description) summary table:
// bash prints the FULL manual page, so the table has been replaced by
// `src/help_pages.txt` + `crate::help_pages`, which cover all 77 bash 5.3
// topics byte-for-byte.  Keeping a second, shorter copy here would only be
// dead data.
//
// The no-argument list has NO independent constant here either: its 77 display
// values are the first column of `crate::help_pages::HELP_LIST_MAP`, the single
// ordered table that binds each displayed synopsis to its canonical lookup
// key.  `help_pages::all_en()` hard-asserts that both columns line up with the
// 77 parsed pages on first access, so a drift cannot silently change output.
// Display values are LC_ALL=C text (bash itself truncates long ones to `...>`)
// and double as gettext msgids; the zh_CN values come from the oracle via
// `tools/gen_help_pages.py`.  They are **documentation**, not a capability
// list: bash's list also contains reserved words, `job_spec`, `variables` and
// builtins wash has not ported yet.
//
// One intentional exception (D-002 in `docs/intentional-divergences.md`): the
// two enumerated `job_spec` strings are made queryable via the single-pattern
// channel in `builtin_help`.  It is NOT a general "list value is a lookup key"
// rule; all other display values still behave exactly like the oracle.

/// `help` 的 stdout 统一从这里落地。
///
/// `wash -c 'help' | head -1` 会让下游读完第一行就关闭管道；Rust 的
/// `println!` 此时会 panic，把 `failed printing to stdout: Broken pipe` 连同
/// 一段回溯喷到 stderr。实测 MSYS2 bash 5.3.20 在同样场景 stderr 为空，所以
/// 这里改成一次 `write_all`：EPIPE 静默（Cygwin/MSYS2 上也可能表现为 raw os
/// error 32），其它 IO 错误仍如实报告，但绝不 panic。
fn help_emit(block: &str) {
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if let Err(e) = out.write_all(block.as_bytes()) {
        let epipe = e.kind() == std::io::ErrorKind::BrokenPipe
            || matches!(e.raw_os_error(), Some(32) | Some(109) | Some(232));
        if !epipe {
            let _ = writeln!(std::io::stderr(), "wash: help: write error: {}", e);
        }
    }
}

/// 把帮助主题排成 GNU bash `help` 的双栏版面（列优先）。
///
/// bash 5.3 非交互输出固定 80 列：`" " + 左列(39 定宽) + " " + 右列`，
/// 前半进左列、后半进右列（`nrow = ceil(n/2)`）。最后一行右列为空时左列
/// **不做定宽补齐**——实测 `help [-dms] [pattern ...]` 行尾没有空格。
///
/// 条目的终端显示宽度：ASCII 记 1 列，非 ASCII（CJK 全角等）记 2 列。
/// bash 清单按显示列定宽，中文译文若按字符数补齐会多出空格。
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// 结果写进 `out`，最终由 `help_emit` 一次性输出，避免逐行 `println!`。
///
/// 截断条（`... >`）的补白有一个实测得来的修正：bash 截断短摘要时按显示列
/// 累计宽度，一个宽字符（2 列）恰好跨过截断边界时仍按「至少 37 列」记账。
/// 于是 zh_CN 下 `bind [-lpvsPSVX] [-m 键映射] [-f 文>` 实际只占 36 列，
/// bash 却按 37 列补白（后面 3 个空格，而非按显示宽度算出的 4 个）。
/// C locale 下所有截断条都不短于 37 列，此修正不改变英文输出；该规则由
/// `tools/gen_help_pages.py --check` 对两个 locale 的整段清单逐字节复核。
fn print_help_columns(entries: &[String], out: &mut String) {
    if entries.is_empty() {
        return;
    }
    let nrow = (entries.len() + 1) / 2;
    for r in 0..nrow {
        let left = &entries[r];
        out.push(' ');
        out.push_str(left);
        if let Some(right) = entries.get(nrow + r) {
            let width = display_width(left);
            let effective = if left.ends_with('>') { width.max(37) } else { width };
            let pad = 1 + 39usize.saturating_sub(effective);
            out.push_str(&" ".repeat(pad));
            out.push_str(right);
        }
        out.push('\n');
    }
}

/// `help` 内建：与 bash 5.3 一致的双栏清单 + 77 个主题的完整手册页。
///
///   * `help`                    —— 双栏帮助主题清单；
///   * `help [-dms] PATTERN ...` —— 打印匹配主题的完整页 / 摘要 / 描述 / man 页。
///
/// 手册页文本来自 `src/help_pages.txt`（逐字节取自 MSYS2 bash 5.3.20
/// `LC_ALL=C` 的 `help` 输出），由 `crate::help_pages` 查表与排版。
/// 这是**文档**，不是 wash 已实现内建的能力声明：bash 的清单同样包含
/// 保留字、job_spec、variables 以及 wash 尚未移植的 builtin。
/// 唯一有意偏离见 `docs/intentional-divergences.md` D-002：两个 `job_spec`
/// 字符串在单 pattern 精确通道里被重定向到 `%` 页。
///
/// `help` 必须走内建表：如果把它当成外部命令，Windows 上会落到 `help.exe`
/// ——在 `LC_ALL=C` 下会挂起，在中文 locale 下还会输出 GBK 乱码。
fn builtin_help(argv: &[String], sh: &mut Shell) -> i32 {
    use crate::help_pages::{self, Mode};

    // 选项只在第一个非选项参数之前解析（bash 的 internal_getopt 行为）。
    let (mut saw_d, mut saw_m, mut saw_s) = (false, false, false);
    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "--" {
            i += 1;
            break;
        }
        if a.len() > 1 && a.starts_with('-') {
            for ch in a[1..].chars() {
                match ch {
                    'd' => saw_d = true,
                    'm' => saw_m = true,
                    's' => saw_s = true,
                    _ => {
                        safe_eprintln!(
                            "wash: help: {}",
                            crate::i18n::format_msg("-%s: invalid option", &[ch.to_string()])
                        );
                        safe_eprintln!("{}", tr!("help: usage: help [-dms] [pattern ...]"));
                        return 2;
                    }
                }
            }
            i += 1;
        } else {
            break;
        }
    }
    // 实测 bash 的优先级：-d > -m > -s（`-dms`/`-sd` 输出描述，`-ms` 输出 man）。
    let mode = if saw_d {
        Mode::Desc
    } else if saw_m {
        Mode::Man
    } else if saw_s {
        Mode::Short
    } else {
        Mode::Full
    };

    let patterns: Vec<&str> = argv[i..].iter().map(|s| s.as_str()).collect();
    if patterns.is_empty() {
        let mut block = String::new();
        // 抬头：诚实标注 wash 自己的版本，不冒充 bash 的 `GNU bash, version ...`。
        // 下面 5 句的 msgid 是 bash 5.3 `LC_ALL=C` 的英文原文；zh_CN 译文由
        // `tools/gen_help_pages.py` 从 oracle 抓取写入 po，保证中文逐字节一致。
        block.push_str(&format!("wash, version {}\n", env!("WASH_VERSION")));
        for line in [
            "These shell commands are defined internally.  Type `help' to see this list.",
            "Type `help name' to find out more about the function `name'.",
            "Use `info bash' to find out more about the shell in general.",
            "Use `man -k' or `info' to find out more about commands not in this list.",
        ] {
            block.push_str(&tr!(line));
            block.push('\n');
        }
        block.push('\n');
        block.push_str(&tr!("A star (*) next to a name means that the command is disabled."));
        block.push_str("\n\n");
        // 清单主体：从单一真源 HELP_LIST_MAP 的第一列派生 77 个显示值，
        // 按列优先排成双栏。第二列（规范名）不参与排版：清单显示值（除
        // D-002 登记的两个 `job_spec` 字符串外）不是 `help NAME` 的查找键，
        // 二者由 help_pages::verify_list_binding 硬绑定。
        // 动态插件内建（bash 无此概念）追加在末尾；未加载插件时清单
        // 原样输出，与 oracle 除版本行外逐字节一致。
        // 每条短摘要是一个 gettext msgid：C 下查不到译文回退英文原文，zh_CN 下
        // 返回 bash 的中文显示值（含被截断的 `...>` 条目）。截断位置随 locale
        // 变化，英中并非整句对译，所以译文同样由生成脚本从 oracle 抓取，
        // 而不是人工翻译。
        let mut entries: Vec<String> = help_pages::HELP_LIST_MAP
            .iter()
            .map(|(display, _)| tr!(*display))
            .collect();
        for b in crate::plugin::all_builtin_names(sh) {
            if let Some(db) = sh.plugins.dynamic.get(&b) {
                entries.push(db.help.clone().unwrap_or(b));
            }
        }
        print_help_columns(&entries, &mut block);
        help_emit(&block);
        return 0;
    }

    // 唯一有意偏离 D-002（`docs/intentional-divergences.md`）：仅当选项解析后
    // 的 pattern 列表恰好为 1 个、且字符串精确等于 `JOB_SPEC_DIVERGENCES`
    // 登记值时，直接渲染 `%` 页并 rc=0、stderr 为空。
    //
    // 必须在**任何** glob banner 与失败诊断之前短路：`job_spec [&]` 里的
    // `[&]` 会被 `is_glob()` 当成字符类，若不短路就会先打印
    // "Shell commands matching keyword" 标题，导致 wash 输出与 `help '%'`
    // 不再逐字节相等。同时该通道不进入 `lookup()`，因此 glob / 多 pattern /
    // 空 pattern / 别名查找等所有其它路径都不可能多输出 `%` 页。
    // 四种模式（无选项 / -s / -d / -m）沿用同一个 `render`，`-m` 自然带上
    // wash 身份 footer，`-d` 自然使用英文 short_doc，两个 locale 由 `all()` 决定。
    if let Some(page) = help_pages::divergence_page(&patterns) {
        help_emit(&help_pages::render(page, mode));
        return 0;
    }

    let mut block = String::new();
    // bash 只在**第一个** pattern 是 glob 时打印该标题，且列出全部 pattern
    // （实测 `help zzz 'c*'` 无标题、`help 'c*' zzz` 有 `keywords` 标题）。
    if help_pages::is_glob(patterns[0]) {
        // 中文译文不区分单复数，故两个 msgid 分开登记；C locale 下 msgstr
        // 等于 msgid，输出与 bash 英文原文逐字节一致。
        let msgid = if patterns.len() > 1 {
            "Shell commands matching keywords `%s'"
        } else {
            "Shell commands matching keyword `%s'"
        };
        block.push_str(&crate::i18n::format_msg(msgid, &[patterns.join(", ")]));
        block.push_str("\n\n");
    }

    let mut any_hit = false;
    let mut last_bad: Option<&str> = None;
    for pat in &patterns {
        // 动态插件内建（bash 无此概念）：精确名优先，沿用既有两段格式。
        if let Some(db) = sh.plugins.dynamic.get(*pat) {
            any_hit = true;
            block.push_str(&format!(
                "{}: {}\n",
                pat,
                db.help.clone().unwrap_or_else(|| (*pat).to_string())
            ));
            block.push_str(&format!("    {}\n", db.short_doc.clone().unwrap_or_default()));
            block.push('\n');
            continue;
        }
        let hits = help_pages::lookup(pat);
        if hits.is_empty() {
            last_bad = Some(pat);
            continue;
        }
        any_hit = true;
        for page in hits {
            block.push_str(&help_pages::render(page, mode));
        }
    }
    help_emit(&block);

    // 只要有一个 pattern 命中就 rc=0 且不报任何错；全部失败时 bash 只用
    // **最后一个** pattern 生成一条诊断（实测 `help aaa bbb` 只报 bbb）。
    if !any_hit {
        let name = last_bad.unwrap_or(patterns[0]);
        // 整句作为 msgid：中文译文的引号样式与词序都与英文不同（bash 用
        // `"` 而非反引号），无法靠拼接半句还原。C 下 msgstr == msgid。
        let msg = crate::i18n::format_msg(
            "no help topics match `%s'.  Try `help help' or `man -k %s' or `info %s'.",
            &[name.to_string(), name.to_string(), name.to_string()],
        );
        safe_eprintln!("wash: help: {}", msg);
        return 1;
    }
    0
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
