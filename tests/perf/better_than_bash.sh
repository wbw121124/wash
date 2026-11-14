#!/usr/bin/env bash
# wash 必须比 GNU bash 更好：性能 / 启动 / 常驻内存回归门禁。
#
# 产品目标（2026-11-09 主理人裁决）：wash 相对 oracle 要做到
#   「性能更高、速度更快、内存占用更小、UX 更友好」。
# 前三项由本脚本做可复测门禁；UX 侧由 tests/e2e/ 的 pty 套件覆盖。
# 非交互 `-c` 的逐字节兼容仍由 tests/bash_diff_*.sh 全量闸门守住，
# 本脚本不放宽、不跳过、不替代任何一对拍判定。
#
# 用法（必须经 MSYS2 bash，且传入**已构建的 release** 二进制）：
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
#     'cd <WASH_REPO> && bash tests/perf/better_than_bash.sh target/release/wash.exe'
#   ROUNDS=9 bash tests/perf/better_than_bash.sh <release-wash>   # 加深轮
#
# 2026-11-09 基线（基线提交 7f85765，ROUNDS=5，两端 links=1 独立副本）：
#   窗口一 TOTAL 0.81x / startup 0.42x / VmRSS 0.65x；
#   窗口二 TOTAL 0.81x / startup 0.38x / VmRSS 0.65x；
#   七项负载 0.58–0.98x，两个窗口全部 <1.00x。
#   路径对称性校验（同一二进制放两条路径）：TOTAL 1.00x / startup 1.01x / VmRSS 1.00x。
# 阈值在基线之上留出余量，只拦「确实不再比 bash 好」的退化，不追求卡噪声：
#   TOTAL <= 1.00x、startup <= 0.60x、VmRSS <= 0.80x。
#
# 边界（如实登记，勿夸大）：本门禁守的是「相对 GNU bash 不再更好」，
# 不是「必须使用 release 优化级别」。按路径名拒绝 debug 只是便利检查，
# cp 改名可绕过；实测把 debug 二进制改名后跑完整基准，三个比值仍为
# TOTAL 0.85x / startup 0.40x / VmRSS 0.72x 并全部 PASS —— 因为 MSYS2 启动
# 约 94% 是 Cygwin 进程创建 / DLL 地板（报告 §10.5），Rust 优化级别非主导项。
# 因此 debug 二进制**不能**用来证伪下面的阈值判定；用「收紧阈值」mutation
# 才能证明判定逻辑会真的 FAIL。前置硬闸（缺失二进制 / debug 路径 / oracle
# 伪装 / 无参 / 版本串不匹配 / 工具缺失）已逐项实测 FATAL。
#
# 测量纪律（docs/perf-compare-2026-11-08.md §10.7，违反即窗口作废）：
#   * 两端必须同目录、同方式 cp 成独立副本，stat -c '%h' == 1；
#     cargo 输出路径是 links=2 硬链接，直接执行会稳定偏慢 24–35%。
#   * 开跑前做路径对称性校验：同一份二进制放两条待测路径，空负载比值 ≈1.00x。
#   * smoke test：负载真的跑成功（rc + stdout 逐字节一致），防止
#     bench.sh 的 time_run 丢弃 stdout/stderr/rc 把「启动失败」计成极快。
#   * ROUNDS 轮 bash/wash 交错取最小；只看同窗口比值，不跨窗口比较绝对值。
#
# 窗口有效性（2026-11-10 补记）：曾有一次后台基准任务因系统睡眠被挂起，
# 任务计时显示「运行约 8 小时」而实际只跑了几分钟。任何跨越睡眠/挂起、
# 与并行重负载重叠、或路径对称性越界的窗口，其数值**一律作废**，不得计入
# 基线，必须在无干扰窗口重测。提交 848dce1 信息里提到的「第三个窗口」
# 正属此类：它只用来证明「收紧阈值会让判定 FAIL」，不作为基线证据；
# 基线仍以 2026-11-09 两个干净窗口（0.81x/0.81x、0.42x/0.38x、0.65x/0.65x）为准。
set -u
export LC_ALL=C LANG=C

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BENCH="$SCRIPT_DIR/bench.sh"
BASH_BIN_SRC="${BASH_BIN_SRC:-/d/msys2/usr/bin/bash.exe}"
WASH_BIN="${1:-}"
ROUNDS="${ROUNDS:-5}"
VERSION_SH="$REPO_ROOT/tools/wash_version.sh"

# 阈值（比值越小越好；wash/bash）
MAX_TOTAL='1.00'
MAX_STARTUP='0.60'
MAX_RSS='0.80'

