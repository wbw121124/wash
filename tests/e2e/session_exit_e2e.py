#!/usr/bin/env python3
# wash REPL「会话终止类」端到端用例（进程级断言）。
#
# 与 repl_smoke.py / repl_e2e.py 的关系：那两个文件只断言 pty 渲染文本
# （`in text`），从不 waitpid 收子进程状态，结构性测不出「交互 `exit 7` 进程
# 退出码恒为 0」这类缺陷。本文件用 proc_assert.py 的进程级 helper：
#   - 每条用例都跑两遍：MSYS2 bash 5.3 作 oracle、wash 作 subject；
#   - 断言 wash 的真实进程状态（WIFEXITED/WEXITSTATUS、WIFSIGNALED/WTERMSIG、
#     TIMEOUT）与 bash **一致**；超时 / 信号致死绝不当成 PASS；
#   - 另加会话存活类断言（alive_after）与必要的文本断言。
#
# 初跑会真实捕获修复前的 FAIL（例如 exit 7 → wash EXIT(0) vs bash EXIT(7)、
# EXIT trap 不触发等）。这些 FAIL 是修复前必须存在的证据，**不放宽、不 skip、
# 不 xfail、不标注为「已知差异」跳过**。src 修复由 wash-eng 负责。
#
# 运行（必须经 MSYS2）：
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc 'cd /d/project/WorkBuddy/wash && python3 tests/e2e/session_exit_e2e.py'
#
# 退出码：全部通过 0；有 FAIL 则 1（FAIL 是真实断言结果，不是脚本错误）。

import os
import re
import sys
import tempfile

# 导入同目录的 proc_assert 时不要生成 tests/e2e/__pycache__/（仓库内不留垃圾）。
sys.dont_write_bytecode = True

_HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, _HERE)

from proc_assert import base_env, run_pty, run_piped, compare  # noqa: E402

WASH = os.path.normpath(os.path.join(_HERE, "..", "..", "target", "debug", "wash.exe"))
BASH = "/usr/bin/bash.exe"

# oracle：bash 交互式（禁 rc/profile，避免用户配置污染）
ORACLE_ARGV = [BASH, "--noprofile", "--norc", "-i"]
ORACLE_ARGV_PIPED = [BASH, "--noprofile", "--norc"]
# subject：wash 强制交互式
WASH_ARGV = [WASH, "-i"]

# ANSI 转义剥离，用于「独立输出行」类文本判据（wash 每敲一个键就重绘整行，
# 单纯 `in text` 会被回显里的同名字符串误判为「已执行」）。
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07")
# REPL 渲染续行提示符的原始字节序列（print!("\r\x1b[K> ")）
CONT_MARKER = "\x1b[K> "


def plain_lines(text):
    s = ANSI.sub("", text).replace("\r\n", "\n").replace("\r", "\n")
    return [ln.strip() for ln in s.split("\n")]


def trap_output_ran(text):
    """EXIT trap 真的执行：plain_lines 里出现独立一行 `TRAP_RAN`（而非仅回显）。"""
    return any(ln == "TRAP_RAN" for ln in plain_lines(text))


# ---------------------------------------------------------------------------
# 用例定义
#
# 每个 case 字段：
#   name            用例名
#   steps           [(payload, pause)] 喂给 pty 的输入序列
#   oracle_argv     oracle 的 argv（默认 ORACLE_ARGV）
#   wash_argv       subject 的 argv（默认 WASH_ARGV）
#   piped           是否走 stdin=pipe（默认 False，走 pty）
#   piped_data      piped 模式喂入的字节
#   timeout         等待自行退出的上限（超时记 TIMEOUT，不算 PASS）
#   expect_alive    {step_index: bool} wash 在该 step 之后是否仍存活
#   expect_cont     None=不检查 / False=不得出现续行提示符 / True=必须出现
#   expect_trap     是否要求 EXIT trap 打印独立一行 TRAP_RAN
#   expect_text     [str] 必须出现在 wash 渲染文本里的子串
#   expect_lines    [str] 必须作为独立输出行出现（plain_lines）
#   note            说明
# ---------------------------------------------------------------------------


