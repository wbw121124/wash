#!/usr/bin/env python3
# wash REPL 补全的 pty 冒烟测试（不依赖 pexpect，仅用标准库 pty/select）。
#
# 目的：在真实 TTY 下触发补全，验证 fish 风格的补全候选确实带类型描述
# （如 `(Directory)` / `(Executable, 34kB)`），且按 locale 本地化
# （LC_ALL=C 下英文，zh_CN.UTF-8 下中文）；候选超屏时显示分页状态行
# `rows N to M of K`。
#
# 用法（必须经 MSYS2 运行）：
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc 'cd /d/project/WorkBuddy/wash && python3 tests/e2e/repl_smoke.py'
#
# 退出码：全部通过 0，否则 1。

import os
import pty
import select
import sys
import time

WASH = "/d/project/WorkBuddy/wash/target/debug/wash.exe"


def run_case(locale: str, trigger: str, expect: list[str], label: str) -> bool:
    env = dict(os.environ)
    env["LC_ALL"] = locale
    env["HOME"] = "/tmp/wash_e2e_home"
    os.makedirs(env["HOME"], exist_ok=True)

    pid, fd = pty.fork()
    if pid == 0:
        # 子进程：在 TTY 里直接 exec wash（无参数 → 交互 REPL）。
        try:
            os.execve(WASH, [WASH], env)
        except Exception:
            os._exit(127)

    # 父进程：发送输入并收集输出。
    buf = b""
    time.sleep(0.6)  # 等 REPL 进入 raw mode 并画出提示符。

    # 触发补全：先输入前缀，再发 Tab。
    os.write(fd, trigger.encode())
    time.sleep(0.3)
    os.write(fd, b"\t")
    time.sleep(0.8)

    # 读取一段时间（补全渲染 + 可能的分页状态行）。
    deadline = time.time() + 2.5
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.25)
        if r:
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            buf += data

    # 尽量干净地退出。
    for seq in (b"\x03", b"exit\r", b"\x04"):
        try:
            os.write(fd, seq)
            time.sleep(0.2)
        except OSError:
            break
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass

    text = buf.decode("utf-8", "replace")
    ok = all(s in text for s in expect)
    print(f"[{label}] locale={locale} -> {'PASS' if ok else 'FAIL'}")
    if not ok:
        for s in expect:
            print(f"    expect {s!r}: {'yes' if s in text else 'NO'}")
        print("    --- output snippet (first 1200 chars) ---")
        print(text[:1200])
    return ok


def main() -> int:
    if not os.path.exists(WASH):
        print(f"FATAL: wash binary not found at {WASH}; build first.", file=sys.stderr)
        return 2

    results = []

    # 用例 1：文件补全，LC_ALL=C 下应出现英文类型描述 `(Directory)`。
    # 触发 `/d/` 列出 /d 下条目（至少含目录），候选带 `(Directory)` 描述。
    results.append(
        run_case("C", "/d/", ["(Directory)"], "file-completion-C")
    )

    # 用例 2：同一触发，zh_CN.UTF-8 下应出现中文描述 `(目录)`。
    results.append(
        run_case("zh_CN.UTF-8", "/d/", ["(目录)"], "file-completion-zh_CN")
    )

    # 用例 3：/usr/bin 下文件极多，几乎必触发分页状态行（英文）。
    # 注意：仅验证分页行存在，不依赖具体数量。
    results.append(
        run_case("C", "/usr/bin/", ["rows", "of"], "pager-status-C")
    )

    passed = sum(1 for r in results if r)
    total = len(results)
    print(f"\nSUMMARY: {passed}/{total} cases passed")
    return 0 if passed == total else 1


if __name__ == "__main__":
    sys.exit(main())