hard_fail() { echo "FATAL: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || hard_fail "required tool '$1' is missing (an assertion would be silently skipped)"; }

for t in stat sha256sum awk seq timeout tail date cp basename grep head mktemp dirname wc cat sleep od tr sort; do need "$t"; done
[ -f "$BENCH" ] || hard_fail "bench.sh not found at $BENCH"
[ -f "$VERSION_SH" ] || hard_fail "tools/wash_version.sh not found"
[ -n "$WASH_BIN" ] || hard_fail "usage: better_than_bash.sh <release-wash-binary>"

# ROUNDS 硬闸：ROUNDS=0 会让 bench.sh 的所有交错循环一次都不跑，mb/mw 保持
# 99999999 哨兵值，TOTAL = 0/0 -> awk 产出 1.00x，恰好 <= MAX_TOTAL 而空洞
# PASS；非整数/负数同样会让计时循环退化。ROUNDS 必须是 >= 2 的十进制整数。
case "$ROUNDS" in
    ''|*[!0-9]*) hard_fail "ROUNDS must be an integer >= 2, got [$ROUNDS]" ;;
esac
# 去掉前导零后再比较，避免 08 被当成八进制；并限制上界防止误输入跑数小时。
ROUNDS_N=$((10#$ROUNDS))
[ "$ROUNDS_N" -ge 2 ] || hard_fail "ROUNDS must be >= 2, got [$ROUNDS] (bench would be vacuous)"
[ "$ROUNDS_N" -le 50 ] || hard_fail "ROUNDS must be <= 50, got [$ROUNDS]"
echo "hard gate: ROUNDS=$ROUNDS_N is an integer in [2,50] (no vacuous benchmark)"

case "$WASH_BIN" in
    /*) ;;
    *) WASH_BIN="$REPO_ROOT/$WASH_BIN" ;;
esac
[ -x "$WASH_BIN" ] || hard_fail "wash binary not executable: $WASH_BIN"
case "$WASH_BIN" in
    */debug/*|*/debug) hard_fail "debug binary given ($WASH_BIN); this gate needs a release build" ;;
esac
[ -x "$BASH_BIN_SRC" ] || hard_fail "oracle bash missing: $BASH_BIN_SRC"

TMP="$(mktemp -d)" || hard_fail 'mktemp failed'
trap 'rm -rf "$TMP"' EXIT
export HOME="$TMP/home"; mkdir -p "$HOME" || hard_fail 'HOME dir not writable'
touch "$TMP/.writable_probe" || hard_fail 'temporary directory is not writable'

# 版本一致性：被测二进制必须来自当前源码树（版本串由唯一真源生成）。
EXPECTED_VER="$(bash "$VERSION_SH")" || hard_fail 'version generator failed'
ACTUAL_VER="$("$WASH_BIN" --version 2>&1)"
[ "$ACTUAL_VER" = "wash $EXPECTED_VER" ] || \
    hard_fail "stale wash binary: got [$ACTUAL_VER], expected [wash $EXPECTED_VER]; rebuild first"

# ---------- 同目录独立副本 ----------
ORACLE="$TMP/bash_oracle.exe"
WASH_A="$TMP/wash_a.exe"
WASH_B="$TMP/wash_b_copy.exe"
cp "$BASH_BIN_SRC" "$ORACLE" || hard_fail 'cannot copy oracle'
cp "$WASH_BIN" "$WASH_A" || hard_fail 'cannot copy wash (A)'
cp "$WASH_BIN" "$WASH_B" || hard_fail 'cannot copy wash (B, symmetry)'
for f in "$ORACLE" "$WASH_A" "$WASH_B"; do
    links="$(stat -c '%h' "$f")" || hard_fail "stat failed on $f"
    [ "$links" = '1' ] || hard_fail "$f has $links hard links; measurements must use links=1 copies"
done
echo "=== 二进制 ==="
echo "  oracle : $("$ORACLE" --version | head -1)  [links=$(stat -c '%h' "$ORACLE")]"
echo "  wash   : $("$WASH_A" --version)  [links=$(stat -c '%h' "$WASH_A")]"
echo "  version gate: wash $EXPECTED_VER == $ACTUAL_VER"

# ---------- smoke：负载真的执行成功，杜绝「启动失败被计成极快」 ----------
# 与 bench.sh 的 CASES 同语义；名称标记核对防止负载集合被改却无人同步。
SMOKE_NAMES=(arith-loop builtin-echo expand-heavy)
SMOKE_MARKS=('arith-loop(2000次)' 'builtin-echo(500次)' 'expand-heavy(1000次)')
SMOKE_LINES=(1 500 0)
SMOKE_SCRIPTS=(
    'i=0; while [ $i -lt 2000 ]; do i=$((i+1)); done; echo $i'
    'i=0; while [ $i -lt 500 ]; do echo "line $i"; i=$((i+1)); done'
    's=abcdef; i=0; while [ $i -lt 1000 ]; do v=${s:1:2}${s^^}${#s}; i=$((i+1)); done'
)
echo "=== smoke（rc + stdout sha256 + 输出行数，双侧逐字节一致）==="
for i in 0 1 2; do
    name="${SMOKE_NAMES[$i]}"
    script="${SMOKE_SCRIPTS[$i]}"
    grep -Fq -- "${SMOKE_MARKS[$i]}" "$BENCH" || \
        hard_fail "bench.sh no longer contains the '$name' workload; sync SMOKE_* first"
    "$ORACLE" -c "$script" >"$TMP/s.b.out" 2>"$TMP/s.b.err"; brc=$?
    "$WASH_A" -c "$script" >"$TMP/s.w.out" 2>"$TMP/s.w.err"; wrc=$?
    [ "$brc" -eq 0 ] && [ "$wrc" -eq 0 ] || hard_fail "smoke '$name' rc: bash=$brc wash=$wrc"
    bh="$(sha256sum "$TMP/s.b.out" | awk '{print $1}')"
    wh="$(sha256sum "$TMP/s.w.out" | awk '{print $1}')"
    [ -n "$bh" ] && [ -n "$wh" ] || hard_fail "smoke '$name' sha256sum produced empty output"
    [ "$bh" = "$wh" ] || hard_fail "smoke '$name' stdout differs: oracle=$bh wash=$wh"
    blines="$(wc -l < "$TMP/s.b.out" | awk '{print $1}')"
    [ "$blines" -eq "${SMOKE_LINES[$i]}" ] || \
        hard_fail "smoke '$name' produced $blines lines, expected ${SMOKE_LINES[$i]} (workload did not really run)"
    if [ "$name" = arith-loop ]; then
        [ "$(cat "$TMP/s.b.out")" = '2000' ] || hard_fail "smoke arith-loop stdout is not 2000"
    fi
    echo "  ok  $name  lines=$blines  sha256=${bh:0:12}"
done

# ---------- 路径对称性校验（同一二进制两条路径）----------
now_ns() { date +%s%N; }
startup_once() { # bin -> ms for 50 cold-ish starts
    local t0 t1 i
    t0="$(now_ns)"
    for i in $(seq 1 50); do "$1" -c 'exit 0' >/dev/null 2>&1; done
    t1="$(now_ns)"
    echo $(( (t1 - t0) / 1000000 ))
}
sb=99999999; sw=99999999
for r in 1 2; do
    t="$(startup_once "$WASH_A")"; [ "$t" -lt "$sb" ] && sb=$t
    t="$(startup_once "$WASH_B")"; [ "$t" -lt "$sw" ] && sw=$t
done
[ "$sb" -gt 0 ] && [ "$sw" -gt 0 ] || hard_fail 'symmetry probe produced zero timings'
SYM="$(awk -v a="$sw" -v b="$sb" 'BEGIN{printf "%.3f", a/b}')"
echo "=== 路径对称性（同一二进制，50 次启动 ×2 轮交错取最小）==="
echo "  path A=${sb}ms  path B=${sw}ms  ratio=${SYM}x"
awk -v r="$SYM" 'BEGIN{exit !(r>=0.90 && r<=1.10)}' || \
    hard_fail "path symmetry ${SYM}x outside 0.90-1.10; this window is invalid (see report §10.7)"

# ---------- 正式基准：复用 bench.sh 的交错 best-of-N ----------
LOG="$TMP/bench.log"
BASH_BIN="$ORACLE" ROUNDS="$ROUNDS" bash "$BENCH" "$WASH_A" >"$LOG" 2>&1
brc=$?
[ "$brc" -eq 0 ] || { cat "$LOG" >&2; hard_fail "bench.sh exited $brc"; }
echo
cat "$LOG"
echo

get_ratio() { # pattern -> bare float, hard-fail on missing line
    local pat="$1" line val
    line="$(grep -F -- "$pat" "$LOG" | head -1)"
    [ -n "$line" ] || hard_fail "bench.sh output has no '$pat' line (assertion would be skipped)"
    val="$(printf '%s\n' "$line" | awk '{gsub(/x$/,"",$NF); print $NF}')"
    case "$val" in
        ''|*[!0-9.]*) hard_fail "cannot parse ratio from [$line]" ;;
    esac
    [ -n "$val" ] || hard_fail "empty ratio for '$pat'"
    printf '%s' "$val"
}

