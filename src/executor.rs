// Phase 1 executor: expand words, then run the AST via fork/execve. Also
// provides `capture_command` for command/process substitution (runs the inner
// command in a forked subshell and returns its stdout).
//
// Pipeline plumbing, redirections, and exit-status decoding go through the
// msys POSIX shim in `posix.rs`.
//
// The builtin commands themselves live in `builtins.rs`; this module only
// decides *where* they run:
//
//   * A builtin that changes the shell state (`cd`, `export`, `set`, ...)
//     runs in the parent shell, so that its side effects survive. Bash also
//     applies such a builtin's redirections to the shell's own descriptors
//     for the duration of the builtin, which is what `apply_redirs_in_parent`
//     does.
//   * In a pipeline every stage runs in a forked child, so the same builtin
//     only changes the child's copy of the state (and is therefore lost, just
//     like in bash).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, CString};
use std::io::Write;

use crate::arith;
use crate::builtins;
use crate::expand::{expand_argv, expand_scalar, expand_word, ExpandCtx};
use crate::lexer::WordPart;
use crate::parser::{AndOr, AndOrOp, CommandList, Compound, Pipeline, Redir, SimpleCommand};
use crate::posix;
use crate::shell::{SetOptions, Shell};

/// Holds CStrings + a null-terminated pointer array for an argv/envp vector.
pub(crate) struct ArgVec {
    _cstrings: Vec<CString>,
    pub(crate) ptrs: Vec<*const c_char>,
}

impl ArgVec {
    pub(crate) fn from(args: &[String]) -> Self {
        let mut cstrings = Vec::with_capacity(args.len());
        let mut ptrs = Vec::with_capacity(args.len() + 1);
        for a in args {
            let c = CString::new(a.as_str()).unwrap_or_default();
            ptrs.push(c.as_ptr());
            cstrings.push(c);
        }
        ptrs.push(std::ptr::null());
        ArgVec {
            _cstrings: cstrings,
            ptrs,
        }
    }
}

/// Build the null-terminated `envp` vector for an environment map.
pub(crate) fn build_env_ptrs(env: &HashMap<String, String>) -> (Vec<CString>, Vec<*const c_char>) {
    let cstrings: Vec<CString> = env
        .iter()
        .map(|(k, v)| CString::new(format!("{}={}", k, v)).unwrap_or_default())
        .collect();
    let mut ptrs: Vec<*const c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (cstrings, ptrs)
}

/// Replace the current process with `path`. Only returns on failure.
pub(crate) fn execve(path: &CString, argv: &[*const c_char], envp: &[*const c_char]) -> i32 {
    posix::execve(path, argv, envp)
}

// 前台作业控制：只有主 shell 在「交互式 + tty」执行前台命令时才接管终端
// 进程组。命令替换（capture_command 用 Shell::from_env，interactive=false）、
// exec_external（子进程直接 execve，不递归回 execute）天然不参与；后台作业
// 的子进程沿用父 Shell（interactive 仍为 true），需显式置位排除，否则它
// 内部的 execute_pipeline 会误把终端前台组抢走。fork 后子进程是独立地址
// 空间，置位不影响主 shell，孙进程继承该值。
static SUPPRESS_JOB_CONTROL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 是否接管终端前台进程组：仅交互式 + tty + 非后台作业子进程。
/// `-c` / piped（isatty 判假）在此判假，整段跳过 setpgid/tcsetpgrp。
fn job_control_active(sh: &Shell) -> bool {
    sh.interactive
        && posix::isatty(0)
        && !SUPPRESS_JOB_CONTROL.load(std::sync::atomic::Ordering::Relaxed)
}

/// Fork a command child, restoring default signal dispositions in the child.
///
/// The interactive shell installs handlers for SIGINT/SIGQUIT/SIGTERM/... so
/// that Ctrl-C during a foreground command does not kill the whole session.
/// Those handlers must NOT survive in a command: an external program has to be
/// interruptible by the terminal-generated SIGINT, and a builtin running in a
/// forked pipeline stage must not leave a pending-signal flag behind.
///
/// `fork_child` 本身不碰进程组（命令替换 / 后台作业 / exec 也走它）。前台
/// 命令的 setpgid + tcsetpgrp 由 run_standalone_exp / execute_pipeline 在
/// 「交互式 + tty」时成对完成：子进程建立独立进程组，父进程先交出终端前台
/// 组、wait 完毕再恢复。这样终端产生的 SIGINT 只到达 job，shell 自身收不到，
/// 与 bash 的作业控制一致（提示符处的 Ctrl-C 仍由 repl.rs 的字节路径派发）。
fn fork_child() -> i32 {
    let pid = posix::fork();
    if pid == 0 {
        posix::reset_signals_default();
    }
    pid
}

/// Look `name` up in `path` (or directly, when it contains a slash).
///
/// Returns the path to execute and the path as the `$PATH` search reports
/// it: Cygwin falls back to the `.exe` suffix when it executes a program,
/// but it reports the name as it stands in the `$PATH`.
pub(crate) fn find_program(name: &str, path: &str) -> Option<(CString, String)> {
    if name.contains('/') || name.contains('\\') {
        if posix::access(name, posix::X_OK) == 0 {
            return Some((posix::to_cstr(name), name.to_string()));
        }
        let exe = format!("{}.exe", name);
        if posix::access(&exe, posix::X_OK) == 0 {
            return Some((posix::to_cstr(&exe), name.to_string()));
        }
        return None;
    }
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = format!("{}/{}", dir, name);
        if posix::access(&cand, posix::X_OK) == 0 {
            return Some((posix::to_cstr(&cand), cand.clone()));
        }
        let cand_exe = format!("{}.exe", cand);
        if posix::access(&cand_exe, posix::X_OK) == 0 {
            return Some((posix::to_cstr(&cand_exe), cand));
        }
    }
    None
}

/// Look `name` up in `path` (or directly, when it contains a slash).
pub(crate) fn find_executable(name: &str, path: &str) -> Option<CString> {
    find_program(name, path).map(|(exe, _)| exe)
}

