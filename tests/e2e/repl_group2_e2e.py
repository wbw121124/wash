#!/usr/bin/env python3
# wash REPL e2e「第二组」用例：会话/信号/提示符/帮助文本（进程级断言）。
#
# 与 session_exit_e2e.py（第一组，17 条）并列，共用 proc_assert.py 的进程级
# helper：每条用例跑两遍（MSYS2 bash 5.3 oracle + wash subject），断言 wash 的
# 真实进程状态（WIFEXITED/WIFSIGNALED/TIMEOUT 三态严格区分）与 bash 一致，并
# 叠加会话存活 / 独立输出行 / 文本断言。
#
# 覆盖（team-lead 指派的第二组）：
#   1. set -o ignoreeof 后 Ctrl-D 不退出且能继续接受命令
#   2. INT trap 在 Ctrl-C 时运行时派发（独立输出行 + shell 存活）
#   3. --help 列出实现支持的 --command
#   4. PS1 转义 \d \t \A \H \# 被展开，渲染文本里不残留字面反斜杠转义串
#   5. exit abc 报错后继续 echo ok（后续命令仍可用）
#   6. 前台命令 Ctrl-C 后继续 echo ok（shell 存活且命令执行完）
#
# 初跑保留真实 FAIL，不放宽 / 不 skip / 不 xfail / 不标「已知差异」。src 修复
# 由 wash-eng 负责；本文件只改 tests/e2e/，不碰 src/。
#
# 运行（必须经 MSYS2）：
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc 'cd /d/project/WorkBuddy/wash && python3 tests/e2e/repl_group2_e2e.py'
#
# 退出码：全部通过 0；有 FAIL 则 1（FAIL 是真实断言结果，不是脚本错误）。

import os
import re
import sys
import tempfile

# 导入同目录的 proc_assert 时不生成 tests/e2e/__pycache__/（仓库内不留垃圾）。
sys.dont_write_bytecode = True

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)

from proc_assert import base_env, run_pty, run_piped, compare  # noqa: E402

WASH = os.path.normpath(os.path.join(_HERE, "..", "..", "target", "debug", "wash.exe"))
BASH = "/usr/bin/bash.exe"

ORACLE_ARGV = [BASH, "--noprofile", "--norc", "-i"]   # bash 交互式
WASH_ARGV = [WASH, "-i"]                              # wash 强制交互式

ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")
CONT_MARKER = "\x1b[K> "


def plain_lines(text):
    """剥离 ANSI 后按行切分；用于判断「独立输出行」，避免输入回显误判。"""
    s = ANSI.sub("", text).replace("\r\n", "\n").replace("\r", "\n")
    return [ln.strip() for ln in s.split("\n")]


# ---------------------------------------------------------------------------
# 用例字段
#   name / steps / oracle_argv / wash_argv / piped / piped_data / timeout
#   env_extra          dict，合并进 oracle 与 wash 的 env（用于注入 PS1）
#   expect_alive       {step_index: bool} wash 在该 step 后是否仍存活
#   expect_text        [str] 必须出现在 wash 渲染文本里的子串
#   expect_lines       [str] 必须作为独立输出行出现（plain_lines）
#   expect_absent_text [str] 必须**不**出现在 wash 渲染文本里的字面串
#   note
# ---------------------------------------------------------------------------

# 自定义 PS1：包含 bash 风格时间/日期/主机/命令序号转义，以及一个可核对的标记。
PS1_TEMPLATE = r"PS1MARK \d \t \A \H \# END "
PS1_ESCAPES = [r"\d", r"\t", r"\A", r"\H", r"\#"]


