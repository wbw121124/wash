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

use crate::builtins;
use crate::expand::{expand_argv, expand_scalar, expand_word, ExpandCtx};
use crate::lexer::WordPart;
use crate::parser::{AndOr, AndOrOp, CommandList, Pipeline, Redir};
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

/// Look `name` up in `path` (or directly, when it contains a slash).
pub(crate) fn find_executable(name: &str, path: &str) -> Option<CString> {
    if name.contains('/') || name.contains('\\') {
        if posix::access(name, posix::X_OK) == 0 {
            return Some(posix::to_cstr(name));
        }
        let exe = format!("{}.exe", name);
        if posix::access(&exe, posix::X_OK) == 0 {
            return Some(posix::to_cstr(&exe));
        }
        return None;
    }
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = format!("{}/{}", dir, name);
        if posix::access(&cand, posix::X_OK) == 0 {
            return Some(posix::to_cstr(&cand));
        }
        let cand_exe = format!("{}.exe", cand);
        if posix::access(&cand_exe, posix::X_OK) == 0 {
            return Some(posix::to_cstr(&cand_exe));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Redirections (post-expansion)
// ---------------------------------------------------------------------------

enum ExpRedir {
    In(i32, String),
    Out(i32, String, bool),
    Both(String),
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
            Redir::Out { fd, file, append } => ExpRedir::Out(*fd, expand_file(file, ctx), *append),
            Redir::Both { file } => ExpRedir::Both(expand_file(file, ctx)),
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
            ExpRedir::Out(fd, file, append) => {
                let f = posix::open_write(file, *append, !*append && noclobber);
                if f >= 0 {
                    posix::dup2(f, *fd);
                    posix::close(f);
                }
            }
            ExpRedir::Both(file) => {
                let f = posix::open_write(file, false, false);
                if f >= 0 {
                    posix::dup2(f, 1);
                    posix::dup2(f, 2);
                    posix::close(f);
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
            ExpRedir::Out(fd, file, append) => {
                let f = posix::open_write(file, *append, !*append && noclobber);
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
            ExpRedir::Both(file) => {
                let f = posix::open_write(file, false, false);
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

/// Apply a command's variable assignments on top of `base` to build its
/// (non-persistent) environment.
///
/// `base` must be the *exported* variables only: a command inherits the
/// exported variables of the shell plus its own temporary assignments.
fn build_cmd_env(
    base: &HashMap<String, String>,
    assigns: &[(String, Vec<WordPart>, bool)],
    ctx: &ExpandCtx,
) -> HashMap<String, String> {
    let mut env = base.clone();
    for (name, valparts, append) in assigns {
        let val = expand_scalar(valparts, ctx);
        if *append {
            let cur = env.get(name).cloned().unwrap_or_default();
            env.insert(name.clone(), cur + &val);
        } else {
            env.insert(name.clone(), val);
        }
    }
    env
}

/// Run `cmd` in a forked subshell, capturing its stdout (with one trailing
/// newline stripped, matching bash).
pub fn capture_command(
    cmd: &str,
    env: &HashMap<String, String>,
    attrs: &crate::shell::VarAttrs,
) -> String {
    let tokens = match crate::lexer::tokenize(cmd) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let list = match crate::parser::parse_command_list(&tokens) {
        Ok(l) => l,
        Err(_) => return String::new(),
    };
    let fds = posix::pipe();
    let r = fds[0];
    let w = fds[1];
    let pid = posix::fork();
    if pid == 0 {
        posix::dup2(w, 1);
        posix::close(w);
        posix::close(r);
        let mut sh = Shell::from_env(env.clone());
        sh.apply_attrs(attrs);
        let code = crate::executor::execute(&list, &mut sh);
        let _ = std::io::stdout().flush();
        posix::exit(code);
    }
    posix::close(w);
    let s = posix::read_all(r);
    posix::close(r);
    posix::wait_pid(pid);
    if let Some(stripped) = s.strip_suffix('\n') {
        stripped.to_string()
    } else {
        s
    }
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
    options: &'a SetOptions,
    positional: &'a [String],
    dollar_zero: &'a str,
    attrs: &'a crate::shell::VarAttrs,
}

/// Run a command inside the forked child: either a builtin (whose state
/// changes are lost with the child) or an external program.
fn child_run_exp(argv: &[String], path: &str, ctx: &ChildContext) -> ! {
    if argv.is_empty() {
        posix::exit(0);
    }
    // The redirections have already been applied by the caller (pipes and the
    // command's own redirections), so the builtins can simply write to stdout.
    let mut sub = Shell::from_env(ctx.shell_env.clone());
    sub.apply_attrs(ctx.attrs);
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
    match find_executable(&argv[0], path) {
        Some(exe) => {
            posix::execve(&exe, &argvec.ptrs, &env_ptrs);
        }
        None => {
            eprintln!("wash: {}: command not found", argv[0]);
        }
    }
    posix::exit(127);
}

/// Run a single (non-pipeline) command in the parent shell.
fn run_standalone_exp(
    argv: &[String],
    redirs: &[ExpRedir],
    assigns: &[(String, Vec<WordPart>, bool)],
    env: &HashMap<String, String>,
    sh: &mut Shell,
    path: &str,
) -> i32 {
    if argv.is_empty() {
        // Standalone assignment(s): persist to the shell environment.
        if !assigns.is_empty() {
            let attrs = sh.attrs();
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
                    dollar_zero: &sh.dollar_zero,
                    attrs: &attrs,
                    pending_assign: Some(&pending),
                };
                assigns
                    .iter()
                    .map(|(n, vp, ap)| (n.clone(), expand_scalar(vp, &ctx), *ap))
                    .collect()
            };
            for (name, val, append) in applied {
                if append {
                    let cur = sh.env.get(&name).cloned().unwrap_or_default();
                    sh.env.insert(name, cur + &val);
                } else {
                    sh.env.insert(name, val);
                }
            }
            // Apply the assignments the expansion requested (`${var=word}`).
            for (name, value) in pending.borrow_mut().drain(..) {
                sh.env.insert(name, value);
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
        return 0;
    }

    if sh.options.xtrace {
        trace_command(argv);
    }

    // Builtins run in the current shell so that their side effects survive.
    if builtins::is_builtin(&argv[0]) {
        sh.persistent_redirs = false;
        let saved = match apply_redirs_in_parent(redirs, sh.options.noclobber) {
            Ok(saved) => saved,
            Err(message) => {
                eprintln!("{}", message);
                return 1;
            }
        };
        let code = builtins::run_builtin(argv, sh).unwrap_or(0);
        if !sh.persistent_redirs {
            restore_fds(&saved);
        }
        return code;
    }

    // External command: fork and apply the redirections in the child.
    let pid = posix::fork();
    if pid == 0 {
        apply_exp_redirs(redirs, sh.options.noclobber);
        let attrs = sh.attrs();
        let ctx = ChildContext {
            env,
            shell_env: &sh.env,
            options: &sh.options,
            positional: &sh.positional,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
        };
        child_run_exp(argv, path, &ctx);
    } else {
        posix::wait_pid(pid)
    }
}

/// A fully prepared (expanded) command: argv, redirections, the assignments
/// it carried, and its private environments.
struct Prepared {
    argv: Vec<String>,
    redirs: Vec<ExpRedir>,
    assigns: Vec<(String, Vec<WordPart>, bool)>,
    /// The environment the command's program receives.
    env: HashMap<String, String>,
    /// The variable table a forked child shell starts with.
    shell_env: HashMap<String, String>,
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
    let mut out = CommandList { andors: Vec::new() };
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
        };
        out.extend(expand_aliases(&single, sh, depth + 1).andors);
    }
    Some(out)
}

/// Expand the words of every command of a pipeline.
///
/// The expansion happens in two steps: first the words are expanded (the
/// expander only reads the environment), then the assignments the expansion
/// requested (`${var=word}`) are written back to the shell and the
/// environments of the commands are built.
fn prepare(p: &Pipeline, sh: &mut Shell) -> Vec<Prepared> {
    let attrs = sh.attrs();
    let pending: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());

    // Step 1: expand the words of every command.
    let mut expanded: Vec<(Vec<String>, Vec<ExpRedir>)> = Vec::new();
    {
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nounset: sh.options.nounset,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            pending_assign: Some(&pending),
        };
        for c in &p.commands {
            let argv = expand_argv(&c.argv, &ctx);
            let redirs = expand_redirs(&c.redirs, &ctx);
            expanded.push((argv, redirs));
        }
    }

    // Step 2: apply the assignments the expansion requested.
    for (name, value) in pending.borrow_mut().drain(..) {
        sh.env.insert(name, value);
    }

    // Step 3: build the environment of every command.
    let mut out = Vec::new();
    {
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nounset: sh.options.nounset,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            pending_assign: Some(&pending),
        };
        for (c, (argv, redirs)) in p.commands.iter().zip(expanded.into_iter()) {
            // Only the exported variables are passed on to the program ...
            let cmd_env = build_cmd_env(&sh.child_env(), &c.assignments, &ctx);
            // ... but a forked child shell inherits every shell variable.
            let shell_env = build_cmd_env(&sh.env, &c.assignments, &ctx);
            out.push(Prepared {
                argv,
                redirs,
                assigns: c.assignments.clone(),
                env: cmd_env,
                shell_env,
            });
        }
    }
    // The expansion of the assignments themselves may have requested further
    // assignments; they are applied to the shell as well.
    for (name, value) in pending.borrow_mut().drain(..) {
        sh.env.insert(name, value);
    }
    out
}

fn execute_pipeline(p: &Pipeline, sh: &mut Shell, path: &str) -> i32 {
    let noclobber = sh.options.noclobber;
    let pipefail = sh.options.pipefail;
    let prepared = prepare(p, sh);
    let n = prepared.len();
    if n == 1 {
        let pr = &prepared[0];
        return run_standalone_exp(&pr.argv, &pr.redirs, &pr.assigns, &pr.env, sh, path);
    }

    let mut prev_read: Option<i32> = None;
    let mut pids: Vec<i32> = Vec::new();

    for (idx, pr) in prepared.iter().enumerate() {
        let make_pipe = idx < n - 1;
        let (this_read, this_write) = if make_pipe {
            let fds = posix::pipe();
            (Some(fds[0]), Some(fds[1]))
        } else {
            (None, None)
        };

        let pid = posix::fork();
        if pid == 0 {
            if let Some(fd) = prev_read {
                posix::dup2(fd, 0);
                posix::close(fd);
            }
            if let Some(fd) = this_write {
                posix::dup2(fd, 1);
                posix::close(fd);
            }
            if let Some(fd) = this_read {
                posix::close(fd);
            }
            apply_exp_redirs(&pr.redirs, noclobber);
            if pr.argv.is_empty() {
                posix::exit(0);
            }
            let attrs = sh.attrs();
            let ctx = ChildContext {
                env: &pr.env,
                shell_env: &pr.shell_env,
                options: &sh.options,
                positional: &sh.positional,
                dollar_zero: &sh.dollar_zero,
                attrs: &attrs,
            };
            child_run_exp(&pr.argv, path, &ctx);
        } else {
            if let Some(fd) = prev_read {
                posix::close(fd);
            }
            if let Some(fd) = this_write {
                posix::close(fd);
            }
            if let Some(fd) = this_read {
                prev_read = Some(fd);
            }
            pids.push(pid);
        }
    }

    if let Some(fd) = prev_read {
        posix::close(fd);
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
    // With `set -o pipefail` the pipeline fails when any stage failed; the
    // status is then the one of the rightmost failing stage.
    if pipefail && failure != 0 {
        failure
    } else {
        last
    }
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
    let path = sh.env.get("PATH").cloned().unwrap_or_default();

    let mut last = 0;
    // Aliases are expanded before the commands run.
    let expanded = expand_aliases(list, sh, 0);
    for andor in &expanded.andors {
        let aborted = execute_andor(andor, sh, &path, &mut last);
        if sh.exit_requested.is_some() {
            break;
        }
        if aborted {
            break;
        }
    }
    last
}