/// The executables named `name` in the colon-separated `$PATH`.
///
/// Like Cygwin's `find_user_command`, the `.exe` suffix is tried as a
/// fallback but it is not reported: bash prints the path exactly as it
/// stands in the `$PATH`.
pub fn path_matches(name: &str, path: &str) -> Vec<String> {
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

// ---------------------------------------------------------------------------
// Redirections (post-expansion)
// ---------------------------------------------------------------------------

enum ExpRedir {
    In(i32, String),
    /// fd, file, append, clobber (`>|` bypasses noclobber).
    Out(i32, String, bool, bool),
    /// file, append (`&>>`).
    Both(String, bool),
    Dup(i32, String), // `N>&M` / `N<&M` / `>&-`
}

fn expand_file(file: &[WordPart], ctx: &ExpandCtx) -> String {
    let words = expand_word(file, ctx);
    words.into_iter().next().unwrap_or_default()
}

fn expand_redirs(redirs: &[Redir], ctx: &ExpandCtx) -> Vec<ExpRedir> {
    redirs
        .iter()
        .map(|r| match r {
            Redir::In { fd, file } => ExpRedir::In(*fd, expand_file(file, ctx)),
            Redir::Out { fd, file, append, clobber } => {
                ExpRedir::Out(*fd, expand_file(file, ctx), *append, *clobber)
            }
            Redir::Both { file, append } => ExpRedir::Both(expand_file(file, ctx), *append),
            Redir::Dup { fd, file } => ExpRedir::Dup(*fd, expand_file(file, ctx)),
        })
        .collect()
}

/// Apply the redirections in the forked child. Failures are ignored here (the
/// child cannot report them any better than the shell could).
fn apply_exp_redirs(redirs: &[ExpRedir], noclobber: bool) {
    for r in redirs {
        match r {
            ExpRedir::In(fd, file) => {
                let f = posix::open_read(file);
                if f >= 0 {
                    posix::dup2(f, *fd);
                    posix::close(f);
                }
            }
            ExpRedir::Out(fd, file, append, clobber) => {
                let f = posix::open_write(file, *append, !*append && !*clobber && noclobber);
                if f >= 0 {
                    posix::dup2(f, *fd);
                    posix::close(f);
                }
            }
            ExpRedir::Both(file, append) => {
                let f = posix::open_write(file, *append, false);
                if f >= 0 {
                    posix::dup2(f, 1);
                    posix::dup2(f, 2);
                    posix::close(f);
                }
            }
            ExpRedir::Dup(fd, target) => {
                // `N>&M` makes N refer to whatever M currently refers to;
                // `>&-` closes the descriptor.
                if target == "-" {
                    posix::close(*fd);
                } else if let Ok(src) = target.parse::<i32>() {
                    posix::dup2(src, *fd);
                }
            }
        }
    }
}

/// Apply the redirections to the *current* shell, saving the previous
/// descriptors so that they can be restored afterwards.
///
/// Bash runs builtins in the current shell and applies the builtin's
/// redirections to the shell's own descriptors for the duration of the
/// command. The returned vector holds the `(fd, backup)` pairs; pass it to
/// `restore_fds` to undo the redirections.
fn apply_redirs_in_parent(
    redirs: &[ExpRedir],
    noclobber: bool,
) -> Result<Vec<(i32, i32)>, String> {
    let mut saved: Vec<(i32, i32)> = Vec::new();
    // Flush pending output so that it cannot leak into the redirected file.
    let _ = std::io::stdout().flush();

    for r in redirs {
        match r {
            ExpRedir::In(fd, file) => {
                let f = posix::open_read(file);
                if f < 0 {
                    let e = posix::errno();
                    restore_fds(&saved);
                    return Err(format!("wash: {}: {}", file, posix::errno_message(e)));
                }
                let backup = posix::dup(*fd);
                saved.push((*fd, backup));
                posix::dup2(f, *fd);
                posix::close(f);
            }
            ExpRedir::Out(fd, file, append, clobber) => {
                let f = posix::open_write(file, *append, !*append && !*clobber && noclobber);
                if f < 0 {
                    let e = posix::errno();
                    restore_fds(&saved);
                    if e == posix::EEXIST {
                        return Err(format!("wash: {}: cannot overwrite existing file", file));
                    }
                    return Err(format!("wash: {}: {}", file, posix::errno_message(e)));
                }
                let backup = posix::dup(*fd);
                saved.push((*fd, backup));
                posix::dup2(f, *fd);
                posix::close(f);
            }
            ExpRedir::Both(file, append) => {
                let f = posix::open_write(file, *append, false);
                if f < 0 {
                    let e = posix::errno();
                    restore_fds(&saved);
                    return Err(format!("wash: {}: {}", file, posix::errno_message(e)));
                }
                for fd in [1, 2] {
                    let backup = posix::dup(fd);
                    saved.push((fd, backup));
                    posix::dup2(f, fd);
                }
                posix::close(f);
            }
            ExpRedir::Dup(fd, target) => {
                // Builtins run in the parent, so the descriptor must be saved
                // for restore_fds() afterwards.
                let backup = posix::dup(*fd);
                saved.push((*fd, backup));
                if target == "-" {
                    posix::close(*fd);
                } else if let Ok(src) = target.parse::<i32>() {
                    posix::dup2(src, *fd);
                } else {
                    restore_fds(&saved);
                    return Err(format!("wash: {}: ambiguous redirect", target));
                }
            }
        }
    }
    Ok(saved)
}

/// Restore the descriptors saved by `apply_redirs_in_parent`.
fn restore_fds(saved: &[(i32, i32)]) {
    for (fd, backup) in saved.iter().rev() {
        posix::dup2(*backup, *fd);
        posix::close(*backup);
    }
}

/// Print the command that is about to run, the way bash's `set -x` does.
fn trace_command(argv: &[String]) {
    let mut line = String::from("+");
    for word in argv {
        line.push(' ');
        line.push_str(&builtins::quote_for_trace(word));
    }
    eprintln!("{}", line);
}

// ---------------------------------------------------------------------------
// Command substitution capture
// ---------------------------------------------------------------------------

/// The exit status of the most recent command substitution, recorded by the
/// forked subshell that ran it.  A command substitution that is the RHS of a
/// standalone assignment (`v=$(cmd)`) makes the assignment's `$?` equal to this
/// status (matching bash).  We keep it in a thread-local because expansion is
/// synchronous on the main thread and the expander otherwise has no access to
/// the parent shell's `last_status`.
thread_local! {
    static LAST_CMDSIB_STATUS: RefCell<Option<i32>> = RefCell::new(None);
}

/// Reset the recorded command-substitution status.  The standalone-assignment
/// path calls this immediately before expanding an assignment's RHS so that a
/// later `take_cmdsub_status` reflects only substitutions produced by *this*
/// assignment (and is `None` when the assignment contained no substitution).
fn reset_cmdsub_status() {
    LAST_CMDSIB_STATUS.with(|c| *c.borrow_mut() = None);
}

/// Return the status recorded by the most recent command substitution and clear
/// it, or `None` if no substitution ran since the last `reset_cmdsub_status`.
fn take_cmdsub_status() -> Option<i32> {
    LAST_CMDSIB_STATUS.with(|c| c.borrow_mut().take())
}

/// Run `cmd` in a forked subshell, capturing its stdout (with all trailing
/// newlines stripped, matching bash).
///
/// `nounset` is the parent shell's `set -u` setting: a command substitution is
/// a subshell that inherits the parent's options, so an unbound variable inside
/// `$(...)` must trigger the same `unbound variable` diagnostic (and failure)
/// as at the top level.
/// Memoized parse of command-substitution bodies.
///
/// Hot loops re-run the same `$(...)` (e.g. `s=$(echo x)`) hundreds of times;
/// re-lexing and re-parsing the identical text each call dominated the cmdsub
/// benchmark.  The cache maps the body text to a shared parsed `CommandList`
/// (`execute` only reads it).  Capped; a full cache is simply cleared.
const CMDSUB_PARSE_CACHE_MAX: usize = 256;

thread_local! {
    static CMDSUB_PARSE_CACHE: std::cell::RefCell<HashMap<String, std::sync::Arc<CommandList>>> =
        std::cell::RefCell::new(HashMap::new());
}

fn parse_cmdsub_cached(cmd: &str) -> Option<std::sync::Arc<CommandList>> {
    if let Some(hit) = CMDSUB_PARSE_CACHE.with(|c| c.borrow().get(cmd).cloned()) {
        return Some(hit);
    }
    let tokens = crate::lexer::tokenize(cmd).ok()?;
    let list = crate::parser::parse_command_list(&tokens).ok()?;
    let arc = std::sync::Arc::new(list);
    CMDSUB_PARSE_CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if map.len() >= CMDSUB_PARSE_CACHE_MAX {
            map.clear();
        }
        map.insert(cmd.to_string(), std::sync::Arc::clone(&arc));
    });
    Some(arc)
}

pub fn capture_command(
    cmd: &str,
    env: &HashMap<String, String>,
    attrs: &crate::shell::AttrRefs,
    functions: &HashMap<String, crate::parser::CommandList>,
    arrays: &HashMap<String, Vec<String>>,
    nounset: bool,
    bg_pid: i32,
) -> String {
    let list = match parse_cmdsub_cached(cmd) {
        Some(list) => list,
        None => return String::new(),
    };
    let fds = posix::pipe();
    let r = fds[0];
    let w = fds[1];
    let pid = fork_child();
    if pid == 0 {
        posix::dup2(w, 1);
        posix::close(w);
        posix::close(r);
        let mut sh = Shell::from_env(env.clone());
        // A command substitution is a subshell that inherits the parent's
        // options, so a `set -u` active in the parent also applies inside
        // `$(...)`.  `Shell::from_env` can only build default options, so the
        // parent's `nounset` flag is threaded through explicitly here.
        sh.options.nounset = nounset;
        // A command substitution also inherits the parent's background-job
        // table, so `$!` inside `$(...)` refers to the most recent background
        // job started *before* the substitution (bash behaviour).  `bg_pid` is
        // threaded through explicitly for the same reason `nounset` is.
        sh.bg_pid = bg_pid;
        sh.apply_attr_refs(attrs);
        // A command substitution inherits the parent's function table, so a
        // function can recurse through `$(...)`.
        sh.functions = functions.clone();
        // Array variables are inherited as well (bash copies them into the
        // subshell).
        sh.arrays = arrays.clone();
        let code = crate::executor::execute(&list, &mut sh);
        // Inside a command substitution, an unbound variable under `set -u`
        // (or a failed `${var?word}`) fails the substitution with status 1,
        // NOT the 127 reserved for a fatal top-level error.
        let code = if crate::expand::expansion_aborted() || crate::expand::expansion_aborted_rc1() {
            1
        } else {
            code
        };
        let _ = std::io::stdout().flush();
        posix::exit(code);
    }
    posix::close(w);
    let s = posix::read_all(r);
    posix::close(r);
    let status = posix::wait_pid(pid);
    // Record the substitution's exit status so a standalone assignment can
    // expose it as `$?` (bash: `v=$(false); echo $?` -> 1).
    LAST_CMDSIB_STATUS.with(|c| *c.borrow_mut() = Some(status));
    // Bash strips *all* trailing newlines from command substitution output
    // (`$(printf 'a\n\n\n')` -> "a"), not just one.
    s.trim_end_matches('\n').to_string()
}