R_TOTAL="$(get_ratio 'TOTAL(best-of-')"
R_STARTUP="$(get_ratio 'startup(50次)')"
R_RSS="$(get_ratio 'VmRSS(read x 阻塞)')"

le() { awk -v a="$1" -v b="$2" 'BEGIN{exit !(a<=b)}'; }
FAILS=0
echo "=== 判定（wash/bash，越小越好；全部为硬判定，无 skip）==="
check() {
    local label="$1" got="$2" limit="$3"
    if le "$got" "$limit"; then
        printf '  PASS  %-10s %sx  <= %sx\n' "$label" "$got" "$limit"
    else
        printf '  FAIL  %-10s %sx  >  %sx\n' "$label" "$got" "$limit"
        FAILS=$((FAILS + 1))
    fi
}
check 'TOTAL'   "$R_TOTAL"   "$MAX_TOTAL"
check 'startup' "$R_STARTUP" "$MAX_STARTUP"
check 'VmRSS'   "$R_RSS"     "$MAX_RSS"

echo
if [ "$FAILS" -ne 0 ]; then
    echo "=== BETTER-THAN-BASH GATE: FAIL ($FAILS dimension(s) regressed) ==="
    exit 1
fi
echo "=== BETTER-THAN-BASH GATE: PASS — wash is faster and smaller than GNU bash ==="
echo "    (逐字节语义兼容仍由 tests/bash_diff_*.sh 独立守门，本脚本不替代)"
