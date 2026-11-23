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
#include <time.h>
#include <dlfcn.h>
#include <stdint.h>

// --- 交互 shell 的信号捕获 -------------------------------------------------
//
// 交互 shell 必须捕获 SIGINT 等「可 trap」信号：前台命令运行期间终端处于
// cooked 模式（ISIG 打开），Ctrl-C 产生的 SIGINT 默认动作会终止整个 shell
// 并丢失历史。handler 只设置一个 volatile sig_atomic_t 标志，绝不做任何非
// async-signal-safe 的调用；Rust 侧在 REPL 循环里读取该标志，派发对应 trap
// 并回到提示符。非交互（-c / 脚本）路径不安装这些 handler，保持默认语义。
//
// 待处理信号使用位图（bit i 对应信号号 signo），以免 Ctrl-C 后紧跟 SIGTERM
// 时丢失先前的信号。位图位 i 置 1 表示信号号 i 已被捕获且尚未被取走。
//
static uint64_t g_pending_signal_bits = 0;
static int g_handlers_installed = 0;

// bash 允许 trap 的常见终止类信号；SIGKILL / SIGSTOP 不可捕获，不列入。
static const int g_caught_signals[] = {
    SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2
};
#define WASH_NCAUGHT ((int)(sizeof(g_caught_signals) / sizeof(g_caught_signals[0])))

static void wash_on_signal(int signo) {
    // 只记录信号号，后续处理全部交给 Rust 侧（异步信号安全）。
    // 使用位图记录，避免后续信号覆盖先前的信号。
    if (signo > 0 && signo < 64) {
        g_pending_signal_bits |= (1ULL << signo);
    }
}

static void wash_do_install(void) {
    struct sigaction sa;
    int i;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = wash_on_signal;
    sigemptyset(&sa.sa_mask);
    // SA_RESTART：避免阻塞的 read() 被信号打断后 REPL 把 EINTR 误判成 EOF
    // 而退出；前台子进程被 SIGINT 终止后 waitpid 仍会正常返回。
    sa.sa_flags = SA_RESTART;
    for (i = 0; i < WASH_NCAUGHT; i++) {
        sigaction(g_caught_signals[i], &sa, NULL);
    }
}

static void wash_do_reset(void) {
    struct sigaction sa;
    int i;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = SIG_DFL;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    for (i = 0; i < WASH_NCAUGHT; i++) {
        sigaction(g_caught_signals[i], &sa, NULL);
    }
}

// 安装（或重新安装）交互 shell 的信号捕获 handler。每次调用都重新执行
// sigaction，因此前台命令 wait 结束后可再次调用以恢复 handler（executor 侧
// 用法）；不清空待处理信号标志，避免丢掉刚收到的 SIGINT。
int wash_install_signal_handlers(void) {
    wash_do_install();
    g_handlers_installed = 1;
    return 0;
}

// 读取并清除待处理信号号（返回 0 表示没有）。
// 返回值是信号号，调用者须负责在信号 trap 中使用。
int wash_take_pending_signal(void) {
    uint64_t bits = g_pending_signal_bits;
    if (bits == 0) {
        return 0;
    }
    // 取出最低位对应的信号号
    int s = __builtin_ctzll(bits);  // 计算最低有效位的索引
    g_pending_signal_bits &= ~(1ULL << s);  // 清除该位
    return s;
}

// 供 fork 后的子进程 / exec 内建显式恢复默认信号处理。
int wash_reset_signals_default(void) {
    wash_do_reset();
    g_pending_signal_bits = 0;
    return 0;
}

int wash_fork(void) { return fork(); }

int wash_execve(const char *path, char *const argv[], char *const envp[]) {
    // 被 exec 的外部程序必须恢复默认信号处理：交互 shell 捕获的 SIGINT 等
    // 不能继承过去，否则 Ctrl-C 到达前台子进程时只会置标志而无法中断命令。
    // execve 失败返回时（exec 内建在当前进程直接调用）把 shell 的 handler
    // 装回去，避免之后的 Ctrl-C 杀死整个 shell。
    int installed = g_handlers_installed;
    if (installed) wash_do_reset();
    int r = execve(path, argv, envp);
    if (installed) wash_do_install();
    return r;
}