/// Run an external program, bypassing the shell functions and the aliases.
///
/// This is what the `command` builtin uses: the command word is looked up
/// in `$PATH` and executed, without consulting the function table.
pub fn exec_external(argv: &[String], sh: &Shell) -> i32 {
    if argv.is_empty() {
        return 0;
    }
    let path = sh.env.get("PATH").cloned().unwrap_or_default();
    let pid = fork_child();
    if pid == 0 {
        let env = sh.child_env();
        let (_cstrings, env_ptrs) = build_env_ptrs(&env);
        let argvec = ArgVec::from(argv);
        match find_executable(&argv[0], &path) {
            Some(exe) => {
                execve(&exe, &argvec.ptrs, &env_ptrs);
            }
            None => {
                eprintln!("wash: {}: command not found", argv[0]);
            }
        }
        posix::exit(127);
    }
    posix::wait_pid(pid)
}

// ---------------------------------------------------------------------------
// Compound commands
// ---------------------------------------------------------------------------

/// Run a compound command (`if ... fi`, `for`, `while`, `until`, `case`) and
/// return its exit status.
fn execute_compound(compound: &Compound, sh: &mut Shell) -> i32 {
    match compound {
        Compound::If { branches, else_body } => {
            for (condition, body) in branches {
                let status = execute(condition, sh);
                sh.last_status = status;
                if sh.exit_requested.is_some() || sh.return_requested.is_some() {
                    return status;
                }
                if status == 0 {
                    let body_status = execute(body, sh);
                    sh.last_status = body_status;
                    return body_status;
                }
            }
            match else_body {
                Some(body) => {
                    let status = execute(body, sh);
                    sh.last_status = status;
                    status
                }
                None => 0,
            }
        }
        Compound::For { var, words, body } => execute_for(var, words, body, sh),
        Compound::While {
            condition,
            body,
            is_until,
        } => execute_while(condition, body, *is_until, sh),
        Compound::Case { word, arms } => execute_case(word, arms, sh),
        // A brace group runs in the *current* shell (no subshell): assignments
        // and `cd` inside it persist.  When the group is part of a pipeline
        // the executor already forked, which matches bash too.
        Compound::Group { body } => {
            let status = execute(body, sh);
            sh.last_status = status;
            status
        }
        // `( LIST )` runs LIST in a forked copy of the shell.  Because we fork,
        // every variable / fd / cwd change stays inside the child and never
        // leaks back to the parent.  `run_standalone_exp` has already applied
        // the subshell's own redirections in the parent, so the child inherits
        // them (just like bash's subshell applies them before the copy).
        Compound::Subshell { body } => {
            let pid = fork_child();
            if pid == 0 {
                // The child is an exact fork of `sh`; reset the per-command
                // expansion / arithmetic flags and run the body in this copy.
                crate::expand::clear_expansion_abort();
                crate::expand::clear_arith_failed();
                let code = execute(body, sh);
                let _ = std::io::stdout().flush();
                posix::exit(code);
            }
            let status = posix::wait_pid(pid);
            sh.last_status = status;
            status
        }
    }
}

/// Run a `for NAME in WORDS; do BODY; done` loop.
///
/// Each word of the (already raw) iteration list is expanded into zero or more
/// fields (so globs and brace expansions work), and `NAME` is bound to each
/// field in turn before the body runs once.
fn execute_for(var: &str, words: &[Vec<WordPart>], body: &CommandList, sh: &mut Shell) -> i32 {
    let attrs = sh.attr_refs();
    let ctx = ExpandCtx {
        env: &sh.env,
        positional: &sh.positional,
        last_status: sh.last_status,
        self_pid: sh.self_pid,
        noglob: sh.options.noglob,
        nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        bg_pid: sh.bg_pid,
        dollar_zero: &sh.dollar_zero,
        attrs: &attrs,
        functions: &sh.functions,
        arrays: &sh.arrays,
        pending_assign: None,
    };
    let mut fields: Vec<String> = Vec::new();
    for w in words {
        fields.extend(expand_word(w, &ctx));
    }
    let mut code = 0;
    for field in fields {
        sh.env.insert(var.to_string(), field);
        code = execute_loop_body(body, sh);
        // bash leaves the loop variable set to its final value (the loop body
        // shares the caller's scope), so we keep it rather than unsetting it.
        if sh.exit_requested.is_some() || sh.return_requested.is_some() {
            return code;
        }
        if let Some(n) = sh.break_requested.take() {
            if n > 1 {
                sh.break_requested = Some(n - 1);
            }
            break;
        }
        // `continue` (if set after the body) starts the next iteration; clear
        // it so the flag does not leak into the next pass.
        sh.continue_requested = None;
    }
    code
}

/// Run a `while CONDITION; do BODY; done` (`is_until == false`) or
/// `until CONDITION; do BODY; done` (`is_until == true`).
fn execute_while(condition: &CommandList, body: &CommandList, is_until: bool, sh: &mut Shell) -> i32 {
    let mut code = 0;
    loop {
        // One loop iteration: the whole construct (condition + body) counts as
        // a single level of `loop_depth` for the `break`/`continue` builtins.
        sh.loop_depth += 1;
        sh.break_requested = None;
        sh.continue_requested = None;

        let cond = execute(condition, sh);
        sh.last_status = cond;
        sh.loop_depth -= 1;
        if sh.exit_requested.is_some() || sh.return_requested.is_some() {
            return cond;
        }
        if let Some(n) = sh.break_requested.take() {
            if n > 1 {
                sh.break_requested = Some(n - 1);
            }
            break;
        }
        if sh.continue_requested.take().is_some() {
            // `continue` in the condition just re-evaluates it.
            continue;
        }

        let stop = if is_until { cond == 0 } else { cond != 0 };
        if !stop {
            code = execute_loop_body(body, sh);
            if sh.exit_requested.is_some() || sh.return_requested.is_some() {
                return code;
            }
            if let Some(n) = sh.break_requested.take() {
                if n > 1 {
                    sh.break_requested = Some(n - 1);
                }
                break;
            }
            if sh.continue_requested.take().is_some() {
                continue;
            }
        } else {
            // The loop terminates because the condition is now false.  Like
            // bash, the exit status is the last command executed in the body
            // (`code`), NOT the (non-zero) final condition status.  If the body
            // never ran, `code` stays at its initial `0`, which is also what
            // bash returns.
            return code;
        }
    }
    code
}

/// Run the body of a loop for a single iteration.
///
/// `loop_depth` is bumped around the body so that `break` / `continue`
/// recognise they are inside a loop, and any carry-over flags from a previous
/// iteration (or a returned function) are cleared before the body runs.
fn execute_loop_body(body: &CommandList, sh: &mut Shell) -> i32 {
    sh.loop_depth += 1;
    sh.break_requested = None;
    sh.continue_requested = None;
    let code = execute(body, sh);
    sh.loop_depth -= 1;
    code
}

/// Run a `case WORD in PATTERN) BODY;; ... esac` command.
///
/// `WORD` is expanded once; the first arm whose pattern matches (using bash
/// glob semantics: `*`/`?`/`[...]`, `|`-separated alternatives) runs its body
/// and the construct returns its status.  With no match the status is 0.
fn execute_case(word: &[WordPart], arms: &[crate::parser::CaseArm], sh: &mut Shell) -> i32 {
    let attrs = sh.attr_refs();
    let ctx = ExpandCtx {
        env: &sh.env,
        positional: &sh.positional,
        last_status: sh.last_status,
        self_pid: sh.self_pid,
        noglob: sh.options.noglob,
        nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        bg_pid: sh.bg_pid,
        dollar_zero: &sh.dollar_zero,
        attrs: &attrs,
        functions: &sh.functions,
        arrays: &sh.arrays,
        pending_assign: None,
    };
    let value = expand_scalar(word, &ctx);
    for arm in arms {
        let mut matched = false;
        for pat in &arm.patterns {
            let p = expand_scalar(pat, &ctx);
            if glob_match(&value, &p) {
                matched = true;
                break;
            }
        }
        if matched {
            return execute(&arm.body, sh);
        }
    }
    0
}

