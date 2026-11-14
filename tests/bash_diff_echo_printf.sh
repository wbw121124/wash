#!/usr/bin/env bash
# Byte-for-byte comparison of wash's `echo` / `printf` builtins against GNU
# bash 5.3 (MSYS2). For every test case we run BOTH bash and wash with the same
# `-c` command and compare the captured STDOUT bytes AND the exit code.
# A case passes only when both match exactly.
#
# Run with:
#   MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
#     'cd <WASH_REPO> && bash tests/bash_diff_echo_printf.sh'
set -u

BASH_BIN="${BASH_BIN:-/d/msys2/usr/bin/bash.exe}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASH_BIN="${WASH_BIN:-$REPO_ROOT/target/debug/wash}"

# 工具缺失必须 FATAL：MSYS2 没有 cmp/diff，字节比较依赖 od，输入真实性依赖
# sha256sum；timeout/mktemp 缺失会让两侧输出被静默丢弃而退化成「空 == 空」
# 的假 PASS。任一缺失即显式 FATAL，绝不静默降级。
for _t in od timeout mktemp sha256sum; do
    command -v "$_t" >/dev/null 2>&1 || {
        echo "FATAL: required tool '$_t' is missing (byte comparison would be silently skipped)" >&2
        exit 1
    }
done

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# 启动前置硬闸（在任何用例执行之前）
#
# PE 加载期 / DLL 解析失败会让 wash 以 rc=127、stdout/stderr 全空退出（QA
# #10/#11）：此时若不拦住，100 条用例会各自被下游 gate 判成「输入不可信」，
# 而不是明确报告「二进制从未启动」。要求 --version 成功且 banner 非空后才
# 进入套件。stdout 与 stderr 分开捕获；不用 `|| true`、不用 `2>/dev/null`
# 吞 rc 或输出。此硬闸不替代下方逐例的字节对拍判定。
# ---------------------------------------------------------------------------
"$WASH_BIN" --version > "$TMP/ver.out" 2> "$TMP/ver.err"; ver_rc=$?
if [ "$ver_rc" -ne 0 ] || [ ! -s "$TMP/ver.out" ]; then
    echo "FATAL: binary failed to start (PE loader / DLL resolution): --version rc=$ver_rc" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# HOME ISOLATION (mandatory)
#
# A few cases read `$HOME` (and `~` expands through it).  Pointing HOME at a
# throw-away directory inside the sandbox guarantees that a test run can
# never read or write anything in the REAL user's home directory, no matter
# which directory the suite is started from.  Both sides of the comparison
# (bash and wash) see the same HOME, so the byte-diff stays valid.
# ---------------------------------------------------------------------------
export HOME="$TMP/home"
mkdir -p "$HOME"

pass=0
fail=0
timeout_wrap() { timeout 10 "$@"; }

# 空文件的 sha256：用来区分「被测 stdout 合法为空」与「od 静默失败产生空串」。
EMPTY_SHA='e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'

run_case() {
    local label="$1"; shift
    local case="$1"
    # Reference: real bash.
    timeout_wrap "$BASH_BIN" -c "$case" >"$TMP/bash.out" 2>/dev/null
    local rb=$?
    # Candidate: wash.
    timeout_wrap "$WASH_BIN" -c "$case" >"$TMP/wash.out" 2>/dev/null
    local rw=$?
    # MSYS2 没有 cmp/diff，字节比较沿用 `od -An -v -tx1` 的文本相等；
    # sha256sum 只作为「比较输入真实性」的硬闸，不替代既有判定。
    local ob ox ob_rc ox_rc hb hw hb_rc hw_rc
    ob="$(od -An -v -tx1 "$TMP/bash.out")"; ob_rc=$?
    ox="$(od -An -v -tx1 "$TMP/wash.out")"; ox_rc=$?
    hb="$(sha256sum "$TMP/bash.out")"; hb_rc=$?; hb="${hb%% *}"
    hw="$(sha256sum "$TMP/wash.out")"; hw_rc=$?; hw="${hw%% *}"

    # 判定输入硬闸：任一「文件不存在/不可读、od 或 sha256sum 失败、digest 非
    # 64 位十六进制、文件非空但 od dump 为空、rc 为 126/127（二进制没起来）」
    # 一律 FAIL。杜绝 `ob=ox=""` 退化成「空 == 空」的假 PASS（P1-2 同类空洞）。
    # 合法空 stdout（digest == EMPTY_SHA）仍按既有口径 PASS，判定语义不变。
    local gate_ok=1 gate_why=""
    [ -f "$TMP/bash.out" ] && [ -r "$TMP/bash.out" ] || { gate_ok=0; gate_why="bash.out missing/unreadable"; }
    [ -f "$TMP/wash.out" ] && [ -r "$TMP/wash.out" ] || { gate_ok=0; gate_why="${gate_why:+$gate_why; }wash.out missing/unreadable"; }
    [ "$ob_rc" -eq 0 ] && [ "$ox_rc" -eq 0 ] || { gate_ok=0; gate_why="${gate_why:+$gate_why; }od failed (rc bash=$ob_rc wash=$ox_rc)"; }
    [ "$hb_rc" -eq 0 ] && [ "$hw_rc" -eq 0 ] || { gate_ok=0; gate_why="${gate_why:+$gate_why; }sha256sum failed (rc bash=$hb_rc wash=$hw_rc)"; }
    case "$hb$hw" in
        *[!0-9a-f]*|'') gate_ok=0; gate_why="${gate_why:+$gate_why; }non-hex/empty digest" ;;
    esac
    [ "${#hb}" -eq 64 ] && [ "${#hw}" -eq 64 ] || { gate_ok=0; gate_why="${gate_why:+$gate_why; }short digest (bash=${#hb} wash=${#hw})"; }
    [ "$hb" != "$EMPTY_SHA" ] && [ -z "$ob" ] && { gate_ok=0; gate_why="${gate_why:+$gate_why; }bash stdout non-empty but od dump is empty"; }
    [ "$hw" != "$EMPTY_SHA" ] && [ -z "$ox" ] && { gate_ok=0; gate_why="${gate_why:+$gate_why; }wash stdout non-empty but od dump is empty"; }
    [ "$rb" -ne 126 ] && [ "$rb" -ne 127 ] && [ "$rw" -ne 126 ] && [ "$rw" -ne 127 ] || { gate_ok=0; gate_why="${gate_why:+$gate_why; }rc 126/127 (binary could not start)"; }

    if [ "$gate_ok" -eq 1 ] && [ "$ob" = "$ox" ] && [ "$rb" = "$rw" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "FAIL [$label]: $case"
        echo "  bash rc=$rb  wash rc=$rw"
        [ "$gate_ok" -eq 0 ] && echo "  input gate FAILED: $gate_why"
        if [ "$ob" != "$ox" ]; then
            echo "  --- bash stdout ---"; echo "$ob" | head -8
            echo "  --- wash stdout ---"; echo "$ox" | head -8
        fi
    fi
}