def build_cases():
    cases = []

    def add(**kw):
        cases.append(kw)

    add(
        name="exit 7",
        steps=[("exit 7\r", 0.5)],
        note="交互 exit 7：bash EXIT(7)；当前 wash 只 EXIT(0)（已知缺陷，须保留 FAIL）",
    )
    add(
        name="exit 0",
        steps=[("exit 0\r", 0.5)],
        note="exit 0：bash EXIT(0)",
    )
    add(
        name="exit 300",
        steps=[("exit 300\r", 0.5)],
        note="exit 300：bash 取模 256 -> EXIT(44)",
    )
    add(
        name="exit abc",
        steps=[("exit abc\r", 1.0), ("echo SURVIVED\r", 1.0), ("exit 0\r", 0.6)],
        expect_alive={0: True},
        expect_text=["numeric argument"],
        note="exit 非数字：报错但 shell 不退出（bash 交互下亦存活），后续命令继续",
    )
    add(
        name="false; exit",
        steps=[("false; exit\r", 0.5)],
        note="exit 无参沿用上一条命令状态：bash EXIT(1)",
    )
    add(
        name="true; exit",
        steps=[("true; exit\r", 0.5)],
        note="exit 无参沿用上一条命令状态：bash EXIT(0)",
    )
    # 说明：submit() 会在执行命令期间切回 cooked 模式（ICANON/ISIG/VEOF 生效）。
    # 若控制字符（Ctrl-D/Ctrl-C）恰好在 cooked 期间写入，会被行规程当作 VEOF /
    # SIGINT 吞掉，导致 wash 卡住 -> TIMEOUT（实测存在该竞态）。因此控制字符之前
    # 留足 pause，确保 REPL 已重新进入 raw mode，结果才是确定的。
    add(
        name="false then Ctrl-D",
        steps=[("false\r", 1.2), ("\x04", 0.6)],
        expect_alive={0: True},
        note="false 后 EOF：bash EXIT(1)（沿用 last_status）",
    )
    add(
        name="true then Ctrl-D",
        steps=[("true\r", 1.2), ("\x04", 0.6)],
        expect_alive={0: True},
        note="true 后 EOF：bash EXIT(0)",
    )
    add(
        name="exec /no/such/prog then echo ok",
        steps=[("exec /no/such/prog\r", 1.2), ("echo ok\r", 1.0), ("exit 0\r", 0.6)],
        expect_alive={0: True},
        expect_lines=["ok"],
        note="exec 失败后 REPL 不应终止，应能继续接受 echo ok 并最终正常退出",
    )
    add(
        name="EXIT trap; exit",
        steps=[("trap 'echo TRAP_RAN' EXIT; exit\r", 0.6)],
        expect_trap=True,
        note="退出前应触发 EXIT trap 打印独立一行 TRAP_RAN（当前 wash 不触发，须保留 FAIL）",
    )
    add(
        name="sleep 2 & (no continuation)",
        steps=[("sleep 2 &\r", 1.2), ("exit 0\r", 0.6)],
        expect_cont=False,
        note="后台 & 是完整命令，不应把 & 当成续行而弹 `> `（当前 wash 会弹，须保留 FAIL）",
    )
    add(
        name="Ctrl-C during foreground sleep",
        # 注意：Ctrl-C 必须在 `sleep` 前台执行期间（cooked 模式）发出，才是在测
        # 「信号不应杀死 shell」。后续 echo/exit 留足 pause 等 REPL 回到 raw mode。
        # 时序加固：Ctrl-C 之前的 pause 由 0.8s 提到 1.2s（与两条 EOF 用例的
        # 1.2s 策略一致），给 oracle/wash 足够时间完成前台作业控制与信号
        # handler 安装，避免 Ctrl-C 过早落在 cooked 窗口被行规程/信号竞态
        # 影响 oracle 侧退出码（实测该竞态曾让 oracle 在 EXIT(0)/EXIT(2) 摆动）。
        # 仅调整发送控制字符前的等待时长，不动任何断言/期望值/进程状态判据。
        steps=[("sleep 8\r", 1.2), ("\x03", 0.8), ("echo ok\r", 1.0), ("exit 0\r", 0.6)],
        timeout=6.0,
        expect_alive={1: True},
        expect_lines=["ok"],
        note="前台命令期间 Ctrl-C：shell 不得被 SIGINT 杀死，能继续 echo ok 并正常退出",
    )

    # --- 探索性用例：探测是否存在「exit 不退出 / 退出码不传播」的更多分支 ---
    add(
        name="exit inside function",
        steps=[("f() { exit 9; }; f\r", 0.5)],
        note="函数体内 exit 9：bash EXIT(9)",
    )

    # source 脚本内 exit：脚本放临时目录（仓库外），路径注入 steps。
    src_dir = tempfile.mkdtemp(prefix="wash_e2e_src_")
    script = os.path.join(src_dir, "exitscript.sh")
    with open(script, "w", encoding="utf-8") as f:
        f.write("exit 9\n")
    add(
        name="exit inside sourced script",
        steps=[("source '%s'\r" % script, 1.2), ("exit 0\r", 0.6)],
        note="source 脚本内 exit 9：bash EXIT(9)",
    )

    add(
        name="exit after multiline command",
        steps=[('echo "a\r', 0.8), ('b"\r', 0.8), ("exit 0\r", 0.6)],
        expect_cont=True,
        note="多行命令（合法续行）之后 exit 0 仍应正常终止；续行提示符应出现",
    )
    add(
        name="exit 7 (wash WITHOUT -i, tty)",
        steps=[("exit 7\r", 0.5)],
        wash_argv=[WASH],
        note="非 -i、但 stdin 为 tty 时：bash EXIT(7)",
    )
    add(
        name="exit 7 (piped stdin)",
        steps=[],
        piped=True,
        piped_data=b"exit 7\n",
        oracle_argv=ORACLE_ARGV_PIPED,
        wash_argv=[WASH],
        note="stdin 为 pipe（run_piped 分支）：bash EXIT(7)",
    )
    return cases


