#!/usr/bin/env bash
# wash binary-plugin C ABI golden contract suite.
#
#   MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc \
#     'cd /d/project/WorkBuddy/wash && bash tests/plugin_contract.sh'
#
# On WSL (native Linux ELF wash):
#   env -u BASH_ENV -u WSLENV CONTRACT_TMP_BASE=/var/tmp \
#     WASH_BIN=/var/tmp/wash-build/target/debug/wash \
#     bash tests/plugin_contract.sh
#
# This is a REAL gate: every case asserts exit status + stdout bytes + stderr
# bytes (compared as hex, so trailing newlines count).  Any mismatch exits 1.
# MSYS2 bash 5.3.20 has "dynamic loading not available", so -f/-d are a
# controlled wash extension: the goldens below come from directly measuring
# wash (none of them contains a version string), and the static parts
# (enable -s, enable -p echo, type -t echo) were separately byte-compared
# against the primary oracle.
set -u
export LC_ALL=C LANG=C

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" || exit 1
cd "$REPO_ROOT" || exit 1

pass=0
fail=0
total=0

hard_fail() {
    printf 'FAIL %s\n' "$1"
    printf -- '-----------------------------------------\n'
    printf 'PASS=%d FAIL=%d TOTAL=%d\n' "$pass" "$((fail + 1))" "$((total + 1))"
    exit 1
}

# --- locate the wash binary -------------------------------------------------
if [ -n "${WASH_BIN:-}" ]; then
    WASH="$WASH_BIN"
elif [ -x "$REPO_ROOT/target/debug/wash.exe" ]; then
    WASH="$REPO_ROOT/target/debug/wash.exe"
elif [ -x "$REPO_ROOT/target/debug/wash" ]; then
    WASH="$REPO_ROOT/target/debug/wash"
elif [ -x "$REPO_ROOT/target/release/wash" ]; then
    WASH="$REPO_ROOT/target/release/wash"
else
    hard_fail 'no wash binary found (build first)'
fi
[ -x "$WASH" ] || hard_fail "wash binary not executable: $WASH"

# --- isolated sandbox (never touch the real HOME) ---------------------------
TMP="$(mktemp -d "${CONTRACT_TMP_BASE:-/tmp}/wash-contract.XXXXXX")" || hard_fail 'mktemp -d failed'
[ -n "$TMP" ] && [ -d "$TMP" ] || hard_fail 'temporary directory is empty or missing'
# WSL's ext4 can flip to emergency_ro: mktemp may then succeed while every
# later write fails, leaving each captured output EMPTY -> fake PASS.  Probe
# writability independently before any assertion is allowed to run.
touch "$TMP/.writable_probe" 2>/dev/null || hard_fail 'temporary directory is not writable'
rm -f "$TMP/.writable_probe" || hard_fail 'cannot remove writable probe'
trap 'rm -rf "$TMP"' EXIT
export HOME="$TMP/home"
mkdir -p "$HOME" "$TMP/lib" "$TMP/outside" "$TMP/wbin" || hard_fail 'cannot create sandbox dirs'
cp "$WASH" "$TMP/wbin/wash" || hard_fail "cannot copy wash binary: $WASH"
W="$TMP/wbin/wash"
chmod +x "$W" 2>/dev/null || true
[ -x "$W" ] || hard_fail 'copied wash binary is not executable'

# Version hard gate: the expected string is generated from Cargo.toml + git
# tag/HEAD by tools/wash_version.sh -- no version number is hardcoded here.
# A stale or wrong binary must abort before a single assertion runs, otherwise
# the suite would silently test a different implementation.
[ -f "$REPO_ROOT/tools/wash_version.sh" ] || hard_fail 'version generator missing'
EXPECTED_VER="$(bash "$REPO_ROOT/tools/wash_version.sh")" \
    || hard_fail 'version generator failed (git metadata missing?)'
[ -n "$EXPECTED_VER" ] || hard_fail 'empty generated version'
WASH_VER="$("$W" --version 2>&1)"
[ "$WASH_VER" = "wash $EXPECTED_VER" ] \
    || hard_fail "binary version mismatch: expected [wash $EXPECTED_VER] got [$WASH_VER]"
