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
use crate::safe_eprint_bytes;
use crate::safe_eprintln;
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
            // 变量值中的 byteval 编码字节在此还原为原始 argv 字节。
            // 内嵌 NUL 按 bash 的 ingress 语义剥除（见 posix::cstr_strip_nul）。
            let c = posix::cstr_strip_nul(crate::byteval::decode_raw(a));
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

/// 构造 `NAME=VALUE` 的 C 字符串，VALUE 中的 byteval 编码在此还原为裸字节。
fn env_entry(k: &str, v: &str) -> CString {
    let mut kv = crate::byteval::decode_raw(k);
    kv.push(b'=');
    kv.extend_from_slice(&crate::byteval::decode_raw(v));
    posix::cstr_strip_nul(kv)
}

/// Build the null-terminated `envp` vector for an environment map.
pub(crate) fn build_env_ptrs(env: &HashMap<String, String>) -> (Vec<CString>, Vec<*const c_char>) {
    let cstrings: Vec<CString> = env
        .iter()
        .map(|(k, v)| env_entry(k, v))
        .collect();
    let mut ptrs: Vec<*const c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (cstrings, ptrs)
}

/// Build `envp` for a fork/exec directly from `Shell` in a SINGLE pass.
///
/// The old `Shell::child_env()` + `build_env_ptrs()` pair walked the whole
/// variable table twice and allocated an intermediate `HashMap` plus two
/// String clones per variable.  This fused form iterates `env` once and
/// allocates exactly one `CString` per exported variable.  `child_env()`
/// always overwrote `PWD` with `self.cwd`, so `PWD` is skipped in the loop
/// and appended once from `self.cwd` -- byte-identical `envp` semantics.
/// `envp` ordering is unspecified (both versions iterate a `HashMap`).
pub(crate) fn build_shell_env_ptrs(sh: &Shell) -> (Vec<CString>, Vec<*const c_char>) {
    let mut cstrings: Vec<CString> = Vec::with_capacity(sh.exported.len() + 1);
    for (k, v) in &sh.env {
        if k == "PWD" || !sh.exported.contains(k) {
            continue;
        }
        cstrings.push(env_entry(k, v));
    }
    cstrings.push(env_entry("PWD", &sh.cwd));
    let mut ptrs: Vec<*const c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (cstrings, ptrs)
}

/// Replace the current process with `path`. Only returns on failure.
///
/// 失败时按 bash 5.3（MSYS2 实测）分类报告 —— 这是 `exec` 内建（父 shell
/// 进程）走的入口：
///   * 文件不存在          → `wash: PATH: <strerror>`，rc 127
///   * 目录                → `wash: PATH: Is a directory`，rc 126
///   * 二进制（头部含 NUL）→ `wash: PATH: cannot execute binary file: Exec format error`，rc 126
///   * shebang 的解释器坏  → `wash: PATH: INTERP: bad interpreter: <strerror>`，rc 126
///   * ENOEXEC 文本        → 作为 shell 脚本执行（fork 子进程），shell 以
///     脚本状态终止（交互 shell 同样终止 —— bash 实测一致）。
///
/// 交互 REPL（`posix::INTERACTIVE_REPL` 置位）下报告类失败**不打印也不
/// 退出**：bash 交互 shell 在 exec 失败后继续存活，调用方（builtins.rs 的
/// exec 内建）会打印自己的诊断并返回。非交互（-c / 脚本）则打印后直接
/// 终止进程，rc 与 bash 一致（调用方的兜底消息永远不会再执行）。
pub(crate) fn execve(path: &CString, argv: &[*const c_char], envp: &[*const c_char]) -> i32 {
    let display = path.to_string_lossy().into_owned();
    // 指针数组在本调用期间有效，读回字符串以重建脚本回退的 argv。
    let argv_strs: Vec<String> = argv
        .iter()
        .filter(|p| !p.is_null())
        .map(|p| unsafe { std::ffi::CStr::from_ptr(*p) }.to_string_lossy().into_owned())
        .collect();
    let env_map = envp_map(envp);
    exec_classified(path, &display, &argv_strs, argv, envp, &env_map, true)
}

/// Rebuild an environment map from an `envp` pointer array (the `exec`
/// builtin's script fallback has no `Shell` at hand; bash 把整个变量表
/// 交给子脚本，这里至少还原导出环境)。
fn envp_map(envp: &[*const c_char]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for p in envp.iter() {
        if p.is_null() {
            break;
        }
        let s = unsafe { std::ffi::CStr::from_ptr(*p) }.to_string_lossy().into_owned();
        if let Some(eq) = s.find('=') {
            out.insert(s[..eq].to_string(), s[eq + 1..].to_string());
        }
    }
    out
}

/// execve 失败后的 bash 语义分类。成功路径永不返回；报告类失败在
/// 非交互进程里直接 `posix::exit`（`exec` 内建语义：shell 被替换，
/// 失败即终止），只有交互 REPL 例外（返回 rc，让 REPL 存活）。
///
/// `parent_shell`：true = `exec` 内建在 shell 本体进程里调用；false =
/// 已经在 fork 出的命令子进程里（普通命令 / `command` 内建），失败
/// 一律退出子进程。
fn exec_classified(
    exe: &CString,
    display: &str,
    argv_strs: &[String],
    argv: &[*const c_char],
    envp: &[*const c_char],
    shell_env: &HashMap<String, String>,
    parent_shell: bool,
) -> i32 {
    posix::execve(exe, argv, envp);
    let e = posix::errno();
    // --- 文件身份判定（优先于 errno：cygwin 对坏解释器脚本报的也是
    // ENOENT，必须靠文件内容区分）---
    let st = posix::stat_path(display, true);
    if let Some(s) = &st {
        if s.file_type == posix::FT_DIR {
            // display 是 byteval 编码串；路径含非法 UTF-8 时按裸字节写诊断。
            return exec_fail_bytes(
                &[&crate::byteval::decode_raw(display), b": Is a directory"],
                126,
                parent_shell,
            );
        }
    }
    let is_reg = matches!(&st, Some(s) if s.file_type == posix::FT_REG);
    if is_reg {
        if let Some(bytes) = posix::read_file_bytes(display) {
            // bash check_binary_file：前 80 字节里出现 NUL，或以 `\x7fELF`
            // 魔数开头 → 二进制（MSYS2 实测：\x01、\xff 等其他不可打印
            // 字节仍走脚本回退）。
            let head = &bytes[..bytes.len().min(80)];
            if head.contains(&0u8) || head.starts_with(b"\x7fELF") {
                return exec_fail_bytes(
                    &[
                        &crate::byteval::decode_raw(display),
                        b": cannot execute binary file: Exec format error",
                    ],
                    126,
                    parent_shell,
                );
            }
            if let Some(interp) = shebang_interpreter(head) {
                // bash 的 re-exec：execve(解释器, [解释器, 脚本, 参数...])。
                // 解释器不存在/不可执行时按其实际 errno 报
                // `bad interpreter: <strerror>`（rc 126，无行号前缀）。
                // interp 是 byteval 编码串，execve 前必须 decode_raw 还原字节。
                let ic = posix::cstr_strip_nul(crate::byteval::decode_raw(&interp));
                let script_c = posix::cstr_strip_nul(display.as_bytes().to_vec());
                let mut ptrs: Vec<*const c_char> =
                    vec![ic.as_ptr(), script_c.as_ptr()];
                for p in argv.iter().skip(1) {
                    if p.is_null() {
                        break;
                    }
                    ptrs.push(*p);
                }
                ptrs.push(std::ptr::null());
                posix::execve(&ic, &ptrs, envp);
                let e2 = posix::errno();
                // interp 含原始字节，诊断必须按裸字节写，不能用 format!
                // （那会把 byteval 编码串或 U+FFFD 写进 stderr）。
                let interp_raw = crate::byteval::decode_raw(&interp);
                let estr = posix::errno_message(e2);
                return exec_fail_bytes(
                    &[
                        &crate::byteval::decode_raw(display),
                        b": ",
                        &interp_raw,
                        b": bad interpreter: ",
                        estr.as_bytes(),
                    ],
                    126,
                    parent_shell,
                );
            }
            // ENOEXEC 纯文本：作为 shell 脚本执行。
            return run_as_script(display, argv_strs, envp, shell_env, parent_shell);
        }
    }
    // 文件消失 / 非 regular（设备等）：报 execve 的 errno。
    let code = if e == 2 /* ENOENT */ || e == 20 /* ENOTDIR */ {
        127
    } else {
        126
    };
    let estr = posix::errno_message(e);
    exec_fail_bytes(
        &[&crate::byteval::decode_raw(display), b": ", estr.as_bytes()],
        code,
        parent_shell,
    )
}

/// Print `wash: MSG` and terminate.  交互 REPL 下不打印、不退出
/// （返回 rc 交给 `exec` 内建兜底）。
fn exec_fail(msg_full: &str, code: i32, parent_shell: bool) -> i32 {
    if parent_shell && posix::INTERACTIVE_REPL.load(std::sync::atomic::Ordering::Relaxed) {
        return code;
    }
    safe_eprintln!("wash: {}", msg_full);
    let _ = std::io::stdout().flush();
    posix::exit(code);
}

/// 与 [`exec_fail`] 同语义，但诊断体是原始字节（如 shebang 解释器路径含
/// 非法 UTF-8）。byteval 编码串必须先 `decode_raw` 再传进来。
fn exec_fail_bytes(parts: &[&[u8]], code: i32, parent_shell: bool) -> i32 {
    if parent_shell && posix::INTERACTIVE_REPL.load(std::sync::atomic::Ordering::Relaxed) {
        return code;
    }
    let mut buf: Vec<u8> = b"wash: ".to_vec();
    for p in parts {
        buf.extend_from_slice(p);
    }
    buf.push(b'\n');
    let _ = std::io::stderr().write_all(&buf);
    let _ = std::io::stdout().flush();
    posix::exit(code);
}

/// Parse the interpreter out of a `#!` line: the first whitespace-delimited
/// token after `#!` (leading blanks skipped).  `#!` 单独成行（无解释器）
/// 返回 None —— bash 把它当普通脚本跑。
fn shebang_interpreter(head: &[u8]) -> Option<String> {
    if !head.starts_with(b"#!") {
        return None;
    }
    let mut i = 2;
    while i < head.len() && (head[i] == b' ' || head[i] == b'\t') {
        i += 1;
    }
    let start = i;
    while i < head.len() && head[i] != b' ' && head[i] != b'\t' && head[i] != b'\n' && head[i] != 0 {
        i += 1;
    }
    if start == i {
        return None;
    }
    // 解释器路径来自文件的原始字节：裸高位字节必须保真，不能
    // from_utf8_lossy 成 U+FFFD（bash 把它当路径里的真实字节）。
    // 返回 byteval 编码串；execve 前用 decode_raw 还原，诊断按裸字节写。
    Some(crate::byteval::encode_raw(&head[start..i]))
}

