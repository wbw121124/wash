#!/usr/bin/env bash
# WSL "second oracle": capability equivalence between GNU bash 5.3.0
# loadable builtins and wash 0.2.0 C ABI plugins.
#
#   MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc \
#     'cd /d/project/WorkBuddy/wash && bash tests/plugin_second_oracle.sh'
#   (the script re-executes itself inside WSL, where all assertions run)
#
# The two ABIs are DIFFERENT (bash's builtins.h vs wash's wash_plugin.h) and
# are intentionally NOT byte-compatible.  What is compared here is behaviour:
# the `enable -f` / `enable -d` / `enable -s` command surface, output,
# exit-code pass-through, variable read/write, four builtins from ONE
# `enable -f`, and per-name hot unload.  Each side is asserted against exact
# rc + stdout bytes + stderr bytes, and the two sides must also be identical.
#
# Where the two ABIs genuinely disagree (e.g. `enable -f` on a missing file:
# bash prefixes diagnostics with its own path + "line 1", wash reports
# "cannot load <file>: <file>: cannot stat"), pair_golden() gives EACH side
# its own exact stderr golden and still demands identical rc + stdout.  The
# divergence itself is asserted too, so stderr is never silently ignored.
# Any mismatch, missing tool or unusable WSL exits 1 -- nothing is skipped.
set -u
export LC_ALL=C LANG=C

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" || exit 1

pass=0
fail=0
total=0

hard_fail() {
    printf 'FAIL %s\n' "$1"
    printf -- '-----------------------------------------\n'
    printf 'PASS=%d FAIL=%d TOTAL=%d\n' "$pass" "$((fail + 1))" "$((total + 1))"
    exit 1
}

# --- re-exec inside real Linux WSL ------------------------------------------
if [ "$(uname -s)" != "Linux" ]; then
    WSL_REPO="$(printf '%s' "$REPO_ROOT" | sed 's|^/\([a-z]\)/|/mnt/\1/|')"
    printf 're-exec inside WSL: %s\n' "$WSL_REPO"
    wsl.exe -e bash -lc "cd '$WSL_REPO' && env -u BASH_ENV -u WSLENV bash tests/plugin_second_oracle.sh"
    rc=$?
    printf 'WSL oracle rc=%d\n' "$rc"
    if [ "$rc" -ne 0 ]; then
        printf 'FAIL WSL second oracle unavailable or failed (see real output above)\n'
        printf -- '-----------------------------------------\n'
        printf 'PASS=0 FAIL=1 TOTAL=1\n'
        echo 'SECOND ORACLE FAILED'
        exit 1
    fi
    exit 0
fi

# --- from here on we are inside WSL -----------------------------------------
BASH_ORACLE=/usr/local/bin/bash
[ -x "$BASH_ORACLE" ] || hard_fail "second oracle missing: $BASH_ORACLE"
ORACLE_VER="$($BASH_ORACLE --version | head -1)"
case "$ORACLE_VER" in
    *5.3.0*) ;;
    *) hard_fail "second oracle is not bash 5.3.0: [$ORACLE_VER]" ;;
esac

for d in /usr/local/include/bash /usr/local/include/bash/include /usr/local/include/bash/builtins; do
    [ -d "$d" ] || hard_fail "bash loadable header dir missing: $d"
done
command -v gcc >/dev/null 2>&1 || hard_fail 'gcc not found inside WSL'

# /tmp is reclaimed by this WSL instance; /var/tmp is not.  An ext4 emergency_ro
# would make mktemp fail and leave every captured output EMPTY (fake PASS), so
# verify the directory is really there and writable before doing anything else.
TMP="$(mktemp -d /var/tmp/wash-oracle.XXXXXX)" || hard_fail 'mktemp -d /var/tmp failed (check ext4 emergency_ro)'
[ -n "$TMP" ] && [ -d "$TMP" ] || hard_fail 'temporary directory is empty or missing'
touch "$TMP/.writable_probe" 2>/dev/null || hard_fail 'temporary directory is not writable'
rm -f "$TMP/.writable_probe"
trap 'rm -rf "$TMP"' EXIT
export HOME="$TMP/home"
mkdir -p "$HOME" || hard_fail 'cannot create sandboxed HOME'