def build_cases():
    cases = []

    def add(**kw):
        cases.append(kw)

    # 1) ignoreeof：bash 的 Ctrl-D 不退出、打印提示、后续命令继续。
    #    Ctrl-D 之前留 1.2s：submit() 切回 cooked 模式期间写入的 0x04 会被
    #    行规程当 VEOF 吞掉（实测竞态），留足 pause 确保回到 raw mode。
    add(
        name="set -o ignoreeof then Ctrl-D",
        steps=[
            ("set -o ignoreeof\r", 1.2),
            ("\x04", 1.0),
            ("echo SURVIVED_IGNOREEOF\r", 1.0),
            ("exit 0\r", 0.6),
        ],
        expect_alive={1: True},
        expect_text=['Use "exit" to leave the shell.'],
        expect_lines=["SURVIVED_IGNOREEOF"],
        note="ignoreeof 下 Ctrl-D 不退出、打印提示，且后续命令仍可执行（bash 行为）",
    )

    # 2) INT trap 运行时派发：Ctrl-C 后应有独立输出行 SIGINT_RAN，shell 存活。
    #    注意用 standalone line 判据：wash 每敲一键重绘整行，回显里也会出现
    #    "SIGINT_RAN"（来自 trap 'echo SIGINT_RAN' INT 这行输入），不能算 trap 已派发。
    add(
        name="INT trap dispatched on Ctrl-C",
        steps=[
            ("trap 'echo SIGINT_RAN' INT\r", 1.2),
            ("\x03", 1.0),
            ("echo AFTER_INT\r", 1.0),
            ("exit 0\r", 0.6),
        ],
        expect_alive={1: True},
        expect_lines=["SIGINT_RAN", "AFTER_INT"],
        note="trap 只做存储、不派发则测不出；断言独立输出行 + shell 存活",
    )

    # 3) --help 应列出实现支持的 --command（usage 文档与实现自洽）。
    #    非交互：stdin 为 pipe；oracle 用 bash --help 对齐进程状态（均 EXIT(0)）。
    add(
        name="--help lists --command",
        steps=[],
        piped=True,
        piped_data=b"",
        oracle_argv=[BASH, "--help"],
        wash_argv=[WASH, "--help"],
        expect_text=["--command"],
        note="实现支持 --command，usage 文本应列出（bash 无此选项，故只对齐退出码）",
    )

    # 4) PS1 转义：设置带 \d \t \A \H \# 的自定义 PS1，断言渲染文本里不残留
    #    字面反斜杠转义串；同时用 PS1MARK 确认 PS1 确实被采用（防止「PS1 被
    #    整体忽略」造成的假 PASS）。
    add(
        name="PS1 escapes expanded (no literal backslashes)",
        steps=[("exit 0\r", 0.6)],
        env_extra={"PS1": PS1_TEMPLATE},
        expect_text=["PS1MARK"],
        expect_absent_text=PS1_ESCAPES,
        note=r"PS1 的 \d \t \A \H \# 应展开为值，而不是原样打印反斜杠串",
    )

    # 5) exit abc 报错后继续输入 echo ok：shell 存活且后续命令能执行完。
    add(
        name="exit abc then echo ok (continued input)",
        steps=[
            ("exit abc\r", 1.0),
            ("echo ABC_OK\r", 1.0),
            ("exit 0\r", 0.6),
        ],
        expect_alive={0: True},
        expect_text=["numeric argument"],
        expect_lines=["ABC_OK"],
        note="第一组已有 exit abc 存活断言，本条补「后续命令仍可用」",
    )

    # 6) 前台命令期间 Ctrl-C 后再输入 echo ok：比只断言进程状态更严格，
    #    要求命令真正执行完（独立输出行），且 shell 未被 SIGINT 杀死。
    add(
        name="Ctrl-C during foreground sleep then echo ok",
        steps=[
            ("sleep 8\r", 0.8),
            ("\x03", 0.8),
            ("echo CTRLC_OK\r", 1.0),
            ("exit 0\r", 0.6),
        ],
        timeout=6.0,
        expect_alive={1: True, 2: True},
        expect_lines=["CTRLC_OK"],
        note="Ctrl-C 不得杀死 shell，且后续 echo ok 必须执行完（独立输出行）",
    )

    return cases


