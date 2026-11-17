// Safe-ish Rust wrappers around the msys POSIX shim (see posix_shim.c).
// Everything here ultimately calls into msys-2.0.dll via the compiled
// `washposix` static lib produced by build.rs.

// This is a low-level FFI module; many wrappers (kill, waitpid_nohang, ...)
// are added ahead of the phases that consume them.
#![allow(dead_code)]

use std::ffi::{c_char, c_int, CStr, CString};
use std::os::raw::c_int as RawCInt;

extern "C" {
    fn wash_fork() -> c_int;
    fn wash_execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> c_int;
    fn wash_waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn wash_pipe(fds: *mut c_int) -> c_int;
    fn wash_heredoc_fd(buf: *const u8, len: usize) -> c_int;
    fn wash_dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn wash_close(fd: c_int) -> c_int;
    fn wash_exit(code: c_int) -> !;
    fn wash_chdir(path: *const c_char) -> c_int;
    fn wash_kill(pid: c_int, sig: c_int) -> c_int;
    fn wash_open_write(path: *const c_char, append: c_int, noclobber: c_int) -> c_int;
    fn wash_dup(fd: c_int) -> c_int;
    fn wash_umask_get() -> c_int;
    fn wash_umask_set(mode: c_int) -> c_int;
    fn wash_times(u: *mut f64, s: *mut f64, cu: *mut f64, cs: *mut f64) -> c_int;
    fn wash_rlimit(resource: c_int, cur: u64, max: u64, set: c_int, got: *mut u64) -> c_int;
    fn wash_rlimit_max(resource: c_int, got: *mut u64) -> c_int;
    fn wash_rlimit_resource(which: c_char) -> c_int;
    fn wash_getcwd() -> *const c_char;
    fn wash_chdir_getcwd(path: *const c_char, physical: c_int) -> *const c_char;
    fn wash_realpath(path: *const c_char) -> *const c_char;
    fn wash_signal_number(name: *const c_char) -> c_int;
    fn wash_nsig() -> c_int;
    fn wash_open_read(path: *const c_char) -> c_int;
    fn wash_access(path: *const c_char, mode: c_int) -> c_int;
    fn wash_read(fd: c_int, buf: *mut u8, count: usize) -> isize;
    fn wash_wif_exited(status: c_int) -> c_int;
    fn wash_wexitstatus(status: c_int) -> c_int;
    fn wash_wif_signaled(status: c_int) -> c_int;
    fn wash_wtermsig(status: c_int) -> c_int;
    fn wash_errno() -> c_int;
    fn wash_strerror(code: c_int) -> *const c_char;
    fn wash_read_file(path: *const c_char, buf: *mut c_char, n: usize) -> isize;
    fn wash_stat(path: *const c_char, follow: c_int, out: *mut StatBuf) -> c_int;
    fn wash_geteuid() -> u32;
    fn wash_getegid() -> u32;
    fn wash_getuid() -> u32;
    fn wash_getppid() -> i32;
    fn wash_getgroups(cap: c_int, list: *mut c_int, ng: *mut c_int) -> c_int;

    // bash-compatible numeric parsing / formatting (msys C library)
    fn wash_parse_ll(s: *const c_char, fully: *mut c_int, overflow: *mut c_int) -> i64;
    fn wash_parse_ull(s: *const c_char, fully: *mut c_int, overflow: *mut c_int) -> u64;
    fn wash_printf_ld(
        buf: *mut c_char,
        n: usize,
        fmt: *const c_char,
        arg: *const c_char,
        valid: *mut c_int,
    ) -> c_int;
    fn wash_snprintf_ll_d(buf: *mut c_char, n: usize, fmt: *const c_char, v: i64) -> c_int;
    fn wash_snprintf_ll_u(buf: *mut c_char, n: usize, fmt: *const c_char, v: u64) -> c_int;

    // raw terminal handling for the interactive REPL
    fn wash_enable_raw(fd: c_int) -> c_int;
    fn wash_disable_raw(fd: c_int) -> c_int;
    fn wash_isatty(fd: c_int) -> c_int;
    fn wash_poll_readable(fd: c_int, ms: c_int) -> c_int;

    // 交互 shell 的信号捕获（见 posix_shim.c）
    fn wash_install_signal_handlers() -> c_int;
    fn wash_take_pending_signal() -> c_int;
    fn wash_reset_signals_default() -> c_int;
    fn wash_setpgid(pid: c_int, pgid: c_int) -> c_int;
    fn wash_tcsetpgrp(fd: c_int, pgrp: c_int) -> c_int;
    fn wash_getpgrp() -> c_int;
    // 本地时间格式化（PS1 的 \d \t \T \A \@）
    fn wash_strftime(buf: *mut c_char, n: usize, fmt: *const c_char) -> c_int;

    // 动态加载（二进制插件系统，见 posix_shim.c）
    fn wash_dlopen(path: *const c_char) -> c_int;
    fn wash_dlclose(slot: c_int) -> c_int;
    fn wash_dlsym(slot: c_int, name: *const c_char, out: *mut usize) -> c_int;
    fn wash_dlerror() -> *const c_char;
}

