#!/usr/bin/env python3
# 进程级 PTY 断言辅助（wash REPL e2e 用，仅依赖标准库）。
#
# 在伪终端里启动一个程序，按 steps 喂入按键 / 输入，然后用 waitpid 收取子进程
# 的**真实终止状态**，明确区分三种结果：
#   - WIFEXITED / WEXITSTATUS   -> rc（正常退出，退出码可用）
#   - WIFSIGNALED / WTERMSIG    -> sig（被信号致死）
#   - 超时（超过 timeout 仍未自行退出）-> timed_out=True，SIGKILL 后记为 TIMEOUT
#
# 关键：绝不把「超时」或「被信号杀死」伪装成可用 exitcode 的 PASS。超时就是
# TIMEOUT，信号致死就是 SIGNAL(n)，二者都不能与正常的 EXIT(rc) 混为一谈。
#
# 这与仓库里原有的 repl_smoke.py / repl_e2e.py 互补：那两个文件只断言渲染文本
# （`in text`），从不 waitpid，结构性测不出「交互 exit 7 进程退出码恒为 0」。
#
# 用法见 session_exit_e2e.py。

import os
import pty
import select
import signal
import subprocess
import tempfile
import time


class ProcResult:
    """一次运行得到的进程级结果（结构化）。"""

    def __init__(self, rc, sig, timed_out, text, alive_after, raw_status=None):
        self.rc = rc                    # int | None —— WEXITSTATUS
        self.sig = sig                  # int | None —— WTERMSIG
        self.timed_out = timed_out      # bool —— 超时后 SIGKILL，结果不参与 exitcode 判定
        self.text = text                # str —— 渲染文本（含 ANSI 转义）
        self.alive_after = alive_after  # list[bool] —— 每个 step 之后进程是否仍存活
        self.raw_status = raw_status    # 原始 waitpid status，便于排查

    @property
    def exited_normally(self):
        """正常退出（WIFEXITED），且不是超时 / 信号致死。"""
        return (not self.timed_out) and self.sig is None and self.rc is not None

    def describe(self):
        if self.timed_out:
            return "TIMEOUT"
        if self.sig is not None:
            return f"SIGNAL({self.sig})"
        if self.rc is not None:
            return f"EXIT({self.rc})"
        return "UNKNOWN"

    def identity(self):
        """用于与 oracle 对齐的进程状态三元组；超时 / 信号 / 正常退出互不等价。"""
        return (self.rc, self.sig, self.timed_out)

    def __repr__(self):
        return (f"ProcResult({self.describe()}, alive_after={self.alive_after}, "
                f"text_len={len(self.text)})")


def base_env(home=None, extra=None):
    """构造沙箱化 env：LC_ALL=C / LANG=C、HOME=临时目录、PATH 含 MSYS2 /usr/bin。

    注意：MSYS2 的 POSIX 路径 `/usr/bin` 即 Windows 视角的 `/d/msys2/usr/bin`，
    这里用 POSIX 形式喂给 MSYS2 二进制（wash / bash 都是 cygwin 程序）。
    """
    env = dict(os.environ)
    env["LC_ALL"] = "C"
    env["LANG"] = "C"
    env["TERM"] = "xterm-256color"
    env["HOME"] = home if home else tempfile.mkdtemp(prefix="wash_e2e_")
    path = env.get("PATH", "")
    if "/usr/bin" not in path.split(":"):
        path = "/usr/bin:" + path
    env["PATH"] = path
    env["HISTFILE"] = os.path.join(env["HOME"], ".bash_history")
    if extra:
        env.update(extra)
    return env


