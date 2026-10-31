// Phase 1 executor: expand words, then run the AST via fork/execve. Also
// provides `capture_command` for command/process substitution (runs the inner
// command in a forked subshell and returns its stdout).
//
// Pipeline plumbing, redirections, and exit-status decoding go through the
// msys POSIX shim in `posix.rs`.

use std::collections::HashMap;
use std::ffi::{c_char, CString};

use crate::expand::{expand_argv, expand_scalar, expand_word, ExpandCtx};
use crate::lexer::WordPart;
use crate::parser::{CommandList, Pipeline, Redir};
use crate::posix;
use crate::shell::Shell;

/// Holds CStrings + a null-terminated pointer array for an argv/envp vector.
struct ArgVec {
    _cstrings: Vec<CString>,
    ptrs: Vec<*const c_char>,
}

impl ArgVec {
    fn from(args: &[String]) -> Self {
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

fn build_env_ptrs(env: &HashMap<String, String>) -> (Vec<CString>, Vec<*const c_char>) {
    let cstrings: Vec<CString> = env
        .iter()
        .map(|(k, v)| CString::new(format!("{}={}", k, v)).unwrap_or_default())
        .collect();
    let mut ptrs: Vec<*const c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (cstrings, ptrs)
}

fn find_executable(name: &str, path: &str) -> Option<CString> {
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

fn try_builtin(argv: &[String], sh: Option<&mut Shell>) -> Option<i32> {
    let name = argv.first()?;
    match name.as_str() {
        "cd" => {
            let target = argv.get(1).map(|s| s.as_str()).unwrap_or("");
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
            let dest: &str = if target.is_empty() { &home } else { target };
            if posix::chdir(dest) != 0 {
                eprintln!("wash: cd: {}: No such file or directory", dest);
                return Some(1);
            }
            if let Some(sh) = sh {
                sh.cwd = dest.to_string();
                sh.env.insert("PWD".to_string(), dest.to_string());
            }
            Some(0)
        }
        "exit" => Some(0),
        _ => None,
    }
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

fn apply_exp_redirs(redirs: &[ExpRedir]) {
    for r in redirs {
        match r {
            ExpRedir::In(fd, file) => {
                let f = posix::open_read(file);
                posix::dup2(f, *fd);
                posix::close(f);
            }
            ExpRedir::Out(fd, file, append) => {
                let f = posix::open_write(file, *append);
                posix::dup2(f, *fd);
                posix::close(f);
            }
            ExpRedir::Both(file) => {
                let f = posix::open_write(file, false);
                posix::dup2(f, 1);
                posix::dup2(f, 2);
                posix::close(f);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Command substitution capture
// ---------------------------------------------------------------------------

/// Apply a command's variable assignments on top of `base` to build its
/// (non-persistent) environment.
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
pub fn capture_command(cmd: &str, env: &HashMap<String, String>) -> String {
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
        crate::executor::execute(&list, &mut sh);
        posix::exit(0);
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

fn child_run_exp(argv: &[String], path: &str, env_ptrs: &[*const c_char]) -> ! {
    if try_builtin(argv, None).is_some() {
        posix::exit(0);
    }
    let argvec = ArgVec::from(argv);
    match find_executable(&argv[0], path) {
        Some(exe) => {
            posix::execve(&exe, &argvec.ptrs, env_ptrs);
        }
        None => {
            eprintln!("wash: {}: command not found", argv[0]);
        }
    }
    posix::exit(127);
}

fn run_standalone_exp(
    argv: &[String],
    redirs: &[ExpRedir],
    assigns: &[(String, Vec<WordPart>, bool)],
    env_ptrs: &[*const c_char],
    sh: &mut Shell,
    path: &str,
) -> i32 {
    if argv.is_empty() {
        // Standalone assignment(s): persist to the shell environment.
        if !assigns.is_empty() {
            let ctx = ExpandCtx {
                env: &sh.env,
                positional: &sh.positional,
                last_status: sh.last_status,
                self_pid: sh.self_pid,
            };
            // Expand all RHS values first (immutable borrow of sh.env), then
            // mutate sh.env once the borrow is released.
            let applied: Vec<(String, String, bool)> = assigns
                .iter()
                .map(|(n, vp, ap)| (n.clone(), expand_scalar(vp, &ctx), *ap))
                .collect();
            for (name, val, append) in applied {
                if append {
                    let cur = sh.env.get(&name).cloned().unwrap_or_default();
                    sh.env.insert(name, cur + &val);
                } else {
                    sh.env.insert(name, val);
                }
            }
        }
        apply_exp_redirs(redirs);
        return 0;
    }
    if let Some(code) = try_builtin(argv, Some(sh)) {
        return code;
    }
    let pid = posix::fork();
    if pid == 0 {
        apply_exp_redirs(redirs);
        child_run_exp(argv, path, env_ptrs);
    } else {
        posix::wait_pid(pid)
    }
}

/// A fully prepared (expanded) command: argv, redirections, the assignments
/// it carried, and its private environment pointer array.
struct Prepared {
    argv: Vec<String>,
    redirs: Vec<ExpRedir>,
    assigns: Vec<(String, Vec<WordPart>, bool)>,
    env_ptrs: Vec<*const c_char>,
}

fn prepare(p: &Pipeline, sh: &Shell) -> Vec<Prepared> {
    let ctx = ExpandCtx {
        env: &sh.env,
        positional: &sh.positional,
        last_status: sh.last_status,
        self_pid: sh.self_pid,
    };
    p.commands
        .iter()
        .map(|c| {
            let argv = expand_argv(&c.argv, &ctx);
            let redirs = expand_redirs(&c.redirs, &ctx);
            let cmd_env = build_cmd_env(&sh.env, &c.assignments, &ctx);
            let (_ec, env_ptrs) = build_env_ptrs(&cmd_env);
            Prepared {
                argv,
                redirs,
                assigns: c.assignments.clone(),
                env_ptrs,
            }
        })
        .collect()
}

fn execute_pipeline(p: &Pipeline, sh: &mut Shell, path: &str) -> i32 {
    let prepared = prepare(p, sh);
    let n = prepared.len();
    if n == 1 {
        let pr = &prepared[0];
        return run_standalone_exp(
            &pr.argv,
            &pr.redirs,
            &pr.assigns,
            &pr.env_ptrs,
            sh,
            path,
        );
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
            apply_exp_redirs(&pr.redirs);
            if pr.argv.is_empty() {
                posix::exit(0);
            }
            child_run_exp(&pr.argv, path, &pr.env_ptrs);
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
    for pid in pids {
        last = posix::wait_pid(pid);
    }
    last
}

pub fn execute(list: &CommandList, sh: &mut Shell) -> i32 {
    let path = sh.env.get("PATH").cloned().unwrap_or_default();

    let mut last = 0;
    for p in &list.pipelines {
        last = execute_pipeline(p, sh, &path);
        // Propagate exit status so `$?` reflects the previous command.
        sh.last_status = last;
    }
    last
}