/// A bash-style glob match (no FNM_PATHNAME, so `*` matches `/` as well).
/// Supports `*`, `?` and `[...]` bracket expressions (`!`/`^` negation and
/// `-` ranges); everything else matches literally.
fn glob_match(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    fn rec(t: &[char], p: &[char], ti: usize, pi: usize) -> bool {
        let mut pi = pi;
        let mut ti = ti;
        while pi < p.len() {
            match p[pi] {
                '*' => {
                    // Try consuming zero or more characters.
                    for skip in ti..=t.len() {
                        if rec(t, p, skip, pi + 1) {
                            return true;
                        }
                    }
                    return false;
                }
                '[' => {
                    // Find the closing `]`.
                    let mut j = pi + 1;
                    if j >= p.len() {
                        // Unterminated `[` matches literally.
                        if ti < t.len() && t[ti] == '[' {
                            pi += 1;
                            ti += 1;
                            continue;
                        }
                        return false;
                    }
                    let mut negate = false;
                    if p[j] == '!' || p[j] == '^' {
                        negate = true;
                        j += 1;
                    }
                    let mut matched = false;
                    let mut k = j;
                    while k < p.len() && p[k] != ']' {
                        if k + 2 < p.len() && p[k + 1] == '-' && p[k + 2] != ']' {
                            let lo = p[k];
                            let hi = p[k + 2];
                            if ti < t.len() && t[ti] >= lo && t[ti] <= hi {
                                matched = true;
                            }
                            k += 3;
                        } else {
                            if ti < t.len() && t[ti] == p[k] {
                                matched = true;
                            }
                            k += 1;
                        }
                    }
                    if k >= p.len() {
                        // Unterminated bracket: fall back to literal `[`.
                        if ti < t.len() && t[ti] == '[' {
                            pi += 1;
                            ti += 1;
                            continue;
                        }
                        return false;
                    }
                    let ok = matched != negate;
                    if !(ti < t.len() && ok) {
                        return false;
                    }
                    pi = k + 1;
                    ti += 1;
                    continue;
                }
                '?' => {
                    if ti >= t.len() {
                        return false;
                    }
                    pi += 1;
                    ti += 1;
                    continue;
                }
                c => {
                    if ti >= t.len() || t[ti] != c {
                        return false;
                    }
                    pi += 1;
                    ti += 1;
                    continue;
                }
            }
        }
        ti == t.len()
    }
    rec(&t, &p, 0, 0)
}

/// Register every function definition found in `list` (recursively, so nested
/// functions are global) into the shell.  Bash functions are global even when
/// defined inside another function or a compound command.
fn register_functions(list: &CommandList, sh: &mut Shell) {
    for fd in &list.func_defs {
        sh.functions.insert(fd.name.clone(), fd.body.clone());
        register_functions(&fd.body, sh);
    }
}

/// Maximum function call nesting depth before wash bails out with an error
/// instead of overflowing the native stack. Bash enforces a similar limit
/// (`maximum function nesting level exceeded`); without this guard a recursive
/// function would abort the whole process via a stack overflow.
const MAX_FUNCTION_DEPTH: usize = 200;

/// Run a shell function: push a new local scope, bind `$1`/`$@` to the call
/// arguments, run the body, restore the caller's positional parameters and
/// local variables, and return the (possibly `return`-driven) status.
fn execute_function(name: &str, args: Vec<String>, sh: &mut Shell) -> i32 {
    let body = match sh.functions.get(name) {
        Some(b) => b.clone(),
        None => {
            eprintln!("wash: {}: command not found", name);
            return 127;
        }
    };
    let saved_positional = std::mem::replace(&mut sh.positional, args);
    sh.function_depth += 1;
    if sh.function_depth > MAX_FUNCTION_DEPTH {
        sh.function_depth -= 1;
        eprintln!("wash: maximum function nesting level exceeded");
        return 1;
    }
    sh.call_stack.push(std::collections::HashMap::new());
    sh.return_requested = None;
    sh.break_requested = None;
    sh.continue_requested = None;

    let code = execute(&body, sh);

    sh.function_depth -= 1;
    // Restore any variables that were shadowed by `local` in this function.
    if let Some(frame) = sh.call_stack.pop() {
        for (var, prev) in frame {
            match prev {
                Some(v) => {
                    sh.env.insert(var, v);
                }
                None => {
                    sh.env.remove(&var);
                }
            }
        }
    }
    sh.positional = saved_positional;
    // A `return` inside the function becomes its exit status; otherwise the
    // code is the status of the last command of the body.
    let code = if let Some(r) = sh.return_requested.take() {
        r
    } else {
        code
    };
    sh.break_requested = None;
    sh.continue_requested = None;
    code
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// The parts of the parent shell state that a forked child inherits.
struct ChildContext<'a> {
    /// The environment the executed program receives (exported variables
    /// only, plus the command's own assignments).
    env: &'a HashMap<String, String>,
    /// The complete variable table the child shell starts with — a subshell
    /// inherits every shell variable, exported or not.
    shell_env: &'a HashMap<String, String>,
    /// The array variables the child shell starts with (a subshell inherits
    /// the parent's arrays, just like bash does).
    arrays: &'a HashMap<String, Vec<String>>,
    options: &'a SetOptions,
    positional: &'a [String],
    dollar_zero: &'a str,
    attrs: &'a crate::shell::AttrRefs<'a>,
    /// The pid of the most recent background job in the parent.  A forked
    /// child (subshell) inherits the parent's background-job table, so `$!`
    /// inside the child refers to that job the same way it would in the
    /// parent.  `Shell::from_env` can only build a default `bg_pid` of 0, so
    /// the parent's value is threaded through here.
    bg_pid: i32,
    /// The compound command (`if ... fi`) the child has to run, if any.
    compound: Option<&'a Compound>,
    /// The program the child has to execute, already resolved by the
    /// parent.
    program: Option<&'a CString>,
}

/// Record an executed program in the shell's hash table.
fn record_hash_entry(sh: &mut Shell, name: &str, path: &str) {
    sh.hash_table.insert(name.to_string(), path.to_string());
    *sh.hash_hits.entry(name.to_string()).or_insert(0) += 1;
}

/// Run a command inside the forked child: either a builtin (whose state
/// changes are lost with the child) or an external program.
fn child_run_exp(argv: &[String], path: &str, ctx: &ChildContext) -> ! {
    if argv.is_empty() {
        // A compound command (`if ... fi`) has no words of its own: it runs
        // inside the child.
        if let Some(compound) = ctx.compound {
            let mut sub = Shell::from_env(ctx.shell_env.clone());
            // Inherit the parent's background-job table so `$!` works inside
            // a forked compound command (e.g. `sleep & ( if ...; fi )`).
            sub.bg_pid = ctx.bg_pid;
            sub.apply_attr_refs(ctx.attrs);
            sub.arrays = ctx.arrays.clone();
            sub.positional = ctx.positional.to_vec();
            sub.options = ctx.options.clone();
            sub.dollar_zero = ctx.dollar_zero.to_string();
            let code = execute_compound(compound, &mut sub);
            let _ = std::io::stdout().flush();
            posix::exit(code);
        }
        posix::exit(0);
    }
    // The redirections have already been applied by the caller (pipes and the
    // command's own redirections), so the builtins can simply write to stdout.
    let mut sub = Shell::from_env(ctx.shell_env.clone());
    // Inherit the parent's background-job table so `$!` works inside a forked
    // subshell / builtin-in-child context.
    sub.bg_pid = ctx.bg_pid;
    sub.apply_attr_refs(ctx.attrs);
    sub.arrays = ctx.arrays.clone();
    sub.positional = ctx.positional.to_vec();
    sub.options = ctx.options.clone();
    sub.dollar_zero = ctx.dollar_zero.to_string();
    if let Some(code) = builtins::run_builtin(argv, &mut sub) {
        let _ = std::io::stdout().flush();
        posix::exit(code);
    }
    // Not a builtin: look it up in $PATH and replace the child process.
    let (_cstrings, env_ptrs) = build_env_ptrs(ctx.env);
    let argvec = ArgVec::from(argv);
    let exe = match ctx.program {
        Some(exe) => exe.clone(),
        None => match find_executable(&argv[0], path) {
            Some(exe) => exe,
            None => {
                eprintln!("wash: {}: command not found", argv[0]);
                posix::exit(127);
            }
        },
    };
    posix::execve(&exe, &argvec.ptrs, &env_ptrs);
    posix::exit(127);
}

/// Apply a single assignment to the shell, honouring the `readonly` and
/// integer (`declare -i`) attributes the way bash does.
///
/// * A `readonly` variable cannot be assigned to (the command prints an error
///   and is left unchanged).
/// * An integer-attributed variable is reassigned by evaluating the value as an
///   arithmetic expression (so `N=N+1` increments `N`).
///
/// Returns `false` when the assignment was refused (a `readonly` variable);
/// the caller turns that into a non-zero exit status the way bash does.
fn assign_var(sh: &mut Shell, name: &str, value: String, append: bool) -> bool {
    if sh.readonly.contains(name) {
        eprintln!("wash: {}: readonly variable", name);
        return false;
    }
    let final_value = if sh.integer_vars.contains(name) {
        match arith::eval(&value, &mut arith::MapEnv { vars: &mut sh.env }) {
            Ok(v) => v.to_string(),
            Err(_) => value,
        }
    } else {
        value
    };
    if append {
        let cur = sh.env.get(name).cloned().unwrap_or_default();
        sh.env.insert(name.to_string(), cur + &final_value);
    } else {
        sh.env.insert(name.to_string(), final_value);
    }
    true
}