pub const X_OK: c_int = 1;
pub const WNOHANG: c_int = 1;
/// `access(2)` modes.
pub const F_OK: c_int = 0;
pub const W_OK: c_int = 2;
pub const R_OK: c_int = 4;
/// `errno` values used by the shell diagnostics.
pub const EEXIST: i32 = 17;

/// The file types `test` distinguishes (mirrors the `WASH_FT_*` constants of
/// the C shim).
pub const FT_UNKNOWN: i32 = 0;
pub const FT_REG: i32 = 1;
pub const FT_DIR: i32 = 2;
pub const FT_LNK: i32 = 3;
pub const FT_BLK: i32 = 4;
pub const FT_CHR: i32 = 5;
pub const FT_FIFO: i32 = 6;
pub const FT_SOCK: i32 = 7;

/// The metadata of a file, as filled in by the C shim's `wash_stat`.
///
/// The field order matches the C struct exactly (all members are fixed
/// width, so the two layouts agree without any padding).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct StatBuf {
    pub size: i64,
    pub mtime: i64,
    pub atime: i64,
    pub dev: i64,
    pub ino: i64,
    pub uid: i64,
    pub gid: i64,
    pub mtime_nsec: i32,
    pub atime_nsec: i32,
    pub file_type: i32,
    pub mode: i32,
}

/// The setuid / setgid / sticky bits of a file mode.
pub const S_ISUID: i32 = 0o4000;
pub const S_ISGID: i32 = 0o2000;
pub const S_ISVTX: i32 = 0o1000;

/// 把 shell 层的路径字符串转成 C 字符串；byteval 编码的裸字节在此还原。
pub(crate) fn path_cstr(path: &str) -> CString {
    CString::new(crate::byteval::decode_raw(path)).unwrap_or_default()
}

/// The metadata of `path`.
///
/// With `follow` set a symbolic link is resolved (like `stat(2)`), otherwise
/// the link itself is described (like `lstat(2)`).  Returns `None` when the
/// file does not exist or cannot be examined.
pub fn stat_path(path: &str, follow: bool) -> Option<StatBuf> {
    let c = path_cstr(path);
    let mut out = StatBuf {
        size: 0,
        mtime: 0,
        atime: 0,
        dev: 0,
        ino: 0,
        uid: 0,
        gid: 0,
        mtime_nsec: 0,
        atime_nsec: 0,
        file_type: FT_UNKNOWN,
        mode: 0,
    };
    let r = unsafe { wash_stat(c.as_ptr(), if follow { 1 } else { 0 }, &mut out) };
    if r < 0 {
        None
    } else {
        Some(out)
    }
}

/// The effective user id of the calling process (`test -O`).
pub fn effective_uid() -> u32 {
    unsafe { wash_geteuid() }
}

/// The effective group id of the calling process (`test -G`).
pub fn effective_gid() -> u32 {
    unsafe { wash_getegid() }
}

/// The (real) user id of the calling process (`$UID`).
pub fn real_uid() -> u32 {
    unsafe { wash_getuid() }
}

