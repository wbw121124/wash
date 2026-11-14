#!/bin/bash
# CLI / argv 对拍套件。
#
# `bash_diff_builtins.sh` 固定用 `$BIN -c "$case_text"`，无法表达这些 argv 形态：
#   * `-c CMD NAME ARGS...`  —— bash 把 NAME 当 $0、其余当 $1/$2...
#   * `SCRIPT ARGS...`       —— bash 读并执行脚本文件，$0=SCRIPT
#   * `--command CMD`、`-lc CMD`、`-c CMD` 与脚本的路径差异
# 本套件逐字节比较 stdout、归一化后的 stderr、以及进程退出码。
#
# 归一化规则与主套件完全一致：只剥 `<shell名>: ` 前缀（及紧跟的 `line N: `），
# 消息正文、行数、尾随换行全部保留。
set -u

BASH_BIN="${BASH_BIN:-/d/msys2/usr/bin/bash.exe}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -z "${WASH_BIN:-}" ]; then
    if [ -x "$REPO_ROOT/target/debug/wash.exe" ]; then
        WASH_BIN="$REPO_ROOT/target/debug/wash.exe"
    else
        WASH_BIN="$REPO_ROOT/target/debug/wash"
    fi
fi

export LC_ALL=C
export LANG=C

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# HOME 沙箱化：登录 shell 会读 ~/.bash_profile 等，绝不能碰真实家目录。
export HOME="$TMP/home"
mkdir -p "$HOME"
SANDBOX="$TMP/sandbox"
mkdir -p "$SANDBOX"

printf 'echo "0=$0 1=$1 2=$2 n=$#"\n'  > "$SANDBOX/args.sh"
printf 'exit 7\n'                        > "$SANDBOX/exit7.sh"
printf 'echo FROM_SCRIPT\n'              > "$SANDBOX/plain.sh"
printf '}\n'                             > "$SANDBOX/bad.sh"
printf 'trap "echo TRAP" EXIT\necho BODY\n' > "$SANDBOX/trap.sh"
printf 'echo "1=$1 2=$2 n=$#"\nfalse\n'  > "$SANDBOX/status.sh"
printf 'echo "$-"\n'                      > "$SANDBOX/dashflags.sh"

RUNNER=()
if command -v timeout >/dev/null 2>&1; then
    RUNNER=(timeout 10)
fi

normalize_stderr() {
    sed -E 's#^[^:]*bash(\.exe)?: (line [0-9]+: )?##; s#^wash(\.exe)?: ##' "$1"
}

# MSYS2 没有 cmp，用 hexdump 比较。
hexdump() { od -An -v -tx1 "$1"; }

pass=0
fail=0

run_cli() {
    local label="$1" rb rw
    shift
    "${RUNNER[@]}" "$BASH_BIN" "$@" >"$TMP/b.out" 2>"$TMP/b.err" </dev/null
    rb=$?
    "${RUNNER[@]}" "$WASH_BIN" "$@" >"$TMP/w.out" 2>"$TMP/w.err" </dev/null
    rw=$?
    normalize_stderr "$TMP/b.err" > "$TMP/b.err.n"
    normalize_stderr "$TMP/w.err" > "$TMP/w.err.n"

    if [ "$(hexdump "$TMP/b.out")" = "$(hexdump "$TMP/w.out")" ] &&
       [ "$rb" = "$rw" ] &&
       [ "$(hexdump "$TMP/b.err.n")" = "$(hexdump "$TMP/w.err.n")" ]; then
        pass=$((pass + 1))
        printf 'PASS [%s]\n' "$label"
    else
        fail=$((fail + 1))
        printf 'FAIL [%s]  rc=%s/%s\n' "$label" "$rb" "$rw"
        printf '  bash.out:\n';    sed 's/^/    |/' "$TMP/b.out"
        printf '  wash.out:\n';    sed 's/^/    |/' "$TMP/w.out"
        printf '  bash.err.n:\n';  sed 's/^/    |/' "$TMP/b.err.n"
        printf '  wash.err.n:\n';  sed 's/^/    |/' "$TMP/w.err.n"
    fi
}

# 变体：从 stdin 喂入脚本内容，用于 `printf ... | bash -s` 这类 argv 形态。
# 逐字节比较 stdout、归一化 stderr、rc，判定与 run_cli 完全一致。
run_cli_stdin() {
    local label="$1" input="$2" rb rw
    shift 2
    printf '%s\n' "$input" | "${RUNNER[@]}" "$BASH_BIN" "$@" >"$TMP/b.out" 2>"$TMP/b.err"
    rb=$?
    printf '%s\n' "$input" | "${RUNNER[@]}" "$WASH_BIN" "$@" >"$TMP/w.out" 2>"$TMP/w.err"
    rw=$?
    normalize_stderr "$TMP/b.err" > "$TMP/b.err.n"
    normalize_stderr "$TMP/w.err" > "$TMP/w.err.n"

    if [ "$(hexdump "$TMP/b.out")" = "$(hexdump "$TMP/w.out")" ] &&
       [ "$rb" = "$rw" ] &&
       [ "$(hexdump "$TMP/b.err.n")" = "$(hexdump "$TMP/w.err.n")" ]; then
        pass=$((pass + 1))
        printf 'PASS [%s]\n' "$label"
    else
        fail=$((fail + 1))
        printf 'FAIL [%s]  rc=%s/%s\n' "$label" "$rb" "$rw"
        printf '  bash.out:\n';    sed 's/^/    |/' "$TMP/b.out"
        printf '  wash.out:\n';    sed 's/^/    |/' "$TMP/w.out"
        printf '  bash.err.n:\n';  sed 's/^/    |/' "$TMP/b.err.n"
        printf '  wash.err.n:\n';  sed 's/^/    |/' "$TMP/w.err.n"
    fi
}

