#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""wash REPL 交互式 UX 四类缺陷的「可断言 FAIL」复现（pty + ANSI 屏幕模型）。

背景（team-lead 派发，源自用户在交互式 ./target/debug/wash 的实测）：
  1. 提示符刷屏：连续十几行只剩路径提示符，行间多出空行；空命令回车就多刷一段。
  2. 错误消息前缀残留字符：`wash: a: command not found` 有时变成
     `awash: a: command not found`（行首多一个 'a'）。
  3. 提示符形态跳变：有时是完整路径，有时变成
     `wbw@MoMo /d/p/W/w/t/debug (main) [127]>`（路径逐段缩写 + 退出码混入）。
  4. 光标落点错：提交后出现 `                    $`，光标停在错误列。

本文件把上述现象变成对「屏幕状态」与「原始字节」的逐条断言：
  - 用 ansi_screen.py 的 Screen 把 pty 字节流还原成用户看到的屏幕（行数、
    非空行、光标列、可见文本），而不是靠肉眼看转储；
  - 用 raw bytes 做精确字节断言（错误行不含残留 'a'、无行间空行、无
    `[127]` / `(main)` / 缩写路径等异常片段）。

期望全部按「正确 UX」书写，刻意不放宽：当前二进制 src/repl.rs 的
render_prompt 第 208 行 `out.push(b as char)` 会把 PS1 模板里的 UTF-8 多字节
逐字节转成 Latin-1 字符，属确定性缺陷；初跑预期出现真实 FAIL。FAIL 即证据，
禁止 skip / xfail / 标「已知差异」/ 放宽阈值。修 src 由 team-lead 后续派发，
本文件只改 tests/e2e/。

运行（必须经 MSYS2，python3 用 MSYS2 的 /usr/bin/python3 才有 pty.fork）：
  MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc 'cd <WASH_REPO> && python3 tests/e2e/repl_ux_e2e.py'

U8 为 P1 第二组新增：`trap 'exit' INT` + Ctrl-C 必须真退出，覆盖 REPL
主循环在派发 trap 后 continue、跳过 exit_requested 检查的缺陷。