/// The parent process id of the calling process (`$PPID`).
pub fn parent_pid() -> i32 {
    unsafe { wash_getppid() }
}

/// The supplementary group ids of the calling process (`$GROUPS`), in the
/// platform's natural order (the caller's real gid first).
pub fn group_ids() -> Vec<i32> {
    let mut buf: Vec<c_int> = vec![0; 512];
    let mut ng: c_int = 0;
    let r = unsafe { wash_getgroups(buf.len() as c_int, buf.as_mut_ptr(), &mut ng) };
    if r != 0 || ng <= 0 {
        Vec::new()
    } else {
        buf.truncate(ng as usize);
        buf
    }
}

pub fn fork() -> i32 {
    unsafe { wash_fork() }
}

/// Create a pipe; returns [read_fd, write_fd].
pub fn pipe() -> [i32; 2] {
    let mut fds = [0i32; 2];
    unsafe {
        wash_pipe(fds.as_mut_ptr());
    }
    fds
}

/// Create an unlinked temp file containing `text`, rewound to offset 0, and
/// return its descriptor in the msys fd space (the same space `dup2`/`close`
/// use).  Returns -1 on failure.  See `wash_heredoc_fd` in posix_shim.c.
pub fn heredoc_fd(text: &str) -> i32 {
    let raw = crate::byteval::decode_raw(text);
    unsafe { wash_heredoc_fd(raw.as_ptr(), raw.len()) }
}

pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
    unsafe { wash_dup2(oldfd, newfd) }
}

pub fn close(fd: i32) -> i32 {
    unsafe { wash_close(fd) }
}

pub fn chdir(path: &str) -> i32 {
    let c = path_cstr(path);
    unsafe { wash_chdir(c.as_ptr()) }
}

pub fn open_write(path: &str, append: bool, noclobber: bool) -> i32 {
    let c = path_cstr(path);
    unsafe { wash_open_write(c.as_ptr(), if append { 1 } else { 0 }, if noclobber { 1 } else { 0 }) }
}

/// Duplicate a file descriptor (save before redirecting a builtin's output).
pub fn dup(fd: i32) -> i32 {
    unsafe { wash_dup(fd) }
}

/// Get the current file-mode creation mask (octal).
pub fn umask_get() -> i32 {
    unsafe { wash_umask_get() }
}

/// Set the file-mode creation mask, returning the previous value.
pub fn umask_set(mode: i32) -> i32 {
    unsafe { wash_umask_set(mode) }
}

/// Fill (self_user, self_sys, child_user, child_sys) CPU times in seconds.
pub fn get_times() -> (f64, f64, f64, f64) {
    let mut u = 0f64;
    let mut s = 0f64;
    let mut cu = 0f64;
    let mut cs = 0f64;
    unsafe {
        wash_times(&mut u, &mut s, &mut cu, &mut cs);
    }
    (u, s, cu, cs)
}

/// Map a bash `ulimit` resource letter to the native rlimit constant and
/// get/set the limit. `set=false` retrieves the current soft limit.
pub fn rlimit(which: u8, cur: u64, max: u64, set: bool) -> Option<u64> {
    let res = unsafe { wash_rlimit_resource(which as c_char) };
    if res < 0 {
        return None;
    }
    let mut got: u64 = 0;
    let r = unsafe {
        wash_rlimit(
            res,
            cur,
            max,
            if set { 1 } else { 0 },
            &mut got,
        )
    };
    if r != 0 {
        None
    } else {
        Some(got)
    }
}

/// Hard-limit variant of [`rlimit`] (returns `rlim_max`), used by `ulimit -H`.
pub fn rlimit_hard(which: u8) -> Option<u64> {
    let res = unsafe { wash_rlimit_resource(which as c_char) };
    if res < 0 {
        return None;
    }
    let mut got: u64 = 0;
    if unsafe { wash_rlimit_max(res, &mut got) } != 0 {
        None
    } else {
        Some(got)
    }
}

pub fn open_read(path: &str) -> i32 {
    let c = path_cstr(path);
    unsafe { wash_open_read(c.as_ptr()) }
}