# --- `-c` 之后的多余参数：$0 / $1 / $# / $@ / $* --------------------------
run_cli c-positional       -c 'echo "0=$0 1=$1 2=$2 n=$#"' NAME A B
run_cli c-positional-one   -c 'echo "0=$0 1=$1 n=$#"' NAME A
run_cli c-only-zero        -c 'echo "0=$0 n=$#"' NAME
run_cli c-at-star          -c 'echo "at=[$@] star=[$*] n=$#"' NAME A B
run_cli c-shift            -c 'shift; echo "1=$1 2=$2 n=$#"' NAME A B C
run_cli c-set-positional   -c 'set X Y; echo "1=$1 2=$2 n=$#"' NAME A
run_cli lc-positional      -lc 'echo "0=$0 1=$1 n=$#"' NAME A

# --- 脚本文件：$0 是脚本路径，参数按序传入 ---------------------------------
run_cli script-args        "$SANDBOX/args.sh" A B
run_cli script-noargs      "$SANDBOX/args.sh"
run_cli script-plain       "$SANDBOX/plain.sh"
run_cli script-exit7       "$SANDBOX/exit7.sh"
run_cli script-status      "$SANDBOX/status.sh" A B
run_cli script-trap        "$SANDBOX/trap.sh"
run_cli script-syntax      "$SANDBOX/bad.sh"
run_cli script-missing     "$SANDBOX/nope.sh" A
run_cli script-args-empty  "$SANDBOX/args.sh" "" "B B"

# --- 特殊参数 `$-` 的 argv 形态（MSYS2 bash 5.3 实测，LC_ALL=C）------------
#   -lc 'echo "$-"'        -> hBc      (login 不影响 $-)
#   -sc 'echo "$-"'        -> hBcs     (-s 与 -c 同置：c + s)
#   printf .. | bash -s    -> hBs      (从 stdin 读脚本，非交互无 i/H)
#   printf .. | bash       -> hBs      (无 -s 但无 -c/脚本，同样读 stdin)
#   bash script.sh         -> hB       (脚本文件：无 c 无 s)
# 以下 -ic / -isc 形态经实测未纳入逐字节闸门，如实说明原因：
#   -ic  'echo "$-"'       -> bash stdout=hiBHc, wash stdout=hiBHc（一致）
#   -isc 'echo "$-"'       -> bash stdout=hiBHcs, wash stdout=hiBHcs（一致）
# 但 bash 在非 tty 交互启动时会向 stderr 打印两条作业控制诊断，其中
# `cannot set terminal process group (N)` 的 N 是当前进程组 id，随父进程 /
# timeout 上下文变化（实测 -1 / 659 / 1578 / 1600 等），是进程绑定的动态值，
# wash 无法逐字节复现。该 stderr 差异无法靠对齐消除，故不把这两条放进闸门，
# 也不伪造诊断、不放宽判定；stdout 侧 `$-` 语义已正确实现。
run_cli        df-dash-lc      -lc 'echo "$-"'
run_cli        df-dash-sc      -sc 'echo "$-"'
run_cli_stdin  df-dash-s       'echo "$-"' -s
run_cli_stdin  df-dash-piped   'echo "$-"'
run_cli        df-dash-script  "$SANDBOX/dashflags.sh"

# --- wash 扩展自洽性（不伪装成 bash 兼容项）--------------------------------
# `--command` 是 wash 的便利扩展，不是 GNU bash 5.3 的选项。MSYS2 oracle 实测：
#   bash --command 'echo hi' NAME A  ->  rc=2
#       `--command: invalid option` + GNU long options 全表，其中 Usage 行含
#       bash 自身绝对路径，属身份绑定，天然无法逐字节对齐。
# 因此这里不做双侧对拍，只断言：wash 的 `--command` 与 `-c` 在相同 argv 下
# 产生完全相同的 stdout / stderr / rc。是否要严格移除该扩展（连带改 --help
# 与对应 e2e 断言）由项目决策，不在本套件里假装它兼容。
wash_only_equivalent() {
    local label="$1" cmd="$2" rx ry
    shift 2
    "${RUNNER[@]}" "$WASH_BIN" --command "$cmd" "$@" >"$TMP/x.out" 2>"$TMP/x.err" </dev/null
    rx=$?
    "${RUNNER[@]}" "$WASH_BIN" -c "$cmd" "$@" >"$TMP/y.out" 2>"$TMP/y.err" </dev/null
    ry=$?
    if [ "$(hexdump "$TMP/x.out")" = "$(hexdump "$TMP/y.out")" ] &&
       [ "$(hexdump "$TMP/x.err")" = "$(hexdump "$TMP/y.err")" ] &&
       [ "$rx" = "$ry" ]; then
        pass=$((pass + 1))
        printf 'PASS [%s] (wash extension self-consistency)\n' "$label"
    else
        fail=$((fail + 1))
        printf 'FAIL [%s] rc=%s/%s\n' "$label" "$rx" "$ry"
    fi
}
wash_only_equivalent command-long 'echo "0=$0 1=$1 n=$#"' NAME A

# 增删用例必须同步改此常量；这是防 PASS=0/空跑假绿的硬闸，不许改成运行时自算。
CLI_EXPECTED=22

printf '=================================================================\n'
printf 'CLI: PASS=%s FAIL=%s TOTAL=%s\n' "$pass" "$fail" "$((pass + fail))"
if [ "$pass" -ne "$CLI_EXPECTED" ]; then
    printf 'FATAL: case count drifted: expected %s got %s\n' "$CLI_EXPECTED" "$pass" >&2
    exit 1
fi
if [ "$fail" -eq 0 ]; then
    printf 'ALL CLI CASES MATCH BASH\n'
else
    exit 1
fi
