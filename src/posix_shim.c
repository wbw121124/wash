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
#include <stdlib.h>
#include <signal.h>

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

int wash_wif_exited(int status) { return WIFEXITED(status); }
int wash_wexitstatus(int status) { return WEXITSTATUS(status); }
int wash_wif_signaled(int status) { return WIFSIGNALED(status); }
int wash_wtermsig(int status) { return WTERMSIG(status); }
