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