echo "version gate: wash $EXPECTED_VER"

# --- compile the three wash-ABI fixtures ------------------------------------
command -v gcc >/dev/null 2>&1 || hard_fail 'gcc not found'
for f in plugin_hello plugin_badabi plugin_conflict; do
    if ! gcc -fPIC -shared -I"$REPO_ROOT/include" \
            -o "$TMP/lib/$f.so" "$REPO_ROOT/tests/fixtures/$f.c" \
            >"$TMP/cc.out" 2>"$TMP/cc.err"; then
        cat "$TMP/cc.err" >&2
        hard_fail "cannot compile tests/fixtures/$f.c"
    fi
    [ -s "$TMP/lib/$f.so" ] || hard_fail "compiled $f.so is empty"
done

SO="$TMP/lib/plugin_hello.so"
BAD="$TMP/lib/plugin_badabi.so"
CON="$TMP/lib/plugin_conflict.so"
LNK="$TMP/lib/link_hello.so"
OUT="$TMP/outside/plugin_hello.so"
WL="$TMP/lib"
cp "$SO" "$OUT" || hard_fail 'cannot stage the out-of-whitelist copy'

# Default `ln -s` degrades to a FILE COPY on this MSYS2 box (lstat sees a
# regular file and the host happily loads it -> fake PASS).  winsymlinks:native
# produces a real symlink; on Linux it is a harmless no-op env var.
MSYS=winsymlinks:native ln -s "$SO" "$LNK" 2>"$TMP/ln.err" || true

# --- harness ----------------------------------------------------------------
# hex_of: exact bytes of a string, including trailing newlines.
hex_of() { printf '%s' "$1" | od -An -tx1 | tr -d ' \n'; }

run_wash() { # whitelist, wash -c script -> RC / *_TXT / *_HEX
    env BASH_LOADABLES_PATH="$1" LC_ALL=C LANG=C HOME="$HOME" \
        "$W" -c "$2" >"$TMP/out" 2>"$TMP/err"
    RC=$?
    OUT_TXT="$(cat "$TMP/out")"
    ERR_TXT="$(cat "$TMP/err")"
    OUT_HEX="$(od -An -tx1 "$TMP/out" | tr -d ' \n')"
    ERR_HEX="$(od -An -tx1 "$TMP/err" | tr -d ' \n')"
}

check() { # label, expected_rc, expected_stdout, expected_stderr
    local label="$1" erc="$2" eo ee
    eo="$(hex_of "$3")"
    ee="$(hex_of "$4")"
    total=$((total + 1))
    if [ "$erc" = "$RC" ] && [ "$eo" = "$OUT_HEX" ] && [ "$ee" = "$ERR_HEX" ]; then
        printf 'PASS %s\n' "$label"
        pass=$((pass + 1))
    else
        printf 'FAIL %s\n' "$label"
        printf '  expected rc=%s stdout_hex=%s stderr_hex=%s\n' "$erc" "$eo" "$ee"
        printf '  actual   rc=%s stdout_hex=%s stderr_hex=%s\n' "$RC" "$OUT_HEX" "$ERR_HEX"
        printf '  actual   stdout=[%s] stderr=[%s]\n' "$OUT_TXT" "$ERR_TXT"
        fail=$((fail + 1))
    fi
}

env_case() { # label, exit-status-of-precondition
    total=$((total + 1))
    if [ "$2" = 0 ]; then
        printf 'PASS %s\n' "$1"; pass=$((pass + 1))
    else
        printf 'FAIL %s\n' "$1"; fail=$((fail + 1))
    fi
}

# Precondition: the symlink fixture must really be a symlink, otherwise the
# symlink rejection case below would silently become a fake PASS.
[ -L "$LNK" ]
env_case 'symlink_fixture_is_real' "$?"