/// Apply the assignments of a command (e.g. `IFS=: read x') and return the
/// previous values, so that they can be restored afterwards.
fn apply_command_assignments(
    assigns: &[(String, Vec<WordPart>, bool)],
    sh: &mut Shell,
) -> Vec<(String, Option<String>)> {
    if assigns.is_empty() {
        return Vec::new();
    }
    let attrs = sh.attr_refs();
    let pending: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
    let applied: Vec<(String, String, bool)> = {
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            pending_assign: Some(&pending),
        };
        assigns
            .iter()
            .map(|(n, vp, ap)| (n.clone(), expand_scalar(vp, &ctx), *ap))
            .collect()
    };
    let mut saved: Vec<(String, Option<String>)> = Vec::new();
    for (name, value, append) in applied {
        saved.push((name.clone(), sh.env.get(&name).cloned()));
        let _ = assign_var(sh, &name, value, append);
    }
    saved
}

/// Undo the assignments of a command.
fn restore_command_assignments(saved: Vec<(String, Option<String>)>, sh: &mut Shell) {
    for (name, value) in saved {
        match value {
            Some(value) => {
                sh.env.insert(name, value);
            }
            None => {
                sh.env.remove(&name);
            }
        }
    }
}

/// Expand the element words of array assignments and store them in `sh.arrays`.
///
/// Returns the previous value of every named array so the caller can restore it
/// afterwards (a temporary `name=(...) cmd` assignment).  For a persistent
/// assignment (a standalone `a=(1 2 3)`) the returned saves may be ignored.
fn apply_array_assignments(
    arrays: &[(String, Vec<Vec<WordPart>>, bool)],
    sh: &mut Shell,
) -> Vec<(String, Option<Vec<String>>)> {
    if arrays.is_empty() {
        return Vec::new();
    }
    let attrs = sh.attr_refs();
    // Expand every element first (immutable borrow of `sh`), then mutate
    // `sh.arrays` once the borrow is released.
    let expanded: Vec<(String, Vec<String>, bool)> = {
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            pending_assign: None,
        };
        arrays
            .iter()
            .map(|(name, elems, append)| {
                let mut v = Vec::new();
                for w in elems {
                    v.extend(expand_word(w, &ctx));
                }
                (name.clone(), v, *append)
            })
            .collect()
    };
    let mut saved = Vec::new();
    for (name, v, append) in expanded {
        saved.push((name.clone(), sh.arrays.get(&name).cloned()));
        if append {
            // `name+=(...)` appends to the existing elements.
            let mut all = sh.arrays.get(&name).cloned().unwrap_or_default();
            all.extend(v);
            sh.arrays.insert(name, all);
        } else {
            sh.arrays.insert(name, v);
        }
    }
    saved
}

/// Undo the array assignments saved by `apply_array_assignments`.
fn restore_array_assignments(saved: Vec<(String, Option<Vec<String>>)>, sh: &mut Shell) {
    for (name, value) in saved {
        match value {
            Some(v) => {
                sh.arrays.insert(name, v);
            }
            None => {
                sh.arrays.remove(&name);
            }
        }
    }
}

/// Update `$_` (the last-argument parameter) the way bash does.
///
/// * After a command with arguments, `$_` holds the command's LAST word.
///   A command without arguments (e.g. `:`) leaves `$_` as the command name.
/// * After an assignment-only command (`X=1`), `$_` becomes empty.
/// * In both cases bash DROPS the export attribute of `$_`, so it disappears
///   from `export -p` as soon as the first command has run.  At shell startup
///   it is still exported, inherited from the parent shell.
fn update_underscore(sh: &mut Shell, argv: &[String]) {
    let value = argv.last().cloned().unwrap_or_default();
    sh.env.insert("_".to_string(), value);
    sh.exported.remove("_");
}

/// Drop the export attribute of `$_` without changing its value.
///
/// A compound command (`if ... fi`) runs its inner commands, which update
/// `$_` themselves; the compound only ends the export attribute.
fn drop_underscore_export(sh: &mut Shell) {
    sh.exported.remove("_");
}

/// Run a single (non-pipeline) command in the parent shell.
fn run_standalone_exp(
    argv: &[String],
    redirs: &[ExpRedir],
    assigns: &[(String, Vec<WordPart>, bool)],
    array_assigns: &[(String, Vec<Vec<WordPart>>, bool)],
    compound: Option<&Compound>,
    env_values: Option<&[(String, String, bool)]>,
    sh: &mut Shell,
    path: &str,
) -> i32 {
    if argv.is_empty() {
        // A compound command (`if ... fi`): its redirections apply to the
        // whole construct, exactly like in bash.
        if let Some(compound) = compound {
            let saved = match apply_redirs_in_parent(redirs, sh.options.noclobber) {
                Ok(saved) => saved,
                Err(message) => {
                    eprintln!("{}", message);
                    return 1;
                }
            };
            let code = execute_compound(compound, sh);
            if !sh.persistent_redirs {
                restore_fds(&saved);
            }
            drop_underscore_export(sh);
            return code;
        }
        // Standalone assignment(s): persist to the shell environment.
        let mut assign_rc: i32 = 0;
        if !assigns.is_empty() || !array_assigns.is_empty() {
            // An assignment-only command sets `$_` to the empty string.
            update_underscore(sh, &[]);
            // Start tracking command-substitution exit statuses afresh for this
            // assignment (so a later `take_cmdsub_status` reflects only what this
            // assignment expanded, not a stale value from a previous command).
            reset_cmdsub_status();
            let mut assign_failed = false;
            let attrs = sh.attr_refs();
            let pending: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
            // Expand all RHS values first (immutable borrow of sh.env), then
            // mutate sh.env once the borrow is released.
            let applied: Vec<(String, String, bool)> = {
                let ctx = ExpandCtx {
                    env: &sh.env,
                    positional: &sh.positional,
                    last_status: sh.last_status,
                    self_pid: sh.self_pid,
                    noglob: sh.options.noglob,
                    nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        bg_pid: sh.bg_pid,
                    dollar_zero: &sh.dollar_zero,
                    attrs: &attrs,
                    functions: &sh.functions,
            arrays: &sh.arrays,
                    pending_assign: Some(&pending),
                };
                assigns
                    .iter()
                    .map(|(n, vp, ap)| (n.clone(), expand_scalar(vp, &ctx), *ap))
                    .collect()
            };
            for (name, val, append) in applied {
                if !assign_var(sh, &name, val, append) {
                    assign_failed = true;
                }
            }
            // Apply the assignments the expansion requested (`${var=word}`).
            for (name, value) in pending.borrow_mut().drain(..) {
                sh.env.insert(name, value);
            }
            // Persist array assignments (`a=(1 2 3)`) to the shell.
            apply_array_assignments(array_assigns, sh);
            // A standalone assignment's `$?` is the exit status of its last
            // command substitution (bash: `v=$(false); echo $?` -> 1), or 0 when
            // it contains none (`v=5; echo $?` -> 0).  This status is also the
            // assignment command's own exit status (so `if v=$(false); then ...`
            // skips the body the way bash does).
            assign_rc = take_cmdsub_status().unwrap_or(0);
            sh.last_status = assign_rc;
            if assign_failed {
                // A failed assignment (e.g. to a readonly variable) makes the
                // command return 1.  In bash a standalone assignment that fails
                // because the variable is readonly also aborts the rest of the
                // `-c` script (the remaining commands do not run), so we mirror
                // that by flagging a fatal error.
                sh.fatal_error = Some(1);
                return 1;
            }
        }
        // A redirection without a command applies to the shell itself, but
        // only for the duration of the command (bash restores it afterwards).
        match apply_redirs_in_parent(redirs, sh.options.noclobber) {
            Ok(saved) => restore_fds(&saved),
            Err(message) => {
                eprintln!("{}", message);
                return 1;
            }
        }
        return assign_rc;
    }

    if sh.options.xtrace {
        trace_command(argv);
    }

    // A shell function shadows builtins and external programs (the `command`
    // and `builtin` builtins can still bypass it when invoked explicitly).
    if sh.functions.contains_key(&argv[0]) {
        let saved = match apply_redirs_in_parent(redirs, sh.options.noclobber) {
            Ok(saved) => saved,
            Err(message) => {
                eprintln!("{}", message);
                return 1;
            }
        };
        let code = execute_function(&argv[0], argv[1..].to_vec(), sh);
        if !sh.persistent_redirs {
            restore_fds(&saved);
        }
        update_underscore(sh, argv);
        return code;
    }

    // Builtins run in the current shell so that their side effects survive.
    if builtins::is_builtin(sh, &argv[0]) {
        sh.persistent_redirs = false;
        let saved = match apply_redirs_in_parent(redirs, sh.options.noclobber) {
            Ok(saved) => saved,
            Err(message) => {
                eprintln!("{}", message);
                return 1;
            }
        };
        // An assignment in front of the builtin (e.g. `IFS=: read x') is
        // visible to it.  It is always temporary: bash undoes it after the
        // command, even for the POSIX special builtins.
        let assignments = apply_command_assignments(assigns, sh);
        let array_saves = apply_array_assignments(array_assigns, sh);
        let code = builtins::run_builtin(argv, sh).unwrap_or(0);
        restore_array_assignments(array_saves, sh);
        restore_command_assignments(assignments, sh);
        if !sh.persistent_redirs {
            restore_fds(&saved);
        }
        // `$_` becomes the last word of the command's expanded argv.
        update_underscore(sh, argv);
        return code;
    }

    // External command: fork and apply the redirections in the child.  The
    // program is resolved in the parent, so that the hash table can record
    // it (bash does the same when the `hashall' option is on).
    // A command-prefixed array assignment (`a=(1) prog`) is visible only to this
    // command; we apply it temporarily and restore it afterwards.
    let array_saves = apply_array_assignments(array_assigns, sh);
    let program = find_program(&argv[0], path);
    // 前台作业控制：仅交互式 + tty 的主 shell 接管终端。job_ctl / shell_pgrp
    // 都在 fork 前算好，子进程继承同一份值；-c / piped（isatty 判假）整段跳过。
    let job_ctl = job_control_active(sh);
    let shell_pgrp = if job_ctl { posix::getpgrp() } else { -1 };
    let pid = fork_child();
    if pid == 0 {
        if job_ctl {
            // 子进程自立门户（pgid = 自身 pid），脱离 shell 的进程组。
            posix::setpgid(0, 0);
        }
        apply_exp_redirs(redirs, sh.options.noclobber);
        let attrs = sh.attr_refs();
        // The command environment (exported vars + this command's own
        // assignments) is built here only -- the builtin / function /
        // assignment / compound paths above never needed it.
        let cmd_env = build_cmd_env(env_values, sh);
        let ctx = ChildContext {
            env: &cmd_env,
            shell_env: &sh.env,
            arrays: &sh.arrays,
            options: &sh.options,
            positional: &sh.positional,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            bg_pid: sh.bg_pid,
            compound: None,
            program: program.as_ref().map(|(exe, _)| exe),
        };
        child_run_exp(argv, path, &ctx);
    } else {
        if let Some((_, reported)) = &program {
            record_hash_entry(sh, &argv[0], reported);
        }
        if job_ctl {
            // 父进程也设置一次，消除父子竞态（谁先执行都不影响结果）。
            posix::setpgid(pid, pid);
            // wait 之前把终端前台进程组交给 job：此后 Ctrl-C 只到 job。
            posix::tcsetpgrp(0, pid);
        }
        let status = posix::wait_pid(pid);
        if job_ctl {
            // 硬要求：恢复 shell 为终端前台进程组，否则 shell 之后读 stdin
            // 会收到 SIGTTIN 被挂起。
            posix::tcsetpgrp(0, shell_pgrp);
        }
        update_underscore(sh, argv);
        restore_array_assignments(array_saves, sh);
        status
    }
}

