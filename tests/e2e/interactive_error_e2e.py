#!/usr/bin/env python3
# wash REPL e2e「第三组」：交互错误不得终止会话（P0 固化）。
#
# 背景（team-lead 派发，源自 #51 只读 pty 实测）：
#   bash 交互下 `set -u` 引用未定义变量、`${VAR?word}` 失败、非法语法都只让
#   **当前命令**失败：shell 保持存活、提示符重现、后续命令可执行、最终 EXIT(0)。
#   修复前 wash 的 src/expand.rs 中 unbound_variable / expansion_error 都是
#   `-> !` + std::process::exit(127)，把注释里自述的「non-interactive bash」
#   语义无条件套到交互路径，直接杀死整个 shell（详见下方「修复前基线」）。
#
# 本文件用 proc_assert.py 的进程级三态断言把该 P0 固化：每条跑 MSYS2 bash 5.3
# oracle + wash subject，断言真实进程状态与 oracle 一致、最终 EXIT(0)（显式
# 排除 WIFEXITED 127 / WIFSIGNALED / TIMEOUT）、逐 step 会话存活、后续命令
# 产生独立输出行。
#
# 初跑（src 尚未修复）保留真实 FAIL，不放宽 / 不 skip / 不 xfail /
# 不标「已知差异」。src 修复由 team-lead/wash-eng 负责；本文件只改 tests/e2e/。
#
# 修复前基线（已实测，2026-11-06，未修复二进制 17:20:56 / 9877644）：
#   python3 tests/e2e/interactive_error_e2e.py -> SUMMARY: 1/3 cases passed
#   1) set -u unbound variable : FAIL
#      oracle EXIT(0) / alive_after=[True,True,True,False] / 独立行 ALIVE present
#      wash   EXIT(127) / alive_after=[True,False,False,False] / 无 ALIVE
#      诊断 wash: NO_SUCH_VAR: unbound variable 打印后进程终止
#   2) ${VAR?word} failure : FAIL（expansion_error 路径）
#      oracle EXIT(0) / alive_after=[True,True,False] / 独立行 ALIVE2 present
#      wash   EXIT(127) / alive_after=[False,False,False] / 无 ALIVE2
#      诊断 wash: NO_SUCH_VAR: boom 打印后进程终止
#   3) syntax error regression fence : PASS（两侧 EXIT(0)、AFTER_SYNTAX present）
#   修复前的独立只读 pty 实测见任务 #51 报告；本组在**未修复二进制**上的初跑
#   结果即上面记录的 1/3（两条真实 FAIL），不另存仓库外临时文件、不重复抓 FAIL。
#
# 运行（必须经 MSYS2）：
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc 'cd /d/project/WorkBuddy/wash && python3 tests/e2e/interactive_error_e2e.py'
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

from proc_assert import base_env, run_pty, compare  # noqa: E402

WASH = os.path.normpath(os.path.join(_HERE, "..", "..", "target", "debug", "wash.exe"))
BASH = "/usr/bin/bash.exe"

ORACLE_ARGV = [BASH, "--noprofile", "--norc", "-i"]   # bash 交互式
WASH_ARGV = [WASH, "-i"]                              # wash 强制交互式

ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")


def plain_lines(text):
    """剥离 ANSI 后按行切分；用于判断「独立输出行」，避免输入回显误判。"""
    s = ANSI.sub("", text).replace("\r\n", "\n").replace("\r", "\n")
    return [ln.strip() for ln in s.split("\n")]


# ---------------------------------------------------------------------------
# 用例字段
#   name / steps / timeout
#   expect_alive   {step_index: bool} wash 在该 step 后是否仍存活
#   expect_lines   [str] 必须作为独立输出行出现（plain_lines）
#   note
# ---------------------------------------------------------------------------