GREET=$'HELLO_FROM_PLUGIN\n'
# $(...) strips trailing newlines; append one explicitly so goldens stay byte-exact.
EXP_4="$(printf 'enable %s\n' plecho plget plhello plset)"$'\n'
EXP_S="$(printf 'enable %s\n' . : break continue eval exec exit export readonly return set shift source times trap unset)"$'\n'
EXP_HELP=$'plhello: plhello -- print from plugin\n    print a greeting (optional RC arg)\n\n'
ERR_OUTSIDE="wash: enable: cannot load $OUT: $OUT: outside BASH_LOADABLES_PATH whitelist"$'\n'
ERR_EMPTY="wash: enable: cannot load $SO: BASH_LOADABLES_PATH has no usable absolute directory"$'\n'
ERR_REL=$'wash: enable: cannot load plugin_hello.so: BASH_LOADABLES_PATH has no usable absolute directory\n'
ERR_LNK="wash: enable: cannot load $LNK: $LNK: symbolic links are not allowed"$'\n'
ERR_BAD="wash: enable: cannot load $BAD: $BAD: ABI version mismatch (plugin 999, host 1)"$'\n'
ERR_CON="wash: enable: cannot load $CON: $CON: builtin \`echo' conflicts with a shell builtin"$'\n'

# --- loading / registration -------------------------------------------------
run_wash "$WL" 'type -t plhello'
check 'unloaded_type_t' 1 '' ''

run_wash "$WL" 'enable -f plugin_hello.so plhello'
check 'load_by_bare_name_in_whitelist' 0 '' ''

run_wash "$WL" "enable -f $SO x; enable -a | grep '^enable pl'"
check 'one_f_registers_four_builtins' 0 "$EXP_4" ''

run_wash "$WL" "enable -f $SO x; plhello"
check 'plhello_stdout' 0 "$GREET" ''

run_wash "$WL" "enable -f $SO x; plhello 7; echo RC=\$?"
check 'plhello_exit_code_7' 0 "$GREET"$'RC=7\n' ''

run_wash "$WL" "enable -f $SO x; plhello 42; echo RC=\$?"
check 'plhello_exit_code_42' 0 "$GREET"$'RC=42\n' ''

run_wash "$WL" "enable -f $SO x; plset PLV VALUE_FROM_PLUGIN; plget PLV; echo SHELLVAR=\$PLV"
check 'plugin_variable_read_write' 0 $'VALUE_FROM_PLUGIN\nSHELLVAR=VALUE_FROM_PLUGIN\n' ''

run_wash "$WL" "enable -f $SO x; plget NO_SUCH_VAR"
check 'plget_unset_marker' 0 $'PLGET_UNSET:NO_SUCH_VAR\n' ''

run_wash "$WL" "enable -f $SO x; plecho STDERR_TEXT"
check 'plecho_writes_stderr' 0 '' $'STDERR_TEXT\n'

# --- builtin-introspection linkage ------------------------------------------
run_wash "$WL" "enable -f $SO x; type -t plhello"
check 'type_t_lists_dynamic' 0 $'builtin\n' ''

run_wash "$WL" "enable -f $SO x; compgen -b | grep -x plhello"
check 'compgen_b_lists_dynamic' 0 $'plhello\n' ''

run_wash "$WL" "enable -f $SO x; help plhello"
check 'help_dynamic' 0 "$EXP_HELP" ''

run_wash "$WL" "enable -f $SO x; enable -p plhello | grep -x 'enable plhello'"
check 'enable_p_single_line' 0 $'enable plhello\n' ''

run_wash "$WL" "enable -f $SO x; enable -s"
check 'enable_s_exactly_16_special' 0 "$EXP_S" ''

run_wash "$WL" "enable -f $SO x; enable -s | grep -c '^enable pl'; echo C=\$?"
check 'enable_s_has_no_dynamic' 0 $'0\nC=1\n' ''

run_wash "$WL" "enable -f $SO x; enable -d"
check 'enable_d_lists_dynamic' 0 "$EXP_4" ''

# --- hot unload --------------------------------------------------------------
run_wash "$WL" "enable -f $SO x; enable -d plhello"
check 'unload_one_rc' 0 '' ''

