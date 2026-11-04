#!/usr/bin/env python3
# wash REPL 端到端测试套件（扩展版，仅依赖标准库 pty/select，不依赖 pexpect）。
#
# 在真实 TTY（pty.fork）下启动 target/debug/wash，向其发送按键序列
# （Tab / 方向键 / Ctrl-C / Enter / 多行输入），捕获 ANSI / 文本输出做断言。
#
# 覆盖（用例清单，详见 main()）：
#   - 文件补全候选的本地化类型描述（C 英文 / zh_CN 中文）—— 复用 smoke 的 3 个
#   - 命令补全候选的 (Builtin)/(Executable, ..) 描述，按 locale 切换
#   - 分页状态行 rows %d to %d of %d 的精确格式（C 与 zh_CN）
#   - ghost 历史建议（灰字 SGR \x1b[38;5;245m）
#   - 语法高亮（命令绿 \x1b[38;5;76m / flag 橙 \x1b[38;5;214m）
#   - 多行续行提示符 `> `
#   - Ctrl-C 中断当前行回到干净提示符
#
# 运行（必须经 MSYS2）：
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc 'cd /d/project/WorkBuddy/wash && python3 tests/e2e/repl_e2e.py'
#
# 退出码：全部通过 0，否则 1。

import os
import pty
import select
import shutil
import sys
import tempfile
import time

# 二进制位置：相对本脚本（tests/e2e/repl_e2e.py -> ../../target/debug/wash.exe）。
_HERE = os.path.dirname(os.path.abspath(__file__))
WASH = os.path.normpath(os.path.join(_HERE, "..", "..", "target", "debug", "wash.exe"))


