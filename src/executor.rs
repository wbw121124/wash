// Phase 1 executor: expand words, then run the AST via fork/execve. Also
// provides `capture_command` for command/process substitution (runs the inner
// command in a forked subshell and returns its stdout).
//
// Pipeline plumbing, redirections, and exit-status decoding go through the
// msys POSIX shim in `posix.rs`.

use std::collections::HashMap;
use std::ffi::{c_char, CString};
use std::io::Write;

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
// Output builtins: echo / printf
//
// These run in the forked child AFTER redirections have been applied, so that
// `echo out > file` and `printf ... > file` actually write the file. Only the
// STATE builtins (`cd`, `exit`) run in the parent process.
// ---------------------------------------------------------------------------

/// Output builtins that must execute in the child (with redirections applied).
/// Returns `Some(exit_code)` if `argv[0]` is one of these builtins.
fn run_output_builtin(argv: &[String]) -> Option<i32> {
    match argv.first().map(|s| s.as_str()) {
        Some("echo") => Some(builtin_echo(argv)),
        Some("printf") => Some(builtin_printf(argv)),
        _ => None,
    }
}

fn builtin_echo(argv: &[String]) -> i32 {
    let s = argv[1..].join(" ");
    let _ = std::io::stdout().write_all(s.as_bytes());
    let _ = std::io::stdout().write_all(b"\n");
    let _ = std::io::stdout().flush();
    0
}

