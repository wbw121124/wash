// Phase 0 executor: run the AST via fork/execve (external commands) and a
// couple of builtins (cd, exit) that must run in the parent process.
//
// Pipeline plumbing, redirections, and exit-status decoding all go through the
// msys POSIX shim in `posix.rs`.

use std::ffi::{c_char, CString};

use crate::parser::{CommandList, Pipeline, Redir, SimpleCommand};
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

fn build_env_ptrs(env: &std::collections::HashMap<String, String>) -> (Vec<CString>, Vec<*const c_char>) {
    let cstrings: Vec<CString> = env
        .iter()
        .map(|(k, v)| CString::new(format!("{}={}", k, v)).unwrap_or_default())
        .collect();
    let mut ptrs: Vec<*const c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (cstrings, ptrs)
}

/// Search $PATH (msys uses ':'). Try the name as-is, then with `.exe`.
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

/// Run a builtin in the current process. `sh` is None in a forked child
/// (state changes are meaningless there); Some in the parent for standalone
/// builtins so they can mutate shell state.
fn try_builtin(cmd: &SimpleCommand, sh: Option<&mut Shell>) -> Option<i32> {
    let name = cmd.argv.first()?;
    match name.as_str() {
        "cd" => {
            let target = cmd.argv.get(1).map(|s| s.as_str()).unwrap_or("");
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

fn apply_redirs(redirs: &[Redir]) {
    for r in redirs {
        match r {
            Redir::In { fd, file } => {
                let f = posix::open_read(file);
                posix::dup2(f, *fd);
                posix::close(f);
            }
            Redir::Out { fd, file, append } => {
                let f = posix::open_write(file, *append);
                posix::dup2(f, *fd);
                posix::close(f);
            }
            Redir::Both { file } => {
                let f = posix::open_write(file, false);
                posix::dup2(f, 1);
                posix::dup2(f, 2);
                posix::close(f);
            }
        }
    }
}

/// In a forked child: run builtin or exec the external command, then exit.
fn child_run(cmd: &SimpleCommand, path: &str, env_ptrs: &[*const c_char]) -> ! {
    if let Some(code) = try_builtin(cmd, None) {
        posix::exit(code);
    }
    let argv = ArgVec::from(&cmd.argv);
    match find_executable(&cmd.argv[0], path) {
        Some(exe) => {
            posix::execve(&exe, &argv.ptrs, env_ptrs);
        }
        None => {
            eprintln!("wash: {}: command not found", cmd.argv[0]);
        }
    }
    posix::exit(127);
}

fn run_standalone(cmd: &SimpleCommand, sh: &mut Shell, path: &str, env_ptrs: &[*const c_char]) -> i32 {
    if let Some(code) = try_builtin(cmd, Some(sh)) {
        return code;
    }
    let pid = posix::fork();
    if pid == 0 {
        apply_redirs(&cmd.redirs);
        child_run(cmd, path, env_ptrs);
    } else {
        posix::wait_pid(pid)
    }
}

fn execute_pipeline(
    p: &Pipeline,
    sh: &mut Shell,
    path: &str,
    env_ptrs: &[*const c_char],
) -> i32 {
    let n = p.commands.len();
    if n == 0 {
        return 0;
    }
    if n == 1 {
        return run_standalone(&p.commands[0], sh, path, env_ptrs);
    }

    let mut prev_read: Option<i32> = None;
    let mut pids: Vec<i32> = Vec::new();

    for (idx, cmd) in p.commands.iter().enumerate() {
        let make_pipe = idx < n - 1;
        let (this_read, this_write) = if make_pipe {
            let fds = posix::pipe();
            (Some(fds[0]), Some(fds[1]))
        } else {
            (None, None)
        };

        let pid = posix::fork();
        if pid == 0 {
            // child
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
            apply_redirs(&cmd.redirs);
            child_run(cmd, path, env_ptrs);
        } else {
            // parent
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
    let (_env_cstrings, env_ptrs) = build_env_ptrs(&sh.env);

    let mut last = 0;
    for p in &list.pipelines {
        last = execute_pipeline(p, sh, &path, &env_ptrs);
    }
    last
}