def build_cases():
    cases = []

    def add(**kw):
        cases.append(kw)

    # 1) set -u 引用未定义变量：unbound_variable 路径（上方行号均为修复前基线）。
    add(
        name="set -u unbound variable must not kill interactive shell",
        steps=[
            ("set -u\r", 1.0),
            ("echo $NO_SUCH_VAR\r", 1.0),
            ("echo ALIVE\r", 1.0),
            ("exit 0\r", 0.8),
        ],
        expect_alive={0: True, 1: True, 2: True},
        expect_lines=["ALIVE"],
        note="bash 交互下仅当前命令失败，shell 存活、后续 echo ALIVE 有效、最终 EXIT(0)",
    )

    # 2) ${VAR?word} 失败：expansion_error 路径（上方行号均为修复前基线）。
    #    注意本条不设 set -u，? 操作符自身即触发错误。
    add(
        name="${VAR?word} failure must not kill interactive shell",
        steps=[
            ("${NO_SUCH_VAR?boom}\r", 1.0),
            ("echo ALIVE2\r", 1.0),
            ("exit 0\r", 0.8),
        ],
        expect_alive={0: True, 1: True},
        expect_lines=["ALIVE2"],
        note="覆盖 expansion_error：bash 打印 bash: NO_SUCH_VAR: boom 后存活，后续命令有效",
    )

    # 3) 非法语法回归栅栏：#51 实测交互 bash 单行报错、无 -c: line N、
    #    无输入回显、会话存活；wash 已对齐，此条应稳定 PASS，防止修 1/2 时被带坏。
    add(
        name="syntax error must not kill interactive shell (regression fence)",
        steps=[
            ("}\r", 1.0),
            ("echo AFTER_SYNTAX\r", 1.0),
            ("exit 0\r", 0.8),
        ],
        expect_alive={0: True, 1: True},
        expect_lines=["AFTER_SYNTAX"],
        note="A 已对齐：交互语法错误只让当前行失败，shell 存活、后续命令有效",
    )

    return cases


# ---------------------------------------------------------------------------
# 执行与断言
# ---------------------------------------------------------------------------

def evaluate(case):
    timeout = case.get("timeout", 8.0)
    env_o = base_env(tempfile.mkdtemp(prefix="wash_e2e_oracle_"))
    env_w = base_env(tempfile.mkdtemp(prefix="wash_e2e_subject_"))

    o = run_pty(ORACLE_ARGV, case["steps"], env_o, timeout)
    w = run_pty(WASH_ARGV, case["steps"], env_w, timeout)

    checks = []

    # 核心：wash 的真实进程状态必须与 bash oracle 一致（超时/信号/正常退出
    # 三态互不等价，超时绝不算 PASS）。
    ok, detail = compare(o, w)
    checks.append(("进程状态与 oracle 一致", ok, detail))

    # 显式排除 WIFEXITED 127 / WIFSIGNALED / TIMEOUT：必须是正常退出且 rc==0。
    checks.append((
        "最终 EXIT(0)（非 WIFEXITED 127 / 非 WIFSIGNALED / 非 TIMEOUT）",
        (not w.timed_out) and w.sig is None and w.rc == 0,
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

    # 独立输出行（排除输入回显里的同名字符串）。
    lines = plain_lines(w.text)
    for line in case.get("expect_lines", []):
        present = any(ln == line for ln in lines)
        checks.append((f"独立输出行 {line!r}", present, f"present={present}"))

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
        # FAIL 时附剥 ANSI 后的末尾输出片段，作为失败现场的证据原文。
        if not passed:
            tail = [ln for ln in plain_lines(w.text) if ln][-12:]
            print("    --- wash tail (ANSI stripped) ---")
            for ln in tail:
                print(f"      | {ln}")
        print()
        results.append(passed)

    passed = sum(1 for r in results if r)
    total = len(results)
    print(f"SUMMARY: {passed}/{total} cases passed")
    print("（FAIL 为真实断言结果，禁止放宽 / skip / xfail / 标为已知差异。）")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