/// A fully prepared (expanded) command: argv, redirections, the assignments
/// it carried, and its (lazily built) private environments.
///
/// The two private environments (`cmd_env`: exported vars + the command's
/// own assignments, for `execve`; `shell_env`: the full table a forked child
/// shell starts with) are NOT materialized here.  Cloning two ~30-entry
/// `HashMap`s of `String`s per simple command cost more than the command
/// itself (a `[` builtin never uses either map), so only the expanded
/// assignment values are kept and the maps are built on demand by the
/// paths that actually fork/exec.
struct Prepared {
    argv: Vec<String>,
    redirs: Vec<ExpRedir>,
    assigns: Vec<(String, Vec<WordPart>, bool)>,
    /// Array assignments (`name=(...)`) carried by this command.
    array_assigns: Vec<(String, Vec<Vec<WordPart>>, bool)>,
    /// The compound command (`if ... fi`) this command stands for.
    compound: Option<Compound>,
    /// The expanded `(name, value, append)` assignment values.  `None` when
    /// the command has no words (a standalone assignment / compound), which
    /// the old code represented by an empty `cmd_env` + a value-less
    /// `sh.env.clone()` as `shell_env`.
    env_values: Option<Vec<(String, String, bool)>>,
}

impl Prepared {
    /// The environment the command's program receives (`execve`).
    fn build_cmd_env(&self, sh: &Shell) -> HashMap<String, String> {
        build_cmd_env(self.env_values.as_deref(), sh)
    }

    /// The variable table a forked child shell starts with.
    fn build_shell_env(&self, sh: &Shell) -> HashMap<String, String> {
        let mut shell_env = sh.env.clone();
        if let Some(values) = &self.env_values {
            for (n, v, ap) in values {
                if *ap {
                    let cur = shell_env.get(n).cloned().unwrap_or_default();
                    shell_env.insert(n.clone(), cur + v);
                } else {
                    shell_env.insert(n.clone(), v.clone());
                }
            }
        }
        shell_env
    }
}

/// Build the environment a program receives: the exported variables plus the
/// command's own (already expanded) assignments.  `None` (a command without
/// words) yields an empty map, faithful to the pre-lazy behavior.
fn build_cmd_env(values: Option<&[(String, String, bool)]>, sh: &Shell) -> HashMap<String, String> {
    match values {
        None => HashMap::new(),
        Some(values) => {
            let mut cmd_env = sh.child_env();
            for (n, v, ap) in values {
                if *ap {
                    let cur = cmd_env.get(n).cloned().unwrap_or_default();
                    cmd_env.insert(n.clone(), cur + v);
                } else {
                    cmd_env.insert(n.clone(), v.clone());
                }
            }
            cmd_env
        }
    }
}

/// The maximum alias expansion depth (guards against an alias whose body
/// expands to itself).
const MAX_ALIAS_DEPTH: usize = 16;

/// The name of the alias a command word refers to, if any.
///
/// Only a plain, unquoted literal word can name an alias; anything else is
/// left alone (bash behaves the same way).
fn alias_name(word: &[WordPart]) -> Option<String> {
    match word {
        [WordPart::Text(text, false)] => Some(text.clone()),
        _ => None,
    }
}

/// Expand the aliases used by the commands of a command list.
///
/// Bash expands aliases only in interactive shells (the `expand_aliases`
/// shell option is off by default in non-interactive shells), so the
/// expansion is skipped for `wash -c` and for scripts.
fn expand_aliases(list: &CommandList, sh: &Shell, depth: usize) -> CommandList {
    if !(sh.interactive || sh.expand_aliases) {
        return list.clone();
    }
    let mut out = CommandList {
        andors: Vec::new(),
        func_defs: Vec::new(),
    };
    for andor in &list.andors {
        expand_andor_aliases(andor, sh, depth, &mut out);
    }
    out
}

/// Expand the aliases of one AND-OR list, appending the resulting lists to
/// `out`.
///
/// Bash replaces the command word with the alias text and parses the result
/// again, so an alias body may contain complete commands, pipes or `&&`/`||`
/// operators. The alias text takes the place of the command it replaces; an
/// alias body with several `;`-separated commands therefore splits the
/// AND-OR list it appears in.
fn expand_andor_aliases(andor: &AndOr, sh: &Shell, depth: usize, out: &mut CommandList) {
    // The AND-OR list under construction.
    let mut pipelines: Vec<Pipeline> = Vec::new();
    let mut ops: Vec<AndOrOp> = Vec::new();

    for (i, pipeline) in andor.pipelines.iter().enumerate() {
        // The operator that precedes this pipeline (`None` for the first).
        let op_before: Option<AndOrOp> = if i == 0 { None } else { Some(andor.ops[i - 1]) };
        let expansion = if depth < MAX_ALIAS_DEPTH {
            expand_alias_in_pipeline(pipeline, sh, depth)
        } else {
            None
        };
        match expansion {
            None => {
                pipelines.push(pipeline.clone());
                if let Some(op) = op_before {
                    ops.push(op);
                }
            }
            Some(groups) => {
                for (gi, group) in groups.iter().enumerate() {
                    if gi > 0 {
                        // The alias body contained a `;`: the commands that
                        // follow it start a new AND-OR list.
                        flush_andor_list(out, &mut pipelines, &mut ops);
                    }
                    for (j, p) in group.pipelines.iter().enumerate() {
                        pipelines.push(p.clone());
                        let op = match (gi, j) {
                            (0, 0) => op_before,
                            (0, _) => Some(group.ops[j - 1]),
                            (_, 0) => None,
                            (_, _) => Some(group.ops[j - 1]),
                        };
                        if let Some(op) = op {
                            ops.push(op);
                        }
                    }
                }
            }
        }
    }
    flush_andor_list(out, &mut pipelines, &mut ops);
}