/// ENOEXEC 纯文本回退：把文件当 shell 脚本执行并以其状态退出。
///
/// 普通命令路径已经在 fork 出的子进程里 → 直接执行后 exit；
/// `exec` 内建在父 shell 里 → bash 实测会 fork 一个子 shell 跑脚本
/// （BASHPID 可证），shell 以脚本状态终止（交互 shell 也不例外）。
fn run_as_script(
    display: &str,
    argv_strs: &[String],
    envp: &[*const c_char],
    shell_env: &HashMap<String, String>,
    parent_shell: bool,
) -> i32 {
    if !parent_shell {
        run_script_in_shell(display, argv_strs, shell_env);
    }
    let pid = fork_child();
    if let Some(pid) = pid {
        if pid == 0 {
            run_script_in_shell(display, argv_strs, shell_env);
        }
        let status = posix::wait_pid(pid);
        let _ = std::io::stdout().flush();
        posix::exit(status);
    }
    1
}

/// 在当前进程里执行脚本内容（`bash 脚本` 语义：全新 Shell、$0=脚本路径、
/// 位置参数=脚本参数），然后退出进程。
fn run_script_in_shell(display: &str, argv_strs: &[String], shell_env: &HashMap<String, String>) {
    let content = match posix::read_file(display) {
        // read_file 已做 byteval 原始字节编码，不能再 escape_literal。
        Some(c) => c,
        None => {
            safe_eprintln!("wash: {}: {}", display, posix::errno_message(posix::errno()));
            let _ = std::io::stdout().flush();
            posix::exit(126);
        }
    };
    let mut sh = Shell::from_env(shell_env.clone());
    sh.positional = argv_strs.iter().skip(1).cloned().collect();
    sh.dollar_zero = display.to_string();
    let code = sh.run_in_error_context(crate::shell::ErrorContext::sourced(display), &content);
    let code = sh.finalize_exit(code);
    let _ = std::io::stdout().flush();
    posix::exit(code);
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
fn fork_child() -> Option<i32> {
    let pid = posix::fork();
    if pid < 0 {
        // fork failed (EAGAIN / ENOMEM).  Report it once here -- the single
        // funnel every fork goes through -- so no caller can leak a negative
        // pid into `wait_pid` / the job table.  bash prints `fork: <strerror>`
        // and gives the command status 1 while the shell keeps running.
        let e = posix::errno();
        safe_eprintln!("wash: fork: {}", posix::errno_message(e));
        return None;
    }
    if pid == 0 {
        posix::reset_signals_default();
    }
    Some(pid)
}

/// `path` 是 regular file（对齐 MSYS2 bash 的搜索语义：只看存在性，
/// 不看执行位 —— Cygwin 对文本/二进制的 ENOEXEC 回退在 execve 之后才发生）。
fn is_regular_file(path: &str) -> bool {
    matches!(
        posix::stat_path(path, true),
        Some(st) if st.file_type == posix::FT_REG
    )
}

/// Look `name` up in `path` (or directly, when it contains a slash).
///
/// bash 5.3（MSYS2 实测对齐）的查找规则：
///   * 名字含斜杠：直接用该路径执行，不做 access 预检 —— execve 失败的
///     errno（ENOENT → `No such file or directory`、目录 → `Is a
///     directory`、ENOEXEC → 二进制/脚本分类）才是错误文案的来源。
///   * `PATH` 未设置（`None`）：对裸名直接 execve（相对路径按 cwd 解析，
///     失败报 `No such file or directory`，rc=127），而不是报
///     `command not found`。
///   * `PATH` 已设置：按冒号切分逐目录找 **regular file**；空元素表示
///     当前目录（`PATH=:` / `PATH=` 都会搜 cwd）。执行位不参与筛选
///     （Cygwin 的 noacl 挂载下权限位本来就不生效，bash 的搜索同样
///     找到并交给 execve 分类）。
///
/// Returns the path to execute and the path as the `$PATH` search reports
/// it: Cygwin falls back to the `.exe` suffix when it executes a program,
/// but it reports the name as it stands in the `$PATH`.
pub(crate) fn find_program(name: &str, path: Option<&str>) -> Option<(CString, String)> {
    if name.contains('/') || name.contains('\\') {
        return Some((posix::to_cstr(name), name.to_string()));
    }
    let path = match path {
        // PATH 未设置：bash 直接对裸名 execve。
        None => return Some((posix::to_cstr(name), name.to_string())),
        Some(p) => p,
    };
    for dir in path.split(':') {
        let cand = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", dir, name)
        };
        if is_regular_file(&cand) {
            return Some((posix::to_cstr(&cand), cand.clone()));
        }
        let cand_exe = format!("{}.exe", cand);
        if is_regular_file(&cand_exe) {
            return Some((posix::to_cstr(&cand), cand));
        }
    }
    None
}

/// Look `name` up in `path` (or directly, when it contains a slash).
pub(crate) fn find_executable(name: &str, path: &str) -> Option<CString> {
    find_program(name, Some(path)).map(|(exe, _)| exe)
}

/// The executables named `name` in the colon-separated `$PATH`.
///
/// Like Cygwin's `find_user_command`, the `.exe` suffix is tried as a
/// fallback but it is not reported: bash prints the path exactly as it
/// stands in the `$PATH`.  搜索语义与 `find_program` 一致：regular file
/// 即命中，执行位不参与（MSYS2 bash 实测）。
pub fn path_matches(name: &str, path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if name.contains('/') || name.contains('\\') {
        // An explicit path is used as it is.
        if is_regular_file(name) {
            out.push(name.to_string());
        }
        return out;
    }
    for dir in path.split(':') {
        let candidate = if dir.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", dir, name)
        };
        if is_regular_file(&candidate) {
            out.push(candidate);
            continue;
        }
        let candidate_exe = format!("{}.exe", candidate);
        if is_regular_file(&candidate_exe) {
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
    /// fd, expanded target, raw source word (`N>&M` / `N<&M` / `>&-`)。
    /// `M` 展开后非数字时回显**展开后的单字段**（`x=abc; 2>&$x` →
    /// `abc: ambiguous redirect`）；raw 仅用于 `M` 展开为空串且 fd 为默认
    /// 输出描述符时的诊断（`1>&""` → `"": Bad file descriptor`）。
    Dup(i32, String, String),
    /// fd, text: a here-document body or here-string after expansion.  Both
    /// are fed to the descriptor the same way (bash spools them to a temp
    /// file too).
    HereDoc(i32, String),
    /// fd, raw source word: the redirection word expanded to several fields
    /// (or zero fields) — bash reports `<raw word>: ambiguous redirect`
    /// with the ORIGINAL word text (MSYS2 bash 5.3 实测：`> $v`、`> ${v}`、
    /// `> $(echo "$v")`、`> "pre"$v` 都原样回显源词，含引号)。
    Ambiguous(i32, String),
}

/// Reconstruct the RAW source text of a redirection word for bash's
/// `<word>: ambiguous redirect` diagnostic.  The AST carries no original
/// text, so the word is rebuilt from its parts: quoted literal fragments are
/// re-wrapped in double quotes (bash 回显的是源词原文；单引号与转义细节
/// 无法从 AST 还原，登记为极端情形下的已知偏差).
fn raw_word_text(word: &[WordPart]) -> String {
    let mut s = String::new();
    for p in word {
        match p {
            WordPart::Text(t, quoted) => {
                if *quoted {
                    s.push('"');
                    s.push_str(t);
                    s.push('"');
                } else {
                    s.push_str(t);
                }
            }
            WordPart::Var(n, _) => {
                s.push('$');
                s.push_str(n);
            }
            WordPart::Braced(b, _) => {
                s.push_str("${");
                s.push_str(b);
                s.push('}');
            }
            WordPart::CmdSub(b, _) => {
                s.push_str("$(");
                s.push_str(b);
                s.push(')');
            }
            WordPart::Backtick(b, _) => {
                s.push('`');
                s.push_str(b);
                s.push('`');
            }
            WordPart::Arith(b, _) => {
                s.push_str("$((");
                s.push_str(b);
                s.push_str("))");
            }
            WordPart::Tilde => s.push('~'),
            WordPart::Brace(alts) => {
                s.push('{');
                for (i, a) in alts.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(a);
                }
                s.push('}');
            }
        }
    }
    s
}

/// Expand one redirection word.  Returns `Err(raw)` — the RAW source word —
/// when the expansion does not yield exactly one field: zero fields (an
/// empty variable, nullglob with no matches) or several fields (word
/// splitting) both make bash report `<raw word>: ambiguous redirect`.
fn expand_redir_file(file: &[WordPart], ctx: &ExpandCtx) -> Result<String, String> {
    let words = expand_word(file, ctx);
    if words.len() == 1 {
        Ok(words.into_iter().next().unwrap_or_default())
    } else {
        Err(raw_word_text(file))
    }
}

fn expand_redirs(redirs: &[Redir], ctx: &ExpandCtx) -> Vec<ExpRedir> {
    redirs
        .iter()
        .map(|r| match r {
            Redir::In { fd, file } => match expand_redir_file(file, ctx) {
                Ok(f) => ExpRedir::In(*fd, f),
                Err(raw) => ExpRedir::Ambiguous(*fd, raw),
            },
            Redir::Out { fd, file, append, clobber } => match expand_redir_file(file, ctx) {
                Ok(f) => ExpRedir::Out(*fd, f, *append, *clobber),
                Err(raw) => ExpRedir::Ambiguous(*fd, raw),
            },
            Redir::Both { file, append } => match expand_redir_file(file, ctx) {
                Ok(f) => ExpRedir::Both(f, *append),
                Err(raw) => ExpRedir::Ambiguous(2, raw),
            },
            Redir::Dup { fd, file } => match expand_redir_file(file, ctx) {
                Ok(t) => ExpRedir::Dup(*fd, t, raw_word_text(file)),
                Err(raw) => ExpRedir::Ambiguous(*fd, raw),
            },
            Redir::HereDoc { fd, body, expand } => ExpRedir::HereDoc(
                *fd,
                if *expand {
                    crate::expand::expand_heredoc_body(body, ctx)
                } else {
                    // Quoted delimiter: the body is used verbatim.
                    body.clone()
                },
            ),
            Redir::HereString { fd, word } => {
                // bash: the word gets tilde/parameter/command/arithmetic
                // expansion but is never globbed or word-split (even
                // unquoted — `cat <<< *` stays literal), and one newline is
                // appended.  `expand_scalar` is exactly those semantics.
                let mut text = crate::expand::expand_scalar(word, ctx);
                text.push('\n');
                ExpRedir::HereDoc(*fd, text)
            }
        })
        .collect()
}

/// Report a failed redirection in the forked child and exit(1).
///
/// 诊断文案与 `apply_redirs_in_parent` 完全一致（`wash: FILE: strerror(errno)`）。
/// MSYS2 bash 5.3.20 对 `cmd > /nonexistent/f` 的 stderr 实测为
/// `<path>/bash: line N: FILE: No such file or directory`，经对拍套件的
/// normalize_stderr 去掉 shell 前缀后，两边逐字节相同，退出码也都是 1。
/// 子进程必须在此终止：否则会带着未重定向的 fd 继续执行命令，把输出写到
/// 错误的地方（旧行为：静默吞掉失败、rc=0、ls 的目录列表漏到 stdout）。
fn redir_failed(file: &str) {
    let e = posix::errno();
    safe_eprintln!("wash: {}: {}", file, posix::errno_message(e));
    let _ = std::io::stdout().flush();
    posix::exit(1);
}