# ---------------------------------------------------------------------------
# 执行与断言
# ---------------------------------------------------------------------------

def evaluate(case):
    timeout = case.get("timeout", 8.0)
    env_o = base_env(tempfile.mkdtemp(prefix="wash_e2e_oracle_"))
    env_w = base_env(tempfile.mkdtemp(prefix="wash_e2e_subject_"))
    extra = case.get("env_extra")
    if extra:
        env_o.update(extra)
        env_w.update(extra)

    o_argv = case.get("oracle_argv", ORACLE_ARGV)
    w_argv = case.get("wash_argv", WASH_ARGV)

    if case.get("piped"):
        data = case.get("piped_data", b"")
        o = run_piped(o_argv, data, env_o, timeout)
        w = run_piped(w_argv, data, env_w, timeout)
    else:
        steps = case["steps"]
        o = run_pty(o_argv, steps, env_o, timeout)
        w = run_pty(w_argv, steps, env_w, timeout)

    checks = []

    # 核心：wash 的真实进程状态必须与 bash oracle 一致（超时/信号/正常退出
    # 三态互不等价，超时绝不算 PASS）。
    ok, detail = compare(o, w)
    checks.append(("进程状态与 oracle 一致", ok, detail))

    # wash 必须在有限时间内自行终止且为正常退出（非超时 kill / 非信号致死）。
    checks.append((
        "wash 自行终止且正常退出（非超时/非信号致死）",
        (not w.timed_out) and w.exited_normally,
        w.describe(),
    ))

    # 会话存活类断言（进程级，不看屏幕文本）。
    for i, expected in case.get("expect_alive", {}).items():
        actual = w.alive_after[i] if i < len(w.alive_after) else None
        checks.append((
            f"wash.alive_after[{i}] == {expected}",
            actual == expected,
            f"actual={actual} (alive_after={w.alive_after})",
        ))

    # 文本子串。
    for needle in case.get("expect_text", []):
        checks.append((f"渲染文本含 {needle!r}", needle in w.text,
                       f"present={needle in w.text}"))

    # 独立输出行（排除输入回显里的同名字符串）。
    for line in case.get("expect_lines", []):
        present = any(ln == line for ln in plain_lines(w.text))
        checks.append((f"独立输出行 {line!r}", present, f"present={present}"))

    # 不得出现的字面串（PS1 转义未被展开）。
    for needle in case.get("expect_absent_text", []):
        checks.append((f"渲染文本不含字面 {needle!r}", needle not in w.text,
                       f"present={needle in w.text}"))

    return o, w, checks


def main():
    if not os.path.exists(WASH):
        print(f"FATAL: wash binary not found at {WASH}; build first.", file=sys.stderr)
        return 2

    cases = build_cases()
    results = []
    for case in cases:
        name = case["name"]
        try:
            o, w, checks = evaluate(case)
        except Exception as e:  # noqa: BLE001
            print(f"ERROR [{name}]: {e}")
            results.append(False)
            continue

        passed = all(c[1] for c in checks)
        print(f"[{'PASS' if passed else 'FAIL'}] {name}")
        print(f"    oracle(bash) : {o.describe():<12} timed_out={o.timed_out} alive_after={o.alive_after}")
        print(f"    subject(wash): {w.describe():<12} timed_out={w.timed_out} alive_after={w.alive_after}")
        for label, ok, detail in checks:
            print(f"      - {'ok ' if ok else 'NO '} {label}  | {detail}")
        print(f"    note: {case.get('note', '')}")
        print()
        results.append(passed)

    passed = sum(1 for r in results if r)
    total = len(results)
    print(f"SUMMARY: {passed}/{total} cases passed")
    print("（FAIL 为真实断言结果；修复前预期存在，禁止放宽/跳过。）")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
