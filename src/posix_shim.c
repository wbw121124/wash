// msys/cygwin POSIX shim for wash.
//
// wash is built with MSYS2's `msys/rust` toolchain (MSYSTEM=MSYS), so the
// resulting binary links msys-2.0.dll and has full POSIX semantics (fork,
// execve, signals, job control, PTY). We route all low-level process/IO
// calls through this shim so that:
//   1. Open flags (O_CREAT/O_TRUNC/O_APPEND/...) come from the real
//      <fcntl.h> instead of being hardcoded (Cygwin values differ from Linux).
//   2. Rust never has to declare raw libc signatures for the windows-gnu
//      target, where the `libc` crate points at msvcrt/ucrt, not msys.
//
// All functions are thin wrappers. Rust calls wash_* via extern "C".

#include <unistd.h>
#include <fcntl.h>
#include <sys/wait.h>
#include <sys/types.h>
#include <stdlib.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <ctype.h>
#include <errno.h>

int wash_fork(void) { return fork(); }

int wash_execve(const char *path, char *const argv[], char *const envp[]) {
    return execve(path, argv, envp);
}

int wash_waitpid(int pid, int *status, int options) {
    return waitpid(pid, status, options);
}

int wash_pipe(int fds[2]) { return pipe(fds); }

int wash_dup2(int oldfd, int newfd) { return dup2(oldfd, newfd); }

int wash_close(int fd) { return close(fd); }

void wash_exit(int code) { _exit(code); }

int wash_chdir(const char *path) { return chdir(path); }

int wash_kill(int pid, int sig) { return kill(pid, sig); }

int wash_getpid(void) { return getpid(); }

int wash_setsid(void) { return setsid(); }

int wash_setpgid(int pid, int pgid) { return setpgid(pid, pgid); }

// Open for writing. `append` selects O_APPEND instead of O_TRUNC.
int wash_open_write(const char *path, int append) {
    int flags = O_WRONLY | O_CREAT;
    flags |= append ? O_APPEND : O_TRUNC;
    return open(path, flags, 0644);
}

// Open for reading.
int wash_open_read(const char *path) { return open(path, O_RDONLY); }

int wash_access(const char *path, int mode) { return access(path, mode); }

ssize_t wash_read(int fd, void *buf, size_t count) { return read(fd, buf, count); }

int wash_wif_exited(int status) { return WIFEXITED(status); }
int wash_wexitstatus(int status) { return WEXITSTATUS(status); }
int wash_wif_signaled(int status) { return WIFSIGNALED(status); }
int wash_wtermsig(int status) { return WTERMSIG(status); }

// --- bash-compatible printf / number parsing helpers -----------------------
//
// These delegate to the SAME msys C library (msys-2.0.dll) that MSYS2 bash
// 5.3 links against, so float formatting and integer parsing match bash
// byte-for-byte. bash's printf uses `long double` for float conversions and
// base-0 `strtoll`/`strtoull` for integer conversions; we replicate that.

// Parse a base-0 integer (signed). Returns the (clamped) value, sets
// *fully (1 iff the whole string was consumed and at least one digit was
// parsed) and *overflow (1 iff ERANGE). Used to decide bash's conv_error.
long long wash_parse_ll(const char *s, int *fully, int *overflow) {
    const char *p = s;
    while (*p && isspace((unsigned char)*p)) p++;
    if (*p == '+' || *p == '-') p++;
    int parsed_digit = (*p >= '0' && *p <= '9');
    errno = 0;
    char *end;
    long long r = strtoll(s, &end, 0);
    *fully = parsed_digit && (*end == '\0');
    *overflow = (errno == ERANGE);
    return r;
}

unsigned long long wash_parse_ull(const char *s, int *fully, int *overflow) {
    const char *p = s;
    while (*p && isspace((unsigned char)*p)) p++;
    if (*p == '+' || *p == '-') p++;
    int parsed_digit = (*p >= '0' && *p <= '9');
    errno = 0;
    char *end;
    unsigned long long r = strtoull(s, &end, 0);
    *fully = parsed_digit && (*end == '\0');
    *overflow = (errno == ERANGE);
    return r;
}

// Parse a `long double` (base 0) and format it with `fmt` (which must contain
// an `L` length modifier before the conversion letter). Sets *valid (1 iff the
// whole string was consumed and at least one digit was parsed). This keeps the
// `long double` value inside C so we never cross the long-double ABI to Rust.
int wash_printf_ld(char *buf, size_t n, const char *fmt, const char *arg, int *valid) {
    const char *p = arg;
    while (*p && isspace((unsigned char)*p)) p++;
    if (*p == '+' || *p == '-') p++;
    int parsed_digit = (*p >= '0' && *p <= '9');
    errno = 0;
    char *end;
    long double v = strtold(arg, &end);
    *valid = parsed_digit && (*end == '\0');
    return snprintf(buf, n, fmt, v);
}

int wash_snprintf_ll_d(char *buf, size_t n, const char *fmt, long long v) {
    return snprintf(buf, n, fmt, v);
}

int wash_snprintf_ll_u(char *buf, size_t n, const char *fmt, unsigned long long v) {
    return snprintf(buf, n, fmt, v);
}