/// Append the AND-OR list under construction to `out`.
fn flush_andor_list(out: &mut CommandList, pipelines: &mut Vec<Pipeline>, ops: &mut Vec<AndOrOp>) {
    if pipelines.is_empty() {
        return;
    }
    out.andors.push(AndOr {
        pipelines: std::mem::take(pipelines),
        ops: std::mem::take(ops),
    });
}

/// If the first command of `pipeline` starts with an alias, return the
/// AND-OR lists the alias expands to.
///
/// Bash replaces the command word with the alias text and parses the result
/// again, so an alias body may contain complete commands, pipes or
/// redirections. The arguments of the original command are appended to the
/// last command of the alias body.
fn expand_alias_in_pipeline(
    pipeline: &Pipeline,
    sh: &Shell,
    depth: usize,
) -> Option<Vec<AndOr>> {
    let first = pipeline.commands.first()?;
    let name = alias_name(first.argv.first()?)?;
    let value = sh.aliases.get(&name)?.clone();
    let tokens = crate::lexer::tokenize(&value).ok()?;
    let list = crate::parser::parse_command_list(&tokens).ok()?;
    if list.andors.is_empty() {
        return None;
    }
    let mut groups = list.andors;
    let extra: Vec<Vec<WordPart>> = first.argv[1..].to_vec();
    if !extra.is_empty() {
        if let Some(last) = groups
            .last_mut()
            .and_then(|g| g.pipelines.last_mut())
            .and_then(|pl| pl.commands.last_mut())
        {
            last.argv.extend(extra);
        }
    }
    // The alias body may itself start with an alias.
    let mut out = Vec::new();
    for group in &groups {
        let single = CommandList {
            andors: vec![group.clone()],
            func_defs: Vec::new(),
        };
        out.extend(expand_aliases(&single, sh, depth + 1).andors);
    }
    Some(out)
}

/// Expand the words of ONE pipeline stage and build the environments it runs
/// with.
///
/// This is called for a single stage, either in the parent (the one-stage
/// pipeline, which bash runs in the current shell) or -- for a multi-stage
/// pipeline -- inside each forked child, so that every stage expands its OWN
/// words in its OWN process (the bash subshell-per-stage model).  When the
/// expander reports a fatal expansion error (`set -u` unbound variable,
/// `${var?word}` or a failed arithmetic expansion) it sets a thread-local
/// flag; the caller then aborts *this* stage (exit 127 / 1) instead of the
/// whole command line.
fn prepare_stage(c: &SimpleCommand, sh: &mut Shell) -> Prepared {
    let pending: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());

    // Expand the stage's words.  The expander only reads the environment.
    let (argv, redirs) = {
        let attrs = sh.attr_refs();
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nounset: sh.options.nounset,
            shell_flags: sh.dash_flags(),
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            pending_assign: Some(&pending),
        };
        let argv = expand_argv(&c.argv, &ctx);
        let redirs = expand_redirs(&c.redirs, &ctx);
        (argv, redirs)
    };

    // Apply the assignments the expansion requested (`${var=word}`).
    for (name, value) in pending.borrow_mut().drain(..) {
        sh.env.insert(name, value);
    }

    // Expand the assignment RHSes (`v=$(...)` etc.) exactly once.  The two
    // env MAPS that used to be built here (the exported-only `cmd_env` and a
    // full `shell_env` clone) are now built LAZILY by the consumers that
    // actually fork or exec: a `[` builtin never reads either map, and the
    // two ~30-entry HashMap deep clones dominated the cost of every simple
    // command.  `None` = no command word (standalone assignment / compound),
    // matching the old "(empty cmd_env, value-less sh.env.clone())" pair.
    let env_values = if argv.is_empty() {
        None
    } else {
        let attrs = sh.attr_refs();
        let value_ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nounset: sh.options.nounset,
            shell_flags: sh.dash_flags(),
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            pending_assign: Some(&pending),
        };
        let values: Vec<(String, String, bool)> = c
            .assignments
            .iter()
            .map(|(n, vp, ap)| (n.clone(), expand_scalar(vp, &value_ctx), *ap))
            .collect();
        Some(values)
    };
    // The expansion of the assignments themselves may have requested further
    // assignments; they are applied to the shell as well.
    for (name, value) in pending.borrow_mut().drain(..) {
        sh.env.insert(name, value);
    }
    Prepared {
        argv,
        redirs,
        assigns: c.assignments.clone(),
        array_assigns: c.array_assignments.clone(),
        compound: c.compound.clone(),
        env_values,
    }
}

/// Expand and run a single pipeline stage in the current (already forked)
/// child process.
///
/// Each stage is its own subshell in bash, so it must expand its OWN words;
/// a `set -u` unbound variable or a failed arithmetic expansion only kills
/// this stage (the child exits 127 / 1 after printing the diagnostic) while
/// the other stages and the rest of the command line keep running.
fn child_prepare_and_run(c: &SimpleCommand, sh: &mut Shell, path: &str, noclobber: bool) -> ! {
    let pr = prepare_stage(c, sh);
    // A fatal parameter-expansion error (`set -u`, `${var?word}`) aborts only
    // this stage, with status 127.  A failed arithmetic expansion aborts only
    // this stage, with status 1.
    if let Some(code) = crate::expand::expansion_abort_code() {
        posix::exit(code);
    }
    if crate::expand::arith_failed() {
        posix::exit(1);
    }
    apply_exp_redirs(&pr.redirs, noclobber);
    let attrs = sh.attr_refs();
    let program = if pr.argv.is_empty() || builtins::is_builtin(sh, &pr.argv[0]) {
        None
    } else {
        find_program(&pr.argv[0], path)
    };
    // This stage runs in an already-forked child: it always needs both
    // private environments, so they are built here on demand.
    let cmd_env = pr.build_cmd_env(sh);
    let shell_env = pr.build_shell_env(sh);
    let ctx = ChildContext {
        env: &cmd_env,
        shell_env: &shell_env,
        arrays: &sh.arrays,
        options: &sh.options,
        positional: &sh.positional,
        dollar_zero: &sh.dollar_zero,
        attrs: &attrs,
        bg_pid: sh.bg_pid,
        compound: pr.compound.as_ref(),
        program: program.as_ref().map(|(exe, _)| exe),
    };
    child_run_exp(&pr.argv, path, &ctx);
}