int wash_waitpid(int pid, int *status, int options) {
    return waitpid(pid, status, options);
}

int wash_pipe(int fds[2]) { return pipe(fds); }

int wash_dup2(int oldfd, int newfd) { return dup2(oldfd, newfd); }

// Materialize here-document / here-string text as a readable file descriptor
// that lives in the SAME msys fd table as wash_dup2/wash_close.
//
// std::fs::File's raw fd is NOT usable here: the binary links msys-2.0.dll
// for all POSIX calls, while Rust std's file descriptors come from a
// different C runtime's fd table.  Everything below (open/write/lseek/
// close/unlink) is therefore done by the C shim, via plain POSIX syscalls.
//
// The file is unlinked while still open: POSIX keeps its data until the last
// descriptor closes, so no cleanup pass is needed.  A real file (not a pipe)
// also means an arbitrarily large heredoc cannot deadlock the writer.
// The offset is rewound to 0 before returning: write() leaves it at EOF and
// dup2() shares the same open file description, so a reader would otherwise
// see an empty stream immediately.
int wash_heredoc_fd(const unsigned char *buf, size_t len) {
    const char *tmpdir = getenv("TMPDIR");
    if (!tmpdir || tmpdir[0] != '/') tmpdir = "/tmp";
    char path[PATH_MAX];
    if (snprintf(path, sizeof(path), "%s/wash-here-XXXXXX", tmpdir) >= (int)sizeof(path)) {
        return -1;
    }
    int fd = mkstemp(path);
    if (fd < 0) return -1;
    unlink(path);

    size_t off = 0;
    while (off < len) {
        ssize_t w = write(fd, buf + off, len - off);
        if (w < 0) {
            if (errno == EINTR) continue;
            close(fd);
            return -1;
        }
        if (w == 0) {
            close(fd);
            return -1;
        }
        off += (size_t)w;
    }
    if (lseek(fd, 0, SEEK_SET) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

int wash_close(int fd) { return close(fd); }

void wash_exit(int code) { _exit(code); }

int wash_chdir(const char *path) { return chdir(path); }

int wash_kill(int pid, int sig) { return kill(pid, sig); }

int wash_getpid(void) { return getpid(); }

int wash_setsid(void) { return setsid(); }

int wash_setpgid(int pid, int pgid) { return setpgid(pid, pgid); }

// 把前台进程组交给终端（作业控制）。pid_t 由 <sys/types.h> 提供。
//
// 前台 job 的 wait 结束后，shell 已经处于「后台」进程组，此时再调用
// tcsetpgrp 恢复自己会被 tty 发 SIGTTOU、默认动作把 shell 停住。这里临时
// 忽略 SIGTTOU 再恢复原处置，保证恢复动作不会挂起 shell；调用方拿到的
// 仍是 tcsetpgrp 本身的返回值。
int wash_tcsetpgrp(int fd, int pgrp) {
    struct sigaction ign, old;
    int saved;
    memset(&ign, 0, sizeof(ign));
    ign.sa_handler = SIG_IGN;
    sigemptyset(&ign.sa_mask);
    ign.sa_flags = 0;
    saved = (sigaction(SIGTTOU, &ign, &old) == 0);
    int r = tcsetpgrp(fd, (pid_t)pgrp);
    if (saved) sigaction(SIGTTOU, &old, NULL);
    return r;
}

// 当前进程组 ID：前台命令 wait 结束后用它恢复终端的前台进程组。
int wash_getpgrp(void) { return getpgrp(); }

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

// Synthetic resource ids for limits bash obtains through non-getrlimit means.
#define WASH_RLIM_PIPE    10001   // pipe size      -> pathconf(_PC_PIPE_BUF)
#define WASH_RLIM_NPROC   10002   // max user procs -> sysconf(_SC_CHILD_MAX)

// setrlimit / getrlimit wrappers. `cur`/`max` are rlim_t values; when
// `set` is 0 only the current soft limit is retrieved into `*got`.
int wash_rlimit(int resource, rlim_t cur, rlim_t max, int set, rlim_t *got) {
    if (resource == WASH_RLIM_PIPE) {
        // pipe size is read-only and comes from pathconf, not getrlimit.
        if (set) { errno = EINVAL; return -1; }
        long r = pathconf(".", _PC_PIPE_BUF);
        if (r < 0) return -1;
        *got = (rlim_t)r;
        return 0;
    }
    if (resource == WASH_RLIM_NPROC) {
        // max user processes is read-only on Cygwin (sysconf).
        if (set) { errno = EINVAL; return -1; }
#ifdef _SC_CHILD_MAX
        long r = sysconf(_SC_CHILD_MAX);
        if (r < 0) return -1;
        *got = (rlim_t)r;
        return 0;
#else
        errno = EINVAL; return -1;
#endif
    }
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

// Like wash_rlimit(get) but returns the hard limit (rlim_max). Used by
// `ulimit -H` / `ulimit -aH`.
int wash_rlimit_max(int resource, rlim_t *got) {
    if (resource == WASH_RLIM_PIPE || resource == WASH_RLIM_NPROC) {
        // No distinct hard limit available; report the soft value.
        return wash_rlimit(resource, 0, 0, 0, got);
    }
    struct rlimit rl;
    if (getrlimit(resource, &rl) != 0) return -1;
    *got = rl.rlim_max;
    return 0;
}

// Map a bash `ulimit` resource letter to the native RLIMIT_* constant (or a
// WASH_RLIM_* synthetic id, or -1 if unsupported). MSYS2/Cygwin's
// <sys/resource.h> defines CORE, DATA, FSIZE, NOFILE, STACK, CPU and AS;
// PIPE and NPROC are obtained via pathconf/sysconf respectively.
int wash_rlimit_resource(char which) {
    switch (which) {
        case 'f': return RLIMIT_FSIZE;
        case 'c': return RLIMIT_CORE;
        case 'd': return RLIMIT_DATA;
        case 's': return RLIMIT_STACK;
        case 't': return RLIMIT_CPU;
        case 'n': return RLIMIT_NOFILE;
        case 'v': return RLIMIT_AS;
        case 'p': return WASH_RLIM_PIPE;
        case 'u': return WASH_RLIM_NPROC;
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

/* The file types `test` distinguishes. */
#define WASH_FT_UNKNOWN 0
#define WASH_FT_REG     1
#define WASH_FT_DIR     2
#define WASH_FT_LNK     3
#define WASH_FT_BLK     4
#define WASH_FT_CHR     5
#define WASH_FT_FIFO    6
#define WASH_FT_SOCK    7

/* The information `test` needs about a file.  The layout matches the Rust
   `StatBuf` struct (all members are fixed-width, so there is no padding). */
struct wash_stat {
  int64_t size;
  int64_t mtime;
  int64_t atime;
  int64_t dev;
  int64_t ino;
  int64_t uid;
  int64_t gid;
  int32_t mtime_nsec;
  int32_t atime_nsec;
  int32_t file_type;
  int32_t mode;
};

/* Fill `out` with the metadata of `path`.  With `follow` set the symbolic
   link is resolved (stat), otherwise the link itself is described (lstat).
   Returns 0 on success and -1 with errno set on failure. */
int wash_stat(const char *path, int follow, struct wash_stat *out) {
  struct stat st;
  int r;

  if (follow)
    r = stat(path, &st);
  else
    r = lstat(path, &st);

  if (r < 0)
    return -1;

  out->size = (int64_t)st.st_size;
  out->mtime = (int64_t)st.st_mtime;
  out->atime = (int64_t)st.st_atime;
  out->dev = (int64_t)st.st_dev;
  out->ino = (int64_t)st.st_ino;
  out->uid = (int64_t)st.st_uid;
  out->gid = (int64_t)st.st_gid;
#ifdef st_mtime
  /* glibc: st_mtime is a macro, st_mtim holds the real members. */
  out->mtime_nsec = (int32_t)st.st_mtim.tv_nsec;
  out->atime_nsec = (int32_t)st.st_atim.tv_nsec;
#elif defined(__CYGWIN__)
  out->mtime_nsec = (int32_t)st.st_mtim.tv_nsec;
  out->atime_nsec = (int32_t)st.st_atim.tv_nsec;
#else
  out->mtime_nsec = 0;
  out->atime_nsec = 0;
#endif
  out->mode = (int32_t)st.st_mode;

  switch (st.st_mode & S_IFMT)
    {
    case S_IFREG:
      out->file_type = WASH_FT_REG;
      break;
    case S_IFDIR:
      out->file_type = WASH_FT_DIR;
      break;
    case S_IFLNK:
      out->file_type = WASH_FT_LNK;
      break;
    case S_IFBLK:
      out->file_type = WASH_FT_BLK;
      break;
    case S_IFCHR:
      out->file_type = WASH_FT_CHR;
      break;
    case S_IFIFO:
      out->file_type = WASH_FT_FIFO;
      break;
    case S_IFSOCK:
      out->file_type = WASH_FT_SOCK;
      break;
    default:
      out->file_type = WASH_FT_UNKNOWN;
      break;
    }

  return 0;
}

/* The effective user and group of the calling process (`test -O` / `-G`). */
unsigned int wash_geteuid(void) { return (unsigned int)geteuid(); }
unsigned int wash_getegid(void) { return (unsigned int)getegid(); }

/* The (real) user id and the parent process id of the calling process. These
   back wash's EUID/UID/PPID internal variables. */
unsigned int wash_getuid(void) { return (unsigned int)getuid(); }
int wash_getppid(void) { return (int)getppid(); }

/* Fill `list` (capacity `cap`) with the calling process's supplementary group
   ids and store the count in `*ng`. Returns 0 on success, -1 on failure. */
int wash_getgroups(int cap, int *list, int *ng) {
    gid_t g[512];
    int n = getgroups(512, g);
    if (n < 0) return -1;
    int i;
    for (i = 0; i < n && i < cap; i++) list[i] = (int)g[i];
    *ng = n;
    return 0;
}

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
// 白名单校验即将直通 snprintf 的格式串（review #14）。
//
// 调用方（builtins.rs 的 printf 内建）会把用户格式串解析后**重组**成只含
// 一个转换的规范形式，但纵深防御要求 C 侧自身把关：格式串必须形如
//   '%' flags(- + 空格 # 0) width(数字) [.prec(数字)] <长度修饰 lhjztqL> CONV
// 且 CONV 只能取 `convs` 白名单内的字母。任何指针/参数型转换（%n %s %p
// %c）与 `*` 宽度一律拒绝：glibc 的 %n 会按 varargs 里的位模式写内存，
// `*` 则会消耗不属于它的 vararg —— 两者都可能把受控输入变成内存写入面。
static int wash_fmt_safe(const char *fmt, const char *convs) {
    const char *p = fmt;
    int conversions = 0;
    while (*p) {
        if (*p != '%') { p++; continue; }
        p++;
        if (*p == '%') { p++; continue; }
        while (*p && strchr("-+ #0", *p)) p++;
        while (isdigit((unsigned char)*p)) p++;
        if (*p == '.') {
            p++;
            while (isdigit((unsigned char)*p)) p++;
        }
        while (*p && strchr("lhjztqL", *p)) p++;
        if (*p == '\0' || strchr(convs, *p) == NULL) return 0;
        p++;
        conversions++;
        if (conversions > 1) return 0;  /* 调用方只传一个转换 */
    }
    return conversions == 1;
}

int wash_printf_ld(char *buf, size_t n, const char *fmt, const char *arg, int *valid) {
    const char *p = arg;
    while (*p && isspace((unsigned char)*p)) p++;
    if (*p == '+' || *p == '-') p++;
    int parsed_digit = (*p >= '0' && *p <= '9');
    errno = 0;
    char *end;
    long double v = strtold(arg, &end);
    *valid = parsed_digit && (*end == '\0');
    if (!wash_fmt_safe(fmt, "aAeEfFgG")) {
        errno = EINVAL;
        return -1;
    }
    return snprintf(buf, n, fmt, v);
}

int wash_snprintf_ll_d(char *buf, size_t n, const char *fmt, long long v) {
    if (!wash_fmt_safe(fmt, "diouxX")) {
        errno = EINVAL;
        return -1;
    }
    return snprintf(buf, n, fmt, v);
}

int wash_snprintf_ll_u(char *buf, size_t n, const char *fmt, unsigned long long v) {
    if (!wash_fmt_safe(fmt, "diouxX")) {
        errno = EINVAL;
        return -1;
    }
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

// 本地时间格式化，供 PS1 的 \d \t \T \A \@ 转义使用。buf 由调用方提供。
// 返回 0 成功、-1 失败。
int wash_strftime(char *buf, size_t n, const char *fmt) {
    time_t now = time(NULL);
    struct tm *tp = localtime(&now);
    if (tp == NULL) return -1;
    if (strftime(buf, n, fmt, tp) == 0) return -1;
    return 0;
}

// Return >0 if fd is readable within `ms` milliseconds (for non-blocking
// detection of the tail of an escape sequence after ESC).
int wash_poll_readable(int fd, int ms) {
    struct pollfd pfd;
    pfd.fd = fd;
    pfd.events = POLLIN;
    pfd.revents = 0;
    return poll(&pfd, 1, ms);
}

// --- 动态加载（二进制插件系统）-------------------------------------------
//
// Rust 侧不直接声明 dlopen/dlsym/dlerror：dlerror() 返回的指针在「下一次
// dl* 调用」后即失效，所以必须在 C 侧把错误字符串拷进 static 缓冲，Rust
// 拿到指针后立刻 CStr→String。句柄存在 256 槽 static 表里，Rust 只持有
// 槽位索引（fork 后随 COW 继承，索引与地址仍有效）。两平台 dl* 都不需要
// 额外的 -ldl（见 docs/spike-dlopen.md）。
#define WASH_DL_MAX 256
#define WASH_DL_ERRBUF 4096
static void *g_dl_handles[WASH_DL_MAX];
static char g_dl_err[WASH_DL_ERRBUF];

static void wash_copy_dlerror(void) {
    const char *e = dlerror();
    if (e == NULL) {
        g_dl_err[0] = '\0';
    } else {
        strncpy(g_dl_err, e, sizeof(g_dl_err) - 1);
        g_dl_err[sizeof(g_dl_err) - 1] = '\0';
    }
}

// 打开共享库，成功返回槽位索引（>=0），失败返回 -1。
int wash_dlopen(const char *path) {
    void *h = dlopen(path, RTLD_NOW | RTLD_LOCAL);
    if (h == NULL) {
        wash_copy_dlerror();
        return -1;
    }
    int i;
    for (i = 0; i < WASH_DL_MAX; i++) {
        if (g_dl_handles[i] == NULL) {
            g_dl_handles[i] = h;
            g_dl_err[0] = '\0';
            return i;
        }
    }
    dlclose(h);
    snprintf(g_dl_err, sizeof(g_dl_err), "plugin handle table full (%d)", WASH_DL_MAX);
    return -1;
}

// 关闭槽位，成功 0，失败 -1（沿用 wash_* 风格）。
int wash_dlclose(int slot) {
    if (slot < 0 || slot >= WASH_DL_MAX || g_dl_handles[slot] == NULL) {
        errno = EINVAL;
        return -1;
    }
    if (dlclose(g_dl_handles[slot]) != 0) {
        wash_copy_dlerror();
        return -1;
    }
    g_dl_handles[slot] = NULL;
    g_dl_err[0] = '\0';
    return 0;
}

// 查符号地址。用 memcpy 把 dlsym 的 void* 位模式写进 uintptr_t，避开
// void*↔函数指针直接转换的实现定义行为（两平台二者同宽）。先 dlerror()
// 清错，再据 dlerror() 是否为 NULL 判成败。成功 0，失败 -1。
int wash_dlsym(int slot, const char *name, uintptr_t *out) {
    if (slot < 0 || slot >= WASH_DL_MAX || g_dl_handles[slot] == NULL) {
        errno = EINVAL;
        return -1;
    }
    dlerror(); /* clear any stale error */
    void *p = dlsym(g_dl_handles[slot], name);
    const char *e = dlerror();
    if (e != NULL) {
        strncpy(g_dl_err, e, sizeof(g_dl_err) - 1);
        g_dl_err[sizeof(g_dl_err) - 1] = '\0';
        return -1;
    }
    uintptr_t bits = 0;
    memcpy(&bits, &p, sizeof(bits));
    *out = bits;
    g_dl_err[0] = '\0';
    return 0;
}

// 最近一次 dl* 错误的 C 字符串（static 缓冲，永不为 NULL）。
const char *wash_dlerror(void) {
    return g_dl_err;
}

// strerror(3) 的只读包装。bash 的错误文案就是 strerror 的输出，直接借
// libc 才能保证逐字节相同且不会漂移。
//
// 注意 errno 数值是平台相关的：Cygwin 的 ENAMETOOLONG = 91、ELOOP = 92，
// 不是 Linux 的 36/40，所以 Rust 侧一旦写死数值表就必然错位并回退
// "Unknown error N"。另：本平台 libc 没有 errno 字符串的 gettext 目录，
// 切到 zh_CN.UTF-8 仍是英文原文（bash 的中文外壳来自 bash 自己的 catalog，
// 与 strerror 无关）。
const char *wash_strerror(int code) {
    return strerror(code);
}

// --- POSIX 正则 ([[ ... =~ ... ]]) ----------------------------------------
//
// bash 的 `[[ str =~ re ]]` 直接调用 libc 的 regcomp/regexec（POSIX
// ERE）。本平台（MSYS2/Cygwin）上的 bash 用的就是同一份实现，所以把
// 正则交给 libc 可以逐字节复刻 bash 的匹配语义 *和* 错误文案
// （例如 "brackets ([ ]) not balanced"），无需自研正则引擎。
//
// regex_t 对 Rust 是不透明结构，这里在 C 侧持有它，只向 Rust 暴露
// 指针级别的句柄与编译/匹配/取错四个动作。
#include <regex.h>

struct wash_regex {
    regex_t re;
};

void *wash_regex_new(void) {
    struct wash_regex *r = (struct wash_regex *)malloc(sizeof(struct wash_regex));
    if (r != NULL) {
        memset(r, 0, sizeof(*r));
    }
    return r;
}

void wash_regex_free(void *p) {
    struct wash_regex *r = (struct wash_regex *)p;
    if (r == NULL) {
        return;
    }
    regfree(&r->re);
    free(r);
}

// 编译；返回 regcomp 的错误码（0 表示成功）。
int wash_regex_compile(void *p, const char *pattern, int cflags) {
    struct wash_regex *r = (struct wash_regex *)p;
    return regcomp(&r->re, pattern, cflags);
}

// regerror：把错误码翻译成 libc 的文案，写入 buf 并返回写入长度。
int wash_regex_error(int code, void *p, char *buf, size_t buflen) {
    struct wash_regex *r = (struct wash_regex *)p;
    return regerror(code, &r->re, buf, buflen);
}

// 子表达式数量（re_nsub）。
size_t wash_regex_nsub(void *p) {
    struct wash_regex *r = (struct wash_regex *)p;
    return (size_t)r->re.re_nsub;
}

// 匹配；starts/ends 长度需 >= nsub+1。返回 0 表示匹配，1 表示不匹配，
// 其它值是 regexec 的错误码。
int wash_regex_exec(void *p, const char *s, long *starts, long *ends, size_t cap) {
    struct wash_regex *r = (struct wash_regex *)p;
    regmatch_t *m = NULL;
    size_t n = 0;
    size_t i;

    if (cap > 0) {
        n = (size_t)r->re.re_nsub + 1;
        if (n > cap) {
            n = cap;
        }
        m = (regmatch_t *)calloc(n, sizeof(regmatch_t));
        if (m == NULL) {
            return 2;
        }
    }
    int rc = regexec(&r->re, s, n, m, 0);
    if (rc == 0 && m != NULL) {
        for (i = 0; i < n; i++) {
            starts[i] = (long)m[i].rm_so;
            ends[i] = (long)m[i].rm_eo;
        }
    }
    free(m);
    return rc;
}