###############################################################################
# echo — default (no option processing)
###############################################################################
run_case "echo-plain"        'echo hello world'
run_case "echo-empty"        'echo'
run_case "echo-noexpand"     'echo $HOME'
run_case "echo-multiple"     'echo a   b   c'
run_case "echo-backslash"    'echo a\b'

###############################################################################
# echo — option parsing (leading only; -- is literal text in bash echo)
###############################################################################
run_case "echo-n"            'echo -n hello'
run_case "echo-n2"           'echo -nnn hello'
run_case "echo-ne"           'echo -ne "x\ny"'
run_case "echo-EN"           'echo -EN "x"'
run_case "echo-dashdash"     'echo -- -n'
run_case "echo-ndashdash"    'echo -n -- x'
run_case "echo-textfirst"    'echo hi -n'
run_case "echo-mixedopt"     'echo -n -e "a\tb"'

###############################################################################
# echo -e — escape interpretation
###############################################################################
run_case "echo-e-tab"        'echo -e "a\tb"'
run_case "echo-e-nl"         'echo -e "a\nb"'
run_case "echo-e-bell"       'echo -e "a\bb"'
run_case "echo-e-r"          'echo -e "a\rb"'
run_case "echo-e-f"          'echo -e "a\fb"'
run_case "echo-e-v"          'echo -e "a\vb"'
run_case "echo-e-backslash"  'echo -e "a\\b"'
run_case "echo-e-octal"      'echo -e "\077"'
run_case "echo-e-octal0"     'echo -e "\0"'
run_case "echo-e-octal0777"  'echo -e "\0777"'
run_case "echo-e-nooctal"    'echo -e "\177"'
run_case "echo-e-hex"        'echo -e "\x41"'
run_case "echo-e-hex1"       'echo -e "\x4"'
run_case "echo-e-hexbad"     'echo -e "\xZZ"'
run_case "echo-e-ucp"        'echo -e "\u0041"'
run_case "echo-e-Ucp"        'echo -e "\U00000041"'
run_case "echo-e-unknown"    'echo -e "\y"'
run_case "echo-e-c-stop"     'echo -e "a\cb"'
run_case "echo-e-c-alone"    'echo -e "\c"'
run_case "echo-e-c-nl"       'echo -e "a\c\nb"'
run_case "echo-e-combo"      'echo -e "\t\n\\"'

###############################################################################
# echo -E — escapes disabled (default)
###############################################################################
run_case "echo-E"            'echo -E "\t\n"'
run_case "echo-E-e"          'echo -E -e "\t"'

###############################################################################
# printf — basic
###############################################################################
run_case "printf-plain"      'printf "hello"'
run_case "printf-noconv"     'printf "no conversion"'
run_case "printf-nl"         'printf "%s\n" hi'
run_case "printf-pct"        'printf "100%%\n"'
run_case "printf-two"        'printf "%s %s\n" foo bar'
run_case "printf-recycle"    'printf "%s\n" a b c'
run_case "printf-missing"    'printf "%s %s\n" only-one'
run_case "printf-missings"   'printf "%s"'