# --- wash native Linux ELF binary -------------------------------------------
if [ -n "${WASH_BIN:-}" ]; then
    WASH_SRC="$WASH_BIN"
elif [ -x /var/tmp/wash-build/target/debug/wash ]; then
    WASH_SRC=/var/tmp/wash-build/target/debug/wash
elif [ -x "$REPO_ROOT/target/debug/wash" ]; then
    WASH_SRC="$REPO_ROOT/target/debug/wash"
else
    hard_fail 'no native Linux ELF wash binary (build in /var/tmp/wash-build first)'
fi
[ -x "$WASH_SRC" ] || hard_fail "wash binary not executable: $WASH_SRC"
cp "$WASH_SRC" "$TMP/wash" || hard_fail "cannot copy wash binary: $WASH_SRC"
W="$TMP/wash"
chmod +x "$W" || true
WASH_VER="$($W --version 2>&1)"
case "$WASH_VER" in
    *0.2.0*) ;;
    *) hard_fail "wash binary is not 0.2.0: [$WASH_VER]" ;;
esac

# --- compile BOTH sides' plugins from the SAME source tree -------------------
BSO="$TMP/bash_hello.so"
WSO="$TMP/plugin_hello.so"
gcc -fPIC -shared \
    -I/usr/local/include/bash \
    -I/usr/local/include/bash/include \
    -I/usr/local/include/bash/builtins \
    -o "$BSO" "$REPO_ROOT/tests/fixtures/bash_hello.c" \
    >"$TMP/cc_bash.out" 2>"$TMP/cc_bash.err" || {
        cat "$TMP/cc_bash.err" >&2
        hard_fail 'cannot compile tests/fixtures/bash_hello.c (bash loadable ABI)'
    }
[ -s "$BSO" ] || hard_fail 'compiled bash_hello.so is empty'

gcc -fPIC -shared -I"$REPO_ROOT/include" \
    -o "$WSO" "$REPO_ROOT/tests/fixtures/plugin_hello.c" \
    >"$TMP/cc_wash.out" 2>"$TMP/cc_wash.err" || {
        cat "$TMP/cc_wash.err" >&2
        hard_fail 'cannot compile tests/fixtures/plugin_hello.c (wash C ABI)'
    }
[ -s "$WSO" ] || hard_fail 'compiled plugin_hello.so is empty'

B_LOAD="enable -f $BSO hello hset hget hecho"
W_LOAD="enable -f $WSO x"

# --- harness ----------------------------------------------------------------
hex_of() { printf '%s' "$1" | od -An -tx1 | tr -d ' \n'; }

run_shell() { # bash|wash, script -> RC / OUT_HEX / ERR_HEX / *_TXT
    local cmd
    if [ "$1" = bash ]; then cmd="$BASH_ORACLE"; else cmd="$W"; fi
    env -u BASH_ENV -u WSLENV BASH_LOADABLES_PATH="$TMP" \
        LC_ALL=C LANG=C HOME="$HOME" \
        "$cmd" -c "$2" >"$TMP/out" 2>"$TMP/err"
    RC=$?
    OUT_TXT="$(cat "$TMP/out")"
    ERR_TXT="$(cat "$TMP/err")"
    OUT_HEX="$(od -An -tx1 "$TMP/out" | tr -d ' \n')"
    ERR_HEX="$(od -An -tx1 "$TMP/err" | tr -d ' \n')"
}

check_one() { # label, erc, eo_hex, ee_hex, arc, ao_hex, ae_hex, aout, aerr
    local label="$1"
    total=$((total + 1))
    if [ "$2" = "$5" ] && [ "$3" = "$6" ] && [ "$4" = "$7" ]; then
        printf 'PASS %s\n' "$label"
        pass=$((pass + 1))
    else
        printf 'FAIL %s\n' "$label"
        printf '  expected rc=%s stdout_hex=%s stderr_hex=%s\n' "$2" "$3" "$4"
        printf '  actual   rc=%s stdout_hex=%s stderr_hex=%s\n' "$5" "$6" "$7"
        printf '  actual   stdout=[%s] stderr=[%s]\n' "$8" "$9"
        fail=$((fail + 1))
    fi
}