fn builtin_printf(argv: &[String]) -> i32 {
    if argv.len() < 2 {
        let _ = std::io::stdout().flush();
        return 0;
    }
    let format = &argv[1];
    let args = &argv[2..];
    let bytes = format.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut ai: usize = 0;

    // Does the format contain at least one conversion? bash only cycles the
    // format when there are conversions; an operand-less format is printed
    // once and extra arguments are ignored.
    let has_conv = {
        let mut i = 0;
        let mut found = false;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 1 < bytes.len() && bytes[i + 1] != b'%' {
                found = true;
                break;
            }
            i += 1;
        }
        found
    };

    loop {
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'%' {
                if i + 1 >= bytes.len() {
                    out.push(b'%');
                    i += 1;
                    continue;
                }
                let nxt = bytes[i + 1];
                if nxt == b'%' {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                let new_i = parse_printf_spec(bytes, i, &mut out, args, &mut ai);
                i = new_i;
            } else if c == b'\\' {
                let (esc, ni) = parse_printf_escape(bytes, i);
                out.extend_from_slice(&esc);
                i = ni;
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
    }

    let _ = std::io::stdout().write_all(&out);
    let _ = std::io::stdout().flush();
    0
}

/// Parse a printf conversion spec starting at `start` (which points at `%`).
/// Advances `ai` for consumed arguments (including `*` width/precision) and
/// appends formatted output to `out`. Returns the new byte index.
fn parse_printf_spec(
    bytes: &[u8],
    start: usize,
    out: &mut Vec<u8>,
    args: &[String],
    ai: &mut usize,
) -> usize {
    let mut i = start + 1; // skip '%'
    // flags
    while i < bytes.len() && matches!(bytes[i], b'-' | b'+' | b' ' | b'#' | b'0') {
        i += 1;
    }
    // width (possibly '*', which consumes an argument)
    if i < bytes.len() && bytes[i] == b'*' {
        *ai += 1;
        i += 1;
    } else {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    // precision
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        if i < bytes.len() && bytes[i] == b'*' {
            *ai += 1;
            i += 1;
        } else {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    // length modifiers
    while i < bytes.len() && matches!(bytes[i], b'l' | b'h' | b'L' | b'j' | b'z' | b't' | b'q') {
        i += 1;
    }
    if i >= bytes.len() {
        out.push(b'%');
        return start + 1;
    }
    let conv = bytes[i];
    i += 1;

    let arg: &str = if *ai < args.len() {
        args[*ai].as_str()
    } else {
        match conv {
            b's' | b'b' | b'c' => "",
            _ => "0",
        }
    };

    match conv {
        b's' => out.extend_from_slice(arg.as_bytes()),
        b'b' => {
            let un = unescape_percent_b(arg);
            out.extend_from_slice(un.as_bytes());
        }
        b'c' => {
            if let Some(ch) = arg.chars().next() {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
        b'd' | b'i' => {
            let n: i64 = arg.trim().parse().unwrap_or(0);
            out.extend_from_slice(n.to_string().as_bytes());
        }
        b'u' => {
            let n: i64 = arg.trim().parse().unwrap_or(0);
            let u = if n < 0 { 0 } else { n } as u64;
            out.extend_from_slice(u.to_string().as_bytes());
        }
        b'o' => {
            let n: i64 = arg.trim().parse().unwrap_or(0);
            out.extend_from_slice(format!("{:o}", n).as_bytes());
        }
        b'x' => {
            let n: i64 = arg.trim().parse().unwrap_or(0);
            out.extend_from_slice(format!("{:x}", n).as_bytes());
        }
        b'X' => {
            let n: i64 = arg.trim().parse().unwrap_or(0);
            out.extend_from_slice(format!("{:X}", n).as_bytes());
        }
        b'f' | b'e' | b'E' | b'g' | b'G' => {
            let n: f64 = arg.trim().parse().unwrap_or(0.0);
            out.extend_from_slice(format!("{}", n).as_bytes());
        }
        _ => {
            out.push(b'%');
            out.push(conv);
        }
    }
    *ai += 1;
    i
}

/// Interpret backslash escapes in a printf format string (or `%b` argument):
/// `\n \t \\ \" \' \a \b \f \r \v \0`, octal `\nnn`, and hex `\xHH`. Returns
/// the rendered bytes and the next index after the escape.
fn parse_printf_escape(bytes: &[u8], start: usize) -> (Vec<u8>, usize) {
    if start + 1 >= bytes.len() {
        return (vec![b'\\'], start + 1);
    }
    let c = bytes[start + 1];
    let mut out = Vec::new();
    let i = start + 2;
    match c {
        b'n' => out.push(b'\n'),
        b't' => out.push(b'\t'),
        b'\\' => out.push(b'\\'),
        b'"' => out.push(b'"'),
        b'\'' => out.push(b'\''),
        b'a' => out.push(0x07),
        b'b' => out.push(0x08),
        b'f' => out.push(0x0c),
        b'r' => out.push(b'\r'),
        b'v' => out.push(0x0b),
        b'0' => {
            let mut val: u32 = 0;
            let mut count = 0;
            let mut j = start + 1;
            while count < 3 && j < bytes.len() && (bytes[j] as char).is_digit(8) {
                val = val * 8 + (bytes[j] - b'0') as u32;
                j += 1;
                count += 1;
            }
            out.push(val as u8);
            return (out, j);
        }
        b'x' | b'X' => {
            let mut val: u32 = 0;
            let mut j = start + 2;
            let mut count = 0;
            while count < 2 && j < bytes.len() && (bytes[j] as char).is_digit(16) {
                val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                j += 1;
                count += 1;
            }
            out.push(val as u8);
            return (out, j);
        }
        _ => out.push(c),
    }
    (out, i)
}

/// Interpret common backslash escapes inside a `%b` argument.
fn unescape_percent_b(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let (esc, ni) = parse_printf_escape(bytes, i);
            out.push_str(&String::from_utf8_lossy(&esc));
            i = ni;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
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
    if argv.is_empty() {
        posix::exit(0);
    }
    // Output builtins (echo/printf) run here, in the child, AFTER the caller
    // has applied redirections. This keeps `echo x > file` / `printf ... > file`
    // working. (State builtins cd/exit are handled in the parent instead.)
    if let Some(code) = run_output_builtin(argv) {
        let _ = std::io::stdout().flush();
        posix::exit(code);
    }
    // Defensive: state builtins should normally be handled in the parent, but
    // honour them here too.
    if let Some(code) = try_builtin(argv, None) {
        posix::exit(code);
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
