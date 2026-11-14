#!/usr/bin/env bash
# wash vs GNU bash 三合一基准：性能 / 启动 / 内存（docs/perf-plan-2026-11-07.md P0-1）
#
# 用法：
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
#     'cd /d/project/WorkBuddy/wash && bash tests/perf/bench.sh'
# 可选参数：$1 = wash 二进制路径（默认 release，缺则退 debug）。
#
# 纪律（2026-11-07 实测教训）：
#   * 本机噪声极大 —— 所有计时用「bash/wash 交错 × ROUNDS 轮取最小」，
#     禁止凭单次读数下结论；
#   * LC_ALL=C、HOME 沙箱化到 mktemp；
#   * 内存探针用 FIFO 阻塞的 `read x` 进程读 /proc/<pid>/status。
set -u
export LC_ALL=C LANG=C
BASH_BIN="${BASH_BIN:-/d/msys2/usr/bin/bash.exe}"
WASH_BIN="${1:-/d/project/WorkBuddy/wash/target/release/wash.exe}"
[ -x "$WASH_BIN" ] || WASH_BIN="/d/project/WorkBuddy/wash/target/debug/wash.exe"
ROUNDS="${ROUNDS:-5}"
# 硬闸：ROUNDS=0 / 非整数会让所有交错计时循环一次都不跑，最小值保持
# 99999999 哨兵，比值变成 1.00x 而空洞 PASS。计时引擎自身必须拒绝这种输入。
case "$ROUNDS" in
    ''|*[!0-9]*) echo "FATAL: ROUNDS must be an integer >= 2, got [$ROUNDS]" >&2; exit 1 ;;
esac
_r=$((10#$ROUNDS))
[ "$_r" -ge 2 ] && [ "$_r" -le 50 ] || {
    echo "FATAL: ROUNDS must be in [2,50], got [$ROUNDS]" >&2; exit 1; }

SB="$(mktemp -d)"
trap 'rm -rf "$SB"' EXIT
export HOME="$SB/home"; mkdir -p "$HOME"

now_ns() { date +%s%N; }

# ---------- ① 性能：工作负载（内部循环 + 单次 -c） ----------
CASES=(
  "arith-loop(2000次)|i=0; while [ \$i -lt 2000 ]; do i=\$((i+1)); done; echo \$i"
  "builtin-echo(500次)|i=0; while [ \$i -lt 500 ]; do echo \"line \$i\"; i=\$((i+1)); done"
  "fork-exec-true(100次)|i=0; while [ \$i -lt 100 ]; do /usr/bin/true; i=\$((i+1)); done"
  "cmdsub(100次)|i=0; while [ \$i -lt 100 ]; do s=\$(echo x); i=\$((i+1)); done"
  "subshell(100次)|i=0; while [ \$i -lt 100 ]; do ( exit 0 ); i=\$((i+1)); done"
  "pipeline(50次)|i=0; while [ \$i -lt 50 ]; do echo hello | cat > /dev/null; i=\$((i+1)); done"
  "expand-heavy(1000次)|s=abcdef; i=0; while [ \$i -lt 1000 ]; do v=\${s:1:2}\${s^^}\${#s}; i=\$((i+1)); done"
)
time_run() { # bin script
    local t0 t1; t0=$(now_ns)
    "$1" -c "$2" > /dev/null 2>&1
    t1=$(now_ns); echo $(( (t1 - t0) / 1000000 ))
}

echo "== ① 性能（bash/wash 交错 x$ROUNDS 轮取最小）=="
printf '%-26s %10s %10s %8s\n' "workload" "bash(ms)" "wash(ms)" "wash/bash"
tot_b=0; tot_w=0
for entry in "${CASES[@]}"; do
    name="${entry%%|*}"; script="${entry#*|}"
    mb=99999999; mw=99999999
    for r in $(seq 1 "$ROUNDS"); do
        t=$(time_run "$BASH_BIN" "$script"); [ "$t" -lt "$mb" ] && mb=$t
        t=$(time_run "$WASH_BIN" "$script"); [ "$t" -lt "$mw" ] && mw=$t
    done
    tot_b=$((tot_b + mb)); tot_w=$((tot_w + mw))
    printf '%-26s %10d %10d %8s\n' "$name" "$mb" "$mw" \
        "$(awk -v a="$mw" -v b="$mb" 'BEGIN{printf "%.2fx", a/b}')"
done
printf '%-26s %10d %10d %8s\n' "TOTAL(best-of-$ROUNDS)" "$tot_b" "$tot_w" \
    "$(awk -v a="$tot_w" -v b="$tot_b" 'BEGIN{printf "%.2fx", a/b}')"

# ---------- ② 启动：50 次进程启动 × ROUNDS 轮，bash/wash 交错 ----------
time_startup() { # bin
    local t0 t1 i
    t0=$(now_ns)
    for i in $(seq 1 50); do "$1" -c 'exit 0' > /dev/null 2>&1; done
    t1=$(now_ns); echo $(( (t1 - t0) / 1000000 ))
}
echo
echo "== ② 启动（50 次进程启动，交错 x$ROUNDS 轮取最小）=="
mb=99999999; mw=99999999
for r in $(seq 1 "$ROUNDS"); do
    t=$(time_startup "$BASH_BIN"); [ "$t" -lt "$mb" ] && mb=$t
    t=$(time_startup "$WASH_BIN"); [ "$t" -lt "$mw" ] && mw=$t
done
printf '%-26s %10d %10d %8s\n' "startup(50次)" "$mb" "$mw" \
    "$(awk -v a="$mw" -v b="$mb" 'BEGIN{printf "%.2fx", a/b}')"

# ---------- ③ 内存：FIFO 阻塞的 `read x` 进程读 /proc/<pid>/status ----------
mem_kb() { # bin -> VmRSS in KB（阻塞期读）
    # `tail -f /dev/null | read x`：管道永远无换行 → 被测 shell 稳定阻塞；
    # $! 取后台流水线最后一个进程 = 被测 shell 本体。tail 由 timeout 兜底退出。
    timeout 5 tail -f /dev/null | "$1" -c 'read x' > /dev/null 2>&1 &
    local pid=$!
    sleep 1
    local rss=0 try
    for try in 1 2 3; do
        rss=$(awk '/^VmRSS:/{print $2}' "/proc/$pid/status" 2>/dev/null)
        [ -n "$rss" ] && break
        sleep 0.5
    done
    kill -9 "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    echo "${rss:-0}"
}
echo
echo "== ③ 内存（阻塞进程 VmRSS，取 3 次最小）=="
mb=99999999; mw=99999999
for r in 1 2 3; do
    t=$(mem_kb "$BASH_BIN"); [ "$t" -gt 0 ] && [ "$t" -lt "$mb" ] && mb=$t
    t=$(mem_kb "$WASH_BIN"); [ "$t" -gt 0 ] && [ "$t" -lt "$mw" ] && mw=$t
done
printf '%-26s %8sKB %8sKB %8s\n' "VmRSS(read x 阻塞)" "$mb" "$mw" \
    "$(awk -v a="$mw" -v b="$mb" 'BEGIN{ if (b>0) printf "%.2fx", a/b; else print "?" }')"
echo
echo "（结论只看同窗口比值；单独一次运行受机器噪声影响大）"