fn execute_pipeline(p: &Pipeline, sh: &mut Shell, path: &str) -> i32 {
    if p.background {
        return execute_background(p, sh, path);
    }
    let noclobber = sh.options.noclobber;
    let pipefail = sh.options.pipefail;
    let n = p.commands.len();
    if n == 1 {
        // A one-stage pipeline is run in the current shell (bash does the
        // same when job control is off), so its words are expanded here in the
        // parent.  A `set -u` unbound variable or a failed arithmetic
        // expansion is therefore fatal for the whole command line, exactly
        // like a standalone command.
        let pr = prepare_stage(&p.commands[0], sh);
        // A failed arithmetic expansion (or a negative-length array slice) is
        // fatal for the whole command line: bash never runs `echo` when one of
        // its own arguments failed to expand.
        // A `set -u` unbound variable or a failed `${var?word}` is fatal for
        // the whole line (status 127), just like a bad arithmetic expansion
        // (status 1).
        if let Some(code) = crate::expand::expansion_abort_code() {
            return code;
        }
        if crate::expand::arith_failed() {
            return 1;
        }
        return run_standalone_exp(
            &pr.argv,
            &pr.redirs,
            &pr.assigns,
            &pr.array_assigns,
            pr.compound.as_ref(),
            pr.env_values.as_deref(),
            sh,
            path,
        );
    }

    let mut prev_read: Option<i32> = None;
    let mut pids: Vec<i32> = Vec::new();
    // 管道所有阶段共用一个 job 进程组（第一个阶段的 pid），不要一阶段一组。
    let job_ctl = job_control_active(sh);
    let shell_pgrp = if job_ctl { posix::getpgrp() } else { -1 };
    let mut job_pgid: Option<i32> = None;

    for (idx, c) in p.commands.iter().enumerate() {
        let make_pipe = idx < n - 1;
        let (this_read, this_write) = if make_pipe {
            let fds = posix::pipe();
            (Some(fds[0]), Some(fds[1]))
        } else {
            (None, None)
        };

        // Each stage is a separate subshell that expands its OWN words (the
        // bash subshell-per-stage model).  The expansion -- and any fatal
        // `set -u` / arithmetic error it triggers -- is confined to the child,
        // so an unbound variable in one stage only kills that stage while the
        // rest of the command line keeps running.
        let pid = fork_child();
        if pid == 0 {
            if job_ctl {
                // 第一个阶段建立新进程组（pgid = 自身 pid），其余阶段加入
                // 同一个组；job_pgid 是 fork 时从父进程继承来的快照。
                posix::setpgid(0, job_pgid.unwrap_or(0));
            }
            if let Some(fd) = prev_read {
                posix::dup2(fd, 0);
                posix::close(fd);
            }            if let Some(fd) = this_write {
                posix::dup2(fd, 1);
                posix::close(fd);
            }
            if let Some(fd) = this_read {
                posix::close(fd);
            }
            child_prepare_and_run(c, sh, path, noclobber);
        } else {
            // The stage runs in a subshell, so its hash entry is lost --
            // exactly like in bash.
            if let Some(fd) = prev_read {
                posix::close(fd);
            }
            if let Some(fd) = this_write {
                posix::close(fd);
            }
            if let Some(fd) = this_read {
                prev_read = Some(fd);
            }
            if job_ctl {
                // 第一个 fork 的 pid 就是整条管道的 job 组 id；父进程也设置
                // 一次以消除竞态。
                let pg = *job_pgid.get_or_insert(pid);
                posix::setpgid(pid, pg);
            }
            pids.push(pid);
        }
    }

    if let Some(fd) = prev_read {
        posix::close(fd);
    }

    if job_ctl {
        if let Some(pg) = job_pgid {
            // 所有阶段 fork 完毕、wait 之前，把终端前台进程组交给整条管道。
            posix::tcsetpgrp(0, pg);
        }
    }

    let mut last = 0;
    let mut failure = 0;
    for pid in pids {
        let status = posix::wait_pid(pid);
        last = status;
        if status != 0 {
            failure = status;
        }
    }
    if job_ctl {
        // 硬要求：所有子进程 wait 完毕后恢复 shell 的终端前台进程组。
        posix::tcsetpgrp(0, shell_pgrp);
    }
    // With `set -o pipefail` the pipeline fails when any stage failed; the
    // status is then the one of the rightmost failing stage.
    if pipefail && failure != 0 {
        failure
    } else {
        last
    }
}

/// Run the pipeline in the background (`cmd &`).
///
/// We fork once; the child runs the whole pipeline normally (with the
/// `background` flag cleared so it does not recurse) and then exits with that
/// pipeline's status.  The parent records the child pid in `Shell.jobs`, sets
/// `$!`, prints the bash-style `[job] pid` line and returns immediately, so the
/// REPL stays responsive.  This is the scaffolding the `jobs`/`fg`/`bg`/`wait`
/// builtins build on.
fn execute_background(p: &Pipeline, sh: &mut Shell, path: &str) -> i32 {
    let pid = fork_child();
    if pid == 0 {
        // 后台作业的子进程沿用父 Shell（interactive 仍为 true），但它不该
        // 接管终端前台进程组；置位后其内部的 execute_pipeline 会跳过接线。
        SUPPRESS_JOB_CONTROL.store(true, std::sync::atomic::Ordering::Relaxed);
        // 子进程：像前台一样跑完整个流水线（每个阶段会再 fork 一次并等待），
        // 然后用该流水线的状态退出。
        let mut p2 = p.clone();
        p2.background = false;
        let code = execute_pipeline(&p2, sh, path);
        posix::exit(code);
    }
    // 父进程：登记真实作业条目。子进程 fork 后走 pid==0 分支并 posix::exit，
    // 不会执行到这里；且子进程持有独立的 Shell 副本，其内部若再起后台作业
    // 也只改自己的副本，不会污染父 shell 的作业表。
    sh.jobs.push(pid);
    sh.job_table.push(crate::shell::JobInfo {
        pid,
        // 命令文本取自流水线渲染（不含结尾的 `&`），是真实输入，非伪造。
        command: crate::builtins::render_pipeline(p),
        done: false,
    });
    sh.bg_pid = pid;
    let jobno = sh.jobs.len();
    // bash 只在交互式 shell（且打开了作业控制）里通报后台作业的启动；
    // 非交互的 `bash -c 'sleep 1 &'` 什么都不打印。通报信息走 stderr。
    if sh.interactive {
        eprintln!("[{}] {}", jobno, pid);
    }
    0
}

/// Run one AND-OR list: the pipelines joined by `&&` / `||`, evaluated from
/// left to right with short-circuiting.
///
/// `last` carries the exit status of the last command that actually ran; it
/// is updated in place so that `$?` reflects it inside the next pipeline.
///
/// Returns `true` when `set -e` aborted the list; the caller then stops
/// executing the remaining lists as well.
fn execute_andor(andor: &AndOr, sh: &mut Shell, path: &str, last: &mut i32) -> bool {
    let count = andor.pipelines.len();
    for (i, pipeline) in andor.pipelines.iter().enumerate() {
        if i > 0 {
            // Short-circuit: `&&` continues only after a success, `||` only
            // after a failure. A skipped pipeline leaves `$?` untouched.
            let skip = match andor.ops[i - 1] {
                AndOrOp::And => *last != 0,
                AndOrOp::Or => *last == 0,
            };
            if skip {
                continue;
            }
        }
        // `set -n` (noexec): the command is read but neither expanded nor
        // executed, exactly like in bash.
        if sh.options.noexec {
            continue;
        }
        *last = execute_pipeline(pipeline, sh, path);
        // Propagate exit status so `$?` reflects the previous command.
        sh.last_status = *last;
        // A fatal expansion error stops the AND-OR list as well: bash does
        // not run the fallback in `echo $((1/0)) || echo fallback`.
        if crate::expand::arith_failed() || crate::expand::expansion_abort_code().is_some() {
            return true;
        }
        // `exit` stops the shell, `set -e` stops it on a failure.
        if sh.exit_requested.is_some() {
            return false;
        }
        // With `set -e` the shell only aborts when the failing command is the
        // last one of the AND-OR list: a command that is part of a `&&`/`||`
        // list (but not the one following the final operator) never triggers
        // the exit.
        if sh.options.errexit && *last != 0 && i + 1 == count {
            return true;
        }
    }
    false
}

pub fn execute(list: &CommandList, sh: &mut Shell) -> i32 {
    // Make every function defined anywhere in this list (including nested
    // ones) callable before any command runs.
    register_functions(list, sh);

    let path = sh.env.get("PATH").cloned().unwrap_or_default();

    let mut last = 0;
    // Aliases are expanded before the commands run.  When aliases cannot
    // apply (non-interactive and `expand_aliases` off -- the common `-c` /
    // script case) we use the list in place: `expand_aliases` otherwise
    // deep-clones the whole AST for EVERY `execute` call, which made each
    // iteration of a shell loop re-allocate its entire body.
    let expanded_owned;
    let expanded: &CommandList = if sh.interactive || sh.expand_aliases {
        expanded_owned = expand_aliases(list, sh, 0);
        &expanded_owned
    } else {
        list
    };
    for andor in &expanded.andors {
        let aborted = execute_andor(andor, sh, &path, &mut last);
        // A fatal expansion error aborts every enclosing command list,
        // including `if` / loop / function bodies that called us.
        if crate::expand::arith_failed() || crate::expand::expansion_abort_code().is_some() {
            break;
        }
        // `exit` stops the shell; `return` / `break` / `continue` stop the
        // current command list and leave their flags set so the enclosing
        // function or loop can consume them (at the top level they are
        // harmless leftovers, cleared by `run_str`).
        if sh.exit_requested.is_some()
            || sh.return_requested.is_some()
            || sh.break_requested.is_some()
            || sh.continue_requested.is_some()
        {
            break;
        }
        if aborted {
            break;
        }
        // A failed assignment to a readonly variable aborts the rest of the
        // command list, exactly like bash does (`readonly R=1; R=2; echo hi`
        // never reaches the `echo`).
        if sh.fatal_error.is_some() {
            break;
        }
    }
    // A fatal expansion error overrides the status of the enclosing compound
    // command: `if echo $((1/0)); then ...; fi` has no `else` and would
    // otherwise report success, while bash reports 1 for the whole line.
    // Parameter-expansion failures carry 127 (`set -u`, `${var?word}`).
    if let Some(code) = crate::expand::expansion_abort_code() {
        code
    } else if crate::expand::arith_failed() {
        1
    } else {
        last
    }
}