# ---------------------------------------------------------------------------
# 执行与断言
# ---------------------------------------------------------------------------


def evaluate(case):
    timeout = case.get("timeout", 7.0)
    env_o = base_env(tempfile.mkdtemp(prefix="wash_e2e_oracle_"))
    env_w = base_env(tempfile.mkdtemp(prefix="wash_e2e_subject_"))
    o_argv = case.get("oracle_argv", ORACLE_ARGV)
    w_argv = case.get("wash_argv", WASH_ARGV)

    if case.get("piped"):
        data = case["piped_data"]
        o = run_piped(o_argv, data, env_o, timeout)
        w = run_piped(w_argv, data, env_w, timeout)
    else:
        steps = case["steps"]
        o = run_pty(o_argv, steps, env_o, timeout)
        w = run_pty(w_argv, steps, env_w, timeout)

    checks = []

    # 1) 核心：wash 的真实进程状态必须与 bash oracle 一致。
    #    超时 / 信号致死 / 正常退出三者互不等价，不会被当成 PASS。
    ok, detail = compare(o, w)
    checks.append(("进程状态与 oracle 一致", ok, detail))

    # 2) wash 必须在有限时间内**自行**终止（不能靠超时 kill 才算通过），
    #    且必须是正常退出（WIFEXITED）。
    checks.append((
        "wash 自行终止且正常退出（非超时/非信号致死）",
        (not w.timed_out) and w.exited_normally,
        w.describe(),
    ))

    # 3) 会话存活类断言（进程级，不看屏幕文本）。
    for i, expected in case.get("expect_alive", {}).items():
        actual = w.alive_after[i] if i < len(w.alive_after) else None
        checks.append((
            f"wash.alive_after[{i}] == {expected}",
            actual == expected,
            f"actual={actual} (alive_after={w.alive_after})",
        ))

    # 4) 续行提示符。
    if case.get("expect_cont") is False:
        checks.append((
            "不出现续行提示符 `> `",
            CONT_MARKER not in w.text,
            f"CONT marker present={CONT_MARKER in w.text}",
        ))
    elif case.get("expect_cont") is True:
        checks.append((
            "出现续行提示符 `> `",
            CONT_MARKER in w.text,
            f"CONT marker present={CONT_MARKER in w.text}",
        ))

    # 5) EXIT trap 是否真的执行（独立输出行，排除输入回显误判）。
    if case.get("expect_trap"):
        ran = trap_output_ran(w.text)
        checks.append((
            "EXIT trap 打印独立一行 TRAP_RAN",
            ran,
            f"trap_output_ran={ran}, raw occurrences={w.text.count('TRAP_RAN')}",
        ))

    # 6) 补充文本断言。
    for needle in case.get("expect_text", []):
        checks.append((f"渲染文本含 {needle!r}", needle in w.text,
                       f"present={needle in w.text}"))
    for line in case.get("expect_lines", []):
        present = any(ln == line for ln in plain_lines(w.text))
        checks.append((f"独立输出行 {line!r}", present,
                       f"present={present}"))

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
