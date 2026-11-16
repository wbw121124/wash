#!/usr/bin/env bash
# P0 regression fence (review.md): arith ternary, arith deep-recursion guard,
# repl cursor_pos.  Byte-for-byte comparison against the MSYS2 bash oracle for
# every case with a defined bash behaviour; deep-recursion cases only assert
# that wash itself does not crash (bash SIGSEGVs at those depths on MSYS2, so
# there is no oracle output to compare against -- qa9_thresholds.txt).
#
# Run with:
#   MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd <repo> && bash tests/bash_diff_p0.sh'
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

# Startup hard gate: --version must succeed with non-empty stdout.
if ! "$WASH_BIN" --version > /dev/null 2>&1; then
    echo "FATAL: $WASH_BIN --version failed" >&2
    exit 1
fi

# Hard case-count gate: an empty run must be FATAL, never a silent PASS.
EXPECTED_CASES=21
TMP="$(mktemp -d)"
trap 'rm -f "$TMP"/*.out "$TMP"/*.err "$TMP"/*.err.n 2>/dev/null; rmdir "$TMP" 2>/dev/null' EXIT

pass=0
fail=0
run=0

# Byte-for-byte normalisation: strip only the shell-name prefix (plus an
# optional "line N:"), exactly like tests/bash_diff_builtins.sh does.
normalize_stderr() {
    sed -E 's#^[^:]*bash(\.exe)?: (line [0-9]+: )?##; s#^wash(\.exe)?: (line [0-9]+: )?##' "$1"
}

hexdump() { od -An -tx1 -v < "$1" | tr -d ' \n'; }

# run_cmp <id> <case>  -- strict bash<->wash diff (rc + stdout + stderr body)
run_cmp() {
    local id="$1" case_text="$2"
    run=$((run + 1))
    "$BASH_BIN" -c "$case_text" > "$TMP/b.out" 2> "$TMP/b.err"
    local rb=$?
    "$WASH_BIN" -c "$case_text" > "$TMP/w.out" 2> "$TMP/w.err"
    local rw=$?
    normalize_stderr "$TMP/b.err" > "$TMP/b.err.n"
    normalize_stderr "$TMP/w.err" > "$TMP/w.err.n"
    if [ "$(hexdump "$TMP/b.out")" = "$(hexdump "$TMP/w.out")" ] &&
       [ "$rb" = "$rw" ] &&
       [ "$(hexdump "$TMP/b.err.n")" = "$(hexdump "$TMP/w.err.n")" ]; then
        pass=$((pass + 1))
        echo "PASS $id"
    else
        fail=$((fail + 1))
        echo "FAIL $id: $case_text"
        echo "  bash rc=$rb out=[$(cat "$TMP/b.out")] err=[$(cat "$TMP/b.err.n")]"
        echo "  wash rc=$rw out=[$(cat "$TMP/w.out")] err=[$(cat "$TMP/w.err.n")]"
    fi
}

# run_nocrash <id> <case>  -- wash must not crash (no rc 139 / 138 / 136) and
# must return promptly; no byte comparison (bash itself SIGSEGVs here).
run_nocrash() {
    local id="$1" case_text="$2"
    run=$((run + 1))
    "$WASH_BIN" -c "$case_text" > "$TMP/w.out" 2> "$TMP/w.err"
    local rw=$?
    if [ "$rw" = 139 ] || [ "$rw" = 138 ] || [ "$rw" = 136 ]; then
        fail=$((fail + 1))
        echo "FAIL $id: wash crashed rc=$rw"
    elif [ "$rw" = 124 ]; then
        fail=$((fail + 1))
        echo "FAIL $id: wash timed out"
    else
        pass=$((pass + 1))
        echo "PASS $id (wash rc=$rw)"
    fi
}

# ---------------------------------------------------------------------------
# P0-1 arith ternary.  Byte-for-byte oracle (qa9 B1, rc/stdout/stderr).
# Legal: right-associative; illegal: rc=1 with bash's exact wording.
# ---------------------------------------------------------------------------
run_cmp ternary-true          'echo $((1?2:3))'
run_cmp ternary-false         'echo $((0?2:3))'
run_cmp ternary-var           'x=5; echo $((x?1:2))'
run_cmp ternary-nested        'echo $((1?2?3:4:5))'
run_cmp ternary-nested2       'echo $((0?1:0?2:3))'
run_cmp ternary-in-expr       'echo $((10 + (1?2:3)))'
run_cmp ternary-missing-colon '$((1?2))'
run_cmp ternary-leading-colon '$((1:2))'
run_cmp ternary-missing-else  '$((1?2:))'
run_cmp ternary-bare-qmark    '$((?))'

# ---------------------------------------------------------------------------
# P0-2 arith deep recursion.  bash SIGSEGVs (rc=-11, qa9 B2/B3); the oracle
# for these is "wash must not crash and must answer deterministically".
# Legal-depth controls still compare byte-for-byte against bash.
# ---------------------------------------------------------------------------
run_cmp deep-control-small    'echo $((!!!!1))'
run_cmp deep-control-paren    'echo $(((((1)))))'
run_cmp deep-control-pow      'echo $((2**2**2))'

run_nocrash deep-bang-1000    '$((!!!!!(1)))'
for n in 5000 20000 50000; do
    bang="$(python3 -c "print('!'*$n)")"
    run_nocrash "deep-bang-$n"    "\$((\${bang}1))"
done
for n in 300 500 2000; do
    p="$(python3 -c "print('('*$n)")"
    q="$(python3 -c "print(')'*$n)")"
    run_nocrash "deep-paren-$n"   "\$((\${p}1\${q}))"
done
# ** chains recurse through parse_power's right-hand side; 300 levels exceed
# the NestGuard cap, so wash must fail deterministically (rc=1), not crash.
pow="$(python3 -c "print('2' + '**2'*300)")"
run_nocrash deep-pow-chain     "\$((\${pow}))"

# ---------------------------------------------------------------------------
# P0-3 repl cursor_pos: unit test lives in src/repl.rs
# (cursor_pos_multibyte_multiline); exercised via `cargo test` by the runner,
# not here -- this file only gates non-interactive byte compatibility.
# ---------------------------------------------------------------------------

# Hard gate: the run must have covered every registered case.
if [ "$run" -ne "$EXPECTED_CASES" ]; then
    echo "FATAL: ran $run cases but expected $EXPECTED_CASES (empty/partial run)" >&2
    exit 1
fi

echo "PASS=$pass FAIL=$fail TOTAL=$run"
if [ "$fail" -ne 0 ]; then
    exit 1
fi
exit 0