/// Current working directory as an msys-style absolute path ("/d/foo/bar").
/// Returns an empty string when `getcwd` fails.
pub fn getcwd() -> String {
    unsafe {
        let p = wash_getcwd();
        if p.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Change directory and return the resulting absolute path. `physical`
/// resolves symbolic links (bash's `cd -P`); otherwise the logical path is
/// returned (bash's `cd -L`, the default).
pub fn chdir_getcwd(path: &str, physical: bool) -> String {
    let c = path_cstr(path);
    unsafe {
        let p = wash_chdir_getcwd(c.as_ptr(), if physical { 1 } else { 0 });
        if p.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Resolve symbolic links in `path`, returning the canonical absolute path.
pub fn realpath(path: &str) -> String {
    let c = path_cstr(path);
    unsafe {
        let p = wash_realpath(c.as_ptr());
        if p.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Map a signal name ("INT", "SIGINT", "TERM", ...) to its number, or -1.
pub fn signal_number(name: &str) -> i32 {
    let c = CString::new(name).unwrap_or_default();
    unsafe { wash_signal_number(c.as_ptr()) }
}

/// The number of signals the platform knows about.
pub fn nsig() -> i32 {
    unsafe { wash_nsig() }
}

pub fn access(path: &str, mode: i32) -> i32 {
    let c = path_cstr(path);
    unsafe { wash_access(c.as_ptr(), mode) }
}

/// The current value of `errno`. Read it immediately after a failing shim
/// call to build accurate diagnostics.
pub fn errno() -> i32 {
    unsafe { wash_errno() }
}

/// A message for an `errno` value.
///
/// The text comes straight from libc `strerror(3)` -- the same call bash's
/// `sys_error()` uses -- so it is byte-identical to bash and cannot drift.
///
/// Locale: `strerror` honours `LC_MESSAGES` by definition, but this
/// platform's libc (msys-2.0 / Cygwin) ships no gettext catalog for the errno
/// strings, so the text stays English even under `zh_CN.UTF-8` -- exactly as
/// bash's own strerror-derived text does. The localized *wrapper* bash prints
/// ("第 1 行：", "无法修改限制") comes from bash's own catalogs, not from
/// strerror, and wash does not replicate it.
///
/// Do NOT reintroduce a hand-written match table. The errno *numbers* are
/// platform-specific (Cygwin has ENAMETOOLONG = 91 and ELOOP = 92, not Linux's
/// 36 and 40), so a Linux-numbered table both mislabels codes it does contain
/// and prints "Unknown error N" for the rest. That was the bug this replaced.
pub fn errno_message(code: i32) -> String {
    let ptr = unsafe { wash_strerror(code) };
    if !ptr.is_null() {
        if let Ok(text) = unsafe { CStr::from_ptr(ptr) }.to_str() {
            return text.to_string();
        }
    }
    // strerror never returns NULL in practice, but FFI must not assume it.
    format!("Unknown error {}", code)
}

/// Read a whole file into memory. Returns `None` when the file cannot be
/// read (the caller can then inspect `errno()`).
pub fn read_file(path: &str) -> Option<String> {
    let c = path_cstr(path);
    let size = unsafe { wash_read_file(c.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return None;
    }
    let mut buf: Vec<u8> = vec![0u8; size as usize + 1];
    let n = unsafe {
        wash_read_file(
            c.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
        )
    };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    // 源文件是「原始字节 ingress」：非法 UTF-8（如裸 0xff）在 bash 下是
    // 源码里的真实字节，不能用 from_utf8_lossy 变成 U+FFFD。统一走 byteval
    // 编码，调用方直接把结果交给 parser，不再额外 escape_literal（encode_raw
    // 已把字面哨兵一并编码，重复 escape 会把 ESC 标记双写）。
    Some(crate::byteval::encode_raw(&buf))
}

/// `read_file` 的字节级版本：不做 UTF-8 有损转换，供 execve 失败分类器
/// 判定二进制文件（NUL / ELF 魔数）使用。
pub fn read_file_bytes(path: &str) -> Option<Vec<u8>> {
    let c = path_cstr(path);
    let size = unsafe { wash_read_file(c.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        return None;
    }
    let mut buf: Vec<u8> = vec![0u8; size as usize + 1];
    let n = unsafe {
        wash_read_file(
            c.as_ptr(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
        )
    };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

pub fn execve(path: &CString, argv: &[*const c_char], envp: &[*const c_char]) -> i32 {
    unsafe { wash_execve(path.as_ptr(), argv.as_ptr(), envp.as_ptr()) }
}

/// Exit the current process immediately (no atexit/dtors). Used in child
/// processes after a failed exec.
pub fn exit(code: i32) -> ! {
    unsafe { wash_exit(code) }
}

pub fn kill(pid: i32, sig: i32) -> i32 {
    unsafe { wash_kill(pid, sig) }
}

/// Wait for a specific pid and decode the exit status into a shell-style code.
pub fn wait_pid(pid: i32) -> i32 {
    let mut status: RawCInt = 0;
    unsafe {
        wash_waitpid(pid, &mut status, 0);
    }
    decode_status(status)
}

pub fn wait_pid_nohang(pid: i32) -> Option<i32> {
    let mut status: RawCInt = 0;
    let r = unsafe { wash_waitpid(pid, &mut status, WNOHANG) };
    if r == 0 {
        None
    } else {
        Some(decode_status(status))
    }
}

/// Like [`wait_pid`] (blocking, decoded status), but returns `None` when the
/// pid is not a waitable child of this process (already reaped, or never a
/// child of this shell → `ECHILD`). This lets the `wait` builtin tell
/// "no such child" apart from a genuine exit status of 0.
pub fn wait_pid_check(pid: i32) -> Option<i32> {
    let mut status: RawCInt = 0;
    let r = unsafe { wash_waitpid(pid, &mut status, 0) };
    if r < 0 {
        None
    } else {
        Some(decode_status(status))
    }
}

fn decode_status(status: RawCInt) -> i32 {
    unsafe {
        if wash_wif_exited(status) != 0 {
            wash_wexitstatus(status)
        } else if wash_wif_signaled(status) != 0 {
            128 + wash_wtermsig(status)
        } else {
            0
        }
    }
}

pub fn to_cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_default()
}

// --- bash-compatible numeric parsing / formatting -------------------------

/// Parse `s` as a base-0 signed integer (bash `%d %i %o %x %X` semantics).
/// Returns `(value, fully_consumed, overflow)`. `fully_consumed` is false when
/// the string has trailing garbage or no digits (→ bash's conv_error). On
/// overflow the value is clamped (LLONG_MAX/MIN) and `overflow` is set.
pub fn parse_long_long(s: &str) -> (i64, bool, bool) {
    let c = CString::new(s).unwrap_or_default();
    let mut fully: c_int = 0;
    let mut overflow: c_int = 0;
    let v = unsafe { wash_parse_ll(c.as_ptr(), &mut fully, &mut overflow) };
    (v, fully != 0, overflow != 0)
}

/// Parse `s` as a base-0 unsigned integer (bash `%u` semantics). Same contract
/// as `parse_long_long` but uses `strtoull` (so `-1` wraps to 2^64-1).
pub fn parse_unsigned_long_long(s: &str) -> (u64, bool, bool) {
    let c = CString::new(s).unwrap_or_default();
    let mut fully: c_int = 0;
    let mut overflow: c_int = 0;
    let v = unsafe { wash_parse_ull(c.as_ptr(), &mut fully, &mut overflow) };
    (v, fully != 0, overflow != 0)
}

/// Format a `long double` parsed from `arg` using `fmt` (which must contain the
/// `L` length modifier before the conversion letter, e.g. `%10.5Lf`). Returns
/// the rendered bytes and whether the argument was fully/validly parsed.
pub fn printf_long_double(fmt: &str, arg: &str) -> (Vec<u8>, bool) {
    let cf = CString::new(fmt).unwrap_or_default();
    let ca = CString::new(arg).unwrap_or_default();
    let mut valid: c_int = 0;
    // Initial buffer; grow if snprintf reports it was too small.
    let mut buf: Vec<u8> = vec![0u8; 4096];
    loop {
        let n = unsafe {
            wash_printf_ld(
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                cf.as_ptr(),
                ca.as_ptr(),
                &mut valid,
            )
        };
        if n < 0 {
            break;
        }
        let n = n as usize;
        if n < buf.len() {
            buf.truncate(n);
            return (buf, valid != 0);
        }
        buf.resize(n + 1, 0);
    }
    (Vec::new(), valid != 0)
}

/// Format a signed `long long` with `fmt` (must contain `ll` length modifier,
/// e.g. `%lld`/`%lli`).
pub fn snprintf_ll_d(fmt: &str, v: i64) -> Vec<u8> {
    let cf = CString::new(fmt).unwrap_or_default();
    let mut buf: Vec<u8> = vec![0u8; 4096];
    loop {
        let n = unsafe {
            wash_snprintf_ll_d(buf.as_mut_ptr() as *mut c_char, buf.len(), cf.as_ptr(), v)
        };
        if n < 0 {
            break;
        }
        let n = n as usize;
        if n < buf.len() {
            buf.truncate(n);
            return buf;
        }
        buf.resize(n + 1, 0);
    }
    Vec::new()
}

/// Format an unsigned `long long` with `fmt` (must contain `ll`, e.g. `%llu`).
pub fn snprintf_ll_u(fmt: &str, v: u64) -> Vec<u8> {
    let cf = CString::new(fmt).unwrap_or_default();
    let mut buf: Vec<u8> = vec![0u8; 4096];
    loop {
        let n = unsafe {
            wash_snprintf_ll_u(buf.as_mut_ptr() as *mut c_char, buf.len(), cf.as_ptr(), v)
        };
        if n < 0 {
            break;
        }
        let n = n as usize;
        if n < buf.len() {
            buf.truncate(n);
            return buf;
        }
        buf.resize(n + 1, 0);
    }
    Vec::new()
}

/// Read all bytes from `fd` until EOF (used for command substitution).
/// Read one byte from `fd`.  Returns `None` at the end of the file.
pub fn read_byte_from(fd: i32) -> Option<u8> {
    let mut buf = [0u8; 1];
    let n = unsafe { wash_read(fd, buf.as_mut_ptr(), 1) };
    if n == 1 {
        Some(buf[0])
    } else {
        None
    }
}

/// Read all of `fd` into a string.
///
/// 返回的是 byteval 规范形式：子进程的原始输出字节按字节编码，绝不走
/// `from_utf8_lossy`（会永久丢失非法 UTF-8 字节）。命令替换的值随后经
/// `decode_raw` 原样吐回，因此外部命令输出里恰好含 `U+E100` + PUA 的
/// 字节序列也不会被误当成编码字节。
pub fn read_all(fd: i32) -> String {
    let mut buf = [0u8; 4096];
    let mut out = Vec::new();
    loop {
        let n = unsafe { wash_read(fd, buf.as_mut_ptr(), buf.len()) };
        if n <= 0 {
            break;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
    crate::byteval::encode_raw(&out)
}

// --- raw terminal handling for the interactive REPL -------------------------

/// Put stdin (fd 0) into raw mode. Returns true on success.
pub fn enable_raw_mode() -> bool {
    unsafe { wash_enable_raw(0) == 0 }
}

/// Restore stdin to its original cooked mode. Returns true on success.
pub fn disable_raw_mode() -> bool {
    unsafe { wash_disable_raw(0) == 0 }
}

/// Returns true if `fd` refers to a terminal (TTY/PTY).
pub fn isatty(fd: i32) -> bool {
    unsafe { wash_isatty(fd) != 0 }
}

/// Returns true if `fd` is readable within `ms` milliseconds.
pub fn poll_readable(fd: i32, ms: i32) -> bool {
    unsafe { wash_poll_readable(fd, ms) > 0 }
}

/// Blocking read of a single byte from stdin. Returns `None` on EOF.
pub fn read_byte() -> Option<u8> {
    let mut buf = [0u8; 1];
    let n = unsafe { wash_read(0, buf.as_mut_ptr(), 1) };
    if n == 1 {
        Some(buf[0])
    } else {
        None
    }
}

/// Read a single byte from stdin if one is available within `ms` milliseconds.
pub fn read_byte_timeout(ms: i32) -> Option<u8> {
    if !poll_readable(0, ms) {
        return None;
    }
    read_byte()
}

// --- 交互 shell 信号 / 作业控制 / 本地时间 -------------------------------

/// 只有交互 REPL 启动路径（`install_signal_handlers`，即 main.rs 里
/// 「无 -c、无脚本且 stdin 为 tty」的那一次调用）才置位。`exec` 内建的
/// execve 失败分类器据此决定是让 REPL 存活（交互，bash 不退出）还是
/// 按 bash 语义终止整个非交互 shell（-c / 脚本）。
pub static INTERACTIVE_REPL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 交互 shell 启动时安装信号捕获 handler（幂等）。非交互（-c / 脚本）路径
/// 不要调用，否则会改变默认的 SIGINT 语义。
pub fn install_signal_handlers() {
    INTERACTIVE_REPL.store(true, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        wash_install_signal_handlers();
    }
}

/// 读取并清除待处理的信号号；返回 0 表示没有待处理信号。
pub fn take_pending_signal() -> i32 {
    unsafe { wash_take_pending_signal() }
}

/// 恢复默认信号处理（fork 后的子进程 / exec 内建用）。
pub fn reset_signals_default() {
    unsafe {
        wash_reset_signals_default();
    }
}

/// 把进程置入指定进程组（作业控制）。
pub fn setpgid(pid: i32, pgid: i32) -> i32 {
    unsafe { wash_setpgid(pid, pgid) }
}

/// 把前台进程组交给终端（作业控制）。
pub fn tcsetpgrp(fd: i32, pgrp: i32) -> i32 {
    unsafe { wash_tcsetpgrp(fd, pgrp) }
}

/// 当前进程组 ID：前台命令 wait 结束后用它恢复终端的前台进程组。
pub fn getpgrp() -> i32 {
    unsafe { wash_getpgrp() }
}

/// 按 strftime 格式取本地时间字符串（供 PS1 转义使用）。
pub fn strftime_local(fmt: &str) -> String {
    let c = CString::new(fmt).unwrap_or_default();
    let mut buf = [0u8; 128];
    let r = unsafe { wash_strftime(buf.as_mut_ptr() as *mut c_char, buf.len(), c.as_ptr()) };
    if r != 0 {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

// --- 动态加载（二进制插件系统）-------------------------------------------

/// 打开共享库，成功返回 shim 句柄槽位（>= 0），失败返回带诊断的 Err。
pub fn dl_open(path: &str) -> Result<i32, String> {
    let c = path_cstr(path);
    let slot = unsafe { wash_dlopen(c.as_ptr()) };
    if slot < 0 {
        Err(dl_error())
    } else {
        Ok(slot)
    }
}

/// 关闭一个句柄槽位。
pub fn dl_close(slot: i32) -> Result<(), String> {
    let r = unsafe { wash_dlclose(slot) };
    if r != 0 {
        Err(dl_error())
    } else {
        Ok(())
    }
}

/// 查符号地址，返回可 transmute 成函数指针的 `usize`（对应 C `uintptr_t`）。
pub fn dl_sym(slot: i32, name: &str) -> Result<usize, String> {
    let c = CString::new(name).unwrap_or_default();
    let mut addr: usize = 0;
    let r = unsafe { wash_dlsym(slot, c.as_ptr(), &mut addr) };
    if r != 0 {
        Err(dl_error())
    } else {
        Ok(addr)
    }
}

/// 最近一次 dl* 错误；立刻把 C 侧 static 缓冲拷成 Rust 自有 String
/// （dlerror() 的原生内存在下次 dl* 调用后即失效，不能留存指针）。
pub fn dl_error() -> String {
    unsafe {
        let p = wash_dlerror();
        if p.is_null() {
            String::new()
        } else {
            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