pair() { # label, exp_rc, exp_stdout, exp_stderr, bash_script, wash_script
    local label="$1" erc="$2" eo ee bs="$5" ws="$6"
    local brc bo be bot bet wrc wo we wot wet
    eo="$(hex_of "$3")"
    ee="$(hex_of "$4")"

    run_shell bash "$bs"
    brc="$RC" bo="$OUT_HEX" be="$ERR_HEX" bot="$OUT_TXT" bet="$ERR_TXT"
    run_shell wash "$ws"
    wrc="$RC" wo="$OUT_HEX" we="$ERR_HEX" wot="$OUT_TXT" wet="$ERR_TXT"

    check_one "$label/bash" "$erc" "$eo" "$ee" "$brc" "$bo" "$be" "$bot" "$bet"
    check_one "$label/wash" "$erc" "$eo" "$ee" "$wrc" "$wo" "$we" "$wot" "$wet"

    # equivalence: the two ABI implementations must produce identical bytes.
    total=$((total + 1))
    if [ "$brc" = "$wrc" ] && [ "$bo" = "$wo" ] && [ "$be" = "$we" ]; then
        printf 'PASS %s/equivalent\n' "$label"
        pass=$((pass + 1))
    else
        printf 'FAIL %s/equivalent\n' "$label"
        printf '  bash rc=%s stdout_hex=%s stderr_hex=%s\n' "$brc" "$bo" "$be"
        printf '  wash rc=%s stdout_hex=%s stderr_hex=%s\n' "$wrc" "$wo" "$we"
        fail=$((fail + 1))
    fi
}

# Like pair(), but the two ABIs' stderr diagnostics are DIFFERENT.  Each side
# still gets its own byte-exact rc + stdout + stderr golden (check_one), so
# nothing is relaxed; only the "stderr must be equal" requirement is replaced
# by "stderr must really differ", plus identical rc and stdout.  This keeps
# the enable -f failure paths inside the oracle instead of skipping them.
pair_golden() { # label, rc, stdout, bash_stderr, wash_stderr, bash_script, wash_script
    local label="$1" erc="$2" eo eb ew bs="$6" ws="$7"
    local brc bo be bot bet wrc wo we wot wet
    eo="$(hex_of "$3")"
    eb="$(hex_of "$4")"
    ew="$(hex_of "$5")"

    run_shell bash "$bs"
    brc="$RC" bo="$OUT_HEX" be="$ERR_HEX" bot="$OUT_TXT" bet="$ERR_TXT"
    run_shell wash "$ws"
    wrc="$RC" wo="$OUT_HEX" we="$ERR_HEX" wot="$OUT_TXT" wet="$ERR_TXT"

    check_one "$label/bash" "$erc" "$eo" "$eb" "$brc" "$bo" "$be" "$bot" "$bet"
    check_one "$label/wash" "$erc" "$eo" "$ew" "$wrc" "$wo" "$we" "$wot" "$wet"

    total=$((total + 1))
    if [ "$brc" = "$wrc" ] && [ "$bo" = "$wo" ] && [ "$be" != "$we" ]; then
        printf 'PASS %s/semantics\n' "$label"
        pass=$((pass + 1))
    else
        printf 'FAIL %s/semantics\n' "$label"
        printf '  bash rc=%s stdout_hex=%s stderr_hex=%s\n' "$brc" "$bo" "$be"
        printf '  wash rc=%s stdout_hex=%s stderr_hex=%s\n' "$wrc" "$wo" "$we"
        fail=$((fail + 1))
    fi
}

env_case() { # label, precondition rc
    total=$((total + 1))
    if [ "$2" = 0 ]; then
        printf 'PASS %s\n' "$1"; pass=$((pass + 1))
    else
        printf 'FAIL %s\n' "$1"; fail=$((fail + 1))
    fi
}