class Session:
    """在 pty 上驱动一个 wash REPL 进程。"""

    def __init__(self, locale, home, lines=None, env_extra=None):
        env = os.environ.copy()
        env["LC_ALL"] = locale
        env["LANG"] = locale
        env["HOME"] = home
        env["TERM"] = "xterm-256color"
        if lines is not None:
            env["LINES"] = str(lines)
        if env_extra:
            env.update(env_extra)

        pid, fd = pty.fork()
        if pid == 0:
            # 子进程：在 TTY 里直接 exec wash（无参数 → 交互 REPL）。
            try:
                os.execve(WASH, [WASH], env)
            except Exception:
                os._exit(127)

        self.pid = pid
        self.fd = fd
        self.buf = b""

    # -- 底层 IO ----------------------------------------------------------

    def _read_some(self, timeout):
        r, _, _ = select.select([self.fd], [], [], timeout)
        if not r:
            return False
        try:
            data = os.read(self.fd, 65536)
        except OSError:
            return False
        if not data:
            return False
        self.buf += data
        return True

    def text(self):
        return self.buf.decode("utf-8", "replace")

    def wait_for(self, needle, timeout=3.0):
        """阻塞直到 `needle`（str）出现在输出里，或超时。"""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if needle in self.text():
                return True
            self._read_some(0.1)
        return needle in self.text()

    def drain(self, timeout=1.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if not self._read_some(0.15):
                break

    def send(self, data):
        if isinstance(data, str):
            data = data.encode()
        try:
            os.write(self.fd, data)
        except OSError:
            pass
        time.sleep(0.06)

    def close(self):
        for seq in (b"\x03", b"exit\r", b"\x04"):
            try:
                os.write(self.fd, seq)
                time.sleep(0.15)
            except OSError:
                break
        try:
            os.waitpid(self.pid, 0)
        except (ChildProcessError, OSError):
            pass


# ---------------------------------------------------------------------------
# 断言辅助
# ---------------------------------------------------------------------------

def check(label, cond, text, needles=None):
    if cond:
        print(f"PASS [{label}]")
        return True
    print(f"FAIL [{label}]")
    if needles:
        for n in needles:
            print(f"    expect {n!r}: {'yes' if n in text else 'NO'}")
    print("    --- output snippet (last 1600 chars) ---")
    print(text[-1600:])
    return False


def fresh_home():
    return tempfile.mkdtemp(prefix="wash_e2e_")


def usr_bin_count():
    try:
        return len(os.listdir("/usr/bin"))
    except OSError:
        return None


# ---------------------------------------------------------------------------
# 用例
# ---------------------------------------------------------------------------

def case_file_completion_c():
    home = fresh_home()
    s = Session("C", home)
    s.wait_for("wash$ ", 3.0)
    s.send("/d/")
    s.send("\t")
    ok = s.wait_for("(Directory)", 3.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check("file-completion-C", ok, text, needles=["(Directory)"])


def case_file_completion_zh():
    home = fresh_home()
    s = Session("zh_CN.UTF-8", home)
    s.wait_for("wash$ ", 3.0)
    s.send("/d/")
    s.send("\t")
    ok = s.wait_for("(目录)", 3.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check("file-completion-zh_CN", ok, text, needles=["(目录)"])


def case_pager_status_c():
    home = fresh_home()
    s = Session("C", home)
    s.wait_for("wash$ ", 3.0)
    s.send("/usr/bin/")
    s.send("\t")
    ok = s.wait_for("rows", 3.0) and ("of" in s.text())
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check("pager-status-C", ok, text, needles=["rows", "of"])


def case_completion_desc_en():
    """命令补全：`(Builtin)` 与某路径型描述（英文）。"""
    home = fresh_home()
    s = Session("C", home)
    s.wait_for("wash$ ", 3.0)
    s.send("e")
    s.send("\t")
    ok = s.wait_for("(Builtin)", 3.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    seen = "(Executable" in text or "(File" in text or "(Symlink" in text
    return check(
        "completion-desc-en", ok and seen, text,
        needles=["(Builtin)", "(Executable", "(File", "(Symlink"],
    )


def case_completion_desc_zh():
    """命令补全：`(内建命令)` 与某路径型描述（中文）。"""
    home = fresh_home()
    s = Session("zh_CN.UTF-8", home)
    s.wait_for("wash$ ", 3.0)
    s.send("e")
    s.send("\t")
    ok = s.wait_for("(内建命令)", 3.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    seen = "(可执行文件" in text or "(文件" in text or "(符号链接" in text
    return check(
        "completion-desc-zh", ok and seen, text,
        needles=["(内建命令)", "(可执行文件", "(文件", "(符号链接"],
    )


def case_pager_exact_en():
    """分页状态行精确格式：`rows 1 to 19 of {K}`（LINES=21 → limit=19）。"""
    k = usr_bin_count()
    if k is None:
        print("SKIP [pager-status-exact-en] (cannot read /usr/bin)")
        return True
    home = fresh_home()
    s = Session("C", home, lines=21)
    s.wait_for("wash$ ", 3.0)
    s.send("/usr/bin/")
    s.send("\t")
    needle = f"rows 1 to 19 of {k}"
    ok = s.wait_for(needle, 3.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check("pager-status-exact-en", ok, text, needles=[needle])


def case_pager_exact_zh():
    """分页状态行精确格式（中文）：`第 1 到 19 行，共 {K} 行`。"""
    k = usr_bin_count()
    if k is None:
        print("SKIP [pager-status-exact-zh] (cannot read /usr/bin)")
        return True
    home = fresh_home()
    s = Session("zh_CN.UTF-8", home, lines=21)
    s.wait_for("wash$ ", 3.0)
    s.send("/usr/bin/")
    s.send("\t")
    needle = f"第 1 到 19 行，共 {k} 行"
    ok = s.wait_for(needle, 3.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check("pager-status-exact-zh", ok, text, needles=[needle])


def case_ghost():
    """ghost 历史建议：输入命中历史前缀时出现灰字 SGR。"""
    home = fresh_home()
    # 预置历史（REPL 启动时加载）。唯一前缀确保只匹配这一条。
    marker = "washqa_ghost_marker"
    with open(os.path.join(home, ".wash_history"), "w", encoding="utf-8") as f:
        f.write(marker + " this is the suggested tail\n")
    s = Session("C", home)
    s.wait_for("wash$ ", 3.0)
    s.send(marker)
    ok = s.wait_for("\x1b[38;5;245m", 2.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check("ghost-suggestion", ok, text, needles=["\x1b[38;5;245m"])


def case_syntax_highlight():
    """语法高亮：命令名绿、flag 橙的 ANSI 转义存在（不校验精确色值）。"""
    home = fresh_home()
    s = Session("C", home)
    s.wait_for("wash$ ", 3.0)
    s.send("echo -n hello")
    ok = s.wait_for("\x1b[38;5;76m", 2.0) and ("\x1b[38;5;214m" in s.text())
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check(
        "syntax-highlight", ok, text,
        needles=["\x1b[38;5;76m", "\x1b[38;5;214m"],
    )


def case_multiline():
    """多行续行：未闭合引号时 Enter 进入 `> ` 续行，闭合后整行提交并执行。"""
    home = fresh_home()
    s = Session("C", home)
    s.wait_for("wash$ ", 3.0)
    s.send('echo "')      # 未闭合双引号
    s.send("\r")           # Enter → 应进入续行
    cont_ok = s.wait_for("> ", 2.0)
    s.send("abc")          # 续行内容
    s.send('"')            # 闭合引号
    s.send("\r")           # Enter → 提交（退出多行，整行作为一条命令执行）
    # `abc` 只会在多行整行提交、命令真正执行后才输出，用作"已退出多行"的标记。
    executed = s.wait_for("abc", 3.0)
    text = s.text()
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    # 续行提示必须出现；命令输出必须出现（证明多行已作为整行提交并执行）。
    cond = cont_ok and executed and text.rfind("wash$ ") > text.find("abc")
    if executed:
        print("    (info) multiline command output 'abc' observed -> run_str handles embedded newline")
    else:
        print("    (info) multiline command output NOT observed -> see reported note on run_str")
    return check("multiline-continuation", cond, text, needles=["> ", "abc", "wash$ "])


def case_ctrl_c():
    """Ctrl-C 中断当前行：打印 ^C 并回到干净的主提示符。"""
    home = fresh_home()
    s = Session("C", home)
    s.wait_for("wash$ ", 3.0)
    s.send("echo interrupted_line_xyz")
    s.send(b"\x03")  # Ctrl-C
    saw_caret = s.wait_for("^C", 2.0)
    text = s.text()
    # 主提示符必须出现在 ^C 之后（即中断后重新画出了干净提示符）。
    after = text.rfind("wash$ ") > text.find("^C")
    s.close()
    shutil.rmtree(home, ignore_errors=True)
    return check("ctrl-c-clean-prompt", saw_caret and after, text, needles=["^C", "wash$ "])


# ---------------------------------------------------------------------------
# 入口
# ---------------------------------------------------------------------------

def main() -> int:
    if not os.path.exists(WASH):
        print(f"FATAL: wash binary not found at {WASH}; build first.", file=sys.stderr)
        return 2

    cases = [
        case_file_completion_c,
        case_file_completion_zh,
        case_pager_status_c,
        case_completion_desc_en,
        case_completion_desc_zh,
        case_pager_exact_en,
        case_pager_exact_zh,
        case_ghost,
        case_syntax_highlight,
        case_multiline,
        case_ctrl_c,
    ]

    results = []
    for c in cases:
        try:
            results.append(c())
        except Exception as e:  # noqa: BLE001 - 单个用例崩溃不应拖垮整套
            print(f"ERROR [{c.__name__}]: {e}")
            results.append(False)

    passed = sum(1 for r in results if r)
    total = len(results)
    print(f"\nSUMMARY: {passed}/{total} cases passed")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
