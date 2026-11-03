#!/usr/bin/env bash
# Byte-for-byte comparison of wash's `echo` / `printf` builtins against GNU
# bash 5.3 (MSYS2). For every test case we run BOTH bash and wash with the same
# `-c` command and compare the captured STDOUT bytes (via `cmp`) AND the exit
# code. A case passes only when both match exactly.
#
# Run with:  MSYSTEM=MSYS /e/.../bash.exe -lc 'cd /d/project/WorkBuddy/wash && bash tests/bash_diff_echo_printf.sh'
set -u

BASH_BIN="${BASH_BIN:-/d/msys2/usr/bin/bash.exe}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASH_BIN="${WASH_BIN:-$REPO_ROOT/target/debug/wash}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

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

run_case() {
    local label="$1"; shift
    local case="$1"
    # Reference: real bash.
    timeout_wrap "$BASH_BIN" -c "$case" >"$TMP/bash.out" 2>/dev/null
    local rb=$?
    # Candidate: wash.
    timeout_wrap "$WASH_BIN" -c "$case" >"$TMP/wash.out" 2>/dev/null
    local rw=$?
    # `cmp` is not always available in the MSYS2 environment, so compare the
    # normalized `od` byte dumps instead — identical bytes yield identical text.
    local ob ox
    ob="$(od -An -v -tx1 "$TMP/bash.out")"
    ox="$(od -An -v -tx1 "$TMP/wash.out")"
    if [ "$ob" = "$ox" ] && [ "$rb" = "$rw" ]; then
        pass=$((pass + 1))
    else
        fail=$((fail + 1))
        echo "FAIL [$label]: $case"
        echo "  bash rc=$rb  wash rc=$rw"
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

echo "================================================================="
echo "PASS=$pass FAIL=$fail"
if [ "$fail" -eq 0 ]; then
    echo "ALL CASES MATCH BASH"
    exit 0
else
    echo "SOME CASES DIFFER FROM BASH"
    exit 1
fi