def run_pty(argv, steps, env=None, timeout=5.0, start_pause=0.6, final_drain=0.2):
    """在 pty 里启动 `argv`（完整 argv，argv[0] 为程序路径），按 steps 喂输入，
    waitpid 收状态，返回 ProcResult。

    steps: [(payload, pause_seconds), ...]
      payload: bytes 或 str；写入 pty master。每个 step 之后记录一次「进程是否
      仍存活」（alive_after），用于会话存活类断言。
    timeout: 所有 steps 之后，等待进程**自行**退出的上限；超时则 SIGKILL 并记为
      TIMEOUT（不算 PASS）。
    """
    if env is None:
        env = base_env()

    pid, fd = pty.fork()
    if pid == 0:  # 子进程
        try:
            os.execve(argv[0], list(argv), env)
        except Exception:
            os._exit(127)

    buf = bytearray()
    alive_after = []
    status = [None]  # waitpid 原始 status，一旦 reaped 就复用

    def pump(seconds):
        """非阻塞地收集 seconds 秒内的输出；slave 关闭（EIO/EOF）即返回。"""
        end = time.time() + seconds
        while True:
            remaining = end - time.time()
            if remaining <= 0:
                return
            r, _, _ = select.select([fd], [], [], min(0.05, remaining))
            if not r:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                return
            if not data:
                return
            buf.extend(data)

    def reaped():
        """非阻塞 reap；已终止返回 True 并缓存 status。"""
        if status[0] is not None:
            return True
        try:
            w, st = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return True
        if w == pid:
            status[0] = st
            return True
        return False

    pump(start_pause)
    reaped()

    for payload, pause in steps:
        if payload and not reaped():
            if isinstance(payload, str):
                payload = payload.encode()
            try:
                os.write(fd, payload)
            except OSError:
                pass  # 对端已关闭：后续由 waitpid 判定，不吞结果
        pump(pause)
        alive_after.append(not reaped())

    # 等待进程**自行**退出；不做无条件 teardown，不主动发 exit/Ctrl-C/Ctrl-D。
    timed_out = False
    deadline = time.time() + timeout
    while not reaped():
        if time.time() >= deadline:
            timed_out = True
            break
        pump(0.1)
        time.sleep(0.02)

    if timed_out:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
        if status[0] is None:
            try:
                _, st = os.waitpid(pid, 0)
                status[0] = st
            except OSError:
                pass

    pump(final_drain)
    try:
        os.close(fd)
    except OSError:
        pass

    rc = sig = None
    st = status[0]
    # 超时一律记为 TIMEOUT：即使 SIGKILL 产生 WIFSIGNALED，也不当作正常退出码。
    if st is not None and not timed_out:
        if os.WIFEXITED(st):
            rc = os.WEXITSTATUS(st)
        elif os.WIFSIGNALED(st):
            sig = os.WTERMSIG(st)

    return ProcResult(
        rc=rc,
        sig=sig,
        timed_out=timed_out,
        text=bytes(buf).decode("utf-8", "replace"),
        alive_after=alive_after,
        raw_status=st,
    )


def run_piped(argv, data, env=None, timeout=5.0):
    """非 TTY（stdin 为 pipe）运行，返回 ProcResult。用于探测 piped 输入下的
    exit 传播（repl::run 的 run_piped 分支）。同样区分超时 / 信号 / 正常退出。
    """
    if env is None:
        env = base_env()
    if isinstance(data, str):
        data = data.encode()

    p = subprocess.Popen(
        list(argv),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        env=env,
    )
    timed_out = False
    try:
        out, _ = p.communicate(input=data, timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        p.kill()
        out, _ = p.communicate()

    rc = sig = None
    if not timed_out:
        if p.returncode < 0:
            sig = -p.returncode
        else:
            rc = p.returncode
    return ProcResult(
        rc=rc,
        sig=sig,
        timed_out=timed_out,
        text=out.decode("utf-8", "replace"),
        alive_after=[],
        raw_status=p.returncode,
    )


def compare(oracle, actual):
    """对齐进程状态三元组；返回 (ok, detail)。"""
    ok = actual.identity() == oracle.identity()
    detail = f"oracle={oracle.describe()} wash={actual.describe()}"
    return ok, detail