[ "$(uname -s)" = Linux ]; env_case 'native_linux_wsl' "$?"
[ -x "$W" ]; env_case 'wash_0_2_0_native_elf' "$?"
[ -s "$BSO" ] && [ -s "$WSO" ]; env_case 'both_plugins_compiled' "$?"
[ -d "$TMP" ] && [ -w "$TMP" ]; env_case 'var_tmp_writable_no_fake_pass' "$?"

# --- 1. loading rc ----------------------------------------------------------
pair 'load_rc' 0 $'LOAD=0\n' '' \
    "$B_LOAD; echo LOAD=\$?" \
    "$W_LOAD; echo LOAD=\$?"

# --- 2. output --------------------------------------------------------------
pair 'output' 0 $'HELLO_FROM_PLUGIN\n' '' \
    "$B_LOAD >/dev/null 2>&1; hello" \
    "$W_LOAD >/dev/null 2>&1; plhello"

# --- 3. exit-code pass-through ----------------------------------------------
pair 'exit_code' 0 $'HELLO_FROM_PLUGIN\nRC=7\n' '' \
    "$B_LOAD >/dev/null 2>&1; hello 7; echo RC=\$?" \
    "$W_LOAD >/dev/null 2>&1; plhello 7; echo RC=\$?"

# --- 4. variable read/write -------------------------------------------------
pair 'var_write_read' 0 $'VALUE_FROM_PLUGIN\nSHELLVAR=VALUE_FROM_PLUGIN\n' '' \
    "$B_LOAD >/dev/null 2>&1; hset V VALUE_FROM_PLUGIN; hget V; echo SHELLVAR=\$V" \
    "$W_LOAD >/dev/null 2>&1; plset V VALUE_FROM_PLUGIN; plget V; echo SHELLVAR=\$V"

pair 'var_unset_marker' 0 $'PLGET_UNSET:NO_SUCH_VAR\n' '' \
    "$B_LOAD >/dev/null 2>&1; hget NO_SUCH_VAR" \
    "$W_LOAD >/dev/null 2>&1; plget NO_SUCH_VAR"

# --- 5. stderr --------------------------------------------------------------
pair 'stderr' 0 '' $'STDERR_TEXT\n' \
    "$B_LOAD >/dev/null 2>&1; hecho STDERR_TEXT" \
    "$W_LOAD >/dev/null 2>&1; plecho STDERR_TEXT"

# --- 6. four builtins from ONE -f -------------------------------------------
pair 'multi_register' 0 $'builtin\nbuiltin\nbuiltin\nbuiltin\n' '' \
    "$B_LOAD >/dev/null 2>&1; for n in hello hset hget hecho; do type -t \$n; done" \
    "$W_LOAD >/dev/null 2>&1; for n in plhello plset plget plecho; do type -t \$n; done"

# --- 7. per-name hot unload -------------------------------------------------
pair 'hot_unload_one_rest' 0 $'STILL\nRC=0\n' '' \
    "$B_LOAD >/dev/null 2>&1; hset V STILL; enable -d hello; hget V; echo RC=\$?" \
    "$W_LOAD >/dev/null 2>&1; plset V STILL; enable -d plhello; plget V; echo RC=\$?"

pair 'unloaded_name_disappears' 0 $'T=1\n' '' \
    "$B_LOAD >/dev/null 2>&1; enable -d hello; type -t hello; echo T=\$?" \
    "$W_LOAD >/dev/null 2>&1; enable -d plhello; type -t plhello; echo T=\$?"

pair 'unload_all_reload' 0 $'HELLO_FROM_PLUGIN\n' '' \
    "$B_LOAD >/dev/null 2>&1; enable -d hello hset hget hecho; $B_LOAD >/dev/null 2>&1; hello" \
    "$W_LOAD >/dev/null 2>&1; enable -d plhello plset plget plecho; $W_LOAD >/dev/null 2>&1; plhello"