/// Apply the redirections in the forked child.  A failed open() is fatal for
/// this child: bash reports the errno and exits 1 instead of running the
/// command with the wrong descriptors.
fn apply_exp_redirs(redirs: &[ExpRedir], noclobber: bool) {
    for r in redirs {
        match r {
            ExpRedir::In(fd, file) => {
                let f = posix::open_read(file);
                if f < 0 {
                    redir_failed(file);
                }
                if posix::dup2(f, *fd) < 0 {
                    posix::close(f);
                    redir_failed(file);
                }
                posix::close(f);
            }
            ExpRedir::Out(fd, file, append, clobber) => {
                let f = posix::open_write(file, *append, !*append && !*clobber && noclobber);
                if f < 0 {
                    redir_failed(file);
                }
                if posix::dup2(f, *fd) < 0 {
                    posix::close(f);
                    redir_failed(file);
                }
                posix::close(f);
            }
            ExpRedir::Both(file, append) => {
                let f = posix::open_write(file, *append, false);
                if f < 0 {
                    redir_failed(file);
                }
                if posix::dup2(f, 1) < 0 || posix::dup2(f, 2) < 0 {
                    posix::close(f);
                    redir_failed(file);
                }
                posix::close(f);
            }
            ExpRedir::Dup(fd, target, raw) => {
                // `N>&M` makes N refer to whatever M currently refers to;
                // `>&-` closes the descriptor.  目标 fd 无效时 bash 报
                // `<M>: Bad file descriptor` 且 rc=1（MSYS2 bash 5.3 实测：
                // `echo hi 2>&9` → `bash: line 1: 9: Bad file descriptor`）。
                if target == "-" {
                    posix::close(*fd);
                } else if target.is_empty() {
                    // `N>&""` 展开为空串：bash 报 `<fd>: Bad file
                    // descriptor`，但当 N 是 `>` 操作的默认 fd（1）时回显
                    // 原始词（`1>&""` → `"": Bad file descriptor`，oracle 实测）。
                    let name = if *fd == 1 {
                        raw.clone()
                    } else {
                        fd.to_string()
                    };
                    dup_empty_target_exit(&name);
                } else if let Ok(src) = target.parse::<i32>() {
                    if posix::dup2(src, *fd) < 0 {
                        dup_bad_fd_exit(&dup_bad_fd_name(raw, target, *fd, src));
                    }
                } else {
                    // `2>&x`：目标展开后不是数字，bash 以**展开后的单字段
                    // 文本**回显 ambiguous redirect（`x=abc; 2>&$x` →
                    // `abc: ambiguous redirect`，oracle 实测）。
                    dup_ambiguous_exit(target);
                }
            }
            ExpRedir::HereDoc(fd, text) => {
                let f = heredoc_fd(text);
                if f < 0 {
                    redir_failed("here-document");
                }
                if posix::dup2(f, *fd) < 0 {
                    // dup2 失败必须关闭已 spool 的临时描述符，不再泄漏。
                    posix::close(f);
                    redir_failed("here-document");
                }
                posix::close(f);
            }
            ExpRedir::Ambiguous(_fd, raw) => {
                // bash 对展开为多字段（或零字段）的重定向词以**原始词文本**
                // 报错并使命令失败（rc 1）：`echo hi > $v`（v="a b"）→
                // `bash: line 1: $v: ambiguous redirect`。
                safe_eprintln!("wash: {}: ambiguous redirect", raw);
                let _ = std::io::stdout().flush();
                posix::exit(1);
            }
        }
    }
}

/// 子进程里 dup 失效目标（`N>&M` 的 M 展开后不是数字且不是 `-`）：bash 报
/// `<展开后的单字段>: ambiguous redirect` 并以 rc 1 终止本命令。
fn dup_ambiguous_exit(raw: &str) -> ! {
    safe_eprintln!("wash: {}: ambiguous redirect", raw);
    let _ = std::io::stdout().flush();
    posix::exit(1);
}

/// 目标 fd 是否是"源词里的字面十进制数"（如 `2>&9`、`2>&09`）。bash 只对
/// 这种形态在 dup2 失败时回显**规范化后的数字**（`09` → `9`）；带引号、
/// 变量、算术/命令替换等产生的数字一律回显目标 fd 号（fd 为默认的 1 时
/// 回显原始词），oracle 实测：
///   `2>&9`        → `9: Bad file descriptor`
///   `2>&09`       → `9: Bad file descriptor`
///   `2>&"9"`      → `2: Bad file descriptor`
///   `x=9; 2>&$x`  → `2: Bad file descriptor`
///   `x=9; 1>&$x`  → `$x: Bad file descriptor`
fn is_literal_dup_number(raw: &str, target: &str) -> bool {
    raw == target && !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit())
}

/// dup2 失败时 bash 回显的名字：`M` 是字面数字则回显规范化数字，否则回显
/// 目标 fd（fd 是默认输出描述符 1 时回显原始词）。
fn dup_bad_fd_name(raw: &str, target: &str, fd: i32, parsed: i32) -> String {
    if is_literal_dup_number(raw, target) {
        parsed.to_string()
    } else if fd == 1 {
        raw.to_string()
    } else {
        fd.to_string()
    }
}

/// 子进程里 dup 目标展开为空串（`N>&""`）：bash 报
/// `<name>: Bad file descriptor` 并以 rc 1 终止本命令。此处不经过 dup2，
/// errno 不一定反映 EBADF，所以直接给出 bash 的固定文案。
fn dup_empty_target_exit(name: &str) -> ! {
    safe_eprintln!("wash: {}: Bad file descriptor", name);
    let _ = std::io::stdout().flush();
    posix::exit(1);
}

/// 子进程里 dup2 到无效源 fd（`N>&M` 的 M 未打开）：bash 报
/// `<M>: Bad file descriptor` 并以 rc 1 终止本命令。
fn dup_bad_fd_exit(target: &str) -> ! {
    let e = posix::errno();
    safe_eprintln!("wash: {}: {}", target, posix::errno_message(e));
    let _ = std::io::stdout().flush();
    posix::exit(1);
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
                if posix::dup2(f, *fd) < 0 {
                    let e = posix::errno();
                    posix::close(f);
                    restore_fds(&saved);
                    return Err(format!("wash: {}: {}", file, posix::errno_message(e)));
                }
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
                if posix::dup2(f, *fd) < 0 {
                    let e = posix::errno();
                    posix::close(f);
                    restore_fds(&saved);
                    return Err(format!("wash: {}: {}", file, posix::errno_message(e)));
                }
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
            ExpRedir::Dup(fd, target, raw) => {
                // Builtins run in the parent, so the descriptor must be saved
                // for restore_fds() afterwards.  先校验目标合法性再保存备份
                // （#17：旧实现先 dup 后校验，ambiguous 失败时把 -1 压入
                // saved，restore 时 dup2(-1, fd) 静默无效）。
                if target == "-" {
                    let backup = posix::dup(*fd);
                    saved.push((*fd, backup));
                    posix::close(*fd);
                } else if target.is_empty() {
                    // `N>&""`：bash 报 `<fd>: Bad file descriptor`，默认 fd 1
                    // 时回显原始词（`1>&""` → `"": ...`，oracle 实测）。
                    let name = if *fd == 1 {
                        raw.clone()
                    } else {
                        fd.to_string()
                    };
                    restore_fds(&saved);
                    return Err(format!("wash: {}: Bad file descriptor", name));
                } else if let Ok(src) = target.parse::<i32>() {
                    let backup = posix::dup(*fd);
                    // backup == -1：原 fd 本就不存在（如 `9>&1` 在 fd 9 上
                    // 新建描述符）；restore_fds 会按「关闭」恢复。
                    saved.push((*fd, backup));
                    if posix::dup2(src, *fd) < 0 {
                        // bash 实测：`echo hi 2>&9`（echo 为父 shell 内建）
                        // → `bash: line 1: 9: Bad file descriptor`，rc=1。
                        let e = posix::errno();
                        restore_fds(&saved);
                        let name = dup_bad_fd_name(raw, target, *fd, src);
                        return Err(format!("wash: {}: {}", name, posix::errno_message(e)));
                    }
                } else {
                    // `2>&x`：目标展开后不是数字，bash 以展开后的单字段文本
                    // 回显 ambiguous redirect（`x=abc; 2>&$x` → `abc: ...`）。
                    // `ExpRedir::Ambiguous`（零/多字段）路径仍回显原始词。
                    restore_fds(&saved);
                    return Err(format!("wash: {}: ambiguous redirect", target));
                }
            }
            ExpRedir::HereDoc(fd, text) => {
                // Builtins (`read x <<EOF`) run in the parent: spool the body
                // and swap the descriptor like a regular `< file`.
                let f = heredoc_fd(text);
                if f < 0 {
                    let e = posix::errno();
                    restore_fds(&saved);
                    return Err(format!("wash: here-document: {}", posix::errno_message(e)));
                }
                let backup = posix::dup(*fd);
                saved.push((*fd, backup));
                if posix::dup2(f, *fd) < 0 {
                    let e = posix::errno();
                    posix::close(f);
                    restore_fds(&saved);
                    return Err(format!("wash: here-document: {}", posix::errno_message(e)));
                }
                posix::close(f);
            }
            ExpRedir::Ambiguous(_fd, raw) => {
                // bash 对展开为多字段（或零字段）的重定向词以**原始词文本**
                // 报错（rc 1）；已应用的重定向按 bash 语义恢复原状。
                restore_fds(&saved);
                return Err(format!("wash: {}: ambiguous redirect", raw));
            }
        }
    }
    Ok(saved)
}

/// Restore the descriptors saved by `apply_redirs_in_parent`.
///
/// `(fd, -1)` 表示保存时原 fd 本就不存在（dup 失败，例如 `9>&1` 在 fd 9
/// 上新建描述符）：恢复动作是**关闭**该 fd，而不是把 -1 喂给 dup2
/// （#17：旧实现静默 dup2(-1, fd)，无效且泄漏新建的 fd）。
fn restore_fds(saved: &[(i32, i32)]) {
    for (fd, backup) in saved.iter().rev() {
        if *backup >= 0 {
            posix::dup2(*backup, *fd);
            posix::close(*backup);
        } else {
            posix::close(*fd);
        }
    }
}

/// Materialize here-document / here-string text as a readable file
/// descriptor.
///
/// The whole operation (create/write/rewind/unlink) is delegated to
/// `posix::heredoc_fd`, i.e. the C shim: Rust std's raw fds are NOT in the
/// msys fd table that `apply_exp_redirs` uses for dup2/close.  A temp file
/// (not a pipe) means the writer can never block on a small pipe buffer.
///
/// Returns -1 on failure; the caller reports it like a failed `open()`.
fn heredoc_fd(text: &str) -> i32 {
    posix::heredoc_fd(text)
}