退出码：全部通过 0；有 FAIL 则 1（FAIL 是真实断言结果，不是脚本错误）。
"""

import os
import pty
import re
import select
import signal
import sys
import tempfile
import time
import unicodedata

# 导入同目录辅助模块时不生成 tests/e2e/__pycache__/（仓库内不留垃圾）。
sys.dont_write_bytecode = True

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)

from ansi_screen import Screen, strip_ansi  # noqa: E402
from proc_assert import base_env  # noqa: E402

WASH = os.path.normpath(
    os.path.join(_HERE, "..", "..", "target", "debug", "wash.exe")
)

# ---------------------------------------------------------------------------
# PS1 模板（通过 env 传给 wash；render_prompt 会展开 \w / \$ / \u / \h 等）
# ---------------------------------------------------------------------------
PROMPT_PLAIN = "P> "                       # 固定短串：隔离变量，专测刷屏/残留
PROMPT_CJK = "提示> "                       # 非 ASCII：触发 UTF-8 逐字节拆坏
PROMPT_PATH = "UXPROMPT:\\w\\$ "           # 完整路径 + \$：贴近用户实测形态
# ANSI 颜色 + 长路径 + \$：专测可见宽度与光标列（ANSI 字节不得计入宽度）
PROMPT_COLOR_LONG = (
    "\x1b[1;32mGREEN\x1b[0m:"
    "/very/long/working/directory/segment/target/debug\\$ "
)
EXPECT_COLOR_LONG = "GREEN:/very/long/working/directory/segment/target/debug$ "
PROMPT_STABLE = "U6:UX\\w\\$ "             # 固定前缀 + 路径：形态稳定性计数
# bash 常见 git-aware PS1（含命令替换 $(...) 与 $?）：用于复现「形态跳变」——
# 用户实测的 `(main) [127]>` 正是这类高级转义被当作字面文本泄漏的残缺形态。
PROMPT_BASH_LEAK = '\\u@\\h \\w$(__git_ps1 " (%s)") [$?]\\$ '


# ---------------------------------------------------------------------------
# 通用工具
# ---------------------------------------------------------------------------

def py_visible_width(s: str) -> int:
    """Python 侧独立计算的「可见宽度」：去掉 ANSI/OSC，宽字符按 2 格。

    与 src/repl.rs 的 visible_width 互相独立，用作断言的期望基准，避免拿
    subject 自己的计算当 oracle。
    """
    s = strip_ansi(s)
    w = 0
    for ch in s:
        if ord(ch) < 0x20 or ch == "\x7f":
            continue
        if unicodedata.combining(ch):
            continue
        w += 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1
    return w


def plain_lines(text: str) -> list:
    """剥离 ANSI 后按行切分（回车也视为换行），用于定位错误行。"""
    return strip_ansi(text).replace("\r\n", "\n").replace("\r", "\n").split("\n")


class Capture:
    """一次 pty 会话在 teardown 之前的字节快照 + 还原出的 ANSI 屏幕。"""

    def __init__(self, snapshot: bytes, scr: Screen, ps1: str):
        self.snapshot = snapshot
        self.text = snapshot.decode("utf-8", "replace")
        self.scr = scr
        self.ps1 = ps1

    def nonblank(self) -> list:
        return self.scr.nonblank()

    def dump(self, limit: int = 1400) -> None:
        """FAIL 现场证据：原始字节、剥 ANSI 文本、屏幕非空行、光标位置。"""
        print("    --- raw bytes (first %d) ---" % limit)
        print("      " + repr(self.snapshot[:limit]))
        print("    --- strip_ansi text (first %d chars) ---" % limit)
        print("      " + repr(strip_ansi(self.text)[:limit]))
        print("    --- screen nonblank lines (rows=%d cols=%d) ---"
              % (self.scr.rows, self.scr.cols))
        for i, ln in enumerate(self.scr.lines()):
            if ln.strip():
                print("      r%-3d len=%-3d | %r" % (i, len(ln), ln.rstrip()))
        print("    --- screen cursor (r, c) = %r ---" % (self.scr.cursor(),))


def capture(ps1, steps, rows=80, cols=240, extra_env=None,
            start_pause=0.7, per_step=0.35) -> Capture:
    """在 pty 里启动 wash（无参数 → 交互 REPL），设置 PS1，按 steps 喂按键，
    在 teardown 之前取字节快照，喂给 Screen 还原屏幕后返回 Capture。

    steps: [bytes | str, ...]，每个 step 之后 pump(per_step) 收集输出。
    teardown 只用于回收子进程，其输出不进入 snapshot（避免污染屏幕分析）。
    """
    home = tempfile.mkdtemp(prefix="wash_ux_")   # HOME 沙箱化，探针不落仓库
    env = base_env(home)
    env["PS1"] = ps1
    if extra_env:
        env.update(extra_env)

    pid, fd = pty.fork()
    if pid == 0:  # 子进程：在真实 TTY 里 exec wash
        try:
            os.execve(WASH, [WASH], env)
        except Exception:
            os._exit(127)
        os._exit(127)

    buf = bytearray()

    def pump(seconds: float) -> None:
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

    pump(start_pause)  # 等 REPL 进入 raw mode 并画出首个提示符
    for payload in steps:
        data = payload.encode("utf-8") if isinstance(payload, str) else payload
        try:
            os.write(fd, data)
        except OSError:
            pass
        pump(per_step)

    snapshot = bytes(buf)  # teardown 前的纯净屏幕字节

    for seq in (b"exit\r", b"\x04", b"\x03"):
        try:
            os.write(fd, seq)
        except OSError:
            break
        pump(0.15)
    pump(0.2)
    try:
        os.waitpid(pid, 0)
    except (ChildProcessError, OSError):
        pass
    try:
        os.close(fd)
    except OSError:
        pass

    scr = Screen(rows=rows, cols=cols)
    scr.feed(snapshot.decode("utf-8", "replace"))
    return Capture(snapshot, scr, ps1)


def case_result(name, expect, cap, checks):
    return {"name": name, "expect": expect, "cap": cap, "checks": checks}


# ---------------------------------------------------------------------------
# U1 / U2 —— 现象 1：空命令连续回车后的屏幕行数 / 提示符次数 / 行间空行
# ---------------------------------------------------------------------------

def case_u1():
    """6 次空回车：一次提交至多重画一个提示符，不得超量刷屏、不得插空行。"""
    n = 6
    cap = capture(PROMPT_PLAIN, ["\r"] * n, per_step=0.30)
    token = PROMPT_PLAIN.encode("utf-8")
    count = cap.snapshot.count(token)
    nb = cap.nonblank()
    no_blank_line = (b"\r\n\r\n" not in cap.snapshot
                     and b"\n\n" not in cap.snapshot
                     and b"\r\n\n" not in cap.snapshot)
    no_dup = all(ln.count("P>") <= 1 for ln in cap.scr.lines())
    checks = [
        ("提示符出现次数 <= N+1（一次空回车至多刷一个）",
         count <= n + 1, "count=%d N=%d" % (count, n)),
        ("屏幕非空行数 <= N+1（不超量刷屏）",
         len(nb) <= n + 1, "nonblank=%d" % len(nb)),
        ("无行间空行（无 CRLFCRLF / LFLF）",
         no_blank_line, "no_blank_line=%s" % no_blank_line),
        ("同一行不出现重复提示符",
         no_dup, "no_dup=%s" % no_dup),
    ]
    return case_result(
        "U1 空命令连续回车 x6：提示符次数 / 屏幕行数 / 空行",
        "6 次空回车后提示符总数 <= 7、屏幕非空行 <= 7、无行间空行、无同行重复",
        cap, checks,
    )


def case_u2():
    """带完整路径的 PS1：单次空回车的提示符增量必须恰好为 1（不多刷一段）。"""
    ps1 = PROMPT_PATH
    token = b"UXPROMPT:"
    cap0 = capture(ps1, [], start_pause=0.7)                 # 基线：仅初始提示符
    cap1 = capture(ps1, ["\r"], start_pause=0.7, per_step=0.35)  # 1 次空回车
    base = cap0.snapshot.count(token)
    after = cap1.snapshot.count(token)
    delta = after - base
    no_blank_line = (b"\r\n\r\n" not in cap1.snapshot
                     and b"\n\n" not in cap1.snapshot)
    checks = [
        ("单次空回车提示符增量 == 1（不「多刷一段」）",
         delta == 1, "base=%d after=%d delta=%d" % (base, after, delta)),
        ("单次空回车后无行间空行",
         no_blank_line, "no_blank_line=%s" % no_blank_line),
        ("单次空回车后屏幕非空行数 <= 2",
         len(cap1.nonblank()) <= 2, "nonblank=%d" % len(cap1.nonblank())),
    ]
    return case_result(
        "U2 完整路径 PS1 单次空回车：提示符增量精确 == 1",
        "delta == 1、无行间空行、屏幕非空行 <= 2",
        cap1, checks,
    )


# ---------------------------------------------------------------------------
# U3 —— 现象 2：命令失败后错误行的精确字节（断言不含残留 'a'）
# ---------------------------------------------------------------------------

def case_u3():
    """重复触发 `a` 失败命令 8 次，逐条校验错误行精确字节，排除 'awash:'。"""
    cap = capture(PROMPT_PLAIN, ["a\r"] * 8, per_step=0.30)
    expect_line = "wash: a: command not found"
    errs = [ln.strip() for ln in plain_lines(cap.text)
            if "command not found" in ln]
    exact = len(errs) >= 1 and all(ln == expect_line for ln in errs)
    no_awash = b"awash:" not in cap.snapshot
    no_aawash = b"aawash" not in cap.snapshot
    no_screen_prefix = not any(
        ln.lstrip().startswith("awash") for ln in cap.scr.lines()
    )
    checks = [
        ("捕获到 command not found 错误行",
         len(errs) >= 1, "errs=%d" % len(errs)),
        ("每条错误行精确等于 %r" % expect_line,
         exact, "errs=%r" % errs[:8]),
        ("原始字节不含 b'awash:'（无残留 'a' 前缀）",
         no_awash, "found=%s" % (not no_awash)),
        ("原始字节不含 b'aawash'",
         no_aawash, "found=%s" % (not no_aawash)),
        ("屏幕无以 'awash' 开头的行",
         no_screen_prefix, "no_screen_prefix=%s" % no_screen_prefix),
    ]
    return case_result(
        "U3 失败命令错误行精确字节：不含残留 'a'",
        "每条错误行 == 'wash: a: command not found'，raw 不含 'awash:' / 'aawash'",
        cap, checks,
    )


# ---------------------------------------------------------------------------
# U4 / U5 —— 现象 3+4：PS1 含非 ASCII / ANSI 颜色 / 长路径时的
#                       可见文本、可见宽度、光标列
# ---------------------------------------------------------------------------

def case_u4():
    """非 ASCII PS1（中文）：屏幕可见文本必须精确、不得被拆成 Latin-1 乱码，
    光标列必须等于按 Unicode 宽度算出的可见宽度。"""
    cap = capture(PROMPT_CJK, ["echo ok\r"], per_step=0.40)
    want = PROMPT_CJK.rstrip()
    w = py_visible_width(PROMPT_CJK)
    lines = cap.scr.lines()
    has_exact = any(ln.rstrip() == want for ln in lines)
    has_replacement = "\ufffd" in cap.text
    has_latin1 = any("\u00c0" <= ch <= "\u00ff" for ch in cap.text)
    checks = [
        ("屏幕存在精确可见文本 %r 的提示符行（非 ASCII 不被拆坏）" % want,
         has_exact, "has_exact=%s lines=%r"
         % (has_exact, [ln for ln in lines if ln.strip()][:6])),
        ("光标列 == PS1 可见宽度 %d" % w,
         cap.scr.cursor()[1] == w, "cursor=%r want_c=%d"
         % (cap.scr.cursor(), w)),
        ("屏幕文本无 U+FFFD 替换符",
         not has_replacement, "has_replacement=%s" % has_replacement),
        ("屏幕文本无 U+00C0..U+00FF Latin-1 乱码字符",
         not has_latin1, "has_latin1=%s" % has_latin1),
    ]
    return case_result(
        "U4 非 ASCII（中文）PS1：可见文本与光标列",
        "屏幕行 == '提示>'，光标列 == 6，无 U+FFFD / Latin-1 乱码",
        cap, checks,
    )


def case_u5():
    """ANSI 颜色 + 长路径 + \\$ 的 PS1：ANSI 字节不得计入可见宽度/文本，
    光标列必须等于可见宽度，且不得出现「连续空格 + 孤立 $」的错列渲染。"""
    cap = capture(PROMPT_COLOR_LONG, ["echo ok\r"], per_step=0.40)
    want = EXPECT_COLOR_LONG.rstrip()
    w = py_visible_width(EXPECT_COLOR_LONG)
    lines = cap.scr.lines()
    has_exact = any(ln.rstrip() == want for ln in lines)
    bad_dollar = any(re.search(r" {6,}\$\s*$", ln) for ln in lines)
    has_replacement = "\ufffd" in cap.text
    checks = [
        ("屏幕存在精确可见文本 %r（ANSI 不进可见文本）" % want,
         has_exact, "has_exact=%s lines=%r"
         % (has_exact, [ln for ln in lines if ln.strip()][:6])),
        ("光标列 == 可见宽度 %d（ANSI/转义不计宽）" % w,
         cap.scr.cursor()[1] == w, "cursor=%r want_c=%d"
         % (cap.scr.cursor(), w)),
        ("屏幕无「连续 >=6 空格 + 孤立 $」错列渲染",
         not bad_dollar, "bad_dollar=%s" % bad_dollar),
        ("屏幕文本无 U+FFFD 替换符",
         not has_replacement, "has_replacement=%s" % has_replacement),
    ]
    return case_result(
        "U5 ANSI 颜色 + 长路径 PS1：可见宽度 / 光标列 / 无空格+$",
        "屏幕行 == 'GREEN:/very/.../debug$ '，光标列 == 可见宽度，无空格+$ 模式",
        cap, checks,
    )


# ---------------------------------------------------------------------------
# U6 —— 现象 3：提示符形态稳定性（不混入退出码 / git 分支 / 缩写路径）
# ---------------------------------------------------------------------------

def case_u6():
    """混合成功/失败命令 5 次：所有提示符行形态必须唯一一致，且不得混入
    `[127]`（退出码）、`(main)`（git 分支）、`/d/p/W/`（逐段缩写路径）。"""
    token = b"U6:UX"
    steps = ["echo one\r", "false\r", "echo two\r", "a\r", "echo three\r"]
    cap = capture(PROMPT_STABLE, steps, per_step=0.35)
    prompt_lines = [ln.rstrip() for ln in cap.scr.lines() if "U6:UX" in ln]
    # 只比较「提示符本体」（到 '$' 为止）：提示符后附着的输入回显是 fish 风格
    # 逐键重绘的正常结果，不属于形态跳变，不能拿整行做去重（否则假阳性）。
    prefixes = []
    for ln in prompt_lines:
        idx = ln.find("$")
        prefixes.append(ln[:idx + 1] if idx >= 0 else ln)
    distinct = sorted(set(prefixes))
    # 逐键重绘会让 raw 里出现很多次提示符（一次按键一次 \r\x1b[K），因此
    # 「次数 == 提交数 + 1」是错误的判据；改用「提交不产生行间空行」判定刷屏。
    no_blank_line = (b"\r\n\r\n" not in cap.snapshot
                     and b"\n\n" not in cap.snapshot)
    checks = [
        ("提示符本体形态唯一（去重后 <= 1 种）",
         len(distinct) <= 1, "distinct=%r" % distinct[:4]),
        ("提示符不混入退出码 [127]",
         b"[127]" not in cap.snapshot and "[127]" not in cap.text,
         "found=%s" % (b"[127]" in cap.snapshot)),
        ("提示符不混入 git 分支 (main)",
         b"(main)" not in cap.snapshot,
         "found=%s" % (b"(main)" in cap.snapshot)),
        ("提示符不出现逐段缩写路径 /d/p/W/",
         b"/d/p/W/" not in cap.snapshot,
         "found=%s" % (b"/d/p/W/" in cap.snapshot)),
        ("提交不产生行间空行（不额外刷屏）",
         no_blank_line, "no_blank_line=%s" % no_blank_line),
    ]
    return case_result(
        "U6 提示符形态稳定性：多条命令后唯一、不混入退出码/分支/缩写路径",
        "提示符本体去重 1 种、无 [127] / (main) / /d/p/W/、无行间空行",
        cap, checks,
    )


# ---------------------------------------------------------------------------
# U7 —— 现象 3：bash 高级 PS1 转义不得作为字面文本泄漏（形态跳变精确复现）
# ---------------------------------------------------------------------------

def case_u7():
    """PS1 含 bash 命令替换 $(...) 与 $? 时，不得把字面片段泄漏到提示符。

    用户实测「有时变成 ... (main) [127]>」正是这类高级转义被残缺渲染的
    表现：render_prompt 只支持 $NAME 展开，会把 `$(` / `__git_ps1` / `$?`
    原样输出成提示符文本，形成与「完整路径」形态不一致的跳变。
    """
    steps = ["echo one\r", "false\r", "echo two\r"]
    cap = capture(PROMPT_BASH_LEAK, steps, per_step=0.35)
    lines = [ln.rstrip() for ln in cap.scr.lines() if ln.strip()]
    leaked_lines = [ln for ln in lines
                    if "$(" in ln or "__git_ps1" in ln or "$?" in ln]
    screen_leak = bool(leaked_lines)
    raw_cmd_subst = b"$(__git_ps1" in cap.snapshot
    raw_qmark = b"$?" in cap.snapshot
    no_blank_line = (b"\r\n\r\n" not in cap.snapshot
                     and b"\n\n" not in cap.snapshot)
    checks = [
        ("屏幕不出现命令替换/退出码字面残留行（$( / __git_ps1 / $?）",
         not screen_leak, "screen_leak=%s leaked=%r"
         % (screen_leak, leaked_lines[:3])),
        ("原始字节不含 b'$(__git_ps1'",
         not raw_cmd_subst, "found=%s" % raw_cmd_subst),
        ("原始字节不含 b'$?'",
         not raw_qmark, "found=%s" % raw_qmark),
        ("提交不产生行间空行",
         no_blank_line, "no_blank_line=%s" % no_blank_line),
    ]
    return case_result(
        "U7 bash 命令替换/退出码 PS1：不得泄漏字面片段（现象 3 形态跳变）",
        "提示符可见文本不含 $( / __git_ps1 / $?，raw 不含 b'$(__git_ps1' / b'$?'",
        cap, checks,
    )


# ---------------------------------------------------------------------------
# U8 —— P1 第二组：trap 'exit' INT + Ctrl-C 必须真退出
# ---------------------------------------------------------------------------

def capture_int_exit(ps1="T> ", start_pause=0.7, per_step=0.4):
    """在 pty 里安装 `trap 'exit' INT`，提交前台长命令期间发 Ctrl-C，
    返回 (Capture, info)：info 含进程是否真退出、是否干净退出、退出后是否
    还响应后续命令。

    不复用 capture() 的 teardown —— 它会额外喂 exit/^D/^C，会掩盖「是否
    真的因 trap 退出」这一判定。本函数只发一条 Ctrl-C，然后非阻塞 waitpid。
    """
    home = tempfile.mkdtemp(prefix="wash_ux_")
    env = base_env(home)
    env["PS1"] = ps1

    pid, fd = pty.fork()
    if pid == 0:  # 子进程：在真实 TTY 里 exec wash（无参数 → 交互 REPL）
        try:
            os.execve(WASH, [WASH], env)
        except Exception:
            os._exit(127)
        os._exit(127)

    buf = bytearray()

    def pump(seconds: float) -> None:
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

    def wr(payload: bytes) -> None:
        try:
            os.write(fd, payload)
        except OSError:
            pass

    pump(start_pause)
    # INT trap 先打印一个独立标记行，再 `exit 7`：既能证明 trap 真的执行，
    # 又能用退出码 7 断言退出是 shell 正常 exit 而非被信号杀死。
    wr(b"trap 'echo MARK_FIRED; exit 7' INT\r")
    pump(per_step)
    wr(b"sleep 2\r")   # 前台命令：submit() 切 cooked 模式，wash 在 waitpid 中
    pump(per_step)
    # Ctrl-C。受控 pty 下 job control 会把 \x03 只投递给前台进程组（sleep），
    # wash 自身收不到 SIGINT，take_pending_signal() 便始终为 0；真实终端里
    # wash 是会收到该信号的。因此同时用 os.kill 把同一个 SIGINT 精确投递给
    # wash 进程，使「信号分支」这条真实代码路径在 pty 里可达（等价语义）。
    wr(b"\x03")
    try:
        os.kill(pid, signal.SIGINT)
    except OSError:
        pass
    # 等 sleep 2 结束、循环回到顶部消费 pending signal、派发 trap 并退出。
    pump(3.5)

    # 非阻塞等待：真退出时 waitpid 立即返回 pid；仍在运行则返回 (0, 0)。
    exited = False
    exit_status = None
    deadline = time.time() + 2.0
    while time.time() < deadline:
        done, status = os.waitpid(pid, os.WNOHANG)
        if done == pid:
            exited = True
            exit_status = status
            break
        time.sleep(0.1)
    snapshot = bytes(buf)

    if not exited:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    try:
        os.waitpid(pid, 0)
    except (ChildProcessError, OSError):
        pass
    try:
        os.close(fd)
    except OSError:
        pass

    scr = Screen(rows=80, cols=240)
    scr.feed(snapshot.decode("utf-8", "replace"))
    cap = Capture(snapshot, scr, ps1)
    # 正常 exit（而非被信号杀死）才算「干净退出」，避免崩溃造成假 PASS。
    clean_exit = exited and exit_status is not None and os.WIFEXITED(exit_status)
    exit_code = os.WEXITSTATUS(exit_status) if clean_exit else None
    info = {
        "exited": exited,
        "clean_exit": clean_exit,
        "exit_code": exit_code,
        # 逐键回显不会形成 "MARK_FIRED\r\n" 独立行，出现即证明 trap 被执行。
        "trap_fired": b"MARK_FIRED\r\n" in snapshot,
    }
    return cap, info


def case_u8():
    """`trap 'echo MARK_FIRED; exit 7' INT` 后 Ctrl-C，REPL 必须真退出。

    根因：src/repl.rs 主循环的 take_pending_signal 分支在派发 trap 后直接
    continue，跳过循环末尾的 exit_requested 检查，trap 里的 `exit` 被吞掉；
    下一轮的 run_str 开头又会清掉 exit_requested，于是再也不会退出。
    修复前本用例 FAIL（进程存活、无退出码）。
    """
    cap, info = capture_int_exit()
    checks = [
        ("Ctrl-C 后进程真的退出（非阻塞 waitpid 返回 pid）",
         info["exited"], "exited=%s" % info["exited"]),
        ("退出为正常 exit 而非被信号杀死（干净退出）",
         info["clean_exit"], "clean_exit=%s" % info["clean_exit"]),
        ("退出码 == 7（trap 里 exit 7 的码正确传递）",
         info["exit_code"] == 7, "exit_code=%r" % info["exit_code"]),
        ("INT trap 真的被执行（出现 MARK_FIRED 独立输出行）",
         info["trap_fired"], "trap_fired=%s" % info["trap_fired"]),
    ]
    return case_result(
        "U8 trap 'exit' INT + Ctrl-C：REPL 必须真退出",
        "进程退出、正常 exit 且退出码 7、INT trap 被执行",
        cap, checks,
    )


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------

def main() -> int:
    if not os.path.exists(WASH):
        print("FATAL: wash binary not found at %s; build first." % WASH,
              file=sys.stderr)
        return 2

    cases = [case_u1, case_u2, case_u3, case_u4, case_u5, case_u6, case_u7,
             case_u8]
    results = []
    for fn in cases:
        try:
            r = fn()
        except Exception as e:  # noqa: BLE001 - 单个用例崩溃不拖垮整套
            print("ERROR [%s]: %r" % (fn.__name__, e))
            results.append(False)
            continue

        cap = r["cap"]
        checks = r["checks"]
        passed = all(c[1] for c in checks)
        print("[%s] %s" % ("PASS" if passed else "FAIL", r["name"]))
        print("    expect: %s" % r["expect"])
        for label, ok, actual in checks:
            print("      - %s %s | %s" % ("ok" if ok else "NO", label, actual))
        if not passed:
            cap.dump()
        print()
        results.append(passed)

    n = sum(1 for x in results if x)
    total = len(results)
    print("SUMMARY: %d/%d cases passed" % (n, total))
    print("（FAIL 为真实断言结果：不放宽 / 不 skip / 不 xfail / 不标已知差异。）")
    return 0 if n == total else 1


if __name__ == "__main__":
    sys.exit(main())
