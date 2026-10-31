// Safe-ish Rust wrappers around the msys POSIX shim (see posix_shim.c).
// Everything here ultimately calls into msys-2.0.dll via the compiled
// `washposix` static lib produced by build.rs.

// This is a low-level FFI module; many wrappers (kill, waitpid_nohang, ...)
// are added ahead of the phases that consume them.
#![allow(dead_code)]

use std::ffi::{c_char, c_int, CString};
use std::os::raw::c_int as RawCInt;

extern "C" {
    fn wash_fork() -> c_int;
    fn wash_execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char) -> c_int;
    fn wash_waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn wash_pipe(fds: *mut c_int) -> c_int;
    fn wash_dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn wash_close(fd: c_int) -> c_int;
    fn wash_exit(code: c_int) -> !;
    fn wash_chdir(path: *const c_char) -> c_int;
    fn wash_kill(pid: c_int, sig: c_int) -> c_int;
    fn wash_open_write(path: *const c_char, append: c_int) -> c_int;
    fn wash_open_read(path: *const c_char) -> c_int;
    fn wash_access(path: *const c_char, mode: c_int) -> c_int;
    fn wash_read(fd: c_int, buf: *mut u8, count: usize) -> isize;
    fn wash_wif_exited(status: c_int) -> c_int;
    fn wash_wexitstatus(status: c_int) -> c_int;
    fn wash_wif_signaled(status: c_int) -> c_int;
    fn wash_wtermsig(status: c_int) -> c_int;

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
}

pub const X_OK: c_int = 1;
pub const WNOHANG: c_int = 1;

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

pub fn dup2(oldfd: i32, newfd: i32) -> i32 {
    unsafe { wash_dup2(oldfd, newfd) }
}

pub fn close(fd: i32) -> i32 {
    unsafe { wash_close(fd) }
}

pub fn chdir(path: &str) -> i32 {
    let c = CString::new(path).unwrap_or_default();
    unsafe { wash_chdir(c.as_ptr()) }
}

pub fn open_write(path: &str, append: bool) -> i32 {
    let c = CString::new(path).unwrap_or_default();
    unsafe { wash_open_write(c.as_ptr(), if append { 1 } else { 0 }) }
}

pub fn open_read(path: &str) -> i32 {
    let c = CString::new(path).unwrap_or_default();
    unsafe { wash_open_read(c.as_ptr()) }
}

pub fn access(path: &str, mode: i32) -> i32 {
    let c = CString::new(path).unwrap_or_default();
    unsafe { wash_access(c.as_ptr(), mode) }
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
    String::from_utf8_lossy(&out).into_owned()
}