/// Print the command that is about to run, the way bash's `set -x` does.
fn trace_command(argv: &[String]) {
    let mut line = String::from("+");
    for word in argv {
        line.push(' ');
        line.push_str(&builtins::quote_for_trace(word));
    }
    safe_eprintln!("{}", line);
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
    let tokens = crate::lexer::tokenize_nested(cmd).ok()?;
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
    nullglob: bool,
    bg_pid: i32,
    positional: &[String],
    dollar_zero: &str,
) -> String {
    let list = match parse_cmdsub_cached(cmd) {
        Some(list) => list,
        None => return String::new(),
    };
    let fds = posix::pipe();
    let r = fds[0];
    let w = fds[1];
    let pid = match fork_child() {
        Some(p) => p,
        None => {
            // fork failed: close both pipe ends and expose an empty
            // substitution with status 1 (never block on a nonexistent child).
            posix::close(w);
            posix::close(r);
            LAST_CMDSIB_STATUS.with(|c| *c.borrow_mut() = Some(1));
            return String::new();
        }
    };
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
        // `shopt` options are subshell-inherited too: a nullglob enabled in
        // the parent must suppress unmatched patterns inside `$(...)`.
        sh.nullglob = nullglob;
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
        // A command substitution runs in a subshell of the current shell: it
        // inherits positional parameters and `$0` (bash 5.3).  Without this,
        // `set -- aa; echo "$(echo $1)"` loses `$1`.
        sh.positional = positional.to_vec();
        sh.dollar_zero = dollar_zero.to_string();
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
    let mut s = posix::read_all(r);
    if s.contains('\0') {
        s.retain(|c| c != '\0');
        safe_eprintln!("wash: warning: command substitution: ignored null byte in input");
    }
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
    let path = sh.env.get("PATH").cloned();
    let pid = match fork_child() {
        Some(p) => p,
        None => return 1,
    };
    if pid == 0 {
        let (_cstrings, env_ptrs) = build_shell_env_ptrs(sh);
        let argvec = ArgVec::from(argv);
        match find_program(&argv[0], path.as_deref()) {
            Some((exe, display)) => {
                posix::exit(exec_classified(
                    &exe,
                    &display,
                    argv,
                    &argvec.ptrs,
                    &env_ptrs,
                    &sh.env,
                    false,
                ));
            }
            None => {
                safe_eprintln!(
                    "wash: {}: command not found",
                    crate::builtins::diag_text(&argv[0])
                );
                posix::exit(127);
            }
        }
        posix::exit(127);
    }
    posix::wait_pid(pid)
}

/// Execute the command built by `jobs -x` after jobspec→pid substitution.
/// bash runs it as an ordinary external command in the foreground; the
/// builtins that `jobs -x` may name are not special-cased here (oracle 实测
/// `jobs -x echo %1` 输出 pid).
pub fn run_job_x_command(args: &[String], sh: &Shell) -> i32 {
    exec_external(args, sh)
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
        Compound::Select {
            var,
            words,
            body,
            line,
        } => execute_select(var, words, body, *line, sh),
        Compound::ArithFor {
            init,
            cond,
            incr,
            body,
        } => execute_arith_for(init, cond, incr, body, sh),
        Compound::While {
            condition,
            body,
            is_until,
        } => execute_while(condition, body, *is_until, sh),
        Compound::Case { word, arms } => execute_case(word, arms, sh),
        // `[[ EXPR ]]` — a conditional command.  It succeeds when the
        // expression is true and fails when it is false; nothing else in
        // the shell changes.
        // `(( EXPR ))` — evaluate as arithmetic; status is 0 when the result
        // is non-zero, 1 when it is zero.  Assignments inside the expression
        // mutate shell variables through the same MapEnv `$(( ))` uses.
        Compound::Arith { expr, .. } => {
            match arith::eval(expr, &mut arith::MapEnv { vars: &mut sh.env }) {
                Ok(value) => {
                    sh.last_status = if value != 0 { 0 } else { 1 };
                    sh.last_status
                }
                Err(_) => {
                    sh.last_status = 1;
                    1
                }
            }
        }
        Compound::Cond { expr, .. } => {
            let attrs = sh.attr_refs();
            let ctx = ExpandCtx {
                env: &sh.env,
                positional: &sh.positional,
                last_status: sh.last_status,
                self_pid: sh.self_pid,
                noglob: sh.options.noglob,
                nullglob: sh.nullglob,
                nounset: sh.options.nounset,
                shell_flags: sh.dash_flags(),
                set_options: &sh.options,
                bg_pid: sh.bg_pid,
                dollar_zero: &sh.dollar_zero,
                attrs: &attrs,
                functions: &sh.functions,
                arrays: &sh.arrays,
                assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
                pending_assign: None,
                arith_overlay: None,
            };
            let status = if crate::cond::eval_cond(expr, &ctx) { 0 } else { 1 };
            // A successful (or failed) `=~` publishes its captures through
            // BASH_REMATCH; other conditional commands leave it untouched.
            if let Some(rematch) = crate::cond::take_rematch() {
                sh.arrays.insert("BASH_REMATCH".to_string(), rematch);
                sh.array_assigned.insert("BASH_REMATCH".to_string());
            }
            sh.last_status = status;
            status
        }
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
            let pid = match fork_child() {
                Some(p) => p,
                None => {
                    sh.last_status = 1;
                    return 1;
                }
            };
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
        nullglob: sh.nullglob,
        nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        set_options: &sh.options,
        bg_pid: sh.bg_pid,
        dollar_zero: &sh.dollar_zero,
        attrs: &attrs,
        functions: &sh.functions,
        arrays: &sh.arrays,
        assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
        pending_assign: None,
        arith_overlay: None,
    };
    let mut fields: Vec<String> = Vec::new();
    for w in words {
        fields.extend(expand_word(w, &ctx));
    }
    let mut code = 0;
    for field in fields {
        // bash still attempts (and fails) the loop-variable assignment for a
        // readonly NAME, reporting the error and aborting the loop before the
        // body runs.  Keep the fast path for the ordinary case untouched.
        if sh.readonly.contains(var) {
            safe_eprintln!("wash: {}: readonly variable", var);
            return 1;
        }
        sh.env.insert(var.to_string(), field);
        code = execute_loop_body(body, sh);
        // bash leaves the loop variable set to its final value (the loop body
        // shares the caller's scope), so we keep it rather than unsetting it.
        if sh.exit_requested.is_some()
            || sh.return_requested.is_some()
            || sh.fatal_error.is_some()
        {
            return code;
        }
        if let Some(n) = sh.break_requested.take() {
            if n > 1 {
                sh.break_requested = Some(n - 1);
            }
            break;
        }
        // A plain `continue` starts the next iteration.  A level > 1 belongs
        // to an enclosing loop: decrement it and leave this loop so that the
        // outer loop can consume the flag instead of swallowing it here.
        if let Some(n) = sh.continue_requested.take() {
            if n > 1 {
                sh.continue_requested = Some(n - 1);
                break;
            }
        }
    }
    code
}