# --- 8. the enable -f/-d/-s command surface ---------------------------------
# Goldens were measured from /usr/local/bin/bash 5.3.0 and wash 0.2.0 with a
# one-off probe.  The temporary path is interpolated, never hard-coded.
B_BARE="${BSO##*/}"
W_BARE="${WSO##*/}"
B_LOAD_BARE="enable -f $B_BARE hello hset hget hecho"
W_LOAD_BARE="enable -f $W_BARE x"
MISS="$TMP/nonexistent_plugin.so"
# $(...) strips trailing newlines; append one explicitly so goldens stay byte-exact.
EXP_S="$(printf 'enable %s\n' . : break continue eval exec exit export readonly return set shift source times trap unset)"$'\n'
WARN_PREFIX='/usr/local/bin/bash: line 1: enable: warning: '
BASH_REPEAT_ERR=''
for n in hello hset hget hecho; do
    BASH_REPEAT_ERR="${BASH_REPEAT_ERR}${WARN_PREFIX}${n}: dynamic builtin already loaded"$'\n'
done
BASH_MISS_ERR="/usr/local/bin/bash: line 1: enable: cannot open shared object $MISS: $MISS: cannot open shared object file: No such file or directory"$'\n'
WASH_MISS_ERR="wash: enable: cannot load $MISS: $MISS: cannot stat"$'\n'
BASH_NOSUCH_ERR='/usr/local/bin/bash: line 1: enable: nosuch: not a shell builtin'$'\n'
WASH_NOSUCH_ERR='wash: enable: nosuch: not a shell builtin'$'\n'

# Both ABIs: a successful -f is silent, rc 0, and registers the builtin.
pair 'enable_f_load_itself' 0 '' '' "$B_LOAD" "$W_LOAD"
pair 'enable_f_bare_name_in_loadables_path' 0 '' '' "$B_LOAD_BARE" "$W_LOAD_BARE"
pair 'enable_s_special_builtins' 0 "$EXP_S" '' \
    "$B_LOAD; enable -s" \
    "$W_LOAD; enable -s"
pair 'enable_f_registers_type_t' 0 $'builtin\n' '' \
    "$B_LOAD; type -t hello" \
    "$W_LOAD; type -t plhello"
pair 'enable_f_compgen_sees_exactly_one' 0 $'1\n' '' \
    "$B_LOAD; compgen -b | grep -x hello | wc -l" \
    "$W_LOAD; compgen -b | grep -x plhello | wc -l"
pair 'enable_d_loaded_name' 0 '' '' \
    "$B_LOAD; enable -d hello" \
    "$W_LOAD; enable -d plhello"
pair 'enable_d_loaded_name_rc' 0 $'D=0\n' '' \
    "$B_LOAD; enable -d hello; echo D=\$?" \
    "$W_LOAD; enable -d plhello; echo D=\$?"

# Failure paths: rc + stdout must still agree; each side's stderr is asserted
# byte-exact against its OWN measured diagnostic (see pair_golden above).
pair_golden 'enable_f_repeat' 0 '' "$BASH_REPEAT_ERR" '' \
    "$B_LOAD; $B_LOAD" \
    "$W_LOAD; $W_LOAD"
pair_golden 'enable_f_missing_file' 1 '' "$BASH_MISS_ERR" "$WASH_MISS_ERR" \
    "enable -f $MISS x" \
    "enable -f $MISS x"
pair_golden 'enable_d_nosuch' 1 '' "$BASH_NOSUCH_ERR" "$WASH_NOSUCH_ERR" \
    'enable -d nosuch' \
    'enable -d nosuch'

# --- verdict ----------------------------------------------------------------
printf -- '-----------------------------------------\n'
printf 'PASS=%d FAIL=%d TOTAL=%d\n' "$pass" "$fail" "$total"
if [ "$fail" -ne 0 ] || [ "$pass" -ne "$total" ]; then
    echo 'SECOND ORACLE FAILED'
    exit 1
fi
echo 'ALL SECOND ORACLE CAPABILITIES MATCH'
