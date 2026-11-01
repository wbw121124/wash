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
#include <sys/times.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <stdlib.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <ctype.h>
#include <errno.h>
#include <termios.h>
#include <poll.h>
#include <limits.h>

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

// Open for writing. `append` selects O_APPEND instead of O_TRUNC. When
// `noclobber` is set, O_TRUNC opens of an existing file fail with EEXIST
// (bash `set -C` semantics).
int wash_open_write(const char *path, int append, int noclobber) {
    int flags = O_WRONLY | O_CREAT;
    if (append) {
        flags |= O_APPEND;
    } else if (noclobber) {
        flags |= O_EXCL;
    } else {
        flags |= O_TRUNC;
    }
    return open(path, flags, 0644);
}

// Duplicate a file descriptor (used to save/restore fds around builtins).
int wash_dup(int fd) { return dup(fd); }

// umask: get and set. `wash_umask_get` returns the current mask without
// changing it; `wash_umask_set` installs `mode` and returns the previous mask.
int wash_umask_get(void) {
    int old = umask(0);
    umask(old);
    return old;
}
int wash_umask_set(int mode) {
    return umask(mode);
}

// Fill the four `times()` fields (self/user, self/system, children/user,
// children/system) expressed in SECONDS (double). Returns 0 on success.
int wash_times(double *u, double *s, double *cu, double *cs) {
    struct tms t;
    clock_t r = times(&t);
    if (r == (clock_t)-1) return -1;
    long clk = sysconf(_SC_CLK_TCK);
    if (clk <= 0) clk = 100;
    double tick = 1.0 / (double)clk;
    *u = (double)t.tms_utime * tick;
    *s = (double)t.tms_stime * tick;
    *cu = (double)t.tms_cutime * tick;
    *cs = (double)t.tms_cstime * tick;
    return 0;
}

// setrlimit / getrlimit wrappers. `cur`/`max` are rlim_t values; when
// `set` is 0 only the current soft limit is retrieved into `*cur`.
int wash_rlimit(int resource, rlim_t cur, rlim_t max, int set, rlim_t *got) {
    if (!set) {
        struct rlimit rl;
        if (getrlimit(resource, &rl) != 0) return -1;
        *got = rl.rlim_cur;
        return 0;
    }
    struct rlimit rl;
    rl.rlim_cur = cur;
    rl.rlim_max = max;
    if (setrlimit(resource, &rl) != 0) return -1;
    *got = cur;
    return 0;
}

// Map a bash `ulimit` resource letter to the native RLIMIT_* constant (or -1
// if unsupported). MSYS2's <sys/resource.h> only defines CPU, FSIZE, DATA,
// STACK, CORE, NOFILE and AS — RLIMIT_NPROC / RLIMIT_RSS / RLIMIT_MEMLOCK do
// not exist there, so they are guarded by #ifdef.
int wash_rlimit_resource(char which) {
    switch (which) {
        case 'f': return RLIMIT_FSIZE;
        case 'c': return RLIMIT_CORE;
        case 'd': return RLIMIT_DATA;
        case 's': return RLIMIT_STACK;
        case 't': return RLIMIT_CPU;
        case 'n': return RLIMIT_NOFILE;
        case 'v': return RLIMIT_AS;
        case 'u':
#ifdef RLIMIT_NPROC
            return RLIMIT_NPROC;
#else
            return -1;
#endif
        case 'm':
#ifdef RLIMIT_RSS
            return RLIMIT_RSS;
#else
            return -1;
#endif
        case 'l':
#ifdef RLIMIT_MEMLOCK
            return RLIMIT_MEMLOCK;
#else
            return -1;
#endif
        default: return -1;
    }
}

// getcwd(3): current working directory as an msys-style path ("/d/foo/bar").
// Needed by `pwd -P` and by `cd -P`.
char *wash_getcwd(void) {
    static char buf[4096];
    if (getcwd(buf, sizeof(buf)) == NULL) {
        buf[0] = '\0';
    }
    return buf;
}

// chdir + getcwd combined, returning the new absolute path (msys style).
// `logical` selects whether symbolic links are resolved (physical) or kept
// (logical) — mirrors bash's `cd -L` / `cd -P`.
char *wash_chdir_getcwd(const char *path, int physical) {
    static char buf[4096];
    if (chdir(path) != 0) {
        buf[0] = '\0';
        return buf;
    }
    if (physical) {
        if (getcwd(buf, sizeof(buf)) == NULL) {
            buf[0] = '\0';
        }
    } else {
        /* Logical: keep the user-supplied path, just make it absolute. */
        if (path[0] == '/') {
            strncpy(buf, path, sizeof(buf) - 1);
            buf[sizeof(buf) - 1] = '\0';
        } else {
            char cwdbuf[4096];
            if (getcwd(cwdbuf, sizeof(cwdbuf)) == NULL) {
                buf[0] = '\0';
                return buf;
            }
            snprintf(buf, sizeof(buf), "%s/%s", cwdbuf, path);
        }
    }
    return buf;
}