/// Print bash 5.3's numbered `select` menu on stderr.
///
/// bash's `print_select_menu` (execute_cmd.c) uses the same column math as
/// `ls -C`, then emits one physical line per row with a literal TAB between
/// cells.  For a single-column menu the item numbers are right-aligned to the
/// widest number (` 1) …` when there are 10+ choices); for multi-column menus
/// the same padded cell is used but the line's leading padding is dropped, so
/// the first cell starts flush at column 0 while later cells keep their pad.
/// Widths are in BYTES (LC_ALL=C); COLUMNS is read from the shell variable and
/// defaults to 80 when unset / zero / non-numeric.
fn print_select_menu(fields: &[String], sh: &Shell) {
    if fields.is_empty() { return; }
    let n = fields.len();
    let number_width = n.to_string().len();
    // A cell is the item number right-aligned to `number_width`, `") "`, and
    // the item's raw bytes.  The widest cell fixes the column pitch.
    let cells: Vec<Vec<u8>> = fields.iter().enumerate().map(|(i, f)| {
        let mut c = format!("{:>w$}) ", i + 1, w = number_width).into_bytes();
        c.extend(crate::byteval::decode_raw(f));
        c
    }).collect();
    let cellw = cells.iter().map(|c| c.len()).max().unwrap_or(0) + 2;
    let columns: usize = sh.env.get("COLUMNS")
        .and_then(|v| v.trim().parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(80);

    // `ls -C` layout: cap the columns that fit, derive the row count, then
    // recompute the real column count from it.  Everything fitting on one
    // row falls back to a single column.
    let maxcols = std::cmp::max(1, columns / cellw);
    let mut nrows = (n + maxcols - 1) / maxcols;
    let mut ncols = (n + nrows - 1) / nrows;
    if ncols >= n {
        ncols = 1;
        nrows = n;
    }
    // The first column holds numbers 1..nrows, so bash right-aligns it to
    // that narrower width; later columns use the global width.  This is why
    // `11` items in one row print `1) 1` but ` 3) 3`.
    let first_width = nrows.to_string().len();

    let mut out: Vec<u8> = Vec::new();
    for r in 0..nrows {
        let mut line: Vec<u8> = Vec::new();
        let mut cur: usize = 0; // expanded display column, 8-wide tabstops
        for c in 0..ncols {
            let idx = c * nrows + r;
            if idx >= n { break; }
            let has_next = c + 1 < ncols && (c + 1) * nrows + r < n;
            let mut cell: Vec<u8> = Vec::new();
            let w = if c == 0 { first_width } else { number_width };
            cell.extend_from_slice(format!("{:>w$}", idx + 1, w = w).as_bytes());
            cell.extend_from_slice(b") ");
            cell.extend(crate::byteval::decode_raw(&fields[idx]));
            line.extend_from_slice(&cell);
            cur += cell.len();
            if has_next {
                // Greedy tabs while the next tabstop does not overshoot the
                // next cell's logical start, then spaces for the remainder.
                let target = (c + 1) * cellw;
                while (cur / 8 + 1) * 8 <= target {
                    line.push(b'\t');
                    cur = (cur / 8 + 1) * 8;
                }
                for _ in cur..target {
                    line.push(b' ');
                }
                cur = target;
            }
        }
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    safe_eprint_bytes!(out);
}

/// Run a `select NAME [in WORDS]; do BODY; done` loop.
///
/// bash prints the numbered menu and `PS3` on stderr, reads one raw line from
/// stdin into `REPLY`, and runs the body once per line — including an invalid
/// choice (NAME is then empty) and EOF (NAME empty, final status 1).  A blank
/// line only redisplays the menu.
fn execute_select(
    var: &str,
    words: &[Vec<WordPart>],
    body: &CommandList,
    line: usize,
    sh: &mut Shell,
) -> i32 {
    // bash validates the loop variable before printing anything.
    if !crate::parser::is_valid_ident(var) {
        safe_eprintln!("wash: line {}: `{}': not a valid identifier", line, var);
        return 1;
    }
    let attrs = sh.attr_refs();
    let ctx = ExpandCtx {
        env: &sh.env,
        positional: &sh.positional,
        last_status: sh.last_status,
        self_pid: sh.self_pid,
        noglob: sh.options.noglob,
        nullglob: sh.nullglob,
        nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        set_options: &sh.options,
        bg_pid: sh.bg_pid,
        dollar_zero: &sh.dollar_zero,
        attrs: &attrs,
        functions: &sh.functions,
        arrays: &sh.arrays,
        assoc_arrays: &sh.assoc_arrays,
        array_set_indices: &sh.array_set_indices,
        pending_assign: None,
        arith_overlay: None,
    };
    let mut fields: Vec<String> = Vec::new();
    for w in words {
        fields.extend(expand_word(w, &ctx));
    }
    // With an empty word list bash does not print a menu or read at all.
    if fields.is_empty() {
        return 0;
    }

    print_select_menu(&fields, sh);

    let mut code = 0;
    let mut first = true;
    loop {
        // bash redisplays the menu whenever `$REPLY` is empty at the start
        // of a selection cycle -- after a blank line, after a body that
        // cleared it, or after a nested `select` hit EOF.
        if !first && sh.env.get("REPLY").map(String::is_empty).unwrap_or(true) {
            print_select_menu(&fields, sh);
        }
        first = false;
        let prompt = match sh.env.get("PS3") {
            Some(p) => p.clone(),
            None => "#? ".to_string(),
        };
        safe_eprint_bytes!(crate::byteval::decode_raw(&prompt));

        // `select` reads a whole line with bash's default `read` semantics
        // (NOT `read -r`): a backslash quotes the next character, and a
        // backslash-newline pair disappears (line continuation).
        let mut bytes: Vec<u8> = Vec::new();
        let mut eof = false;
        let mut pass_next = false;
        loop {
            match posix::read_byte_from(0) {
                Some(b'\n') => {
                    if pass_next {
                        // Backslash-newline: continue with the next line.
                        pass_next = false;
                        continue;
                    }
                    break;
                }
                Some(b'\\') if !pass_next => {
                    pass_next = true;
                }
                Some(b) => {
                    pass_next = false;
                    bytes.push(b);
                }
                None => {
                    eof = true;
                    break;
                }
            }
        }

        if eof {
            // bash prints one newline for the failed read, clears `REPLY`,
            // and leaves the loop without running the body (status 1).
            sh.env.insert("REPLY".to_string(), String::new());
            let _ = std::io::stdout().write_all(b"\n");
            code = 1;
            break;
        }
        if bytes.is_empty() {
            // A blank reply sets `REPLY` to empty and redisplays the menu at
            // the next cycle without running the body.
            sh.env.insert("REPLY".to_string(), String::new());
            continue;
        }
        sh.env.insert("REPLY".to_string(), crate::byteval::encode_raw(&bytes));

        let reply = sh.env.get("REPLY").map(String::as_str).unwrap_or("");
        let chosen = reply.trim().parse::<i64>().ok().and_then(|n| {
            if n >= 1 && n <= fields.len() as i64 {
                Some(n as usize - 1)
            } else {
                None
            }
        });
        let selected = chosen.map(|i| fields[i].clone()).unwrap_or_default();
        // As with `for`, a readonly NAME makes bash report the failed
        // assignment after PS3 / REPLY are handled, skip the body and end the
        // loop with status 1 (the menu/prompt/REPLY side effects stay intact).
        if sh.readonly.contains(var) {
            safe_eprintln!("wash: {}: readonly variable", var);
            code = 1;
            break;
        }
        sh.env.insert(var.to_string(), selected);
        code = execute_loop_body(body, sh);
        if sh.exit_requested.is_some()
            || sh.return_requested.is_some()
            || sh.fatal_error.is_some()
        {
            return code;
        }
        if let Some(n) = sh.break_requested.take() {
            if n > 1 {
                sh.break_requested = Some(n - 1);
            }
            break;
        }
        if let Some(n) = sh.continue_requested.take() {
            if n > 1 {
                sh.continue_requested = Some(n - 1);
                break;
            }
        }
    }
    code
}

/// bash reports the error token as the raw remainder of the expression,
/// including trailing whitespace, so `x/0 ` reports token `"0 "`.
fn arith_error_token(expr: &str, token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    let trailing: String = expr
        .chars()
        .rev()
        .take_while(|c| c.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    format!("{}{}", token, trailing)
}

/// Evaluate one clause of a C-style `for (( ... ))` loop.
///
/// Returns `false` when bash would abort the whole command (arithmetic
/// error).  The expression text is already trimmed; empty clauses are
/// skipped by the caller.
fn eval_arith_clause(expr: &str, sh: &mut Shell) -> bool {
    match arith::eval(expr, &mut arith::MapEnv { vars: &mut sh.env }) {
        Ok(_) => true,
        Err(e) => {
            let token = if e.token.is_empty() {
                String::new()
            } else {
                format!(
                    " (error token is \"{}\")",
                    arith_error_token(expr, &e.token)
                )
            };
            safe_eprintln!("wash: ((: {}: {}{}", expr, e.message, token);
            sh.last_status = 1;
            false
        }
    }
}

/// Run `for (( INIT ; COND ; INCR )); do BODY; done`.
///
/// INIT runs once; COND runs before each iteration (an empty COND means
/// "forever"); INCR runs after each iteration, including one ended by
/// `continue`, but not after `break`.  The loop's status is the status of
/// the last body command, or 0 when the body never ran.
fn execute_arith_for(
    init: &str,
    cond: &str,
    incr: &str,
    body: &CommandList,
    sh: &mut Shell,
) -> i32 {
    if !init.is_empty() && !eval_arith_clause(init, sh) {
        return 1;
    }
    let mut code = 0;
    loop {
        if !cond.is_empty() {
            match arith::eval(cond, &mut arith::MapEnv { vars: &mut sh.env }) {
                Ok(value) if value != 0 => {}
                Ok(_) => return code,
                Err(e) => {
                    let token = if e.token.is_empty() {
                        String::new()
                    } else {
                        format!(
                    " (error token is \"{}\")",
                    arith_error_token(cond, &e.token)
                )
                    };
                    safe_eprintln!("wash: ((: {}: {}{}", cond, e.message, token);
                    sh.last_status = 1;
                    return 1;
                }
            }
        }
        code = execute_loop_body(body, sh);
        if sh.exit_requested.is_some()
            || sh.return_requested.is_some()
            || sh.fatal_error.is_some()
        {
            return code;
        }
        if let Some(n) = sh.break_requested.take() {
            if n > 1 {
                sh.break_requested = Some(n - 1);
            }
            break;
        }
        // `continue` still runs the increment clause.  A level > 1 belongs
        // to an enclosing loop: decrement it, then leave this loop so that
        // the outer one can consume the flag before its own body resets it.
        let continue_level = sh.continue_requested.take();
        if let Some(n) = continue_level {
            if n > 1 {
                sh.continue_requested = Some(n - 1);
            }
            if !incr.is_empty() && !eval_arith_clause(incr, sh) {
                return 1;
            }
            if n > 1 {
                break;
            }
            continue;
        }
        if !incr.is_empty() && !eval_arith_clause(incr, sh) {
            return 1;
        }
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
        if sh.exit_requested.is_some()
            || sh.return_requested.is_some()
            || sh.fatal_error.is_some()
        {
            return cond;
        }
        if let Some(n) = sh.break_requested.take() {
            if n > 1 {
                sh.break_requested = Some(n - 1);
            }
            break;
        }
        if let Some(n) = sh.continue_requested.take() {
            // `continue` in the condition re-evaluates it; a level > 1 is
            // aimed at an enclosing loop and must be carried outwards.
            if n > 1 {
                sh.continue_requested = Some(n - 1);
                break;
            }
            continue;
        }

        let stop = if is_until { cond == 0 } else { cond != 0 };
        if !stop {
            code = execute_loop_body(body, sh);
            if sh.exit_requested.is_some()
                || sh.return_requested.is_some()
                || sh.fatal_error.is_some()
            {
                return code;
            }
            if let Some(n) = sh.break_requested.take() {
                if n > 1 {
                    sh.break_requested = Some(n - 1);
                }
                break;
            }
            if let Some(n) = sh.continue_requested.take() {
                if n > 1 {
                    sh.continue_requested = Some(n - 1);
                    break;
                }
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
    // bash expands the case word exactly once, before any arm runs.
    let value = {
        let attrs = sh.attr_refs();
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nullglob: sh.nullglob,
            nounset: sh.options.nounset,
            shell_flags: sh.dash_flags(),
            set_options: &sh.options,
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
            pending_assign: None,
            arith_overlay: None,
        };
        expand_scalar(word, &ctx)
    };
    // bash's case terminators form a small state machine:
    //   `;;`   stop after this body;
    //   `;&`   execute the next arm's body unconditionally (no pattern test);
    //   `;;&`  continue pattern matching with the next arm.
    // `skip_patterns` is set by `;&`; once set, the next arm runs its body
    // regardless of its patterns, and its own terminator decides what follows.
    let mut status = 0;
    let mut skip_patterns = false;
    for arm in arms {
        let mut matched = skip_patterns;
        skip_patterns = false;
        if !matched {
            // A fresh short-lived ctx per arm: `;;&` re-tests patterns after
            // earlier bodies ran, so they must be expanded at that point.
            let attrs = sh.attr_refs();
            let ctx = ExpandCtx {
                env: &sh.env,
                positional: &sh.positional,
                last_status: sh.last_status,
                self_pid: sh.self_pid,
                noglob: sh.options.noglob,
                nullglob: sh.nullglob,
                nounset: sh.options.nounset,
                shell_flags: sh.dash_flags(),
                set_options: &sh.options,
                bg_pid: sh.bg_pid,
                dollar_zero: &sh.dollar_zero,
                attrs: &attrs,
                functions: &sh.functions,
                arrays: &sh.arrays,
                assoc_arrays: &sh.assoc_arrays,
                array_set_indices: &sh.array_set_indices,
                pending_assign: None,
                arith_overlay: None,
            };
            for pat in &arm.patterns {
                let p = expand_scalar(pat, &ctx);
                if glob_match(&value, &p) {
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            continue;
        }
        status = execute(&arm.body, sh);
        match arm.terminator {
            crate::parser::CaseTerminator::DoubleSemi
            | crate::parser::CaseTerminator::Esac => return status,
            crate::parser::CaseTerminator::Fall => skip_patterns = true,
            crate::parser::CaseTerminator::FallTest => {}
        }
    }
    status
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
            safe_eprintln!(
                "wash: {}: command not found",
                crate::builtins::diag_text(name)
            );
            return 127;
        }
    };
    let saved_positional = std::mem::replace(&mut sh.positional, args);
    sh.function_depth += 1;
    if sh.function_depth > MAX_FUNCTION_DEPTH {
        sh.function_depth -= 1;
        safe_eprintln!("wash: maximum function nesting level exceeded");
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
    /// `shopt nullglob` is not part of `SetOptions`, so forked children need
    /// this explicit snapshot to expand patterns the same way as the parent.
    nullglob: bool,
    positional: &'a [String],
    dollar_zero: &'a str,
    attrs: &'a crate::shell::AttrRefs<'a>,
    /// The pid of the most recent background job in the parent.  A forked
    /// child (subshell) inherits the parent's background-job table, so `$!`
    /// inside the child refers to that job the same way it would in the
    /// parent.  `Shell::from_env` can only build a default `bg_pid` of 0, so
    /// the parent's value is threaded through here.
    bg_pid: i32,
    /// The parent shell's function table.  A forked pipeline stage / subshell
    /// must be able to call functions defined in the parent (bash behaviour);
    /// `Shell::from_env` only builds an empty table, which made `myfunc | cat`
    /// fail with 127.  Borrowed (no clone) so the pure-external fast path that
    /// skips `Shell::from_env()` entirely stays untouched.
    functions: &'a HashMap<String, crate::parser::CommandList>,
    /// The compound command (`if ... fi`) the child has to run, if any.
    compound: Option<&'a Compound>,
    /// The program the child has to execute, already resolved by the
    /// parent: the execve path plus the name bash-style diagnostics
    /// report (the `$PATH` entry without the implicit `.exe`).
    program: Option<&'a (CString, String)>,
    /// True when the parent ALREADY performed the `$PATH` search for this
    /// command (with the command's own `PATH=` assignments applied).  When
    /// `program` is `None` and this is true, the child must report
    /// `command not found` instead of re-searching a stale snapshot PATH —
    /// the snapshot would silently ignore `PATH=... cmd` prefixes.
    program_searched: bool,
    /// True when the parent determined the command word may be a builtin
    /// (static or plugin), which is why `program` was left `None`.  When
    /// `program` is `Some` AND this is false, the child can exec straight
    /// away without constructing a full `Shell` first.
    builtin_candidate: bool,
}

/// Record an executed program in the shell's hash table.
fn record_hash_entry(sh: &mut Shell, name: &str, path: &str) {
    sh.hash_table.insert(name.to_string(), path.to_string());
    *sh.hash_hits.entry(name.to_string()).or_insert(0) += 1;
}

/// Run a command inside the forked child: either a builtin (whose state
/// changes are lost with the child) or an external program.
fn child_run_exp(argv: &[String], path: Option<&str>, ctx: &ChildContext) -> ! {
    if argv.is_empty() {
        // A compound command (`if ... fi`) has no words of its own: it runs
        // inside the child.
        if let Some(compound) = ctx.compound {
            let mut sub = Shell::from_env(ctx.shell_env.clone());
            // Inherit the parent's background-job table so `$!` works inside
            // a forked compound command (e.g. `sleep & ( if ...; fi )`).
            sub.bg_pid = ctx.bg_pid;
            sub.apply_attr_refs(ctx.attrs);
            sub.functions = ctx.functions.clone();
            sub.arrays = ctx.arrays.clone();
            sub.positional = ctx.positional.to_vec();
            sub.options = ctx.options.clone();
            sub.nullglob = ctx.nullglob;
            sub.dollar_zero = ctx.dollar_zero.to_string();
            let code = execute_compound(compound, &mut sub);
            let _ = std::io::stdout().flush();
            posix::exit(code);
        }
        posix::exit(0);
    }
    // Pure external fast path: the parent already resolved `program` and
    // proved the word is NOT a builtin (static or plugin).  Constructing a
    // full `Shell` here would deep-copy the whole variable table, rebuild
    // `exported`, install 15 internal variables (3 uid/ppid FFI calls +
    // `group_ids()`), and clone arrays / positional / options -- all thrown
    // away by `execve`.  Go straight to exec.  `run_builtin` would have
    // returned `None` for this argv, so stdout/stderr/rc are unchanged.
    if !ctx.builtin_candidate {
        if let Some((exe, display)) = ctx.program {
            let (_cstrings, env_ptrs) = build_env_ptrs(ctx.env);
            let argvec = ArgVec::from(argv);
            // 子进程里 exec_classified 永不返回（失败分类后 exit）。
            posix::exit(exec_classified(
                exe,
                display,
                argv,
                &argvec.ptrs,
                &env_ptrs,
                ctx.shell_env,
                false,
            ));
        }
    }
    // The redirections have already been applied by the caller (pipes and the
    // command's own redirections), so the builtins can simply write to stdout.
    let mut sub = Shell::from_env(ctx.shell_env.clone());
    // Inherit the parent's background-job table so `$!` works inside a forked
    // subshell / builtin-in-child context.
    sub.bg_pid = ctx.bg_pid;
    sub.apply_attr_refs(ctx.attrs);
    sub.functions = ctx.functions.clone();
    sub.arrays = ctx.arrays.clone();
    sub.positional = ctx.positional.to_vec();
    sub.options = ctx.options.clone();
    sub.nullglob = ctx.nullglob;
    sub.dollar_zero = ctx.dollar_zero.to_string();
    // 管道阶段 / fork 子进程里的 shell 函数：父进程 execute_simple 通过
    // execute_function 直接执行，但子进程由 Shell::from_env 重建，必须显式
    // 查（ChildContext 已透传的）函数表执行，否则 `myfunc | cat` 报 127。
    // 纯 external 快路径在上面已经 return/exec，不经过这里。
    if sub.functions.contains_key(&argv[0]) {
        let code = execute_function(&argv[0], argv[1..].to_vec(), &mut sub);
        let _ = std::io::stdout().flush();
        posix::exit(code);
    }
    if let Some(code) = builtins::run_builtin(argv, &mut sub) {
        let _ = std::io::stdout().flush();
        posix::exit(code);
    }
    // Not a builtin: look it up in $PATH and replace the child process.
    let (_cstrings, env_ptrs) = build_env_ptrs(ctx.env);
    let argvec = ArgVec::from(argv);
    let (exe, display) = match ctx.program {
        Some((exe, display)) => (exe.clone(), display.clone()),
        None if ctx.program_searched => {
            // 父进程已按（应用过 `PATH=` 前缀赋值的）PATH 搜索且未命中：
            // 直接报 command not found，不得用快照 PATH 重搜。
            safe_eprintln!(
                "wash: {}: command not found",
                crate::builtins::diag_text(&argv[0])
            );
            posix::exit(127);
        }
        None => match find_program(&argv[0], path) {
            Some((exe, display)) => (exe, display),
            None => {
                safe_eprintln!(
                    "wash: {}: command not found",
                    crate::builtins::diag_text(&argv[0])
                );
                posix::exit(127);
            }
        },
    };
    exec_classified(
        &exe,
        &display,
        argv,
        &argvec.ptrs,
        &env_ptrs,
        ctx.shell_env,
        false,
    );
    // 子进程里 exec_classified 永不返回；保险起见按 127 退出。
    posix::exit(127)
}

/// Assign to an array element (`a[i]=v` / `m[k]=v`).  The subscript source is
/// expanded first; associative arrays use the result as a literal key, while
/// indexed arrays evaluate it arithmetically exactly like bash.
fn assign_subscript_var(
    sh: &mut Shell,
    base: &str,
    sub_src: &str,
    value: String,
    append: bool,
) -> bool {
    let subscript = {
        let attrs = sh.attr_refs();
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nullglob: sh.nullglob,
            nounset: sh.options.nounset,
            shell_flags: sh.dash_flags(),
            set_options: &sh.options,
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
            pending_assign: None,
            arith_overlay: None,
        };
        // `sub_src` is source text (`$k`, `${k}`), not an already-expanded
        // value: re-lex it so `$k` becomes a Var part and is expanded by
        // `expand_scalar` instead of being written as a literal key.
        let parts = crate::lexer::tokenize(sub_src)
            .ok()
            .and_then(|toks| toks.into_iter().next())
            .and_then(|tok| match tok {
                crate::lexer::Token::Word(p) => Some(p),
                _ => None,
            })
            .unwrap_or_else(|| vec![WordPart::Text(sub_src.to_string(), false)]);
        expand_scalar(&parts, &ctx)
    };
    if sh.assoc_declared.contains(base) {
        let entries = sh.assoc_arrays.entry(base.to_string()).or_default();
        match entries.iter_mut().find(|(k, _)| k == &subscript) {
            Some(pair) => {
                if append {
                    pair.1.push_str(&value);
                } else {
                    pair.1 = value;
                }
            }
            None => entries.push((subscript, value)),
        }
    } else {
        let idx_i64 = arith::eval(&subscript, &mut arith::MapEnv { vars: &mut sh.env })
            .unwrap_or(0);
        if idx_i64 < 0 {
            safe_eprintln!("wash: {}: bad array subscript", base);
            return false;
        }
        let entries = sh.arrays.entry(base.to_string()).or_default();
        let idx = idx_i64 as usize;
        if entries.len() <= idx {
            entries.resize(idx + 1, String::new());
        }
        if append {
            entries[idx].push_str(&value);
        } else {
            entries[idx] = value;
        }
        sh.array_set_indices
            .entry(base.to_string())
            .or_default()
            .insert(idx_i64);
    }
    sh.array_assigned.insert(base.to_string());
    true
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
    if let Some(open) = name.find('[') {
        if name.ends_with(']') && open + 1 < name.len() - 1 {
            return assign_subscript_var(sh, &name[..open], &name[open + 1..name.len() - 1], value, append);
        }
    }
    if sh.readonly.contains(name) {
        safe_eprintln!("wash: {}: readonly variable", name);
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
/// 
/// The `env_values` slice contains the already-expanded RHS values from
/// `prepare_stage`, so we avoid re‑expanding the original `WordPart`s
/// (which would double‑expand `$(…)` etc.).
fn apply_command_assignments(
    env_values: &[(String, String, bool)],
    sh: &mut Shell,
) -> Vec<(String, Option<String>)> {
    if env_values.is_empty() {
        return Vec::new();
    }
    let attrs = sh.attr_refs();
    let pending: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
    // Arithmetic assignments become visible to later expansions on this
    // command immediately (bash semantics); `pending` still writes them back
    // to `sh.env` once expansion of the command is complete.
    let overlay: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    let applied: Vec<(String, String, bool)> = env_values
        .iter()
        .map(|(n, v, ap)| (n.clone(), v.clone(), *ap))
        .collect();
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
            nullglob: sh.nullglob,
            nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        set_options: &sh.options,
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
            pending_assign: None,
            arith_overlay: None,
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
            let base = all.len() as i64;
            all.extend(v.iter().cloned());
            sh.arrays.insert(name.clone(), all);
            let indices = sh.array_set_indices.entry(name).or_default();
            for k in base..base + v.len() as i64 {
                indices.insert(k);
            }
        } else {
            sh.arrays.insert(name.clone(), v.iter().cloned().collect());
            sh.array_set_indices
                .insert(name, (0..v.len() as i64).collect());
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

/// bash 在命令查找之前先应用命令前缀的赋值：`PATH= ls`、
/// `PATH=/nonexistent ls`、`PATH+=/x ls` 的命令查找都用**应用后**的 PATH
/// （MSYS2 实测：`PATH= ls` 报 command not found，`PATH= foo` 在 cwd 有
/// `foo` 时能执行 —— 空元素即当前目录）。返回查找用的 PATH；
/// `None` = PATH 未设置（裸名直接 execve，失败报 ENOENT 文案）。
fn command_search_path(
    env_values: Option<&[(String, String, bool)]>,
    sh: &Shell,
) -> Option<String> {
    let mut cur: Option<String> = sh.env.get("PATH").cloned();
    if let Some(values) = env_values {
        for (n, v, ap) in values {
            if n == "PATH" {
                cur = Some(if *ap {
                    format!("{}{}", cur.unwrap_or_default(), v)
                } else {
                    v.clone()
                });
            }
        }
    }
    cur
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
    path: Option<&str>,
) -> i32 {
    if argv.is_empty() {
        // A compound command (`if ... fi`): its redirections apply to the
        // whole construct, exactly like in bash.
        if let Some(compound) = compound {
            let saved = match apply_redirs_in_parent(redirs, sh.options.noclobber) {
                Ok(saved) => saved,
                Err(message) => {
                    safe_eprintln!("{}", message);
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
            // Arithmetic assignments become visible to later expansions on this
            // command immediately (bash semantics); `pending` still writes them back
            // to `sh.env` once expansion of the command is complete.
            let overlay: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
            // Expand all RHS values first (immutable borrow of sh.env), then
            // mutate sh.env once the borrow is released.
            let applied: Vec<(String, String, bool)> = {
                let ctx = ExpandCtx {
                    env: &sh.env,
                    positional: &sh.positional,
                    last_status: sh.last_status,
                    self_pid: sh.self_pid,
                    noglob: sh.options.noglob,
                    nullglob: sh.nullglob,
                    nounset: sh.options.nounset,
        shell_flags: sh.dash_flags(),
        set_options: &sh.options,
        bg_pid: sh.bg_pid,
                    dollar_zero: &sh.dollar_zero,
                    attrs: &attrs,
                    functions: &sh.functions,
            arrays: &sh.arrays,
            assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
                    pending_assign: Some(&pending),
                    arith_overlay: Some(&overlay),
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
                safe_eprintln!("{}", message);
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
                safe_eprintln!("{}", message);
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
                safe_eprintln!("{}", message);
                return 1;
            }
        };
        // An assignment in front of the builtin (e.g. `IFS=: read x') is
        // visible to it.  It is always temporary: bash undoes it after the
        // command, even for the POSIX special builtins.
        let assignments = apply_command_assignments(
            env_values.as_deref().unwrap_or(&[]),
            sh,
        );
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
    let search_path = command_search_path(env_values, sh);
    let program = find_program(&argv[0], search_path.as_deref());
    // 前台作业控制：仅交互式 + tty 的主 shell 接管终端。job_ctl / shell_pgrp
    // 都在 fork 前算好，子进程继承同一份值；-c / piped（isatty 判假）整段跳过。
    let job_ctl = job_control_active(sh);
    let shell_pgrp = if job_ctl { posix::getpgrp() } else { -1 };
    let pid = match fork_child() {
        Some(p) => p,
        None => {
            // The external command could not be forked: undo the temporary
            // array assignments, fail this command with status 1 (bash
            // behaviour), and never record a bogus pid in the job table.
            restore_array_assignments(array_saves, sh);
            return 1;
        }
    };
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
            nullglob: sh.nullglob,
            positional: &sh.positional,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            bg_pid: sh.bg_pid,
            functions: &sh.functions,
            compound: None,
            program: program.as_ref(),
            // 外部命令的 PATH 搜索已在父进程完成（含命令前缀赋值）。
            program_searched: true,
            // Builtins and functions were already handled above in this
            // function, so reaching this fork means the word is external.
            builtin_candidate: false,
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
    let tokens = crate::lexer::tokenize_nested(&value).ok()?;
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
    // Arithmetic assignments become visible to later expansions on this
    // command immediately (bash semantics); `pending` still writes them back
    // to `sh.env` once expansion of the command is complete.
    let overlay: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());

    // `$-` is derived only from option/startup flags, never from `env`.
    // Both expansion passes below need the same snapshot, and the only
    // mutation between them is `pending` assignments into `sh.env`, so build
    // it ONCE here instead of calling `dash_flags()` twice per command.
    let shell_flags = sh.dash_flags();

    // Expand the stage's words.  The expander only reads the environment.
    let (argv, redirs) = {
        let attrs = sh.attr_refs();
        let ctx = ExpandCtx {
            env: &sh.env,
            positional: &sh.positional,
            last_status: sh.last_status,
            self_pid: sh.self_pid,
            noglob: sh.options.noglob,
            nullglob: sh.nullglob,
            nounset: sh.options.nounset,
            shell_flags: shell_flags.clone(),
            set_options: &sh.options,
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
            pending_assign: Some(&pending),
            arith_overlay: Some(&overlay),
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
            nullglob: sh.nullglob,
            nounset: sh.options.nounset,
            shell_flags,
            set_options: &sh.options,
            bg_pid: sh.bg_pid,
            dollar_zero: &sh.dollar_zero,
            attrs: &attrs,
            functions: &sh.functions,
            arrays: &sh.arrays,
            assoc_arrays: &sh.assoc_arrays,
            array_set_indices: &sh.array_set_indices,
            pending_assign: Some(&pending),
            arith_overlay: Some(&overlay),
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
fn child_prepare_and_run(c: &SimpleCommand, sh: &mut Shell, path: Option<&str>, noclobber: bool) -> ! {
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
    // A shell function shadows an external program of the same name (bash:
    // functions > builtins > PATH).  Count it as a "builtin candidate" so the
    // pure-external fast path in child_run_exp does not execve the external
    // file instead of running the function.  Pure externals are unaffected:
    // their names are not in sh.functions, so the fast path still applies.
    let builtin_candidate = pr.argv.is_empty()
        || builtins::is_builtin(sh, &pr.argv[0])
        || sh.functions.contains_key(&pr.argv[0]);
    let search_path = command_search_path(pr.env_values.as_deref(), sh);
    let program = if builtin_candidate {
        None
    } else {
        find_program(&pr.argv[0], search_path.as_deref())
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
        nullglob: sh.nullglob,
        positional: &sh.positional,
        dollar_zero: &sh.dollar_zero,
        attrs: &attrs,
        bg_pid: sh.bg_pid,
        functions: &sh.functions,
        compound: pr.compound.as_ref(),
        program: program.as_ref(),
        // 仅非 builtin 候选阶段由父进程完成搜索；builtin 候选（函数/
        // 内建）走 None + 未搜索，子进程兜底重搜才被允许。
        program_searched: !builtin_candidate,
        builtin_candidate,
    };
    child_run_exp(&pr.argv, path, &ctx);
}

fn execute_pipeline(p: &Pipeline, sh: &mut Shell, path: Option<&str>) -> i32 {
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
        let code = run_standalone_exp(
            &pr.argv,
            &pr.redirs,
            &pr.assigns,
            &pr.array_assigns,
            pr.compound.as_ref(),
            pr.env_values.as_deref(),
            sh,
            path,
        );
        // PIPESTATUS is rewritten by every completed command.  A single
        // command stores its own status as a one-element array.  The write is
        // placed after run_standalone_exp so an inner pipeline (inside a
        // function or compound command) is overridden by the enclosing
        // command's status, matching bash's "last command wins" semantics.
        sh.arrays
            .insert("PIPESTATUS".to_string(), vec![code.to_string()]);
        return code;
    }

    let mut prev_read: Option<i32> = None;
    let mut pids: Vec<i32> = Vec::new();
    let mut fork_failed = false;
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
        let pid = match fork_child() {
            Some(p) => p,
            None => {
                // A stage could not be forked: close this stage's own pipe
                // ends plus the inherited read end, stop spawning stages, and
                // still reap the stages already forked below (no zombies).
                if let Some(fd) = prev_read.take() {
                    posix::close(fd);
                }
                if let Some(fd) = this_read {
                    posix::close(fd);
                }
                if let Some(fd) = this_write {
                    posix::close(fd);
                }
                fork_failed = true;
                break;
            }
        };
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
    // PIPESTATUS holds every stage's raw exit status.  It is NOT affected by
    // `set -o pipefail`, which only adjusts the pipeline's own `$?`.
    let mut statuses: Vec<String> = Vec::with_capacity(n);
    for pid in pids {
        let status = posix::wait_pid(pid);
        statuses.push(status.to_string());
        last = status;
        if status != 0 {
            failure = status;
        }
    }
    if job_ctl {
        // 硬要求：所有子进程 wait 完毕后恢复 shell 的终端前台进程组。
        posix::tcsetpgrp(0, shell_pgrp);
    }
    // A fork failure fails the whole pipeline with status 1, regardless of
    // the stages that did run (bash semantics).  The stages already forked
    // were reaped above and the terminal foreground group was restored.
    if fork_failed {
        // A fork failure fails the whole pipeline with status 1.  Its exact
        // PIPESTATUS oracle value is not stably reproducible; record [1].
        sh.arrays
            .insert("PIPESTATUS".to_string(), vec!["1".to_string()]);
        return 1;
    }
    // With `set -o pipefail` the pipeline fails when any stage failed; the
    // status is then the one of the rightmost failing stage.  PIPESTATUS keeps
    // the raw per-stage statuses regardless of pipefail.
    let code = if pipefail && failure != 0 {
        failure
    } else {
        last
    };
    sh.arrays.insert("PIPESTATUS".to_string(), statuses);
    code
}

/// Normalize a raw background-command fragment for the `jobs` display.
///
/// bash rebuilds the job's command text from its source words: runs of
/// unquoted whitespace collapse to a single separator, both ends are trimmed,
/// quotes / escapes / variables stay literal (nothing is re-expanded), the
/// whitespace inside a `$(`-substitution is kept verbatim, and every
/// redirection operator is separated from its target word by exactly one
/// space — except descriptor duplication (`2>&1`, `<&-`), which bash renders
/// without any space (and with an implicit `1` / `0` fd when the source had
/// none, e.g. `>&1` becomes `1>&1`).
fn normalize_job_text(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    // Depth of `$(` / `$((` substitutions: their inner whitespace is part of
    // the displayed text and must not be collapsed.
    let mut subst_depth = 0usize;
    // Set by an unquoted run of whitespace at group depth 0; flushed as a
    // single space before the next non-space fragment.
    let mut pending_space = false;

    while i < n {
        let c = chars[i];
        if in_single {
            out.push(c);
            if c == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            // Quoted whitespace stays verbatim, exactly like in bash.
            if c == '"' {
                in_double = false;
                out.push('"');
            } else if c == '\\' && i + 1 < n {
                out.push('\\');
                out.push(chars[i + 1]);
                i += 2;
                continue;
            } else {
                out.push(c);
            }
            i += 1;
            continue;
        }
        match c {
            '\'' => {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                in_single = true;
                out.push('\'');
                i += 1;
            }
            '"' => {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                in_double = true;
                out.push('"');
                i += 1;
            }
            '\\' => {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                out.push('\\');
                if i + 1 < n {
                    out.push(chars[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '$' if i + 1 < n && chars[i + 1] == '(' => {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                out.push('$');
                out.push('(');
                i += 2;
                subst_depth += 1;
            }
            ' ' | '\t' | '\n' => {
                if subst_depth > 0 {
                    out.push(c);
                } else {
                    pending_space = true;
                }
                i += 1;
            }
            '(' if subst_depth > 0 => {
                out.push('(');
                subst_depth += 1;
                i += 1;
            }
            ')' if subst_depth > 0 => {
                out.push(')');
                subst_depth -= 1;
                i += 1;
            }
            '<' | '>' if subst_depth == 0 => {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                // Descriptor duplication (`>&N` / `<&N` / `>&-`): bash keeps
                // operator and target glued.  An implicit `1` / `0` fd is
                // printed only when the target is itself a valid fd spec
                // (`>&1`, `>&12`, `>&-`); an ambiguous word target (`>&1x`)
                // keeps the raw source form.
                let is_dup = i + 1 < n && chars[i + 1] == '&';
                if is_dup && is_valid_dup_target(&chars, i + 2)
                    && !ends_with_bare_fd(&out)
                {
                    if !out.ends_with(' ') && !out.is_empty() {
                        out.push(' ');
                    }
                    out.push(if c == '>' { '1' } else { '0' });
                }
                out.push(c);
                i += 1;
                // Operator suffixes: `>>`, `<<`, `<<<`, `<>`, `<<-`.
                let mut heredoc = false;
                if i < n && (chars[i] == '<' || chars[i] == '>') {
                    let second = chars[i];
                    out.push(second);
                    i += 1;
                    if second == '<' {
                        if i < n && chars[i] == '<' {
                            // `<<<` here-string: bash separates it from its
                            // word (`<<< xyz`).
                            out.push('<');
                            i += 1;
                        } else {
                            // `<<` / `<<-` heredocs: bash keeps the
                            // delimiter glued (`cat <<EOF`).
                            heredoc = true;
                            if i < n && chars[i] == '-' {
                                out.push('-');
                                i += 1;
                            }
                        }
                    }
                }
                if is_dup {
                    out.push('&');
                    i += 1;
                } else if !heredoc
                    && i < n
                    && chars[i] != ' '
                    && chars[i] != '\t'
                    && chars[i] != '\n'
                {
                    // Every other redirection takes one space before its
                    // target (`>/dev/null` displays as `> /dev/null`).
                    out.push(' ');
                }
            }
            '&' if subst_depth == 0 => {
                if pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                out.push('&');
                i += 1;
                if i < n && chars[i] == '>' {
                    // `&>` / `&>>`: one space before the target, like bash.
                    out.push('>');
                    i += 1;
                    if i < n && chars[i] == '>' {
                        out.push('>');
                        i += 1;
                    }
                    if i < n
                        && chars[i] != ' '
                        && chars[i] != '\t'
                        && chars[i] != '\n'
                    {
                        out.push(' ');
                    }
                }
            }
            _ => {
                if subst_depth == 0 && pending_space {
                    out.push(' ');
                    pending_space = false;
                }
                out.push(c);
                i += 1;
            }
        }
    }
    out.trim().to_string()
}

/// True when the word at `start` is a valid descriptor-duplication target:
/// either all digits (an fd number) or a lone `-` (close the descriptor).
/// Anything else (`1x`) is an ambiguous word target, which bash displays
/// verbatim without an implicit fd.
fn is_valid_dup_target(chars: &[char], start: usize) -> bool {
    if start >= chars.len() {
        return false;
    }
    if chars[start] == '-' {
        return start + 1 == chars.len()
            || !(chars[start + 1].is_ascii_alphanumeric() || chars[start + 1] == '_');
    }
    let mut end = start;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }
    end > start
        && (end == chars.len()
            || !(chars[end].is_ascii_alphanumeric() || chars[end] == '_'))
}

/// True when `out` ends with a bare fd number: a digit run directly after a
/// separator (or at the very start).  A digit glued to a word (`hi2>&1`) is
/// part of that word, not a descriptor prefix.
fn ends_with_bare_fd(out: &str) -> bool {
    let b: Vec<char> = out.chars().collect();
    if b.is_empty() || !b[b.len() - 1].is_ascii_digit() {
        return false;
    }
    if b.len() == 1 {
        return true;
    }
    let prev = b[b.len() - 2];
    !(prev.is_ascii_alphanumeric() || prev == '_')
}

/// Run the pipeline in the background (`cmd &`).
///
/// We fork once; the child runs the whole pipeline normally (with the
/// `background` flag cleared so it does not recurse) and then exits with that
/// pipeline's status.  The parent records the child pid in `Shell.jobs`, sets
/// `$!`, prints the bash-style `[job] pid` line and returns immediately, so the
/// REPL stays responsive.  This is the scaffolding the `jobs`/`fg`/`bg`/`wait`
/// builtins build on.
fn execute_background(p: &Pipeline, sh: &mut Shell, path: Option<&str>) -> i32 {
    let pid = match fork_child() {
        Some(p) => p,
        None => {
            // The background job could not be forked.  Do NOT register a job,
            // set `$!`, or print the `[job] pid` line; fail with status 1.
            return 1;
        }
    };
    let job_ctl = job_control_active(sh);
    if pid == 0 {
        // Phase 2: with job control active the background job gets its own
        // process group, so terminal-generated signals (Ctrl-C / Ctrl-Z) go
        // to the job and never to the shell.  `fg` later hands the terminal
        // foreground group to this pgid.  Both parent and child call setpgid
        // to avoid the fork/exec race where the parent runs first.
        if job_ctl {
            posix::setpgid(0, 0);
        }
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
    // 父进程同样 setpgid（竞态防护）；pgid 就是子进程 pid。
    if job_ctl {
        posix::setpgid(pid, pid);
    }
    // 父进程：登记真实作业条目。子进程 fork 后走 pid==0 分支并 posix::exit，
    // 不会执行到这里；且子进程持有独立的 Shell 副本，其内部若再起后台作业
    // 也只改自己的副本，不会污染父 shell 的作业表。
    sh.jobs.push(pid);
    // 持久作业号（bash 语义）：单调分配，作业删除后不回收不重排。
    let jobno = sh.next_jobno;
    sh.next_jobno += 1;
    sh.job_table.push(crate::shell::JobInfo {
        pid,
        // 命令文本优先取 `&` 携带的原始输入片段（保留引号/转义/重定向空格，
        // 经 normalize_job_text 按 bash 语义规整），仅在没有原始文本时才退回
        // AST 渲染；两者都不含结尾的 `&`，是真实输入，非伪造。
        command: p
            .raw
            .clone()
            .map(|s| normalize_job_text(&s))
            .unwrap_or_else(|| crate::builtins::render_pipeline(p)),
        done: false,
        status: None,
        jobno,
        stopped_signal: None,
        pgid: if job_ctl { pid } else { 0 },
    });
    sh.bg_pid = pid;
    // bash 只在交互式 shell（且打开了作业控制）里通报后台作业的启动；
    // 非交互的 `bash -c 'sleep 1 &'` 什么都不打印。通报信息走 stderr。
    // 通报用持久作业号，与 jobs 内建显示的 `[N]` 同源。
    if sh.interactive {
        safe_eprintln!("[{}] {}", jobno, pid);
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
fn execute_andor(andor: &AndOr, sh: &mut Shell, path: Option<&str>, last: &mut i32) -> bool {
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
        // A fatal builtin error (e.g. `continue abc`) is not a command failure
        // that `&&` / `||` may recover from: bash aborts the whole list.
        if sh.fatal_error.is_some() {
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

/// 自动收割已结束的后台作业（review #21）。
///
/// 以前除 `wait` 内建之外没有任何 waitpid 收割点，交互 shell 里大量
/// `sleep &` 后僵尸进程无限累积、`jobs` 永远显示 Running。这里用
/// `waitpid(-1, WNOHANG)` 轮询收割：收割到的 pid 在作业表中标记
/// `done = true`（`jobs` 内建据此显示 bash 的 `Done`），其退出状态由
/// `posix::wait_any_nohang` 记入 REAPED_STATUS 表，`wait` 内建随后仍能
/// 取到真实状态（bash 保留已结束作业状态直到被通知的语义）。
///
/// 只碰**自己的**子进程（waitpid(-1) 的语义边界），不会干扰任何调用方
/// 正在按 pid 等待的子进程：所有按 pid 的同步等待（命令替换、子 shell、
/// 管道阶段）从 fork 到 wait 之间都不会重入 execute()。
fn reap_finished_jobs(sh: &mut Shell) {
    while let Some((pid, state)) = posix::wait_any_nohang_jobs() {
        if let Some(j) = sh.job_table.iter_mut().find(|j| j.pid == pid) {
            match state {
                posix::JobWaitState::Exited(status) => {
                    j.done = true;
                    j.stopped_signal = None;
                    // 退出状态（shell 风格：exit code 或 128+sig）随收割一并记录，
                    // 供显示端区分 bash 的 Done / Exit N / Terminated 等；与
                    // REAPED_STATUS 表同源，`wait` 内建取走缓存不会丢状态。
                    j.status = Some(status);
                }
                posix::JobWaitState::Stopped(sig) => {
                    // 子进程仍在（waitpid 只在退出时回收），不可标 done。
                    j.done = false;
                    j.stopped_signal = Some(sig);
                }
                posix::JobWaitState::Continued => {
                    j.done = false;
                    j.stopped_signal = None;
                }
            }
        }
    }
}

pub fn execute(list: &CommandList, sh: &mut Shell) -> i32 {
    // Make every function defined anywhere in this list (including nested
    // ones) callable before any command runs.
    register_functions(list, sh);

    // 自动收割点（#21）：每条命令列表执行前轮询回收已结束的后台作业。
    reap_finished_jobs(sh);

    // `PATH` 未设置（键不存在）与设置为空串要区分开：未设置 → 裸名直接
    // execve（bash 报 `No such file or directory`），设置为空 → 只搜
    // 空元素代表的当前目录（搜不到报 `command not found`）。
    let path_owned: Option<String> = sh.env.get("PATH").cloned();
    let path: Option<&str> = path_owned.as_deref();

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
        let aborted = execute_andor(andor, sh, path, &mut last);
        // 自动收割点（#21）：AND-OR 列表之间也收割，保证
        // `sleep 0.1 & sleep 0.3; jobs` 在 jobs 运行前把作业标记为 Done
        // （bash 实测输出：`[1]+  Done   sleep 0.1`，rc=0）。
        reap_finished_jobs(sh);
        // A fatal expansion error aborts every enclosing command list,
        // including `if` / loop / function bodies that called us.
        if crate::expand::arith_failed() || crate::expand::expansion_abort_code().is_some() {
            break;
        }
        // `exit` stops the shell; `return` / `break` / `continue` stop the
        // current command list and leave their flags set so the enclosing
        // function or loop can consume them.
        // A `break N` / `continue N` whose level exceeds the actual nesting
        // depth is simply exhausted by the outermost loop; bash then keeps
        // executing the rest of the command list.  Only stop a list while a
        // loop is still active to consume the flag.
        if sh.exit_requested.is_some()
            || sh.return_requested.is_some()
            || (sh.loop_depth > 0
                && (sh.break_requested.is_some() || sh.continue_requested.is_some()))
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