###############################################################################
# printf — integer conversions (base 0)
###############################################################################
run_case "printf-d"          'printf "%d\n" 42'
run_case "printf-d-hex"      'printf "%d\n" 0x1f'
run_case "printf-d-oct"      'printf "%d\n" 077'
run_case "printf-i"          'printf "%i\n" -17'
run_case "printf-u"          'printf "%u\n" 42'
run_case "printf-u-neg"      'printf "%u\n" -5'
run_case "printf-o"          'printf "%o\n" 64'
run_case "printf-x"          'printf "%x\n" 255'
run_case "printf-X"          'printf "%X\n" 255'
run_case "printf-overflow"   'printf "%d\n" 99999999999999999999999'

###############################################################################
# printf — float conversions (long double via C shim)
###############################################################################
run_case "printf-f"          'printf "%f\n" 3.5'
run_case "printf-f20"        'printf "%.20f\n" 0.1'
run_case "printf-e"          'printf "%e\n" 100'
run_case "printf-E"          'printf "%E\n" 100'
run_case "printf-g"          'printf "%g\n" 0.00001234'
run_case "printf-G"          'printf "%G\n" 0.00001234'
run_case "printf-a"          'printf "%a\n" 1.5'
run_case "printf-A"          'printf "%A\n" 1.5'
run_case "printf-f-big"      'printf "%f\n" 1e400'
run_case "printf-f-neg"      'printf "%05.2f\n" -3.14159'
run_case "printf-f-wide"     'printf "%10.3f\n" 2.71828'

###############################################################################
# printf — string / char / byte
###############################################################################
run_case "printf-s"          'printf "[%s]\n" hello'
run_case "printf-c"          'printf "[%c]\n" A'
run_case "printf-c-empty"    'printf "[%c]\n" ""'
run_case "printf-c-multi"    'printf "%c%c\n" A B'
run_case "printf-b"          'printf "%b\n" "a\tb"'
run_case "printf-b-escaped"  'printf "%b\n" "line1\nline2"'
run_case "printf-width"      'printf "[%5s]\n" hi'
run_case "printf-ljust"      'printf "[%-5s]\n" hi'
run_case "printf-zero"       'printf "[%05d]\n" 7'
run_case "printf-prec"       'printf "[%.3s]\n" hello'
run_case "printf-wp"         'printf "[%8.3s]\n" hello'
run_case "printf-star"       'printf "[%*s]\n" 5 hi'
run_case "printf-star2"      'printf "[%*.*f]\n" 8 2 3.14159'

###############################################################################
# printf — %q quoting
###############################################################################
run_case "printf-q-space"    'printf "%q\n" "a b"'
run_case "printf-q-special"  'printf "%q\n" "a$b"'
run_case "printf-q-empty"    'printf "%q\n" ""'
run_case "printf-q-plain"    'printf "%q\n" hello'
run_case "printf-q-control"  'printf "%q\n" "$(printf "\001\002")"'

###############################################################################
# printf — error handling / exit codes
###############################################################################
run_case "printf-err-int"    'printf "%d\n" abc'
run_case "printf-err-fmt"    'printf "%d"'
run_case "printf-err-trail"  'printf "%"'
run_case "printf-err-badopt" 'printf -x hi'
run_case "printf-usage1"     'printf'
run_case "printf-usage2"     'printf -v x'
run_case "printf-usage3"     'printf -v'
run_case "printf-badident"   'printf -v 1x "%s" hi'

###############################################################################
# printf — option parsing (--, -v)
###############################################################################
run_case "printf-dashdash"   'printf -- "%s" hi'
run_case "printf-dashdash2"  'printf -v x -- "%s" hi; echo "$x"'

###############################################################################
# printf -v (parent-side assignment — must persist)
###############################################################################
run_case "printf-v-s"        'printf -v x "%s" hi; echo "$x"'
run_case "printf-v-b"        'printf -v x "%b" "a\tb"; echo "$x"'
run_case "printf-v-d"        'printf -v x "%d" 0x1f; echo "$x"'
run_case "printf-v-f"        'printf -v x "%.2f" 3.14159; echo "$x"'
run_case "printf-v-q"        'printf -v x "%q" "a b"; echo "$x"'
run_case "printf-v-attached" 'printf -vvar "%s" hi; echo "$var"'
run_case "printf-v-conv"     'printf -v x "%d" abc; echo "rc=$? x=$x"'

# 增删用例必须同步改此常量；这是防 PASS=0/空跑假绿的硬闸，不许改成运行时自算。
ECHO_PRINTF_EXPECTED=100

echo "================================================================="
echo "PASS=$pass FAIL=$fail TOTAL=$((pass + fail))"
if [ "$pass" -ne "$ECHO_PRINTF_EXPECTED" ]; then
    echo "FATAL: case count drifted: expected $ECHO_PRINTF_EXPECTED got $pass" >&2
    exit 1
fi

if [ "$fail" -eq 0 ]; then
    echo "ALL CASES MATCH BASH"
    exit 0
else
    echo "SOME CASES DIFFER FROM BASH"
    exit 1
fi