// Realpath(3): resolve symbolic links in PATH and return the canonical path.
char *wash_realpath(const char *path) {
    static char buf[4096];
    char *r = realpath(path, buf);
    if (r == NULL) {
        buf[0] = '\0';
    }
    return buf;
}

// The current value of errno. Rust reads this right after a failing shim
// call to produce bash-style diagnostics ("No such file or directory", ...).
int wash_errno(void) { return errno; }

// Read an entire file into the caller's buffer. When `buf` is NULL only the
// file size is reported. Returns the number of bytes read, or -1 on error
// (errno is set by the underlying stdio call).
long wash_read_file(const char *path, char *buf, size_t n) {
    FILE *fp;
    size_t got;
    long size;

    fp = fopen (path, "rb");
    if (fp == NULL)
        return -1;
    if (fseek (fp, 0, SEEK_END) != 0) {
        fclose (fp);
        return -1;
    }
    size = ftell (fp);
    if (size < 0) {
        fclose (fp);
        return -1;
    }
    rewind (fp);
    if (buf == NULL) {
        fclose (fp);
        return size;
    }
    if ((size_t) size >= n) {
        fclose (fp);
        errno = ENOMEM;
        return -1;
    }
    got = fread (buf, 1, (size_t) size, fp);
    fclose (fp);
    buf[got] = '\0';
    return (long) got;
}

// kill(3) wrapper already present; also expose the signal number for a name.
int wash_signal_number(const char *name) {
    static const struct { const char *name; int num; } sigs[] = {
        { "HUP", 1 }, { "INT", 2 }, { "QUIT", 3 }, { "ILL", 4 }, { "TRAP", 5 },
        { "ABRT", 6 }, { "IOT", 6 }, { "EMT", 7 }, { "FPE", 8 }, { "KILL", 9 },
        { "BUS", 10 }, { "SEGV", 11 }, { "SYS", 12 }, { "PIPE", 13 },
        { "ALRM", 14 }, { "TERM", 15 }, { "URG", 16 }, { "STOP", 17 },
        { "TSTP", 18 }, { "CONT", 19 }, { "CHLD", 20 }, { "TTIN", 21 },
        { "TTOU", 22 }, { "IO", 23 }, { "XCPU", 24 }, { "XFSZ", 25 },
        { "VTALRM", 26 }, { "PROF", 27 }, { "WINCH", 28 }, { "PWR", 29 },
        { "USR1", 30 }, { "USR2", 31 }, { "RTMIN", 32 }, { "RTMAX", 64 },
        { NULL, 0 }
    };
    int i;
    for (i = 0; sigs[i].name; i++) {
        if (strcmp(name, sigs[i].name) == 0) {
            return sigs[i].num;
        }
    }
    return -1;
}

// Number of the highest signal bash knows about (64 on this platform).
int wash_nsig(void) { return 64; }

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

// --- raw terminal handling for the interactive REPL -------------------------
//
// The REPL implements its own line editor over an MSYS2 PTY using termios +
// ANSI escapes (no Win32 console API). We route tcgetattr/tcsetattr through
// this shim so the `struct termios` layout and the ECHO/ICANON/ISIG/VMIN/VTIME
// constants come from the real msys <termios.h> (Cygwin values), not from any
// hardcoded rust libc.

static struct termios g_orig_termios;
static int g_raw_enabled = 0;

// Put fd (stdin) into raw mode: disable canonical line editing (ICANON), local
// echo (ECHO) and signal generation (ISIG, so Ctrl-C becomes a byte we handle);
// disable output post-processing (OPOST) and software flow control (IXON); set
// 8-bit chars (CS8); read one byte at a time (VMIN=1, VTIME=0). Returns 0 on
// success, -1 on failure (e.g. fd is not a tty).
int wash_enable_raw(int fd) {
    struct termios raw;
    if (tcgetattr(fd, &g_orig_termios) != 0) {
        return -1;
    }
    raw = g_orig_termios;
    raw.c_iflag &= ~(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
    raw.c_oflag &= ~(OPOST);
    raw.c_cflag |= (CS8);
    raw.c_lflag &= ~(ECHO | ICANON | IEXTEN | ISIG);
    raw.c_cc[VMIN] = 1;
    raw.c_cc[VTIME] = 0;
    if (tcsetattr(fd, TCSAFLUSH, &raw) != 0) {
        return -1;
    }
    g_raw_enabled = 1;
    return 0;
}

// Restore the terminal to its original (cooked) mode. Safe to call even if raw
// mode was never enabled.
int wash_disable_raw(int fd) {
    if (!g_raw_enabled) {
        return 0;
    }
    int r = tcsetattr(fd, TCSAFLUSH, &g_orig_termios);
    g_raw_enabled = 0;
    return r;
}

int wash_isatty(int fd) { return isatty(fd); }

// Return >0 if fd is readable within `ms` milliseconds (for non-blocking
// detection of the tail of an escape sequence after ESC).
int wash_poll_readable(int fd, int ms) {
    struct pollfd pfd;
    pfd.fd = fd;
    pfd.events = POLLIN;
    pfd.revents = 0;
    return poll(&pfd, 1, ms);
}