run_wash "$WL" "enable -f $SO x; enable -d plhello; plset A B; plget A; plecho STILL"
check 'unload_one_others_still_callable' 0 $'B\n' $'STILL\n' ''

run_wash "$WL" "enable -f $SO x; enable -d plhello; type -t plhello; echo T=\$?"
check 'unloaded_name_disappears' 0 $'T=1\n' ''

run_wash "$WL" "enable -f $SO x; enable -d plhello plset; enable -d"
check 'partial_unload_lists_remaining' 0 $'enable plecho\nenable plget\n' ''

run_wash "$WL" "enable -f $SO x; enable -d plhello plset plget plecho; enable -a | grep '^enable pl'; echo G=\$?"
check 'unload_all_clears_registry' 0 $'G=1\n' ''

run_wash "$WL" "enable -f $SO x; enable -d plhello plset plget plecho; enable -f $SO x; plhello"
check 'reload_succeeds_after_last_unload' 0 "$GREET" ''

run_wash "$WL" 'enable -d nosuch'
check 'enable_d_nosuch_rc1' 1 '' $'wash: enable: nosuch: not a shell builtin\n'

# --- whitelist ---------------------------------------------------------------
run_wash "$WL" "enable -f $OUT plhello"
check 'wl_outside_path_rejected' 1 '' "$ERR_OUTSIDE"

run_wash '.' "enable -f $SO plhello"
check 'wl_dot_gives_no_usable_dir' 1 '' "$ERR_EMPTY"

run_wash 'relative_only' 'enable -f plugin_hello.so plhello'
check 'wl_relative_segment_dropped' 1 '' "$ERR_REL"

run_wash "relative_only:$WL" "enable -f $OUT plhello"
check 'wl_relative_plus_absolute_still_rejects' 1 '' "$ERR_OUTSIDE"

run_wash "$WL" "enable -f $LNK plhello"
check 'wl_symlink_rejected' 1 '' "$ERR_LNK"

run_wash "$WL" "enable -f $OUT plhello >/dev/null 2>&1; type -t plhello; echo T=\$?"
check 'wl_rejection_leaves_no_half_registration' 0 $'T=1\n' ''

# --- ABI / name-conflict rejection -------------------------------------------
run_wash "$WL" "enable -f $BAD plbadabi"
check 'abi_version_mismatch_rejected' 1 '' "$ERR_BAD"

run_wash "$WL" "enable -f $BAD plbadabi >/dev/null 2>&1; compgen -b | grep -x plbadabi; echo G=\$?"
check 'abi_failure_leaves_no_half_registration' 0 $'G=1\n' ''

run_wash "$WL" "enable -f $CON echo"
check 'static_builtin_name_conflict_rejected' 1 '' "$ERR_CON"

run_wash "$WL" "enable -f $CON echo >/dev/null 2>&1; echo PROBE; type -t echo"
check 'static_echo_untouched' 0 $'PROBE\nbuiltin\n' ''

run_wash "$WL" "enable -f $CON echo >/dev/null 2>&1; compgen -b | grep -x echo; echo E=\$?"
check 'conflict_failure_leaves_no_half_registration' 0 $'echo\nE=0\n' ''

# --- verdict -----------------------------------------------------------------
# 增删用例必须同步改此常量；这是防 PASS=0/空跑假绿的硬闸，不许改成运行时自算。
PLUGIN_CONTRACT_EXPECTED=35

printf -- '-----------------------------------------\n'
printf 'PASS=%d FAIL=%d TOTAL=%d\n' "$pass" "$fail" "$total"
if [ "$pass" -ne "$PLUGIN_CONTRACT_EXPECTED" ]; then
    echo "FATAL: case count drifted: expected $PLUGIN_CONTRACT_EXPECTED got $pass" >&2
    exit 1
fi
if [ "$fail" -ne 0 ] || [ "$pass" -ne "$total" ]; then
    echo 'PLUGIN CONTRACTS FAILED'
    exit 1
fi
echo 'ALL PLUGIN CONTRACTS MATCH'
