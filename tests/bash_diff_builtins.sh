#!/usr/bin/env bash
# Byte-for-byte comparison of wash's builtin commands against GNU bash 5.3
# (MSYS2). For every test case we run BOTH bash and wash with the same `-c`
# command and compare:
#
#   * stdout     -- STRICTLY byte-for-byte identical (no normalization at all)
#   * exit code  -- exactly equal
#   * stderr     -- byte-for-byte identical AFTER normalizing ONLY the
#                   shell-name prefix of each line:
#                     bash : "<path>/bash: line N: msg"  ->  "E: msg"
#                     wash : "wash: msg"                ->  "E: msg"
#                   Everything else (message body, line count, trailing
#                   newlines) is left untouched.
#
# ---------------------------------------------------------------------------
# WHY `LC_ALL=C` IS MANDATORY
# ---------------------------------------------------------------------------
# The MSYS2 bash on this machine defaults to a Chinese locale, so its
# diagnostics get localized ("第 1 行", "未找到", "空值目录", ...).  Comparing
# those against wash's English messages could never succeed.  Exporting
# `LC_ALL=C` for BOTH children forces bash to emit English messages, which is
# what makes the stderr comparison meaningful at all.  Do not remove it.
#
# Run with:
#   MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc \
#       'cd <WASH_REPO> && bash tests/bash_diff_builtins.sh'
#
# Optional: ONLY_GROUP=A|K|B|C|D runs a single group.
#   A = core semantics + finished builtins (634 cases: 340 + 33 brace-range
#       + 24 brace word-boundary + 15 brace nest/escape/quote-removal
#       + 1 brace deep-nesting stack guard + 37 heredoc / herestring
#       + 11 unclosed `$(` syntax-error fence
#       + 11 redir-no-target / line-wise partial-execution fence
#       + 11 unclosed-delimiter family fence (round 6: ` / ${ / $(( / "$( )
#          single + multiline + closed-control, mirrors syntax_oracle_r6.tsv))
#       + 20 round-7 fence: unterminated compound commands (if/while/for/
#          case/{/(), innermost-opener line numbers, heredoc-body line
#          counting, `>;` near-token, family priority, heredoc-EOF warning)
#          -- mirrors qa9_oracle.tsv GROUP A)
#       + 25 command-exec semantics fence (PATH unset/empty/empty-element,
#          command-prefix assignment, ENOEXEC 4-class, exec builtin)
#       + 22 builtins fence (kill signal-0 probe, ulimit out-of-range/hex,
#          source directory, builtin write errors, read -n/-N byte counting,
#          printf empty format)
#       + 8 stderr-write-failure fence (`fd2` also /dev/full: diagnostics must
#          be dropped silently, never panic with rc=101)
#       + 15 byteval raw-byte fence (truncated UTF-8 via REPLY/printf -v,
#          ${#v} byte length, substring, declare -p audit; no PUA leakage;
#          literal U+E100+U+E000 sentinel forgery via -c source, command
#          substitution, source file, mapfile and inherited-env ingress;
#          stderr diagnostics must not leak encoded values)
#       + 12 ANSI-C quoting fence ($'...' raw bytes for \x/\0, locale-dependent
#          \u/\U fallback, NUL truncation, generated-sentinel escaping;
#          9 under LC_ALL=C + 3 under LC_ALL=C.UTF-8)
#       + 5 from_utf8_lossy fence (script/source ingress, printf invalid
#          format char, shebang bad interpreter, command-name errno diagnostic;
#          raw 0xff must survive, never become U+FFFD or U+00FF)
#       + 44 [[ ]] conditional-command fence (operators, ERE/BASH_REMATCH,
#          no word splitting, bash's two-line malformed-expression diagnostics)
#       -> MUST stay green (regressions)
#   K = KNOWN-DIFF: cannot be byte-equal by design (documented, not fixed)
#   B = regression fence for already-fixed defects (GROUP B, 21 cases, green today)
#   C = third-priority builtins (may not be implemented yet)
#   D = extended builtins / compatibility-matrix evidence
# ---------------------------------------------------------------------------
set -u

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------
BASH_BIN="${BASH_BIN:-/d/msys2/usr/bin/bash.exe}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -z "${WASH_BIN:-}" ]; then
    if [ -x "$REPO_ROOT/target/debug/wash.exe" ]; then
        WASH_BIN="$REPO_ROOT/target/debug/wash.exe"
    else
        WASH_BIN="$REPO_ROOT/target/debug/wash"
    fi
fi

# Mandatory: English diagnostics on both sides.
export LC_ALL=C
export LANG=C

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ---------------------------------------------------------------------------
# HOME ISOLATION (mandatory)
#
# Some cases read `$HOME` (`cd` without arguments, `~` expansion) and the
# REPL writes `$HOME/.wash_history`.  Pointing HOME at a throw-away directory
# inside the sandbox guarantees that a test run can never touch the REAL
# user's home directory, no matter which directory the suite is started from.
# Both sides of the comparison (bash and wash) see the same HOME, so the
# byte-diff stays valid.
# ---------------------------------------------------------------------------
export HOME="$TMP/home"
mkdir -p "$HOME"

# Sandbox with the files / directories the cases need.
SANDBOX="$TMP/sandbox"
mkdir -p "$SANDBOX/sub" "$SANDBOX/bin" "$SANDBOX/cwd"
printf 'echo FROM_PATH\n'         > "$SANDBOX/bin/probe.sh"
printf 'echo FROM_CWD\n'          > "$SANDBOX/cwd/probe.sh"
# --- 命令执行语义 fixtures（PATH 搜索 / ENOEXEC 分类 / exec 内建）-----------
# oracle（MSYS2 bash 5.3.20, LC_ALL=C）实测：PATH 搜索只要求「存在的
# regular file」（noacl 挂载上执行位不生效，这里也一律不 chmod +x）；
# execve 失败再按 errno + 文件头分类：坏解释器 / ELF 或 NUL 二进制 /
# 目录 / 纯文本回退为 shell 脚本执行。
mkdir -p "$SANDBOX/execfix/pdir1"
printf '#!/bin/sh\necho SHBANG-OK\n'            > "$SANDBOX/execfix/goodinterp"
printf 'echo PLAIN-from-script\n'               > "$SANDBOX/execfix/plain"
printf '#!/bin/definitely-not-exist\necho NO\n' > "$SANDBOX/execfix/badinterp"
printf '\x7fELFfake'                            > "$SANDBOX/execfix/elf"
printf 'AB\x00CD\n'                             > "$SANDBOX/execfix/nulf"
printf 'echo FROM-EXECFIX\n'                    > "$SANDBOX/execfix/pdir1/nonexec"
# byteval 哨兵伪造夹具：原始字节 ee 84 80 ee 80 80（字面 U+E100 + U+E000）。
# 必须进行「命令替换 / source / mapfile」三个独立 ingress；用 printf 八进制
# 转义写文件，禁止在 Python/脚本字符串里写 \356（会被当 Unicode 转义而失真）。
printf '\356\204\200\356\200\200' > "$SANDBOX/execfix/forge.bin"
printf 'printf %%s "\356\204\200\356\200\200" | od -An -tx1\n' > "$SANDBOX/execfix/forge_src.sh"
printf '\377' > "$SANDBOX/execfix/ff.bin"
# from_utf8_lossy 修复夹具：源码 / printf spec / shebang 解释器 / 命令名各含
# 裸 0xff。这些字节必须原样保真，不得变成 U+FFFD(ef bf bd) 或 U+00FF(c3 bf)。
printf 'printf %%s "\377" | od -An -tx1\n'      > "$SANDBOX/execfix/lossy_src.sh"
printf '%%\377'                                > "$SANDBOX/execfix/lossy_fmt.bin"
printf '#!/tmp/no_such_\377\necho LOSSY-HI\n'  > "$SANDBOX/execfix/lossy_shebang"
printf '/tmp/no_such_\377'                     > "$SANDBOX/execfix/rawname.bin"
printf 'echo "hello from src"\n' > "$SANDBOX/script.sh"
printf 'echo "1=$1 2=$2 n=$#"\n'  > "$SANDBOX/args.sh"
printf 'exit 7\n'                 > "$SANDBOX/exit7.sh"
printf 'false\n'                  > "$SANDBOX/false.sh"
printf 'line1\nline2\n'           > "$SANDBOX/in.txt"
# Fixtures for the `read` field-splitting cases: one line separated by
# SPACES, one by COLONS.  `IFS` decides which separator counts.
printf 'a b c\n'                   > "$SANDBOX/spaced.txt"
printf 'a:b:c\n'                   > "$SANDBOX/colons.txt"
# Fixture for the `return` cases: `return` is only legal inside a function or
# a SOURCED script, and a sourced script needs no function support -- so this
# is the cheapest precondition that makes `return` observable.
printf 'return 3\n'                > "$SANDBOX/ret3.sh"
# `return` with no argument returns the status of the last command (`false`
# -> 1); `return 0` leaves the sourced script successfully.
printf 'false\nreturn\n'          > "$SANDBOX/retlast.sh"
printf 'return 0\n'                > "$SANDBOX/ret0.sh"
# Syntax-error fixtures for nested input sources (source / eval / traps).
# bash labels these errors with the input source, not with `-c`.
printf '}\n'                       > "$SANDBOX/bad_tok.sh"
printf '{ echo hi\n'               > "$SANDBOX/bad_eof.sh"
# Fixture for the GROUP A cd-logical / cd-physical / cd-logical-flag cases
# (the three `cd` cases that resolve $SANDBOX/link logically vs physically).
# A bare MSYS2 `ln -s` silently degrades to a DIRECTORY COPY when native
# symlink privileges are unavailable.  A copy turns those cases into a
# false PASS: both sides just cd into a plain directory, so the logical
# (`cd -L`) vs physical (`cd -P`) resolution they must test is never used.
# Force a real MSYS symlink with MSYS=winsymlinks:lnk (no admin rights,
# unlike nativestrict), then HARD-GATE on it with the `[ -L ... ]` check
# below: a degraded or missing fixture is a loud FATAL, never a silent green.
#
# PREREQUISITE / DESIGN INTENT (do not "fix" a FATAL by weakening the gate):
# MSYS=winsymlinks:lnk only takes effect in an MSYS2 *login* shell
# (MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc ...) whose PATH resolves the
# MSYS2-provided `ln`.  In a non-login `-c` shell, or on a machine whose
# PATH/toolchain differs, the very same command degrades to a directory copy
# and the hard gate below FATALs.  That is BY DESIGN: the three cd cases are
# only meaningful against a real symlink.  NEVER delete or relax the hard
# gate just to make this suite run green under a non-login shell or on some
# other machine -- provide a genuinely-symlink-capable fixture instead.
MSYS=winsymlinks:lnk ln -s sub "$SANDBOX/link" 2>/dev/null || true
[ -L "$SANDBOX/link" ] || {
    echo "FATAL: sandbox symlink fixture is not a real symlink (ln -s degraded to a directory copy or creation failed)" >&2
    exit 1
}

# `timeout` guards against a hanging case; degrade gracefully if absent.
RUNNER=()
if command -v timeout >/dev/null 2>&1; then
    RUNNER=(timeout 10)
fi

# ---------------------------------------------------------------------------
# Accounting
# ---------------------------------------------------------------------------
# Group A : core semantics + the builtins the engineer has already finished.
#           MUST be green -- every failure here is a real regression.
# Group K : KNOWN-DIFF -- cases where bash and wash can NOT be byte-identical
#           BY DESIGN (shell-instance-specific values such as `$0` = the
#           path the shell was invoked with, or bash's internal variable
#           set).  These are documented, not "fixed"; they are reported
#           separately so they never hide a genuine regression.
# Group B : regression fence for defects that are already fixed.  They are
#           green today; a failure here means a known defect came back.
# Group C : third-priority builtins (may or may not be implemented yet).
# Group D : extra builtins, measured so the compatibility matrix in
#           AGENTS.md / README.md is backed by real observations.
declare -A g_total g_pass g_fail
GROUPS=(A K B C D)
ONLY_GROUP="${ONLY_GROUP:-}"

pass=0
fail=0

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

# Normalize ONLY the shell-name prefix of every stderr line.
#   bash : "<any path>/bash: line <N>: msg"  ->  "msg"
#   bash : "<any path>/bash: msg"            ->  "msg"   (no line number)
#   bash : "<builtin>: usage: ..."           ->  UNCHANGED
#   wash : "wash: msg"                       ->  "msg"
#
# WHY THE PREFIX IS DELETED INSTEAD OF BEING REPLACED BY A MARKER
#   bash does NOT put a shell prefix on usage errors: `unalias` with no
#   argument prints
#       unalias: usage: unalias [-a] name [name ...]
#   i.e. the message starts with the BUILTIN name and nothing else.  wash
#   prints the same message behind its own shell prefix ("wash: unalias:
#   usage: ...").  If the normalizer replaced the prefix by a fixed marker
#   ("E: "), bash's usage error would stay "unalias: usage: ..." while
#   wash's became "E: unalias: usage: ..." and the two could never be
#   equal -- the case would be unfixable by construction.  Deleting the
#   prefix on both sides leaves exactly the message body, which is what
#   has to match.  The message body, the number of lines and the trailing
#   newline are all left untouched, so the comparison stays byte-for-byte.
normalize_stderr() {
    sed -E 's#^[^:]*bash(\.exe)?: (line [0-9]+: )?##; s#^wash(\.exe)?: (line [0-9]+: )?##' "$1"
}

# `cmp` is not available in this MSYS2 installation -> compare hex dumps.
hexdump() {
    od -An -v -tx1 "$1"
}

show_stream() {
    local label="$1" file="$2"
    echo "    --- $label ---"
    if [ -s "$file" ]; then
        sed -e 's/^/    /' "$file"
    else
        echo "    <empty>"
    fi
}

report_failure() {
    local label="$1" case_text="$2" rb="$3" rw="$4"
    local ob ox eb ew

    ob="$(hexdump "$TMP/bash.out")"
    ox="$(hexdump "$TMP/wash.out")"
    eb="$(hexdump "$TMP/bash.err.n")"
    ew="$(hexdump "$TMP/wash.err.n")"

    echo "FAIL [$label]: $case_text"
    echo "    bash rc=$rb   wash rc=$rw"
    if [ "$ob" != "$ox" ]; then
        echo "    stdout differs:"
        show_stream "bash stdout" "$TMP/bash.out"
        show_stream "wash stdout" "$TMP/wash.out"
        if [ "$(cat "$TMP/bash.out")" = "$(cat "$TMP/wash.out")" ]; then
            echo "    (stdout text is equal but BYTES differ)"
            echo "    bash hex: $(echo "$ob" | tr -s ' ' | head -4)"
            echo "    wash hex: $(echo "$ox" | tr -s ' ' | head -4)"
        fi
    fi
    if [ "$eb" != "$ew" ]; then
        echo "    stderr differs (after prefix normalization):"
        show_stream "bash stderr" "$TMP/bash.err.n"
        show_stream "wash stderr" "$TMP/wash.err.n"
    fi
}

# ---------------------------------------------------------------------------
# 语义不变量对比。用于 GROUP K 的「身份绑定」用例：bash 与 wash 在设计上
# 永远不可能逐字节相等（被比较的内容绑定到运行中的二进制身份：
# BASH_VERSION / EUID / PPID / shell 自身的 $0 路径）。这里绝不伪造 bash 的
# 身份——我们不去断言某个字面量值，而是断言一个「与 shell 无关」的不变量，
# 它对 bash 和 wash 都必须成立。
#
#   $1 = 分组（A/K/B/C/D）
#   $2 = 标签
#   $3 = 用例文本（与 run_case 一样支持 @SANDBOX@ 替换）
#   $4 = INVARIANT 表达式，在子 shell 中对「每一侧」各求值一次，作用域内可见
#        $OUT（stdout 文件路径）、$ERR（stderr 文件路径）、$RC（退出码）。
#        仅当不变量对 bash 与 wash「两侧都成立」时，该用例才算通过。
# ---------------------------------------------------------------------------
run_case_sem() {
    local group="$1" label="$2" case_text="$3" invariant="$4"
    local rb rw

    if [ -n "$ONLY_GROUP" ] && [ "$group" != "$ONLY_GROUP" ]; then
        return 0
    fi

    case_text="${case_text//@SANDBOX@/$SANDBOX}"
    g_total[$group]=$(( ${g_total[$group]:-0} + 1 ))

    "${RUNNER[@]}" "${CASE_ENV[@]}" "$BASH_BIN" -c "$case_text" \
        >"$TMP/bash.out" 2>"$TMP/bash.err" </dev/null
    rb=$?
    "${RUNNER[@]}" "${CASE_ENV[@]}" "$WASH_BIN" -c "$case_text" \
        >"$TMP/wash.out" 2>"$TMP/wash.err" </dev/null
    rw=$?

    if ( OUT="$TMP/bash.out" ERR="$TMP/bash.err" RC=$rb eval "$invariant" ) 2>/dev/null &&
       ( OUT="$TMP/wash.out" ERR="$TMP/wash.err" RC=$rw eval "$invariant" ) 2>/dev/null; then
        pass=$((pass + 1))
        g_pass[$group]=$(( ${g_pass[$group]:-0} + 1 ))
    else
        fail=$((fail + 1))
        g_fail[$group]=$(( ${g_fail[$group]:-0} + 1 ))
        report_failure_sem "$label" "$case_text" "$rb" "$rw" "$invariant"
    fi
}

report_failure_sem() {
    local label="$1" case_text="$2" rb="$3" rw="$4" invariant="$5"
    echo "FAIL [$label]: $case_text"
    echo "    （语义不变量用例；bash rc=$rb   wash rc=$rw）"
    echo "    不变量：$invariant"
    show_stream "bash stdout" "$TMP/bash.out"
    show_stream "wash stdout" "$TMP/wash.out"
}

# ---------------------------------------------------------------------------
# Core comparison: run the same case under bash and under wash.
#   $1 = group (A/K/B/C/D)
#   $2 = label
#   $3 = case text.  Use the literal token @SANDBOX@ for the sandbox path;
#        it is substituted before running, so case texts can stay
#        single-quoted (which keeps $VAR expansion for the inner shell).
# ---------------------------------------------------------------------------
# 每个用例可临时注入一行环境变量（CASE_ENV），仅作用于两侧被测进程；
# 用于 byteval 环境变量 ingress 栅栏，且不污染 export-p / export-count 等
# 全量环境用例。调用方用完必须重置 CASE_ENV=()。
CASE_ENV=()

run_case() {
    local group="$1" label="$2" case_text="$3"
    local rb rw

    if [ -n "$ONLY_GROUP" ] && [ "$group" != "$ONLY_GROUP" ]; then
        return 0
    fi

    case_text="${case_text//@SANDBOX@/$SANDBOX}"
    g_total[$group]=$(( ${g_total[$group]:-0} + 1 ))

    # Reference: real GNU bash.
    "${RUNNER[@]}" "$BASH_BIN" -c "$case_text" \
        >"$TMP/bash.out" 2>"$TMP/bash.err" </dev/null
    rb=$?

    # Candidate: wash.
    "${RUNNER[@]}" "$WASH_BIN" -c "$case_text" \
        >"$TMP/wash.out" 2>"$TMP/wash.err" </dev/null
    rw=$?

    normalize_stderr "$TMP/bash.err" > "$TMP/bash.err.n"
    normalize_stderr "$TMP/wash.err" > "$TMP/wash.err.n"

    if [ "$(hexdump "$TMP/bash.out")" = "$(hexdump "$TMP/wash.out")" ] &&
       [ "$rb" = "$rw" ] &&
       [ "$(hexdump "$TMP/bash.err.n")" = "$(hexdump "$TMP/wash.err.n")" ]; then
        pass=$((pass + 1))
        g_pass[$group]=$(( ${g_pass[$group]:-0} + 1 ))
    else
        fail=$((fail + 1))
        g_fail[$group]=$(( ${g_fail[$group]:-0} + 1 ))
        report_failure "$label" "$case_text" "$rb" "$rw"
    fi
}

###############################################################################
# GROUP A -- core semantics + the builtins that are already implemented.
#             Every failure in this group is a REAL regression.
###############################################################################

# --- cd ---------------------------------------------------------------------
run_case A cd-abs            'cd /tmp; echo "PWD=$PWD"; echo "OLDPWD=$OLDPWD"'
run_case A cd-relative-ok    'cd @SANDBOX@; cd sub; echo "rc=$? PWD=$PWD"'
run_case A cd-relative-fail  'cd @SANDBOX@; cd nosuchdir; echo "rc=$? PWD=$PWD"'
run_case A cd-logical        'cd @SANDBOX@/link; echo "PWD=$PWD"'
run_case A cd-physical       'cd @SANDBOX@/link; cd -P .; echo "P=$PWD"'
run_case A cd-logical-flag   'cd @SANDBOX@/link; cd -L .; echo "L=$PWD"'
run_case A cd-noarg          'cd; echo "rc=$? PWD=$PWD"'
run_case A cd-dash           'cd /tmp; cd /; cd -; echo "PWD=$PWD OLDPWD=$OLDPWD"'
run_case A cd-empty          'cd ""; echo "rc=$?"'
run_case A cd-missing        'cd /no/such/dir; echo "rc=$? PWD=$PWD"'
run_case A cd-cdpath         'CDPATH=@SANDBOX@; cd sub; echo "rc=$? PWD=$PWD"'
# bash refuses to change directory when more than one operand is given:
# "cd: too many arguments" (rc=2) and PWD stays untouched.
run_case A cd-too-many       'cd /tmp /var; echo "rc=$? PWD=$PWD"'
# `--` ends the option list, so the operand is the directory.
run_case A cd-dashdash       'cd -- /tmp; echo "rc=$? PWD=$PWD"'
run_case A cd-dashdash-fail  'cd -- nosuchdir; echo "rc=$? PWD=$PWD"'
run_case A cd-no-oldpwd      'cd -; echo "rc=$?"'
run_case A cd-then-fail      'cd /tmp; cd /no/such/dir; echo "rc=$? PWD=$PWD"'

# --- exit -------------------------------------------------------------------
run_case A exit-noarg        'false; exit'
run_case A exit-noarg-true   'true; exit'
run_case A exit-42           'exit 42'
# bash does NOT terminate on a non-numeric argument: it reports the error,
# leaves $? = 2 and CONTINUES with the next command.
run_case A exit-abc          'exit abc; echo "not reached"'
run_case A exit-abc-status   'exit abc; echo "rc=$?"'
run_case A exit-300          'exit 300'
run_case A exit-negative     'exit -1'
run_case A exit-in-source    'source @SANDBOX@/exit7.sh; echo "not reached"'

# --- export -----------------------------------------------------------------
# `SHLVL=1` pins the one environment variable bash mutates on startup
# (bash increments SHLVL, wash does not -- see GROUP D `shlvl-increment`).
# `_` is filtered out: it holds the path of the last executed command and
# bash drops its export attribute as soon as a command runs, while wash
# keeps it exported (see GROUP D `underscore-export-status`).
run_case A export-p          "SHLVL=1; export -p | grep -v '^declare -x _='"
run_case A export-noarg      "SHLVL=1; export | grep -v '^declare -x _='"
run_case A export-assign     'export FOO=bar; echo "FOO=$FOO"'
run_case A export-assign-p   "export FOO=bar; SHLVL=1; export -p | grep '^declare -x FOO='"
run_case A export-multi      "export A=1 B=2; SHLVL=1; export -p | grep -E '^declare -x (A|B)='"
run_case A export-n          "export FOO=bar; export -n FOO; SHLVL=1; export -p | grep '^declare -x FOO='"
run_case A export-n-rc       'export -n FOO; echo "rc=$?"'
run_case A export-invalid    'export 1bad=x; echo "rc=$?"'
# `export` without options lists every exported variable.  `SHLVL` is
# filtered here (instead of pinned) because its value depends on how many
# shells the harness happens to nest -- see GROUP D `shlvl-increment`.
run_case A export-no-name    "export | grep -v -e '^declare -x _=' -e '^declare -x SHLVL='"

# --- unset ------------------------------------------------------------------
run_case A unset-v           'export V=1; unset -v V; echo "V=[$V]"'
run_case A unset-default     'export V=1; unset V; echo "V=[$V]"'
run_case A unset-multi       'export A=1 B=2; unset A B; echo "A=[$A] B=[$B]"'
run_case A unset-missing     'unset NOSUCHVAR; echo "rc=$?"'
run_case A unset-f           'unset -f nosuchfunc; echo "rc=$?"'
run_case A unset-noarg       'unset; echo "rc=$?"'

# --- set --------------------------------------------------------------------
run_case A set-o             'set -o'
run_case A set-plus-o        'set +o'
# `igncr` is a Cygwin-only option: the Cygwin bash oracle accepts it (rc 0)
# and lists it in `set -o`; real Linux bash 5.3.0 rejects it ("invalid option
# name", rc 2) and omits it.  wash must match ITS OWN platform's bash, so
# these cases compare against whatever oracle runs the suite.
run_case A set-o-igncr       'set -o igncr; echo "rc=$?"'
run_case A set-o-igncr-count 'set -o | grep -c igncr'
run_case A set-dashdash      'set -- a b c; echo "n=$# 1=$1 3=$3"'
run_case A set-clear         'set -- a b c; set --; echo "n=$#"'
run_case A set-empty-dashdash 'set --; echo "n=$#"'
run_case A set-invalid       'set -Z; echo "rc=$?"'
run_case A set-invalid-o     'set -o nosuchopt; echo "rc=$?"'
run_case A set-errexit       'set -e; set -o | grep errexit'
run_case A set-noglob        'set -f; set -o | grep noglob'
# `set -n` (noexec): the shell reads commands but does not EXECUTE them.
# Implemented on purpose (src/executor.rs), so it is a regression fence.
run_case A set-noexec        'set -n; echo hi'
run_case A set-noexec-table  'set -n; set -o | grep noexec'
run_case A set-noexec-cmd    'set -n; /bin/echo hi'
run_case A set-noexec-cmdsubst 'set -n; echo $(echo hi)'

# --- alias ------------------------------------------------------------------
run_case A alias-noarg       'alias; echo "rc=$?"'
run_case A alias-define      "alias x='echo 1'; alias"
run_case A alias-p           "alias x='echo 1'; alias -p"
run_case A alias-undefined   'alias nosuchalias; echo "rc=$?"'
run_case A alias-noninteractive "alias ll='echo aliased'; ll; echo \"rc=\$?\""
run_case A alias-invalid     'alias 1bad=x; echo "rc=$?"'

# --- unalias ----------------------------------------------------------------
run_case A unalias-a         'unalias -a; echo "rc=$?"'
run_case A unalias-one       "alias x='echo 1'; unalias x; alias; echo \"rc=\$?\""
run_case A unalias-missing   'unalias nosuchalias; echo "rc=$?"'
# bash prints the usage line (with the builtin name as prefix, no shell
# prefix) and exits with 2.
run_case A unalias-noarg     'unalias; echo "rc=$?"'

# --- source -----------------------------------------------------------------
run_case A source-basic      'source @SANDBOX@/script.sh'
run_case A source-dot        '. @SANDBOX@/script.sh'
run_case A source-args       'source @SANDBOX@/args.sh A B; echo "1=$1 2=$2 n=$#"'
run_case A source-dot-args   '. @SANDBOX@/args.sh A B; echo "1=$1 2=$2 n=$#"'
run_case A source-missing    'source /no/such/file; echo "rc=$?"'
run_case A source-noarg      'source; echo "rc=$?"'
run_case A source-status     'source @SANDBOX@/false.sh; echo "rc=$?"'
# `$0` is the path the shell itself was invoked with, so its VALUE can never
# be equal between bash and wash (see GROUP K `source-zero-raw`).  What has
# to match is the SEMANTIC: sourcing a file must NOT change `$0` (bash keeps
# the shell's own path, in default mode as well as in --posix mode).
#
# TWO SPELLINGS OF THE SAME PREDICATE
#   `source-zero-semantics`     uses `if`/`then`/`else`/`fi` -- the natural
#                                bash spelling, and the one that would break
#                                the moment `if` regressed.
#   `source-zero-semantics-cond` uses `&&`/`||` only -- no dependency on
#                                control flow, so it isolates the `$0`
#                                semantics even if the parser regresses.
#   Both print the SAME vocabulary ("0=CHANGED" / "0=UNCHANGED"), so a
#   divergence between them points straight at the control-flow layer.
run_case A source-zero-semantics 'source @SANDBOX@/args.sh; if [ "$0" = "@SANDBOX@/args.sh" ]; then echo "0=CHANGED"; else echo "0=UNCHANGED"; fi'
run_case A source-zero-semantics-cond 'source @SANDBOX@/args.sh; [ "$0" = "@SANDBOX@/args.sh" ] && echo "0=CHANGED" || echo "0=UNCHANGED"'
# Stronger form of the same semantic: `$0` must be IDENTICAL before and
# after sourcing (not merely "different from the sourced file's path").
run_case A source-zero-unchanged 'BEFORE="$0"; source @SANDBOX@/args.sh; [ "$BEFORE" = "$0" ] && echo "0=UNCHANGED" || echo "0=CHANGED"'
# Lookup order: a name without a slash is searched in PATH FIRST and only
# then in the current directory.
run_case A source-lookup     'cd @SANDBOX@/cwd; PATH=@SANDBOX@/bin:$PATH; source probe.sh'
run_case A source-lookup-dot 'cd @SANDBOX@/cwd; PATH=@SANDBOX@/bin:$PATH; source ./probe.sh'
# Not in PATH at all -> bash falls back to the current directory.
run_case A source-lookup-cwd 'cd @SANDBOX@/cwd; source probe.sh'

# --- eval -------------------------------------------------------------------
run_case A eval-sideeffect   'eval "X=42"; echo "X=$X"'
run_case A eval-command      'eval "echo hi"; echo "rc=$?"'
run_case A eval-empty        "eval ''; echo \"rc=\$?\""
run_case A eval-noarg        'eval; echo "rc=$?"'
run_case A eval-exit         "eval 'exit 3'; echo \"not reached\""
run_case A eval-multi-word   'eval echo a b; echo "rc=$?"'

# --- exec -------------------------------------------------------------------
run_case A exec-a            'exec -a myname echo hi; echo "rc=$?"'
run_case A exec-command      'exec echo hi; echo "rc=$?"'
run_case A exec-missing      'exec nosuchcommand12345; echo "rc=$?"'
run_case A exec-noarg        'exec; echo "rc=$?"'
run_case A exec-replaces     'exec echo hi; echo "after"'

# --- shift ------------------------------------------------------------------
run_case A shift-basic       'set -- a b c; shift; echo "1=$1 n=$#"'
run_case A shift-twice       'set -- a b c; shift; shift; echo "1=$1 n=$#"'

# --- && / || (AND-OR lists; fixed in 4f55f93 -- kept as a regression fence) --
run_case A and-and           'cd /tmp && cd /tmp && echo hi'
run_case A and-true          'true && echo yes'
run_case A and-false         'false && echo no'
run_case A and-pipe          'echo a | grep a && echo piped'
run_case A or-false          'false || echo fallback'
run_case A or-true           'true || echo not-reached'

# --- parameter expansion ${var-word} / ${var+word} ---------------------------
run_case A expand-dash       'unset -v ZZZ; echo "[${ZZZ-unset}]"'
run_case A expand-empty      'A=; echo "[${A-empty}]"'
run_case A expand-plus       'SET=1; echo "[${SET+plus}]"'

# --- if / then / elif / else / fi (Phase 3 compound command) ---------------
run_case A if-true           'if true; then echo YES; fi'
run_case A if-false          'if false; then echo YES; else echo NO; fi'
run_case A if-elif           'if false; then echo A; elif true; then echo B; else echo C; fi'
run_case A if-nested         'if true; then if false; then echo X; else echo Y; fi; fi'
run_case A if-status         'if false; then echo A; fi; echo "rc=$?"'
run_case A if-assign         'if true; then X=1; fi; echo "X=$X"'
run_case A if-test           'if [ 1 -eq 1 ]; then echo EQ; fi'
run_case A if-test-false     'if [ 1 -eq 2 ]; then echo EQ; else echo NE; fi'

# --- code review 盲区补齐（5 条，来源：team-lead 派发的 GROUP A 闸门补强用例）---
# 覆盖此前对拍未触及的核心语义：fd 数字前缀重定向、fd 复制 2>&1、当前目录
# 无斜杠 glob（跳过 dotfile）、位置参数 ${1}/${2} 花括号展开，以及负指数算术
# 的「不 panic + 报负指数错误」哨兵。前 4 条用 run_case 逐字节对拍 stdout +
# 规范化 stderr + rc；arith-neg-exp 两侧错误文案前缀/错误记号不同（实测 LC_ALL=C
# 下 bash 为 `bash: line 1: 2 ** -1: exponent less than 0 (error token is "1")`、
# wash 为 `wash: exponent less than 0 (error token is "")`），stderr 天然无法
# 逐字节对拍，按框架约定改用 run_case_sem 语义不变量：
#   1) rc < 128 —— 挡住 Rust panic(101)、abort(134)、SIGSEGV(139)、timeout(124)
#      等一切信号级崩溃（只判 rc≠101 会漏过这些）；
#   2) stderr 含 "exponent less than 0" —— 必须真的报出负指数错误，不能静默吞错；
#   3) stdout 不含任何非空白字符 —— 拒绝把算术结果误打到 stdout。
# 更严的形式（要求 rc 相等或 stdout 全空）会破坏「两侧都成立」前提：bash 为
# rc=1/stdout 空，wash 为 rc=0/stdout 一个空行；stderr 前缀与错误记号也不同。
# 以上三项已由 QA 变异测试验证：bash/wash 两侧均成立，且能拒绝上述各反例。
run_case A redir-fd2          'echo hi 2>@SANDBOX@/e; echo "rc=$?"; cat @SANDBOX@/e'
run_case A redir-dup-2and1    'echo x 2>&1; echo "rc=$?"'
run_case A glob-cwd-star      'mkdir -p @SANDBOX@/gd; cd @SANDBOX@/gd; touch a.txt b.txt c.log .hidden; echo *'
run_case A param-pos-braced   'set -- foo bar; echo "[${1}][${2}][${3}]"'
run_case_sem A arith-neg-exp  'echo $((2 ** -1))' '[ "$RC" -lt 128 ] && grep -q "exponent less than 0" "$ERR" && ! grep -q "[^[:space:]]" "$OUT"'
# 同批修复的 P1#9 / P1#12：`$( )` 内引号里的 ) 是字面量；命令替换剥全部尾随换行。
run_case A cmdsub-quoted-paren 'echo $(echo ")")'
run_case A cmdsub-strip-newlines 'x=$(printf "a\n\n\n"); echo "[${#x}]"'
# 第三轮 review：算术内赋值必须落盘（ArithCtx::set 经 pending_assign 写回）、
# 逗号表达式（arith lexer 的 Tok::Comma 与 eat_op(",") 对齐）、以及未引号变量
# 展开后的 glob 多匹配必须是多个词（旧 glob_one 用 join(" ") 折叠成一个词）。
run_case A arith-assign-persist 'x=0; echo $((x=5)); echo $x'
run_case A arith-comma          'echo $((1,2,3))'
run_case A glob-multiword-printf 'mkdir -p @SANDBOX@/gsub/gtest; touch @SANDBOX@/gsub/gtest/a.txt @SANDBOX@/gsub/gtest/b.txt; cd @SANDBOX@/gsub; x=gtest/*.txt; printf "[%s]" $x; echo'
run_case A glob-multiword-for    'mkdir -p @SANDBOX@/gsub/gtest; touch @SANDBOX@/gsub/gtest/a.txt @SANDBOX@/gsub/gtest/b.txt; cd @SANDBOX@/gsub; x=gtest/*.txt; for f in $x; do echo "F=$f"; done'
# 第三轮 review P0 #1：`{ LIST; }` 命令组。lexer 已把词边界的 `{`/`}` 切成
# 独立 Word，parse_compounds 只在「命令位置」把它们当保留字（`echo {` 仍是
# 普通参数、`{a,b}` 仍是 brace expansion、函数体花括号仍归 funcdef 消费）。
# 组体在当前 shell 执行（非子壳），所以赋值 / cd / exit 状态会持久化。
run_case A group-basic          '{ echo hi; }'
run_case A group-persist-var    '{ x=1; }; echo $x'
run_case A group-status         '{ false; }; echo $?'
run_case A group-andor          '{ echo a; } && { echo b; }; { false; } || { echo c; }'
run_case A group-nested         '{ echo a; { echo b; }; }'
run_case A group-pipeline       '{ echo a; echo b; } | tr a-z A-Z'
run_case A group-redir          '{ echo hi; } > @SANDBOX@/gout; cat @SANDBOX@/gout'
run_case A group-dup-redir      '{ echo o; echo e >&2; } 2>/dev/null'
run_case A group-in-if          'if true; then { echo g; }; fi'
run_case A group-in-loop        'for i in 1 2; do { echo f$i; }; done'
run_case A group-cd-persist     'cd @SANDBOX@; { cd sub; pwd; }; pwd'
run_case A group-func-return    'f() { { return 5; }; echo nope; }; f; echo $?'
run_case A group-exit-status    '{ exit 7; }; echo unreachable'
run_case A group-brace-arg      'echo {'
run_case A group-brace-expand   'echo {a,b}'
run_case A group-funcdef        'f() { echo fn; }; f'
# 语法错误路径：bash 会额外回显出错行、前缀也不同，stderr 无法逐字节对齐，
# 故用语义不变量锁住 rc=2 / stdout 空 / 真的报了语法错（挡住静默吞错与崩溃）。
run_case_sem A group-unclosed   '{ echo hi' '[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] && grep -q "unexpected end of file" "$ERR"'
run_case_sem A group-empty      '{ }' '[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] && grep -q "syntax error" "$ERR"'
run_case_sem A group-no-semi    '{ echo a }' '[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] && grep -q "unexpected end of file" "$ERR"'
run_case_sem A group-stray-close '}' '[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] && grep -q "syntax error" "$ERR"'
# --- code review P3：实现 ( ) 子 shell 语法（fork 一个 shell 副本执行 LIST，
#     内部赋值 / cd / fd 变化不回写父 shell）---
run_case A subshell-basic          '( echo inner )'
run_case A subshell-isolate-var    'x=outer; ( x=inner ); echo "x=$x"'
run_case A subshell-isolate-cd     'cd @SANDBOX@; ( cd sub; pwd ); pwd'
run_case A subshell-redir          '( echo hi ) > @SANDBOX@/ss; cat @SANDBOX@/ss'
run_case A subshell-pipeline       '( echo a; echo b ) | tr a-z A-Z'
run_case A subshell-status         '( false ); echo "rc=$?"'
run_case A subshell-nested         '( ( echo deep ) )'
run_case A subshell-andor          '( echo a ) && ( echo b )'
run_case A subshell-func-isolated  'f() { echo fn; }; ( f ); echo "r=$?"'
run_case A subshell-bg-internal    '( true & wait "$!"; echo "done" )'
# 空子 shell / 括号不匹配属语法错误：语义不变量锁 rc=2、stdout 空、真的报语法错。
run_case_sem A subshell-empty      '( )' '[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] && grep -q "syntax error" "$ERR"'
run_case_sem A subshell-unclosed   '( echo hi' '[ "$RC" -eq 2 ] && [ ! -s "$OUT" ] && grep -q "unexpected end of file" "$ERR"'
# 第二/三轮 review P1 #7 / #11 / P3 #30：自定义 IFS 分词、未引号 $@/$* 的
# 二次分词、以及多字段展开时「前缀只接首字段、后缀只接末字段」。
# 旧实现 split_ifs 硬编码空格/tab/换行、At/Star 不分词、且给每个字段都拼前缀。
# 这里同时锁住 IFS 非空白分隔符的连续/前导/尾随空字段规则。
run_case A ifs-colon-basic     'IFS=:; x="a:b:c"; printf "[%s]" $x; echo'
run_case A ifs-colon-empty     'IFS=:; x=":a:b:"; printf "[%s]" $x; echo'
run_case A ifs-colon-double    'IFS=:; x="a::b"; printf "[%s]" $x; echo'
run_case A ifs-colon-trailing  'IFS=:; x="a:b:"; printf "[%s]" $x; echo'
run_case A ifs-colon-only      'IFS=:; x=":"; printf "[%s]" $x; echo'
run_case A ifs-comma-empty     'IFS=,; x="a,,b,"; printf "[%s]" $x; echo'
run_case A ifs-empty-no-split  'IFS=; x="a b"; printf "[%s]" $x; echo'
run_case A ifs-unset-default   'unset IFS; x="a b"; printf "[%s]" $x; echo'
run_case A ifs-mixed-ws        'IFS=" :"; x="a : b"; printf "[%s]" $x; echo'
run_case A ifs-mixed-double    'IFS=" :"; x="a: :b"; printf "[%s]" $x; echo'
run_case A ifs-mixed-leading   'IFS=" :"; x=" :a: "; printf "[%s]" $x; echo'
run_case A ifs-custom-char     'IFS=x; v="axb"; printf "[%s]" $v; echo'
run_case A ifs-cmdsub          'IFS=:; y=$(echo "a:b"); printf "[%s]" $y; echo'
run_case A ifs-quoted-no-split 'IFS=:; x="a:b"; printf "[%s]" "$x"; echo'
run_case A dollar-at-unquoted  'set -- "a b" c; printf "[%s]" $@; echo'
run_case A dollar-star-unquoted 'set -- a b c; printf "[%s]" $*; echo'
run_case A dollar-at-quoted    'set -- "a b" c; printf "[%s]" "$@"; echo'
run_case A dollar-star-quoted  'set -- a b; printf "[%s]" "$*"; echo'
run_case A dollar-star-ifs     'IFS=:; set -- a b; printf "[%s]" "$*"; echo'
run_case A dollar-star-ifs-empty 'IFS=; set -- a b; printf "[%s]" "$*"; echo'
run_case A dollar-at-ifs-split 'set -- "a:b" c; IFS=:; printf "[%s]" $@; echo'
run_case A dollar-at-prefix    'set -- a b; printf "[%s]" pre$@; echo'
run_case A dollar-at-suffix    'set -- a b; printf "[%s]" $@suf; echo'
run_case A dollar-at-pre-suf   'set -- a b; printf "[%s]" pre$@suf; echo'
run_case A dollar-at-quoted-pre 'set -- a b; printf "[%s]" "pre$@"; echo'
run_case A scalar-split-prefix 'x="a b"; printf "[%s]" pre$x; echo'
run_case A scalar-split-suffix 'x="a b"; printf "[%s]" $x-suf; echo'
run_case A array-at-ifs        'a=(p q); IFS=:; printf "[%s]" "${a[*]}"; echo'
run_case A brace-after-split   'x="a b"; echo pre$x-suf'
run_case A for-in-ifs          'IFS=:; x="a:b"; for f in $x; do echo "F=$f"; done'
run_case A for-in-dollar-at    'set -- "a b" c; for f in $@; do echo "F=$f"; done'
run_case A for-in-quoted-at    'set -- "a b" c; for f in "$@"; do echo "F=$f"; done'
# 第二轮 review P1 #8：`${v/pat/rep}` 旧实现用字面 replacen，完全不支持 glob
# 模式。改为按 glob 匹配（最长优先）、支持 // 全局、#/% 锚定、\& 与 & 替换、
# 以及被反斜杠转义的元字符和分隔符。
run_case A patsub-class        'v="a1b2c3"; echo "${v/[0-9]/X}"'
run_case A patsub-class-all    'v="a1b2c3"; echo "${v//[0-9]/X}"'
run_case A patsub-literal      'v="abcabc"; echo "${v//a/X}"'
run_case A patsub-star         'v="abcdef"; echo "${v/b*/Q}"'
run_case A patsub-any-all      'v="abcdef"; echo "${v//?/Q}"'
run_case A patsub-range-all    'v="abcdef"; echo "${v//[a-c]/Q}"'
run_case A patsub-neg-all      'v="abc"; echo "${v//[!a]/X}"'
run_case A patsub-anchor-start 'v="abcabc"; echo "${v/#a/Q}"'
run_case A patsub-anchor-miss  'v="abc"; echo "${v/#b/X}"'
run_case A patsub-anchor-end   'v="abcabc"; echo "${v/%c/Q}"'
run_case A patsub-anchor-suffix 'v="abcabc"; echo "${v/%b*/Q}"'
run_case A patsub-escaped-slash 'v="a/b/c"; echo "${v/\//-}"'
run_case A patsub-escaped-slash-all 'v="a/b/c"; echo "${v//\//-}"'
run_case A patsub-escaped-star 'v="a*b"; echo "${v/\*/X}"'
run_case A patsub-escaped-any  'v="a?b"; echo "${v/\?/X}"'
run_case A patsub-escaped-bracket 'v="a[b"; echo "${v/\[/X}"'
run_case A patsub-delete       'v="abcabc"; echo "${v//b}"'
run_case A patsub-amp-match    'v="abc"; echo "${v/b/&}"'
run_case A patsub-amp-literal  'v="abc"; echo "${v/b/\&}"'
run_case A patsub-empty-star   'v="abc"; echo "[${v//*/Q}]"'
run_case A patsub-no-match     'v="abc"; echo "${v//z/X}"'
run_case A patsub-empty-value  'v=""; echo "[${v//a/X}]"'
run_case A patsub-utf8         'v="中文abc"; echo "${v/中/X}"'
run_case A patsub-utf8-class   'v="中文abc"; echo "${v//[!中]/X}"'
# 第三轮 review P2：`$` 后跟空白/标点/EOF 时 bash 保留字面 `$`，旧 lexer
# 用 read_var_name 把该字符当变量名首字符吞掉（"a$ b" -> "ab"）。
# 不把 $$ 放进闸门：PID 绑定到进程身份，属 GROUP K 的已知差异。
run_case A dollar-literal-space 'echo "a$ b"'
run_case A dollar-literal-only  'echo "$ "'
run_case A dollar-literal-eof   'echo a$'
run_case A dollar-literal-quoted 'echo "$"'
run_case A dollar-literal-dot   'echo "a$.b"'
run_case A dollar-literal-slash 'echo "a$/b"'
run_case A dollar-literal-colon 'echo "a$:b"'
run_case A dollar-literal-comma 'echo "a$,b"'
run_case A dollar-literal-prefix 'echo "pre$ mid"'
run_case A dollar-unset-var     'echo "x$z"'
run_case A dollar-var-still-ok  'x=5; echo "$x"'
run_case A dollar-cmdsub-still-ok 'echo "$(echo ok)"'
run_case A dollar-arith-still-ok 'echo "$((1+2))"'

# 第三轮 review P2：失败的算术展开是「整行致命错误」——当前命令不执行、
# 后续命令全部跳过、rc=1，而交互 shell 本身存活。旧实现只 eprintln 后返回
# 空串，导致 `echo $((1/0)); echo after` 仍然打印 after 且 rc=0。
# 这些用例是逐字节断言：normalize_stderr 只剥 shell 名前缀，消息正文、
# 行数、尾随换行都必须与 bash 完全相同。
run_case A arith-divzero          'echo $((1/0))'
run_case A arith-divzero-abort    'echo $((1/0)); echo after'
run_case A arith-syntax-abort     'echo $((1+)); echo after'
run_case A arith-divzero-in-word  'echo hi $((1/0)) bye; echo after'
run_case A arith-divzero-assign   'x=$((1/0)); echo "x=[$x]"; echo after'
run_case A arith-divzero-after-ok 'echo $((2+2)); echo $((1/0)); echo after'
run_case A arith-divzero-in-func  'f() { echo $((1/0)); }; f; echo after'
run_case A arith-divzero-in-if    'if echo $((1/0)); then echo yes; fi; echo after'
run_case A arith-divzero-or       'echo $((1/0)) || echo fallback; echo after'
run_case A arith-divzero-redir    'echo $((1/0)) > /dev/null; echo after'
run_case A arith-divzero-spaces   'echo $(( 1 / 0 ))'
run_case A arith-syntax-token     'echo $((1+*2))'
run_case A arith-negexp           'echo $((2**-1))'
run_case A arith-divzero-expr     'echo $((x=1/0))'
run_case A arith-let-still-ok     'let x=1/0; echo "x=$x rc=$?"'

# 数组 / 位置参数切片 `${a[@]:off:len}`、`${@:off:len}`。
# 位置参数切片从 $0 起算（${@:1:2} 得到 $1 $2），故不测 offset 0 —— 那会
# 绑定 shell 自身路径，属 GROUP K 身份差异。
run_case A slice-arr-basic        'a=(1 2 3); echo "${a[@]:1:2}"'
run_case A slice-arr-star         'a=(1 2 3); echo "${a[*]:1:2}"'
run_case A slice-arr-no-len       'a=(1 2 3); echo "${a[@]:1}"'
run_case A slice-arr-neg-off      'a=(1 2 3); echo "${a[@]: -2}"'
run_case A slice-arr-neg-off-len  'a=(1 2 3); echo "${a[@]: -2:1}"'
run_case A slice-arr-zero-one     'a=(1 2 3); echo "${a[@]:0:1}"'
run_case A slice-arr-arith-off    'a=(1 2 3); echo "${a[@]:1+1:1}"'
run_case A slice-arr-neg-len      'a=(1 2 3); echo "${a[@]:0:-1}"; echo after'
run_case A slice-arr-elem-sub     'a=(10 22 33); echo "${a[1]:0:1}"'
run_case A slice-pos-at           'set -- a b c d; echo "${@:2:2}"'
run_case A slice-pos-star         'set -- a b c d; echo "${*:2:2}"'
run_case A slice-pos-one          'set -- a b c d; echo "${@:1:2}"'
run_case A slice-pos-no-len       'set -- a b c d; echo "${@:3}"'
run_case A slice-pos-neg          'set -- a b c d; echo "${@: -2}"'
run_case A substr-neg-len         'v=abcdef; echo "${v:0:-1}"'
run_case A substr-neg-len-off     'v=abcdef; echo "${v:2:-1}"'
run_case A substr-neg-off         'v=abcdef; echo "${v: -3}"'
run_case A substr-neg-len-fatal   'v=abcdef; echo "${v:0:-10}"'

# `$[ expr ]`：bash 5.3 仍支持的旧式算术替换，等价于 `$(( expr ))`。
run_case A dollar-bracket-basic   'echo $[1+2]'
run_case A dollar-bracket-prec    'echo $[1+2*3]'
run_case A dollar-bracket-mid     'echo "a$[1+2]b"'
run_case A dollar-bracket-assign  'echo $[x=3]'
run_case A dollar-bracket-divzero 'echo $[1/0]'

# 第三轮 review P2：`>|` 强制截断（绕过 set -C）、`&>>` 追加 stdout+stderr、
# 以及「只有重定向、没有命令」的合法形式。executor 本就有
# apply_redirs_in_parent -> restore_fds -> 0 的路径，是 parser 误报语法错误。
# append 类用例一律先用 `>` / `&>` 截断，避免 bash、wash 两侧共享沙箱时
# 相互追加导致假 FAIL。
run_case A redir-clobber         'echo hi >| @SANDBOX@/r1; cat @SANDBOX@/r1'
run_case A redir-clobber-setC    'set -C; echo hi >| @SANDBOX@/r2; cat @SANDBOX@/r2'
run_case A redir-clobber-twice   'echo hi >| @SANDBOX@/r3; echo hi > @SANDBOX@/r3; cat @SANDBOX@/r3'
run_case A redir-noclobber       'set -C; echo seed >| @SANDBOX@/r4; echo hi > @SANDBOX@/r4; echo rc=$?'
run_case A redir-both-append     'echo one &> @SANDBOX@/r5; echo two &>> @SANDBOX@/r5; cat @SANDBOX@/r5'
run_case A redir-both-group      '{ echo out; echo err >&2; } &> @SANDBOX@/r6; cat @SANDBOX@/r6'
run_case A redir-both-app-group  '> @SANDBOX@/r7; { echo out; echo err >&2; } &>> @SANDBOX@/r7; cat @SANDBOX@/r7'
run_case A redir-no-command      '> @SANDBOX@/r8; echo "rc=$? exists=$([ -f @SANDBOX@/r8 ] && echo y)"'
run_case A redir-no-command-fd   '2> @SANDBOX@/r9; echo ok'
run_case A redir-no-command-app  '> @SANDBOX@/r10; echo one >> @SANDBOX@/r10; echo two &>> @SANDBOX@/r10; cat @SANDBOX@/r10'

# 赋值语句的前导赋值词整段不做 brace expansion（bash 规则），但普通命令的
# 参数、export 的参数仍然展开。旧 lexer 无条件把 {a,b} 结构化为 Brace，
# expand_scalar 用 alts.concat() 拼成 "ab"，丢失字面文本。
run_case A brace-assign-basic    'x={a,b}; echo "$x"'
run_case A brace-assign-prefix   'x=pre{a,b}; echo "$x"'
run_case A brace-assign-mid      'x=a{b,c}d; echo "$x"'
run_case A brace-assign-range    'x={1..3}; echo "$x"'
run_case A brace-assign-var      'v=Z; x=$v{a,b}; echo "$x"'
run_case A brace-assign-append   'x=1; x+={a,b}; echo "$x"'
run_case A brace-arg-not-assign  'echo x={a,b}'
run_case A brace-nested-assign   'a=x={a,b}; echo "$a"'
run_case A brace-export-expands  'export x={a,b}; echo "$x"'
run_case A brace-multi-assign    'x={a,b} y={c,d}; echo "$x $y"'
run_case A brace-plain-still     'echo {a,b}; echo pre{a,b}suf'
run_case A brace-quoted-still    'x="pre{a,b}"; echo "$x"'
run_case A brace-cmdsub-still    'x=$(echo {a,b}); echo "$x"'

# --- brace range 语义（#17a）------------------------------------------------
# 来源：QA 的 brace oracle 语料（92 条，/d/msys2/tmp/qa21/brace_matrix*.tsv）。
# 旧 parse_range 的 5 类缺陷在这里各有代表用例：
#   A 方向：{10..1} 曾展开成 0 个词（整词消失）、{1..3..-1}/{3..1..1} 零个词；
#   B step=0：{1..3..0} 曾不展开（bash 视为 step=1）；
#   C 字符 step：{a..c..2} 曾忽略 step 吐全量；
#   D 类型不一致：{1..a} 曾走字符分支吐 49 个词（该项要等 #17b 的 {/} 词边界
#     修完才能入闸，当前 wash 输出 `{` + `1..a}` 两个词，故暂不纳入本题）；
#   E 补零：{01..05} 曾丢前导零，宽度规则 = max(两端字面量长度)（符号计入）。
# 所有期望值均由 MSYS2 bash 5.3 实测（printf 包裹，逐字节比 stdout）。
run_case A brace-range-desc        'printf "<%s>|" {10..1}; echo'
run_case A brace-range-negstep     'printf "<%s>|" {1..3..-1}; echo'
run_case A brace-range-desc-posstep 'printf "<%s>|" {3..1..1}; echo'
run_case A brace-range-cross-zero  'printf "<%s>|" {1..-3..2}; echo'
run_case A brace-range-neg-both    'printf "<%s>|" {-1..-5..-2}; echo'
run_case A brace-range-step0       'printf "<%s>|" {1..3..0}; echo'
run_case A brace-range-step-neg0   'printf "<%s>|" {5..1..-0}; echo'
run_case A brace-range-char-step0  'printf "<%s>|" {a..c..0}; echo'
run_case A brace-range-char-step2  'printf "<%s>|" {a..c..2}; echo'
run_case A brace-range-char-step5  'printf "<%s>|" {a..z..5}; echo'
run_case A brace-range-char-rev    'printf "<%s>|" {z..a}; echo'
run_case A brace-range-char-mixstep 'printf "<%s>|" {A..Z..7}; echo'
run_case A brace-range-pad-basic   'printf "<%s>|" {01..05}; echo'
run_case A brace-range-pad-max     'printf "<%s>|" {1..05}; echo'
run_case A brace-range-pad-3wide   'printf "<%s>|" {08..010}; echo'
run_case A brace-range-pad-neg     'printf "<%s>|" {-05..5}; echo'
run_case A brace-range-pad-zero    'printf "<%s>|" {0..00}; echo'
run_case A brace-range-pad-desc    'printf "<%s>|" {010..1}; echo'
run_case A brace-range-pad-b1      'printf "<%s>|" {0001..3}; echo'
run_case A brace-range-pad-b2      'printf "<%s>|" {1..0009}; echo'
run_case A brace-range-pad-b3      'printf "<%s>|" {-005..5..2}; echo'
run_case A brace-range-pad-b4      'printf "<%s>|" {01..010}; echo'
run_case A brace-range-pad-b5      'printf "<%s>|" {-01..-010}; echo'
run_case A brace-range-pad-none    'printf "<%s>|" {1..200}; echo'
run_case A brace-range-pad-plus    'printf "<%s>|" {+05..9}; echo'
run_case A brace-range-quoted-dq   'printf "<%s>|" "{1..3}"; echo'
run_case A brace-range-quoted-sq   "printf '<%s>|' '{1..3}'; echo"
run_case A brace-range-escaped     'printf "<%s>|" \{1..3\}; echo'
run_case A brace-range-var-quoted  'X="{1..3}"; printf "<%s>|" "$X"; echo'
run_case A brace-range-export      'export x={1..3}; echo "$x"'
run_case A brace-range-assign-lit  'x={10..1}; echo "$x"'
run_case A brace-range-overflow-val 'printf "<%s>|" {9223372036854775807..9223372036854775807}; echo'
run_case A brace-range-overflow-step 'printf "<%s>|" {1..10..9223372036854775807}; echo'

# --- brace 词边界（#17b）----------------------------------------------------
# bash 只在 `{` / `}` 自成 token（相邻字符是分词分隔符）时把它当保留字；
# 与任何字符粘连时它是普通字面字符，整词保留 1 个词。旧 lexer 无条件把词首的
# `{` 切成独立 Token::Word("{")，于是 `{abc}` 变成 `{ abc}`、`{1..a}` 变成
# `{ 1..a}`，并且 `{echo hi;}` 被误当成合法命令组（把语法错误当成功执行）。
run_case A brace-literal-abc      'printf "<%s>|" {abc}; echo'
run_case A brace-literal-empty    'printf "<%s>|" {}; echo'
run_case A brace-literal-one      'printf "<%s>|" {a}; echo'
run_case A brace-literal-float    'printf "<%s>|" {1.5..3.5}; echo'
run_case A brace-literal-sandwich 'printf "<%s>|" }{1..3}{; echo'
run_case A brace-literal-mixed    'printf "<%s>|" {1..a}; echo'
run_case A brace-literal-mixed-rev 'printf "<%s>|" {a..1}; echo'
run_case A brace-literal-open     'printf "<%s>|" {1..}; echo'
run_case A brace-literal-close    'printf "<%s>|" {..3}; echo'
run_case A brace-literal-step-empty 'printf "<%s>|" {1..3..}; echo'
run_case A brace-literal-step-x   'printf "<%s>|" {1..3..x}; echo'
run_case A brace-literal-mixed-step 'printf "<%s>|" {1..a..2}; echo'
run_case A brace-literal-4parts   'printf "<%s>|" {1..3..2..4}; echo'
run_case A brace-literal-hex      'printf "<%s>|" {0x10..0x12}; echo'
run_case A brace-glue-close2      'echo }}x'
run_case A brace-glue-a-close     'echo a}b'
run_case A brace-glue-open-tail   'echo x{a'
run_case A brace-glue-arg-close   'echo }'
# `{echo` 是一个普通词（没有分隔符），所以这里的 `}` 没有配对 -> bash 报语法错误。
# 期望值含 rc=2 与逐字节 stderr，专门用来证明修的是词法边界而不是打了个特判。
run_case A brace-group-no-space   '{echo hi;}'
run_case A brace-group-tight-close '{ echo hi;}'
run_case A brace-group-semicolon  '{ echo hi ; }; echo rc=$?'
run_case A brace-group-cmd-word   '{}; echo rc=$?'
run_case A brace-funcdef-tight    'f(){ echo hi; }; f'
run_case A brace-funcdef-space    'f() { echo hi; }; f'

# --- brace 嵌套 / 逗号转义 / quote removal（#25）-----------------------------
# bash 的 brace 展开是「文本式 + 最内层先展开 + 展开后做 quote removal」：
#   * 由内向外：`{a,{b,c}}` → a b c；`{{a,b}}` → `{a}` `{b}`；`{a{b,c}d}` → `{abd}` `{acd}`；
#   * 逗号切分尊重 `\,` 与引号：`{a\,b}`（无顶层逗号）是**字面量 1 词** `{a,b}`，
#     `{a\,b,c}` 切出 `a,b` + `c`，`{a\{b,c\},d}` 切出 `a{b` `c}` `d`；
#   * 引号内的 `{` 不参与展开：`{a,"{b,c}"}` → `a` + `{b,c}`（2 词）；
#   * 每个备选词最后过 quote removal：`{a,"b,c"}` → `b,c`、`{a,b\ c}` → `b c`。
run_case A brace-nest-basic       'printf "<%s>|" {a,{b,c}}; echo'
run_case A brace-nest-suffix      'printf "<%s>|" {a,{b,c}}d; echo'
run_case A brace-nest-pairs       'printf "<%s>|" {{a,b},{c,d}}; echo'
run_case A brace-nest-infix       'printf "<%s>|" {a{b,c}d}; echo'
run_case A brace-nest-three       'printf "<%s>|" {a,{b,{c,d}}}; echo'
run_case A brace-nest-three-brace 'printf "<%s>|" {{{a,b}}}; echo'
run_case A brace-nest-range       'printf "<%s>|" {{1..3},x}; echo'
run_case A brace-nest-order       'printf "<%s>|" x{a,{b,c}}y{1,2}; echo'
run_case A brace-nest-quoted      'printf "<%s>|" {a,"{b,c}"}; echo'
run_case A brace-esc-comma-lit    'printf "<%s>|" {a\,b}; echo'
run_case A brace-esc-comma-split  'printf "<%s>|" {a\,b,c}; echo'
run_case A brace-esc-brace-comma  'printf "<%s>|" {a\{b,c\},d}; echo'
run_case A brace-esc-space        'printf "<%s>|" {a,b\ c}; echo'
run_case A brace-quoted-comma     'printf "<%s>|" {a,"b,c"}; echo'
# `{A..z}` 的第 28 个码点是 0x5C：bash 的 quote removal 把它吃掉，元素变成**空词**
# （词数仍是 58）。这条锁定「展开层 quote removal」而不是「把反斜杠原样留下」。
run_case A brace-backslash-empty  'set -- {A..z}; printf "n=%d 27=[%s] 28=[%s] 29=[%s]" $# "${27}" "${28}" "${29}"; echo'
# #30 P0 回归栅栏：嵌套 brace 的展开深度曾等于 Rust 调用栈深度，约 1700 层即
# 爆栈（rc=139 / `thread 'main' has overflowed its stack`）。迭代化后必须与
# oracle 展开出**同样多的词**并逐字节相同；这里用 while 循环现场构造 2000 层
# `{a,` + `z` + 2000 层 `}`，`eval` 触发 brace 展开（4002 字节 / 2001 词）。
# 承重证据：改前主仓二进制 rc=139 + 0 字节输出，改后与 bash sha256 相同。
run_case A brace-deep-2000       's=z; i=0; while [ $i -lt 2000 ]; do s="{a,$s}"; i=$((i+1)); done; eval "echo $s"'

# --- round 7 fence: unterminated compound commands & `>;` -------------------
# 20 cases mirroring QA qa9_oracle.tsv GROUP A (bash 5.3.20 oracle, LC_ALL=C).
# C-family template: `-c: line <n_lines+1>: syntax error: unexpected end of
# file from `<kw>' command on line <open_line>` (open_line = INNERMOST opener,
# heredoc body lines counted), single line, no echo, rc=2.  An unterminated
# heredoc is a warning + rc=0 with the body executed.  Priority: B-family
# delimiters > CmdsubEof > compound-EOF > near-token; a literal `;` right
# after a target-less redirection always wins (bash streams the parse).
run_case A compound-eof-if-single     'if true; then'
run_case A compound-eof-if-body       'if true; then echo hi'
run_case A compound-eof-if-multi      'if true; then
echo hi'
run_case A near-token-redirect-semi   'echo hi >; echo after'
run_case A near-token-redirect-eof    'echo hi >'
run_case A near-token-redirect-multi  'echo a
 echo hi >; echo after'
run_case A compound-eof-while         'while true; do'
run_case A compound-eof-for           'for x in 1; do'
run_case A compound-eof-case          'case x in'
run_case A compound-eof-group         '{ echo hi'
run_case A compound-eof-subshell      '( echo hi'
run_case A compound-eof-nested-if     'if true; then
if true; then
echo hi'
run_case A compound-eof-hd-then-if    'cat <<EOF
hello
EOF
if true; then'
run_case A compound-eof-if-hd         'if true; then
cat <<EOF
hello
EOF'
run_case A heredoc-eof-warning        'cat <<EOF
hello
'
run_case A prio-compound-backtick     'if true; then
echo `echo hi'
run_case A prio-compound-cmdsub       'if true; then
echo $(echo hi'
run_case A prio-near-token-backtick   'echo hi > `echo x'
run_case A prio-compound-brace        'while true; do
echo ${x'
run_case A compound-closed-control    'if true; then
  echo ok
fi'

# --- 特殊参数 `$-`（当前 shell 选项标志串）-----------------------------------
# 全部期望值均先用 MSYS2 bash 5.3 实测（本套件固定 `-c`，故末尾恒有 `c`）。
# 字母固定顺序（实测 `set -abefhkmptuvxBCEHPT` -> `abefhkmptuvxBCEHPTc`）：
#   a b e f h i k m p t u v x B C E H P T c s
# 默认 `hashall`/`braceexpand` 开，故裸 `echo "$-"` 为 `hBc`。
# 含 `set -x`/`-v` 的组合会引入 xtrace/verbose 的 stderr，除非能真正对齐，
# 否则不纳入闸门；下列用例 stderr 均为空。
run_case A df-basic              'echo "$-"'                       # hBc
run_case A df-set-eu             'set -eu; echo "$-"'              # ehuBc
run_case A df-set-plus-B         'set +B; echo "$-"'               # hc
run_case A df-set-plus-h         'set +h; echo "$-"'               # Bc
run_case A df-set-C              'set -C; echo "$-"'               # hBCc
run_case A df-set-m              'set -m; echo "$-"'               # hmBc
run_case A df-set-a              'set -a; echo "$-"'               # ahBc
run_case A df-set-f              'set -f; echo "$-"'               # fhBc
run_case A df-set-BCEHPT         'set -BCEHPT; echo "$-"'          # hBCEHPTc
run_case A df-set-plus-h-B       'set +h +B; echo "$-"'            # c
run_case A df-braced             'echo "${-}"'                     # hBc（花括号形态）
run_case A df-set-aef            'set -aef; echo "$-"'             # aefhBc
run_case A df-set-eum            'set -eum; echo "$-"'             # ehmuBc
run_case A df-set-all            'set -aefhmuBCEHPT; echo "$-"'    # aefhmuBCEHPTc
run_case A df-set-b              'set -b; echo "$-"'               # bhBc (notify)
run_case A df-set-k              'set -k; echo "$-"'               # hkBc (keyword)
run_case A df-set-p              'set -p; echo "$-"'               # hpBc (privileged)

# --- nounset 差异 2 修复：$*/${*} 等 special param 在 set -u 下不报 unbound ---
# bash 5.3 实测：下列在 set -u 下 rc=0、stdout 如注释、无诊断（与 $1 不同）。
run_case A ns2-bare-star       'set -u; echo "[$*]"'             # [](空)
run_case A ns2-bare-at         'set -u; echo "[$@]"'             # [](空)
run_case A ns2-bstar           'set -u; echo "[${*}]"'           # [](空)
run_case A ns2-bat             'set -u; echo "[${@}]"'           # [](空)
run_case A ns2-bq              'set -u; echo "[${?}]"'           # [0]
run_case A ns2-bdash           'set -u; echo "[${-}]"'           # [huBc]（当前 $- 含 u）
# --- 差异 1 修复：set -u 下算术展开引用 UNSET 具名变量须 FATAL unbound variable（rc 127）---
run_case A ns1-arith-unbound    'set -u; echo $((NOPE+1))'        # rc=127, 无 stdout, stderr "NOPE: unbound variable"
# set -t 应置 onecmd，使 $- 含 t（bash 5.3 实测 htBc）
run_case A set-t-dash          'set -t; echo "$-"'               # htBc

###############################################################################
# --- 差异 3 修复：pipeline 各 stage 在子进程内展开自身词，unbound 只杀该 stage ---
run_case A nounset-pipe-stage-unbound 'set -u; echo $NOPE | cat; echo after'   # rc=0, 打 after, stderr "NOPE: unbound variable"
run_case A nounset-pipe-stage-arith   'echo $((1/0)) | cat; echo after'        # rc=0, 打 after, 算术失败只杀该 stage
run_case A nounset-standalone-aborts  'set -u; echo $NOPE; echo after'         # rc=127, 不打印 after（单命令仍整行中止）
# --- 差异 4 修复：命令替换继承 set -u、退出状态传播、$? 与 RHS 只展开一次 ---
run_case A ns4-cmdsub-unbound    'set -u; echo x$(echo $NOPE)y'   # rc=0, stdout xy, stderr "NOPE: unbound variable"
run_case A ns4-cmdsub-status-false 'v=$(false); echo $?'          # 1
run_case A ns4-cmdsub-status-true  'v=$(true); echo $?'           # 0
run_case A ns4-cmdsub-rhs-once   'v=$(printf OOPS >&2); echo done'  # 只打 1 条 OOPS, stdout done
run_case A ns4-cmdsub-unbound-status 'set -u; out=$(echo $UNSET); echo "rc=$?"'  # rc=1, stderr "UNSET: unbound variable"
# --- 差异 5 修复：$! / ${!} 在 set -u 下须 FATAL unbound variable（rc 127）---
# 这是差异 2 修复（f07ae61）的「反例」：$* $@ $# $? $- $0 $_ 等 special param
# 在 set -u 下不报 unbound，但 $! / ${!} / ${!*} / ${!@} 仍须 fatal，与 bash 一致。
run_case A ns5-bang-unbound        'set -u; echo "x$!y"'            # rc=127, 无 stdout, stderr "$!: unbound variable"
run_case A ns5-bang-braced-unbound 'set -u; echo "${!}"'            # rc=127, stderr "!: unbound variable"
run_case A ns5-bang-star-unbound   'set -u; echo "${!*}"'           # rc=127, stderr "!*: unbound variable"
run_case A ns5-bang-at-unbound     'set -u; echo "${!@}"'           # rc=127, stderr "!@: unbound variable"
run_case A ns5-bang-empty          'set +u; echo "x$!y"'            # rc=0, stdout xy（无 bg job 时展开为空）
run_case A ns5-bang-braced-empty   'set +u; echo "${!}"'            # rc=0, 无 stdout（展开为空）
# 差异 5 的正向锚点（有后台作业时 $! / ${!} 必须展开为 pid，不得再报 unbound）。
# 这些用例用纯内建（true & + wait / [ -n ] / $(...）触发，避免打印随机 pid 导致
# 字节不等；用 wait "$!" / wait "${!}" 消费掉 pid，或用 [ -n "$!" ] 判定其非空。
run_case A ns5-bang-with-job       'set -u; true & wait "$!"; echo "rc=$?"'   # rc=0, stdout rc=0
run_case A ns5-bang-braced-with-job 'set -u; true & wait "${!}"; echo "rc=$?"' # rc=0, stdout rc=0（${!}≡$!）
run_case A ns5-bang-cmdsub         'set -u; true & v=$([ -n "$!" ] && echo set); echo "v=$v"' # rc=0, stdout v=set（穿透到 $(...)）
run_case A ns5-bang-pipeline       'set -u; true & : | if [ -n "$!" ]; then echo yes; fi; echo "p=$?"' # rc=0, stdout yes / p=0（穿透到 fork 复合命令）
# --- code review P3：修复 ${!VAR} / ${!prefix@} / ${!prefix*} / ${!#} 此前被「间接展开分支」
#     静默吞成空串（rc 也错）的三类缺口 ---
# ${!prefix@} / ${!prefix*}：列出所有以 prefix 开头（标量或数组）的变量名，排序后用空格（@）
# 或 $IFS 首字符（*）连接。用罕见前缀 MYV 避免与环境变量碰撞；并列 Bash 自身为 oracle。
run_case A bang-prefix-at-space    'MYV=1; MYVA=2; MYVB=3; echo "[${!MYV@}]"'        # [MYV MYVA MYVB]
run_case A bang-prefix-star-ifs    'IFS=:; MYV=1; MYVA=2; MYVB=3; echo "[${!MYV*}]"' # [MYV:MYVA:MYVB]
run_case A bang-prefix-at-array    'MYA=1; MYAR=(x y); echo "[${!MYA@}]"'           # [MYA MYAR]
# ${!arr[@]} / ${!arr[*]}：数组键（下标）列表 —— 不是前缀变量名列表。bash 规则（全部 rc0，
# 且**不受 set -u 影响**：未设置的名字展开为空串）：数组 N 元素 -> "0 1 ... N-1"（升序），
# 标量 -> "0"（标量即单元素数组），未设置 -> 空。[@] 恒用空格连接，[*] 用 $IFS 首字符。
run_case A bang-arr-keys-basic     'A=(x y z); echo "[${!A[@]}]"'                   # [0 1 2]
run_case A bang-arr-keys-star-ifs  'A=(x y); IFS=:; echo "[${!A[*]}]"'              # [0:1]
run_case A bang-arr-keys-at-ifs    'A=(x y); IFS=:; echo "[${!A[@]}]"'              # [0 1]（[@] 不受 IFS 影响）
run_case A bang-arr-keys-empty     'A=(); echo "[${!A[@]}]"'                        # []（空数组 -> 空）
run_case A bang-arr-keys-unset     'echo "[${!U[@]}]"'                              # []（未设置 -> 空 rc0）
run_case A bang-arr-keys-set-u     'set -u; echo "[${!U[@]}]"'                      # []（set -u 下仍 rc0，不报 unbound）
run_case A bang-arr-keys-scalar    'A=hello; echo "[${!A[@]}]"'                     # [0]（标量即第 0 元素）
# ${!#} 是 $0 的另一种写法：对 bash / wash 各自成立（语义不变量，不比字面量）。
run_case_sem A bang-hash-eq-zero   'echo "${!#}"; echo "$0"' '[ "$(sed -n 1p "$OUT")" = "$(sed -n 2p "$OUT")" ]'
# ${!VAR} 间接展开（rc 1，非 set -u 的 127）：非法时打印 "VAR: invalid indirect expansion" /
# "VAR: invalid variable name" / ": invalid variable name"，且 stdout 为空。
run_case A bang-indirect-unset     'unset U; echo "x${!U}y"'      # rc=1, 空 stdout, "U: invalid indirect expansion"
run_case A bang-indirect-empty-name 'FOO=""; echo "x${!FOO}y"'    # rc=1, 空 stdout, ": invalid variable name"
run_case A bang-indirect-bad-name   'BAR=1bad; FOO=BAR; echo "x${!FOO}y"' # rc=1, "1bad: invalid variable name"
run_case A bang-indirect-valid     'BAR=hello; FOO=BAR; echo "[${!FOO}]"' # [hello] rc=0
run_case A bang-indirect-empty-target 'FOO=bar; echo "[${!FOO}]"'          # [  ] rc=0（bar 未设置，间接展开为空）
run_case A bang-indirect-set-u     'set -u; unset U2; echo "x${!U2}y"'    # rc=1, 与 set -u 无关，仍是 invalid indirect
# --- heredoc / herestring（`<<` / `<<-` / `<<<`）GROUP A 全语义回归栅栏 ------
# 每一条都先用 MSYS2 bash 5.3.20 oracle 实测，再走主闸门既有 normalize_stderr
# 口径逐字节比较 rc + stdout + stderr。case 文本用跨行双引号保留真实换行，
# `\$` 保护内层 shell 的 `$`，`HD_TAB` / `HD_SP` 提供真实 tab / 行尾空格。
# 覆盖：basic 3 形态 / `<<-` tab / quoted 定界符（含部分引号 E'OF'）/ 未引号
# 展开 / herestring / 同行多重定向 / heredoc 压管道 stdin / 双 heredoc 后者胜 /
# 定界符在 body 中间 / 空 body + rc 传播 / 嵌套上下文 EOF 告警（hd-061/062）。
HD_TAB="$(printf '\t')"
HD_SP=" "

run_case A heredoc-basic-1 "cat <<EOF
hello
EOF"
run_case A heredoc-basic-multi "cat <<EOF
a
b
c
EOF"
run_case A heredoc-basic-empty "cat <<EOF
EOF"
run_case A heredoc-empty-rc "cat <<EOF
EOF
echo \"rc=\$?\""

run_case A heredoc-minus-tab "cat <<-EOF
${HD_TAB}tabbed
EOF"
run_case A heredoc-minus-spaces "cat <<-EOF
  spaces
EOF"
run_case A heredoc-minus-delim-tab "cat <<-EOF
x
${HD_TAB}EOF"

run_case A heredoc-quoted-single "x=VAL; cat <<'EOF'
\$x
EOF"
run_case A heredoc-quoted-double "x=VAL; cat <<\"EOF\"
\$x
EOF"
run_case A heredoc-quoted-partial "x=VAL; cat <<E'OF'
\$x
EOF"
run_case A heredoc-quoted-cmdsub "cat <<'EOF'
\$(echo hi)
EOF"
run_case A heredoc-quoted-backtick "cat <<'EOF'
\`echo hi\`
EOF"

run_case A heredoc-unq-var "x=VAL; cat <<EOF
\$x
EOF"
run_case A heredoc-unq-cmdsub "cat <<EOF
\$(echo hi)
EOF"
run_case A heredoc-unq-backtick "cat <<EOF
\`echo hi\`
EOF"
run_case A heredoc-unq-escaped-dollar "x=VAL; cat <<EOF
\\\$x
EOF"
run_case A heredoc-unq-double-backslash "x=VAL; cat <<EOF
\\\\\$x
EOF"
run_case A heredoc-unq-line-cont "cat <<EOF
one \\
two
EOF"

run_case A herestring-simple "cat <<<hello"
run_case A herestring-quoted-space "x=\"a b\"; cat <<<\"\$x\""
run_case A herestring-var "x=VAL; cat <<<\$x"
run_case A herestring-extra-arg "printf '<%s>\\n' <<<one two"

run_case A heredoc-mred-devnull "cat <<EOF >/dev/null
hi
EOF"
run_case A heredoc-mred-file "cat > @SANDBOX@/_hd_f1 <<EOF
hi
EOF
cat @SANDBOX@/_hd_f1"
run_case A heredoc-mred-wc "wc -c <<EOF > @SANDBOX@/_hd_f1
hello
EOF
cat @SANDBOX@/_hd_f1"

run_case A heredoc-pipe-stdin "cat <<EOF | wc -c
hello
EOF"
run_case A heredoc-pipe-override "printf 'a\\n' | cat <<EOF
b
EOF"
run_case A heredoc-double-last-wins "cat <<A <<B
first
A
second
B"

run_case A heredoc-delim-in-body-indent "cat <<EOF
  EOF
x
EOF"
run_case A heredoc-delim-prefix "cat <<EOF
EOFx
x
EOF"
run_case A heredoc-delim-trailing-space "cat <<EOF
EOF${HD_SP}
x
EOF"
run_case A heredoc-delim-case "cat <<EOF
eof
x
EOF"

run_case A heredoc-rc-propagate "cat <<EOF
false
EOF
echo \$?"
run_case A heredoc-rc-devnull "cat <<EOF >/dev/null
x
EOF
echo \$?"
run_case A herestring-rc "cat <<<hi >/dev/null; echo \$?"

# 第 2 回合修复的回归栅栏：子 shell / 命令替换里未闭合 heredoc 的 EOF 告警。
# 主闸门 normalize_stderr 剥掉 shell 名 + 可选 `line N:` 前缀后，正文逐字节相等；
# 若告警被吞掉，这两条会立刻 FAIL，是本回合的承重用例。
run_case A heredoc-eof-warn-subshell "( cat <<EOF
hi
EOF )"
run_case A heredoc-eof-warn-cmdsub "x=\$(cat <<EOF
hi
EOF); echo \"\$x\""

# --- 未闭合 `$(` 语法错（任务板 #33 / 派单 #32）GROUP A 回归栅栏 ------------
# bash 5.3.20 实测：命令替换 `$(` 缺匹配 `)` 时 rc=2 + 单行诊断
#   `-c: line N: unexpected EOF while looking for matching `)'`（N = 输入行数 + 1）；
# hd-073 还会先打 here-document EOF warning，故 stderr 为两行且顺序固定。
# 对照条目锁定「不许破坏」：闭合 cmdsub / 单引号内 `$(` 字面量 / 双引号内闭合
# cmdsub / `( )` 子 shell 未闭合的既有 rc=2。case 文本经 round4_probe.sh 按主
# 闸门 normalize_stderr 口径 oracle 预检后写入。
run_case A unclosed-cmdsub-echo "echo \$(echo hi"
run_case A unclosed-cmdsub-assign "x=\$(echo hi"
run_case A unclosed-cmdsub-heredoc "echo \$(cat <<EOF
x"
run_case A unclosed-cmdsub-nested "echo \$(echo \$(echo hi"
run_case A unclosed-cmdsub-second "echo \$(echo hi) \$(echo bye"
run_case A unclosed-cmdsub-heredoc-closed "echo \$(cat <<EOF
hi
EOF"
run_case A unclosed-cmdsub-empty "echo \$(
"
run_case A cmdsub-closed-control "echo \$(echo hi)"
run_case A cmdsub-single-quote-literal "echo '\$(echo hi)'"
run_case A subshell-unclosed-control "( echo hi"
run_case A cmdsub-double-quote-closed "echo \"\$(echo hi)\""

# --- 重定向无目标语法错 / 逐行部分执行（任务板 #34）GROUP A 回归栅栏 ------
# 判据：QA syntax_oracle.tsv 类 B sy-b01..b08（只读）；主闸门 normalize_stderr
# 口径逐字节比较 rc + stdout + stderr。b07 用 @SANDBOX@ 隔离副作用文件。
# 两套行号：near unexpected token 族用出错行真实行号（b01=1 / b08=2 /
# b02=3，heredoc body 计入）；unexpected EOF 族用 lines+1（#33，未改）。
run_case A redir-no-target-out "echo hi >"
run_case A redir-no-target-heredoc "cat <<EOF >
x
EOF"
run_case A redir-no-target-in "echo x <"
run_case A redir-no-target-append "echo x >>"
run_case A redir-no-target-fd "echo x 2>"
run_case A redir-no-target-pipe "echo x |"
run_case A redir-standalone-control "> @SANDBOX@/_sy_f1"
run_case A redir-no-target-partial "echo ok
echo x >
echo after"
# 逐行部分执行根因加固：与 #34 同源的第 4 回合遗留多行 DIFFER，以及三行前缀。
run_case A redir-partial-cmdsub "echo \$(echo hi)
echo \$(echo bye"
run_case A redir-partial-three "echo one
echo two
echo x >"
run_case A redir-trailing-andand "echo hi &&"

# --- 未闭合定界符异族（任务板 #33/#34 之后，第 6 回合）GROUP A 回归栅栏 ------
# 判据：QA syntax_oracle_r6.tsv（只读，26 条，sha 6169e65b...）；主闸门
# normalize_stderr 口径逐字节比较 rc + stdout + stderr。本回合修 4 类「未闭合
# 定界符异族」（B 族 other-delimiter-eof）行号：反引号 / `${` / `$((` / 双引号内
# `$(` 字面量未闭合；行号 = 未闭合构造起始行（open_line），echo=None，诊断正文
# 逐字节等于 bash。多行变体行号随起始行变化（line1 / line2）。闭合对照锁定
# 「不许破坏」既有合法行为。case 文本经 round6_oracle.py 按主闸门口径 oracle
# 预检后写入（26/26 MATCH）。
run_case A backtick-unclosed-single 'echo `echo hi'
run_case A brace-unclosed-single   'echo ${foo'
run_case A arith-unclosed-single   'echo $((1+2'
run_case A dquote-cmdsub-unclosed  'echo "$(echo hi"'
run_case A backtick-unclosed-multi 'echo ok
echo `echo hi'
run_case A brace-unclosed-multi    'echo ok
echo ${foo'
run_case A backtick-closed-control 'echo `echo hi`'
run_case A brace-closed-control    'echo ${foo}'
run_case A arith-closed-control    'echo $((1+2))'
run_case A dquote-cmdsub-closed-control 'echo "$(echo hi)"'
run_case A brace-assign-closed-control  'x=${y}'

# --- 命令执行语义（PATH 空串/未设置/空元素 + ENOEXEC 四类 + exec 内建）------
# oracle = MSYS2 bash 5.3.20（LC_ALL=C）逐字节对拍，本节 22 条。
# PATH 语义：未设置 → 对裸名直接 execve（ENOENT 报
# `No such file or directory`，rc=127，不是 command not found）；已设置 →
# 冒号切分逐目录找「存在的 regular file」（执行位不参与，空元素 = 当前
# 目录）；命令前缀赋值（`PATH=... cmd`）在查找之前应用。
run_case A path-unset-bare       'unset PATH; ls; echo "rc=$?"'
run_case A path-empty-notfound   'PATH= ls; echo "rc=$?"'
run_case A path-empty-cwd-miss   'PATH= no-such-cmd-zz; echo "rc=$?"'
run_case A path-colon-cwd        'PATH=: no-such-cmd-zz; echo "rc=$?"'
run_case A path-baddir-notfound  'PATH=/nonexistent-dir ls; echo "rc=$?"'
run_case A path-assign-found     'PATH=@SANDBOX@/execfix/pdir1 nonexec; echo "rc=$?"'
run_case A path-stage-pipe       'PATH= no-such-cmd-zz | wc -l; echo "rc=$?"'
run_case A path-unset-direct-ok  'unset PATH; @SANDBOX@/execfix/plain; echo "rc=$?"'
# ENOEXEC 四类：① shebang 解释器缺失 → `<脚本>: <解释器>: bad interpreter:
# <strerror>`（无行号前缀，rc=126）；② ELF 魔数 / 头部 NUL →
# `cannot execute binary file: Exec format error`（rc=126）；③ 目录 →
# `Is a directory`（rc=126）；④ 无 shebang 纯文本 → 作为 shell 脚本执行。
run_case A enoexec-badinterp     '@SANDBOX@/execfix/badinterp; echo "rc=$?"'
run_case A enoexec-elf           '@SANDBOX@/execfix/elf; echo "rc=$?"'
run_case A enoexec-nul           '@SANDBOX@/execfix/nulf; echo "rc=$?"'
run_case A enoexec-text-script   '@SANDBOX@/execfix/plain; echo "rc=$?"'
run_case A enoexec-shebang-ok    '@SANDBOX@/execfix/goodinterp; echo "rc=$?"'
# 直接路径的 errno 分类：ENOENT → `No such file or directory`（127）。
run_case A exec-enoent           '/nonexistent-zz-washfix; echo "rc=$?"'
run_case A exec-isdir            '@SANDBOX@/execfix/pdir1; echo "rc=$?"'
# exec 内建：非交互 shell 失败即终止（AFTER 不打印），rc=127/126；
# 文本脚本回退在子 shell 里跑完并以其状态终止（rc=0 也不打印 AFTER）。
run_case A exec-builtin-enoent   'exec /nonexistent-zz-washfix; echo AFTER'
run_case A exec-builtin-pathmiss 'exec no-such-cmd-zz; echo AFTER'
run_case A exec-builtin-isdir    'exec @SANDBOX@/execfix/pdir1; echo AFTER'
run_case A exec-builtin-elf      'exec @SANDBOX@/execfix/elf; echo AFTER'
run_case A exec-builtin-badinterp 'exec @SANDBOX@/execfix/badinterp; echo AFTER'
run_case A exec-builtin-script   'exec @SANDBOX@/execfix/plain; echo AFTER'
run_case A exec-builtin-ok       'exec @SANDBOX@/execfix/goodinterp; echo AFTER'
run_case A exec-builtin-subshell '(exec /nonexistent-zz-washfix); echo "rc=$?"'
# command 内建与普通命令走同一套搜索/分类器。
run_case A command-enoent        'command /nonexistent-zz-washfix; echo "rc=$?"'
run_case A command-pathmiss      'PATH=/nonexistent-dir command ls; echo "rc=$?"'

# --- builtins 语义（kill 信号 0 / ulimit / source / 写错误 / read 字节计数）----
# oracle = MSYS2 bash 5.3.20（LC_ALL=C）逐字节对拍，本节 22 条。
# `read -n/-N` 按裸字节计数（不按 UTF-8 字符边界）；内建 stdout 写失败
# （/dev/full）报 `<builtin>: write error: <strerror>` rc=1。
printf '\344\270\255\346\226\207ab\n' > "$SANDBOX/execfix/utf8"
run_case A kill-zero-self       'kill -0 $$; echo "rc=$?"'
run_case A kill-zero-dead       'kill -0 999999; echo "rc=$?"'
run_case A kill-zero-usage      'kill -0; echo "rc=$?"'
run_case A kill-s-zero          'kill -s 0 $$; echo "rc=$?"'
run_case A kill-n-zero          'kill -n 0 $$; echo "rc=$?"'
run_case A kill-l-zero          'kill -l 0; echo "rc=$?"'
run_case A kill-l-130           'kill -l 130; echo "rc=$?"'
run_case A kill-badpid          'kill -0 abc; echo "rc=$?"'
run_case A ulimit-range         'ulimit -f 99999999999999999999; echo "rc=$?"'
run_case A ulimit-hex           'ulimit -f 0x10; echo "rc=$?"'
run_case A ulimit-invalid       'ulimit -f abc; echo "rc=$?"'
run_case A ulimit-boundary      'ulimit -f 18014398509481984; echo "rc=$?"'
run_case A source-dir           'source @SANDBOX@/execfix/pdir1; echo "rc=$?"'
run_case A echo-devfull         'echo hi > /dev/full; echo "rc=$?"'
run_case A printf-devfull       'printf hi > /dev/full; echo "rc=$?"'
run_case A pwd-devfull          'pwd > /dev/full; echo "rc=$?"'
run_case A printf-empty-format  'printf ""; echo "rc=$?"'
run_case A read-n-byte1         'read -n 1 v < @SANDBOX@/execfix/utf8; printf %s "$v"; echo ":rc=$?"'
run_case A read-n-byte4         'read -n 4 v < @SANDBOX@/execfix/utf8; printf %s "$v" | od -An -tx1; echo "rc=$?"'
run_case A read-n-zero          'read -n 0 v < @SANDBOX@/execfix/utf8; echo "[$v] rc=$?"; read w < @SANDBOX@/execfix/utf8; echo "[$w]"'
run_case A read-N-byte4         'read -N 4 v < @SANDBOX@/execfix/utf8; printf %s "$v" | od -An -tx1; echo "rc=$?"'
# byteval 无损字节编码：截断字节经 REPLY / printf -v 两个独立 ingress 后，
# 必须在 echo/printf 的 egress 逐字节还原，不能出现 PUA 泄漏或 U+FFFD。
run_case A read-reply-raw-byte  'read -n 4 < @SANDBOX@/execfix/utf8; printf %s "$REPLY" | od -An -tx1; echo "rc=$?"'
run_case A printf-v-raw-byte    'printf -v v "\xff"; printf %s "$v" | od -An -tx1; echo "rc=$?"'
# QA 复核新增回归：所有「按字节解释变量」的操作必须先 decode_raw，
# 不能用编码后 String 的 .len()/.chars() 当作字节长度。
run_case A byteval-len-trunc    'read -n 4 v < @SANDBOX@/execfix/utf8; echo "len=${#v}"'
run_case A byteval-len-utf8     'v=中文; echo "len=${#v}"'
run_case A byteval-substring    'read -n 4 v < @SANDBOX@/execfix/utf8; printf %s "${v:0:2}" | od -An -tx1; echo "rc=$?"'
run_case A byteval-declare-raw  'printf -v v "\xff"; declare -p v; echo "rc=$?"'
# QA 最终复验残留：字面 U+E100 紧邻 U+E000 的字节序列来自源码文本或外部
# 命令输出时，曾被 decode_raw 误当成编码字节。下列 4 条覆盖所有 ingress：
# -c 源码、命令替换原始字节、source 源码文件、mapfile 原始字节。
run_case A byteval-sentinel-inline  'printf %s "" | od -An -tx1; echo "rc=$?"'
run_case A byteval-sentinel-cmdsub  'v=$(cat @SANDBOX@/execfix/forge.bin); printf %s "$v" | od -An -tx1; echo "len=${#v}"'
run_case A byteval-sentinel-source  'source @SANDBOX@/execfix/forge_src.sh; echo "rc=$?"'
run_case A byteval-sentinel-mapfile 'mapfile -t A < @SANDBOX@/execfix/forge.bin; printf %s "${A[0]}" | od -An -tx1; echo "len=${#A[0]}"'
# 环境变量 ingress：字面哨兵经继承环境进入 shell。只在两侧被测进程注入，
# 不污染 export-p / export-count 等全量环境用例。
LITPUA_RAW="$(printf '\356\204\200\356\200\200')"
CASE_ENV=(env LITPUA="$LITPUA_RAW")
run_case A byteval-env-ingress 'printf %s "$LITPUA" | od -An -tx1; echo "len=${#LITPUA}"'
CASE_ENV=()
# 诊断 egress：变量值中的裸字节按 bash 规则显示（cd/command-not-found 用
# $'...'；source/type 按 %s 原样写出），禁止泄漏 byteval 编码串。
run_case A byteval-diag-cd 'v=$(cat @SANDBOX@/execfix/ff.bin); cd "$v"; echo "rc=$?"'
run_case A byteval-diag-cmdnotfound 'v=$(cat @SANDBOX@/execfix/ff.bin); "$v"; echo "rc=$?"'
run_case A byteval-diag-source 'v=$(cat @SANDBOX@/execfix/ff.bin); source "$v"; echo "rc=$?"'
run_case A byteval-diag-type 'v=$(cat @SANDBOX@/execfix/ff.bin); type "$v"; echo "rc=$?"'
# ANSI-C quoting（$'...'）字节保真。bash 实测：
# * \xNN / \NNN 在任何 locale 下都产生裸字节（0xff 不得变成 UTF-8 的 c3 bf）；
# * \uHHHH / \UHHHHHHHH 取决于字符分类 locale：单字节 locale（C/POSIX）下
#   码点 <=0xff 输出单字节，>0xff 回退成字面 \uXXXX（>0xffff 则 \UXXXXXXXX）；
#   多字节 locale（*.UTF-8）下按 Unicode 字符输出；
# * 任何转义产生 NUL 都会截断其后全部内容（bash 用 C 字符串保存）；
# * 多字节 locale 下 \uE100 是新生成的字面哨兵，必须双写否则与紧邻 PUA 碰撞。
run_case A ansi-u-e000-c       "printf %s \$'\\uE000' | od -An -tx1"
run_case A ansi-u-latin1-c     "printf %s \$'\\u00e9' | od -An -tx1"
run_case A ansi-u-short-c      "printf %s \$'\\uE00' | od -An -tx1"
run_case A ansi-U-big-c        "printf %s \$'\\U0001F600' | od -An -tx1"
run_case A ansi-sentinel-c     "printf %s \$'\\uE100\\uE000' | od -An -tx1"
run_case A ansi-x-ff           "printf %s \$'\\xff' | od -An -tx1"
run_case A ansi-oct-377        "printf %s \$'\\377' | od -An -tx1"
run_case A ansi-nul-x          "printf %s \$'a\\x00b' | od -An -tx1"
run_case A ansi-nul-u          "printf %s \$'a\\u0000b' | od -An -tx1"
CASE_ENV=(env LC_ALL=C.UTF-8 LANG=C.UTF-8)
run_case A ansi-u-e000-utf8    "printf %s \$'\\uE000' | od -An -tx1"
run_case A ansi-u-latin1-utf8  "printf %s \$'\\u00e9' | od -An -tx1"
run_case A ansi-sentinel-utf8  "printf %s \$'\\uE100\\uE000' | od -An -tx1"
CASE_ENV=()
# from_utf8_lossy 站点：非法 UTF-8 裸字节必须原样保真（不变量=各站点不再
# 用 from_utf8_lossy / to_string_lossy 丢失字节）。oracle 实测：
# * 脚本源码 / source 文件内容含裸 0xff → printf 输出 0xff；
# * printf 格式串的非法转换字符 → 诊断写裸 0xff（不是 c3 bf / ef bf bd）；
# * shebang 解释器路径含裸 0xff（间接 -c 路径）→ bad interpreter 诊断写裸字节；
# * 不存在的命令名含裸 0xff → errno 诊断写裸字节。
run_case A fromlossy-script     '@SANDBOX@/execfix/lossy_src.sh'
run_case A fromlossy-source     'source @SANDBOX@/execfix/lossy_src.sh'
run_case A fromlossy-printf-spec 'fmt=$(cat @SANDBOX@/execfix/lossy_fmt.bin); printf "$fmt" x; echo "rc=$?"'
run_case A fromlossy-shebang    '@SANDBOX@/execfix/lossy_shebang; echo "rc=$?"'
run_case A fromlossy-cmd-enoent 'v=$(cat @SANDBOX@/execfix/rawname.bin); "$v"; echo "rc=$?"'
run_case A plain-script         '@SANDBOX@/execfix/plain; echo "rc=$?"'
# stderr 本身写失败（2>&1 指向 /dev/full）：bash 静默放弃诊断并保留命令自身
# 的 rc；Rust 的 eprintln!/println! 在写失败时 panic(rc=101)，属逐字节/rc
# 偏离。所有用户诊断走 safe_eprintln! 宏（写失败忽略，绝不 panic）。
run_case A stderr-full-echo-n    'echo -n foo > /dev/full 2>&1; echo "rc=$?"'
run_case A stderr-full-echo      'echo foo > /dev/full 2>&1; echo "rc=$?"'
run_case A stderr-full-printf    'printf foo > /dev/full 2>&1; echo "rc=$?"'
run_case A stderr-full-pwd       'pwd > /dev/full 2>&1; echo "rc=$?"'
run_case A stderr-full-source    'source @SANDBOX@/execfix/pdir1 > /dev/full 2>&1; echo "rc=$?"'
run_case A stderr-full-kill      'kill -0 999999 > /dev/full 2>&1; echo "rc=$?"'
run_case A stderr-full-ulimit    'ulimit -f 99999999999999999999 > /dev/full 2>&1; echo "rc=$?"'
run_case A stderr-full-notfound  'no-such-cmd-xyz-washfix > /dev/full 2>&1; echo "rc=$?"'

# ---------------------------------------------------------------------------
# GROUP A: `[[ ... ]]` conditional-command fence (44 cases).
# Every case was first run against the MSYS2 bash 5.3.20 oracle and then
# compared byte-for-byte with wash (rc + stdout + normalized stderr).  Covers
# the lone-word `-n` special case, unary/binary/file/numeric/string/pattern
# operators, POSIX ERE via libc regcomp/regexec, BASH_REMATCH capture groups,
# `!`/`&&`/`||`/grouping, no word splitting inside `[[ ]]`, and bash's exact
# two-line diagnostics for malformed expressions (including the yacc
# "syntax error near" line and the unmatched `(` lexer EOF).
# ---------------------------------------------------------------------------
run_case A cond-lone-word       '[[ abc ]]; echo R=$?'
run_case A cond-lone-quoted     '[[ "abc" ]]; echo R=$?'
run_case A cond-unary-n         '[[ -n abc ]]; echo R=$?'
run_case A cond-unary-n-empty   '[[ -n "" ]]; echo R=$?'
run_case A cond-unary-z-empty   '[[ -z "" ]]; echo R=$?'
run_case A cond-unary-z-non     '[[ -z abc ]]; echo R=$?'
run_case A cond-eq-plain        '[[ abc = abc ]]; echo R=$?'
run_case A cond-eq-diff         '[[ abc = abd ]]; echo R=$?'
run_case A cond-eq-glob         '[[ abc = a* ]]; echo R=$?'
run_case A cond-eq-ne           '[[ abc != a* ]]; echo R=$?'
run_case A cond-eqeq-glob       '[[ abc == a?c ]]; echo R=$?'
run_case A cond-eq-quoted-glob  '[[ "a*" = a* ]]; echo R=$?'
run_case A cond-regex           '[[ abc =~ ^a.c$ ]]; echo R=$?'
run_case A cond-regex-group     '[[ abc =~ a(b)c ]]; echo R=${BASH_REMATCH[1]}'
run_case A cond-regex-noquote   '[[ "a.c" =~ ^a.c$ ]]; echo R=$?'
run_case A cond-regex-literal   '[[ "a.c" =~ ^a\.c$ ]]; echo R=$?'
run_case A cond-num-lt          '[[ 1 -lt 2 ]]; echo R=$?'
run_case A cond-num-lt-false    '[[ 2 -lt 1 ]]; echo R=$?'
run_case A cond-num-eq-abc      '[[ abc -eq 0 ]]; echo R=$?'
run_case A cond-num-ge          '[[ 3 -ge 3 ]]; echo R=$?'
run_case A cond-bang            '[[ ! -f /nonexistent_zz ]]; echo R=$?'
run_case A cond-andand          '[[ -n a && -n b ]]; echo R=$?'
run_case A cond-andand-false    '[[ -z a && -n b ]]; echo R=$?'
run_case A cond-oror            '[[ -z a || -n b ]]; echo R=$?'
run_case A cond-group           '[[ ( -n a ) ]]; echo R=$?'
run_case A cond-nested          '[[ ( -n a || -z b ) && -n c ]]; echo R=$?'
run_case A cond-var-nosplit     'x="a b"; [[ -n $x ]]; echo R=$?'
run_case A cond-var-unquoted    'x="a b"; [[ $x = "a b" ]]; echo R=$?'
run_case A cond-file-test       '[[ -e /tmp ]]; echo R=$?'
run_case A cond-empty           '[[ ]]; echo R=$?'
run_case A cond-two-words       '[[ a b ]]; echo R=$?'
run_case A cond-bin-missing     '[[ a = ]]; echo R=$?'
run_case A cond-bin-badop       '[[ a -zz b ]]; echo R=$?'
run_case A cond-unary-missing   '[[ -n ]]; echo R=$?'
run_case A cond-no-close        '[[ -n a; echo R=$?'
run_case A cond-unmatched-paren '[[ ( -n a ]]; echo R=$?'
run_case A cond-lt-op           '[[ a < b ]]; echo R=$?'
run_case A cond-gt-op           '[[ b > a ]]; echo R=$?'
run_case A cond-multiline       '[[ -n a &&
 -n b ]]; echo R=$?'
run_case A cond-regex-alt       '[[ abc =~ ^(a|b)c$ ]]; echo R=$? M=${BASH_REMATCH[1]}'
run_case A cond-regex-two-grp   '[[ abc =~ ^(a)(b)c$ ]]; declare -p BASH_REMATCH'
run_case A cond-regex-fail      '[[ x =~ ^(a) ]]; echo R=$?; declare -p BASH_REMATCH'
run_case A cond-regex-persist   '[[ abc =~ (b) ]]; echo one=${BASH_REMATCH[1]}; [[ -n x ]]; declare -p BASH_REMATCH'
run_case A cond-regex-invalid   '[[ abc =~ a(b ]]; echo R=$?'

# GROUP B -- regression fence for already-fixed defects (21 cases, green today).
# (每条用例都曾是真实缺陷，修复后留在这里作为回归栅栏：通过 run_case 同时跑
#  MSYS2 bash 5.3.20 与 wash，逐字节比较 rc + stdout + stderr（stderr 仅
#  normalize shell 名前缀）。P1 第一组 4 项的 21 条修复用例就在这里。
#  当前全绿；一旦 FAIL 即表示某个已修复缺陷回归。)
###############################################################################

# --- P1-1 未加引号的空展开字段必须删除（droppable）-----------------------
# bash: `set -- "$@"` 空时是 0 个词；显式 ""、带引号的 $var、已拼接字面量
# 的字段必须保留为 1 个空词。修复前 wash 把它们都当成 1 个空词。
run_case B p1-empty-quoted-at      'set --; set -- "$@"; echo "n=$#"'
run_case B p1-unquoted-empty-var   'x=; set -- $x; echo "n=$#"'
run_case B p1-empty-array-at       'arr=(); set -- "${arr[@]}"; echo "n=$#"'
run_case B p1-quoted-empty-var     'x=; set -- "$x"; echo "n=$#"'
run_case B p1-explicit-empty       'set -- ""; echo "n=$# [$1]"'
run_case B p1-mixed-literal        'x=; set -- a"$x"b; echo "n=$# [$1]"'
run_case B p1-printf-empty-at      'set --; printf "[%s]" "$@"; echo ":n=$#"'

# --- P1-2 管道阶段 / fork 子进程必须继承函数表 ---------------------------
# 修复前 wash 的 Shell::from_env 重建子 Shell 时 functions 为空 -> 127。
run_case B p2-func-first-stage     'myfunc() { echo FUNC_OK; }; myfunc | cat'
run_case B p2-func-second-stage    'myfunc() { cat; echo FUNC_OK; }; echo hi | myfunc'
run_case B p2-func-three-stage     'f() { echo F3; }; f | cat | cat'

# --- P1-3 子进程重定向失败不得静默吞掉 -----------------------------------
# bash: 报 `<shell>: line N: FILE: No such file or directory` 且 rc=1；
# 修复前 wash 继续执行并把 ls 的目录列表漏到 stdout、rc=0。
run_case B p3-redir-in-fail        'cat < /nonexistent/f; echo "rc=$?"'
run_case B p3-redir-out-fail       'ls > /nonexistent/f; echo "rc=$?"'
run_case B p3-redir-append-fail    'echo hi >> /nonexistent/f; echo "rc=$?"'

# --- P1-4 readonly 不得被 declare / export / local 绕过 -------------------
# 诊断文案逐字节对齐 bash：declare/local 带 builtin 前缀，export/普通赋值
# 不带；readonly 内建自身也不带 `readonly:` 前缀。
run_case B p4-declare-readonly     'readonly X=1; declare X=2; echo "X=$X rc=$?"'
run_case B p4-export-readonly      'readonly Y=1; export Y=2; echo "Y=$Y rc=$?"'
run_case B p4-local-readonly       'readonly Z=1; f() { local Z=9; }; f; echo "Z=$Z rc=$?"'
run_case B p4-declare-x-readonly   'readonly W=1; declare -x W=2; echo "W=$W rc=$?"'
run_case B p4-readonly-builtin     'readonly A=1; readonly A=2; echo "A=$A rc=$?"'
run_case B p4-declare-r-readonly   'readonly A=1; declare -r A=2; echo "A=$A rc=$?"'
run_case B p4-local-r-readonly     'readonly Z=1; f() { local -r Z=9; }; f; echo "Z=$Z rc=$?"'
run_case B p4-export-no-value      'readonly V=1; export V; echo "V=$V rc=$?"'


# GROUP K -- KNOWN-DIFF.
# Cases where bash and wash can NOT be byte-identical BY DESIGN.  These are
# not bugs and must not be "fixed" by weakening the comparison; they are
# documented here so that the difference stays visible and the reason is on
# record.  Every case in this group states WHY full byte equality is
# impossible.
#
# --- set 无参 ---------------------------------------------------------------
# bash 会打印其「整个变量表」，其中包含 bash 的内部变量（BASH、BASHOPTS、
# BASH_ARGC、BASH_ARGV、BASH_CMDS、BASH_LINENO、BASH_SOURCE、BASH_VERSINFO、
# BASH_VERSION、DIRSTACK、EPOCHREALTIME、EPOCHSECONDS、EUID、FUNCNAME、GROUPS、
# HOSTTYPE、MACHTYPE、OSTYPE、PIPESTATUS、PPID、RANDOM、SECONDS、SHELLOPTS、UID
# 等）。wash 有自己一套小得多的变量表，且从不增长 bash 的内部变量，因此两份
# 列表的「变量集合」必然不同——任何归一化都无法让它们逐字节相等。此用例曾是
# 逐字节对比的 KNOWN-DIFF 用例；现改为「语义不变量」用例（run_case_sem），
# 在不伪造 bash 取值的前提下断言身份绑定的性质：
#   * `set` 退出码为 0；
#   * 输出含 BASH_VERSION= 一行（非空版本串）；
#   * EUID 为非负整数；
#   * PPID 为非负整数。
# 以上对 bash 与 wash 均真实成立（wash 把 BASH_VERSION 设成「自己的版本串」，
# 而非伪造的 bash 版本）。wash 侧这一行的精确值并未放过：`tests/check_version.sh`
# 的 `set output BASH_VERSION` 断言逐字节校验它等于 tools/wash_version.sh
# 从 Cargo.toml + git tag/HEAD 生成的版本串。`set` 的选项处理仍由 GROUP A
# 的 `set-o` / `set-plus-o` / `set-invalid` / `set-errexit` / `set-noglob` 逐字节覆盖。
run_case_sem K set-noargs 'set' '
    [ -s "$OUT" ] &&
    grep -q "^BASH_VERSION=" "$OUT" &&
    grep -Eq "^EUID=[0-9]+$" "$OUT" &&
    grep -Eq "^PPID=[0-9]+$" "$OUT" &&
    [ "$RC" -eq 0 ]'
#
# --- counting the exported variables -----------------------------------------
# The exported-variable SET is identical between bash and wash (GROUP A
# `export-p` / `export-noarg` / `export-no-name` prove that byte-for-byte).
# The COUNT differs by exactly one because bash drops the export attribute of
# `$_` as soon as a command has run, while wash keeps `$_` exported -- a
# genuine (but different) divergence, tracked on its own in GROUP D
# (`underscore-export-status`).  The count is therefore a redundant and
# needlessly opaque proxy for a difference that is already measured
# elsewhere.
run_case K export-count      "SHLVL=1; export -p | grep -c '^declare -x'"
#
# --- `$0` 的字面值 -----------------------------------------------------------
# `$0` 是 shell 自身被调用时的路径：bash 打印 "<...>/bash.exe"，wash 打印
# "<...>/wash.exe"。两个不同的二进制永远不可能产出相同路径，因此若对字面值做
# 逐字节对比，必然「按设计失败」。所以我们改为断言「语义不变量」，而不比较
# 字面路径，也绝不伪造 bash 的身份：
#   * source 一个脚本后，`$0` 非空；
#   * `$0` 不会被重绑定到被 source 的脚本路径（source 不得改变 $0——shell
#     保留自己的调用路径）。
# 以上对 bash 与 wash 均真实成立。字面值相等被「刻意不」断言。对应的 GROUP A
# 用例 `source-zero-semantics* / source-zero-unchanged` 进一步逐字节证明 $0
# 在 source 前后保持不变。
run_case_sem K source-zero-raw 'source @SANDBOX@/args.sh; echo "0=$0"' '
    val=$(grep "^0=" "$OUT"); val=${val#0=};
    [ -n "$val" ] &&
    [ "${val##*/}" != "args.sh" ] &&
    [ "$val" != "args.sh" ]'

###############################################################################
# GROUP C -- third priority builtins.
# The engineer is implementing these in the current batch; a failure here
# means "not implemented yet" (or a real bug), not a regression of the
# already-finished A group.
###############################################################################

# --- test / [ ---------------------------------------------------------------
run_case C test-eq           'test 1 -eq 1; echo "rc=$?"'
run_case C test-ne           'test 1 -ne 1; echo "rc=$?"'
run_case C test-gt           'test 2 -gt 1; echo "rc=$?"'
run_case C test-lt           'test 1 -lt 2; echo "rc=$?"'
run_case C test-ge           'test 2 -ge 2; echo "rc=$?"'
run_case C test-le           'test 2 -le 2; echo "rc=$?"'
run_case C test-eq-str       'test abc = abc; echo "rc=$?"'
run_case C test-eq-str-diff  'test abc = abd; echo "rc=$?"'
run_case C test-numeq-str    'test abc != abd; echo "rc=$?"'
run_case C test-z-empty      "test -z ''; echo \"rc=\$?\""
run_case C test-z-nonempty   'test -z x; echo "rc=$?"'
run_case C test-n            'test -n x; echo "rc=$?"'
run_case C test-n-empty      "test -n ''; echo \"rc=\$?\""
run_case C test-f            'test -f /etc/passwd; echo "rc=$?"'
run_case C test-d            'test -d /tmp; echo "rc=$?"'
run_case C test-e            'test -e /tmp; echo "rc=$?"'
run_case C test-r            'test -r /tmp; echo "rc=$?"'
run_case C test-w            'test -w /tmp; echo "rc=$?"'
run_case C test-x            'test -x /bin/sh; echo "rc=$?"'
run_case C test-s            'test -s /etc/passwd; echo "rc=$?"'
run_case C test-s-empty      'test -s /dev/null; echo "rc=$?"'
run_case C test-not          'test ! -e /no/such; echo "rc=$?"'
run_case C test-and          'test 1 -eq 1 -a 2 -eq 2; echo "rc=$?"'
run_case C test-or           'test 1 -eq 1 -o 1 -eq 2; echo "rc=$?"'
run_case C test-paren        'test 1 -eq 1 \( 2 -eq 2 \); echo "rc=$?"'
run_case C test-bracket      '[ 1 -eq 1 ]; echo "rc=$?"'
run_case C test-bracket-missing '[ ; echo "rc=$?"'
run_case C test-bracket-unclosed '[ 1 -eq 1 ; echo "rc=$?"'
run_case C test-nt           'test @SANDBOX@/script.sh -nt @SANDBOX@/in.txt; echo "rc=$?"'
run_case C test-ot           'test @SANDBOX@/script.sh -ot @SANDBOX@/in.txt; echo "rc=$?"'
run_case C test-ef           'test @SANDBOX@/script.sh -ef @SANDBOX@/script.sh; echo "rc=$?"'

# --- type -------------------------------------------------------------------
run_case C type-t-builtin    'type -t echo; echo "rc=$?"'
run_case C type-t-file       'type -t ls; echo "rc=$?"'
run_case C type-t-missing    'type -t nosuchcommand12345; echo "rc=$?"'
run_case C type-a            'type -a echo; echo "rc=$?"'
run_case C type-f            'type -f ls; echo "rc=$?"'
run_case C type-P            'type -P ls; echo "rc=$?"'
run_case C type-plain        'type echo; echo "rc=$?"'

# --- command ----------------------------------------------------------------
run_case C command-v         'command -v echo; echo "rc=$?"'
run_case C command-v-missing 'command -v nosuchcommand12345; echo "rc=$?"'
run_case C command-V         'command -V echo; echo "rc=$?"'
run_case C command-p         'command -p echo; echo "rc=$?"'
run_case C command-a         'command -a echo; echo "rc=$?"'
run_case C command-plain     'command echo hi; echo "rc=$?"'

# --- builtin ----------------------------------------------------------------
run_case C builtin-plain     'builtin echo hi; echo "rc=$?"'
run_case C builtin-missing   'builtin nosuchbuiltin; echo "rc=$?"'
run_case C builtin-list      'builtin; echo "rc=$?"'

# --- hash -------------------------------------------------------------------
run_case C hash-plain        'hash; echo "rc=$?"'
run_case C hash-t            'hash -t ls; echo "rc=$?"'
run_case C hash-l            'hash -l; echo "rc=$?"'
run_case C hash-r            'hash -r; echo "rc=$?"'
run_case C hash-p            'hash -p /usr/bin/ls ls; hash -t ls; echo "rc=$?"'

# --- read -------------------------------------------------------------------
run_case C read-basic        'read v < @SANDBOX@/in.txt; echo "[$v]"'
run_case C read-r            'read -r v < @SANDBOX@/in.txt; echo "[$v]"'
run_case C read-reply        'read < @SANDBOX@/in.txt; echo "[$REPLY]"'
run_case C read-n            'read -n 3 v < @SANDBOX@/in.txt; echo "[$v]"'
run_case C read-d            'read -d x v < @SANDBOX@/in.txt; echo "[$v]"'
run_case C read-p            'read -p prompt v < @SANDBOX@/in.txt; echo "[$v]"'
run_case C read-eof          'read v < /dev/null; echo "rc=$?"'
# --- read: field splitting driven by IFS -----------------------------------
# `IFS` decides which characters separate fields.  With IFS=":" the input
# "a b c" contains no separator at all, so the WHOLE line is one field and
# the second variable stays empty.  These cases pin that down.
# `IFS` set as a plain statement (not a prefix assignment) -- the path that
# already works, kept as a regression fence.
run_case C read-ifs-global   'IFS=:; read a b < @SANDBOX@/colons.txt; echo "a=[$a] b=[$b]"'
# `IFS` as a PREFIX assignment to `read` -- bash applies it to the split,
# so "a b c" (no colon in it) is ONE field.
run_case C read-ifs-prefix   'IFS=: read a b < @SANDBOX@/spaced.txt; echo "a=[$a] b=[$b]"'
# An EMPTY `IFS` disables field splitting entirely -- again the whole line
# is one field.
run_case C read-ifs-empty    'IFS= read a b < @SANDBOX@/spaced.txt; echo "a=[$a] b=[$b]"'

# --- pwd --------------------------------------------------------------------
run_case C pwd-plain         'pwd'
run_case C pwd-L             'pwd -L'
run_case C pwd-P             'pwd -P'

# --- : / true / false -------------------------------------------------------
run_case C colon             ':; echo "rc=$?"'
run_case C colon-args        ': foo; echo "rc=$?"'
run_case C true              'true; echo "rc=$?"'
run_case C false             'false; echo "rc=$?"'

# --- pwd / true / false must be REAL BUILTINS, not external programs ------
#
# WHY THIS BLOCK EXISTS
#   `pwd`, `true` and `false` used to PASS in wash -- but only because wash
#   fell through to the external programs /usr/bin/pwd, /usr/bin/true and
#   /usr/bin/false.  Their stdout and exit code happened to agree with
#   bash's builtins, so the plain output comparison could not tell the
#   difference.  A shell that runs /usr/bin/pwd has different semantics:
#   the builtin `pwd` is a shell primitive, it honours $PWD even when the
#   directory has been deleted, it is not subject to $PATH, and it is
#   reported as `builtin` by `type`.
#
#   These cases therefore assert the BUILTIN-NESS, not just the output:
#     * `type -t <name>`  -> "builtin" in bash, "file" for an external prog
#     * `builtin <name>`  -> bash RUNS the builtin; for an external program
#                            it fails with "not a shell builtin" (rc=1)
#     * `command -v <n>`  -> bash prints the bare name ("true"); for an
#                            external program it prints the full path
#                            ("/usr/bin/true")
run_case C pwd-is-builtin    'type -t pwd; echo "rc=$?"'
run_case C true-is-builtin   'type -t true; echo "rc=$?"'
run_case C false-is-builtin  'type -t false; echo "rc=$?"'
run_case C builtin-true      'builtin true; echo "rc=$?"'
run_case C builtin-false     'builtin false; echo "rc=$?"'
run_case C builtin-pwd       'builtin pwd; echo "rc=$?"'
run_case C command-v-true    'command -v true; echo "rc=$?"'
run_case C command-v-pwd     'command -v pwd; echo "rc=$?"'
# `cd` IS already a real builtin in wash, so `builtin cd` must actually
# change the PARENT shell's working directory (the point of `builtin`).
run_case C builtin-cd        'builtin cd /tmp; pwd'

# --- type / command: the distinctions that are easy to get wrong -----------
# `type -t` reports the KIND of a name: "builtin", "file", "keyword", ...
run_case C type-t-keyword    'type -t if; echo "rc=$?"'
# `command -v` prints the BARE name for a builtin ("cd"), not a path.
run_case C command-v-builtin 'command -v cd; echo "rc=$?"'
# `type -t` / `type -P` on a name that does not exist are SILENT (rc=1),
# while plain `type` prints "type: NAME: not found" on stderr.
run_case C type-t-missing    'type -t nosuchcommand12345; echo "rc=$?"'
run_case C type-plain-missing 'type nosuchcommand12345; echo "rc=$?"'
# In a NON-interactive shell aliases are not expanded, so `type` does not
# report an alias as "x is aliased to ..." -- it says "not found" (rc=1).
run_case C type-alias        'alias x="echo 1"; type x; echo "rc=$?"'
run_case C command-v-alias   'alias x="echo 1"; command -v x; echo "rc=$?"'

# --- test: operator details -------------------------------------------------
# `==` is a legal alias for `=` in bash's `test` (returns 1 for "x" vs "y",
# with NO error message).
run_case C test-eq-op        'test x == y; echo "rc=$?"'
# `=~` is NOT an operator: bash reports "test: =~: binary operator
# expected" and exits 2.
run_case C test-tilde-op     'test x =~ y; echo "rc=$?"'
# `( )` is a SHELL operator, not a `test` one: only the escaped form
# reaches `test`, where it is just two extra arguments -> "test: too many
# arguments" (rc=2).
run_case C test-paren-escaped 'test 1 -eq 1 \( 2 -eq 2 \); echo "rc=$?"'
# `-nt` / `-ot` compare with NANOSECOND precision, so two files created
# within the same second still have a defined order.  The files are
# created by this very case (in the sandbox, never in the repo root), and
# `touch` processes its arguments in order, so `nt_b` is always newer.
run_case C test-nt-nano      'touch @SANDBOX@/nt_a @SANDBOX@/nt_b; test @SANDBOX@/nt_b -nt @SANDBOX@/nt_a; echo "rc=$?"'
run_case C test-ot-nano      'touch @SANDBOX@/nt_a @SANDBOX@/nt_b; test @SANDBOX@/nt_a -ot @SANDBOX@/nt_b; echo "rc=$?"'

###############################################################################
# GROUP C -- the Phase 3 builtin group (16 builtins).
#
# These builtins need PRECONDITIONS to be observable at all -- a function
# scope for `local`, a loop for `break`/`continue`, a sourced script for
# `return`, a live child process for `kill`.  Every case below therefore
# BUILDS the precondition it needs instead of skipping the case.
#
# SAFETY RULES OBSERVED HERE
#   * `kill` only ever signals a CHILD process this very case started
#     (`sleep 2 & p=$!`).  No case sends a signal to the shell itself or to
#     any pre-existing process.
#   * `trap`'s USR1 case installs a handler for USR1 FIRST and then signals
#     `$$`; with the handler in place the signal is caught, not fatal.  No
#     case sends SIGINT/SIGTERM to the shell.
#   * `ulimit` only ever lowers the SOFT core-file-size limit (`-c`), which
#     is restored inside the same case.  It never touches `-v`, `-u`, `-t`
#     or the hard limits, so the test process cannot lock itself up.
#   * `umask` is process-wide, so every `umask` case restores the original
#     value inside the same case.
#   * `enable -n` disables a builtin, so the `enable` case re-enables it
#     inside the same case before the next one runs.
###############################################################################

# --- declare / typeset / readonly -------------------------------------------
# `declare -p NAME` prints the variable WITH its attributes, which is the
# observable part of `declare`; `declare`/`typeset` with no name would print
# bash's whole internal variable table (BASHOPTS, BASH_VERSINFO, ...), which
# is a KNOWN-DIFF and therefore not used here.
run_case C declare-x          'declare -x Q=1; echo "Q=$Q"'
run_case C declare-p          'declare -x Q=1; declare -p Q'
# `-i` makes the variable INTEGER: `N=N+1` must evaluate to 6, not the
# literal string "N=N+1".
run_case C declare-i          'declare -i N=5; N=N+1; echo "N=$N"'
run_case C declare-r          'declare -r R=1; declare -p R'
run_case C declare-invalid    'declare 1bad=x; echo "rc=$?"'
# `typeset` is a synonym of `declare`: both must produce the same output.
run_case C typeset-synonym    'typeset -x Q=1; declare -p Q'
# ... and `typeset -p` prints the same attributes as `declare -p`.
run_case C typeset-p          'typeset -x Q=1; typeset -p Q'
# `typeset -i` is the integer attribute, exactly like `declare -i`.
run_case C typeset-i          'typeset -i N=5; N=N+1; echo "N=$N"'
# `typeset -r` is the readonly attribute, exactly like `declare -r`.
run_case C typeset-r          'typeset -r R=1; typeset -p R'
# Assigning to a readonly variable must fail with bash's exact wording.
run_case C readonly-assign    'readonly R=1; R=2; echo "rc=$?"'
# `readonly -p` prints EVERY readonly variable (bash internals included), so
# the case greps for the one the test created.
run_case C readonly-p         'readonly R=1; readonly -p | grep "^declare -r R="'
# A readonly variable keeps its attribute when printed by `declare -p`.
run_case C readonly-declare-p 'readonly R=1; declare -p R'
# A readonly variable cannot be unset either: bash refuses and keeps it.
run_case C readonly-unset     'readonly R=1; unset R; echo "rc=$? R=[$R]"'

# --- let --------------------------------------------------------------------
run_case C let-basic          'let x=1+1; echo "x=$x"'
run_case C let-quoted         'let "x=2*3"; echo "x=$x"'
run_case C let-aug            'let x=5; let x+=3; echo "x=$x"'
# Division by zero must fail with bash's exact wording and rc=1.
run_case C let-div0           'let x=1/0; echo "rc=$?"'

# --- ulimit -----------------------------------------------------------------
run_case C ulimit-n           'ulimit -n'
run_case C ulimit-f           'ulimit -f'
run_case C ulimit-a           'ulimit -a'
# Set and restore the SOFT core-file-size limit.  `-c` is the only limit
# that is both settable on this platform and harmless (it caps the size of
# a core dump, nothing else).  The original value is captured first and
# restored inside the same case.
run_case C ulimit-c-set-restore 'orig=$(ulimit -c); ulimit -c 8; echo "now=$(ulimit -c)"; ulimit -c "$orig"; echo "restored=$(ulimit -c)"'
run_case C ulimit-bad-opt     'ulimit -Z; echo "rc=$?"'

# --- umask ------------------------------------------------------------------
# `umask` is PROCESS-wide state: every case restores the original value
# before it ends, so no case can leak a umask into the next one.
run_case C umask-plain        'umask'
run_case C umask-set          'umask 022; umask'
run_case C umask-S            'umask -S'
run_case C umask-set-restore  'orig=$(umask); umask 022; echo "now=$(umask)"; umask "$orig"; echo "restored=$(umask)"'

# --- times ------------------------------------------------------------------
# The CPU times themselves differ from run to run, so the case compares the
# SHAPE of the output (two lines of "<user> <system>" pairs) instead of the
# numbers -- the point of the builtin is that it prints those two lines.
run_case C times-shape        'times | sed -E "s/[0-9]+m[0-9]+\.[0-9]+s/<T>/g"'
# The output is exactly two lines (shell times + children times).
run_case C times-lines        'times | wc -l'
run_case C times-status       'times > /dev/null; echo "rc=$?"'

# --- kill -------------------------------------------------------------------
run_case C kill-l-num         'kill -l 9'
run_case C kill-l-name        'kill -l KILL'
# `kill -l` with NO argument prints the FULL numbered signal table
# (` 1) SIGHUP\t 2) SIGINT\t ...`, 5 列、按行优先、共 64 个信号，含
# RTMIN+1..RTMIN+16 与 RTMAX-15..RTMAX）。wash 已逐字节对齐 bash。
run_case C kill-l-list        'kill -l'
run_case C kill-noargs        'kill; echo "rc=$?"'
run_case C kill-bad-sig       'kill -NOSUCHSIG 12345; echo "rc=$?"'
run_case C kill-noexist       'kill -TERM 999999; echo "rc=$?"'
# THE SIGNAL-SENDING PATH.  The case starts its own `sleep 2` child and
# signals THAT child only -- never the shell, never another process.  A
# child killed by SIGTERM makes `wait` report 128+15 = 143.
#
# NOTE: `&` (background jobs) and `wait` are Phase 2 features.  Until they
# exist this case fails with "background jobs (`&`) are not supported yet
# (Phase 2)" -- that failure measures the MISSING JOB CONTROL, not the
# `kill` semantics.  It is kept here (rather than skipped) because sending
# a real signal is the only way to observe `kill` end-to-end, and it
# turns green as soon as Phase 2 lands.
# 时序加固（只调时序，不动断言 / 期望值 / 判定方式）：`sleep 2 &` fork 之后立刻
# `kill -TERM $p` 会与子进程 exec 竞态 —— MSYS2 bash 5.3 实测 20 次里有 4 次
# 信号打在尚未 exec 的子 shell 上，导致 oracle 自己的 `wait` 返回 0（其余 16 次
# 为 143），逐字节基线随机波动。插入 `sleep 0.1` 等后台作业真正进入 sleep 后再
# 发信号；20 次连跑 bash / wash 均为 rc=143、零差异。判定标准一字未改。
run_case C kill-TERM-child    'sleep 2 & p=$!; sleep 0.1; kill -TERM $p; wait $p; echo "rc=$?"'

# --- trap -------------------------------------------------------------------
run_case C trap-empty         'trap'
run_case C trap-set-list      'trap "echo hi" SIGINT; trap'
run_case C trap-p             'trap "echo hi" SIGINT; trap -p SIGINT'
run_case C trap-reset         'trap "echo hi" SIGINT; trap - SIGINT; trap -p SIGINT'
run_case C trap-ignore        'trap "" SIGINT; trap -p SIGINT'
run_case C trap-invalid-sig   'trap "echo x" NOSUCHSIG; echo "rc=$?"'
# The EXIT trap runs when the shell exits.
run_case C trap-exit          'trap "echo EXITING" EXIT; echo body'
# A trap that actually FIRES.  USR1 has no default action that would
# interrupt the shell, and the handler is installed BEFORE the signal is
# sent, so the signal is caught (bash prints CAUGHT and then continues).
run_case C trap-usr1          'trap "echo CAUGHT" USR1; kill -USR1 $$; echo done'

# --- dirs / pushd / popd ----------------------------------------------------
# bash's directory stack has the CURRENT directory on top; `cd` REPLACES
# that top entry and `pushd` pushes the old one down while swapping the
# new directory in.  Every case therefore starts with an absolute `cd`
# so that the expected stack is FIXED and does not depend on the
# directory the suite happens to be run from.
#   `cd /tmp`            -> stack: "/tmp"
#   `pushd /var`         -> stack: "/var /tmp"   (and pushd prints it)
#   `pushd /etc`         -> stack: "/etc /var /tmp"
#   `popd`               -> pops the top, stack: "/tmp"
run_case C dirs-empty         'cd /tmp; dirs'
run_case C pushd              'cd /tmp; pushd /var; dirs'
run_case C pushd-twice        'cd /tmp; pushd /var; pushd /etc; dirs'
run_case C popd               'cd /tmp; pushd /var; popd; dirs'
# `popd` with no argument on an empty stack is an error.
run_case C popd-empty         'popd; echo "rc=$?"'
# `dirs -v` numbers the entries, `dirs -l` prints the long form, `dirs -c`
# prints the stack without any abbreviation.
run_case C dirs-v             'cd /tmp; pushd /var; dirs -v'
run_case C dirs-l             'cd /tmp; pushd /var; dirs -l'
run_case C dirs-c             'cd /tmp; pushd /var; dirs -c'
# `pushd` with no argument swaps the top two entries.
run_case C pushd-noarg        'cd /tmp; pushd /var; pushd; dirs'
# `popd +N` pops the N-th entry (0-based) instead of the top one.
run_case C popd-arg           'cd /tmp; pushd /var; popd +1; dirs'
# An out-of-range index is an error.
run_case C popd-bad-index     'cd /tmp; pushd /var; popd +5; echo "rc=$?"'

# --- help -------------------------------------------------------------------
# `help` prints the FULL manual page for a builtin, which is far too long
# for a byte-diff case and would drown the failure report; the cases keep
# the first lines, which carry the usage synopsis.  The full text is not
# compared because bash's help pages are prose, not structured output --
# what is worth pinning down is the SYNOPSIS line and the exit code.
run_case C help-cd-head       'help cd | head -2'
run_case C help-read-head     'help read | head -1'
run_case C help-nosuch        'help nosuchbuiltin; echo "rc=$?"'

# --- enable -----------------------------------------------------------------
run_case C enable-bad         'enable nosuchbuiltin; echo "rc=$?"'
# `enable -n` really does disable the builtin: `type -t echo` must report
# "file" while it is disabled and "builtin" again after re-enabling.  The
# re-enable happens INSIDE the case, so the next case is unaffected.
run_case C enable-disable-restore 'enable -n echo; type -t echo; enable echo; type -t echo'
run_case C enable-already-on  'enable echo; echo "rc=$?"'

# --- enable: reusable 列表 / POSIX special builtins（与内建集合无关）-------
# `enable -p` 会忽略 NAME 打印全表，而 wash 的静态内建集合比 bash 少
# caller/compopt/getopts/logout/suspend，全表无法整段逐字节对拍；故用 grep
# 单行断言 reusable 行的格式，只钉住「与内建集合无关」的行为。
run_case C enable-p-reusable  'enable -p echo | grep -x "enable echo"'
# `-n` 无参（未 disable 任何内建）=> 空列 + rc=0。
run_case C enable-n-empty     'enable -n; echo "rc=$?"'
# `-z` 未知选项 => usage 诊断两行 + rc=2。
run_case C enable-z           'enable -z; echo "rc=$?"'
# disabled 项在 `-a` 里显示 `enable -n <name>`；同 case 内重新启用。
run_case C enable-a-disabled  'enable -n echo; enable -a | grep -x "enable -n echo"; enable echo'
# `-n` 无参列 disabled 同为 `enable -n <name>`；同 case 内重新启用。
run_case C enable-n-list      'enable -n echo; enable -n | grep -x "enable -n echo"; enable echo'
# 无参 `enable` 同样打印 `enable <name>`（reusable）。
run_case C enable-noarg-reusable 'enable | grep -x "enable echo"'
# `-s` = 16 个 POSIX special builtins，wash 静态表全含，可整段逐字节对拍。
run_case C enable-s           'enable -s'

# --- return -----------------------------------------------------------------
# `return` is only legal inside a function or a SOURCED script.  The
# sourced-script form needs no function support, so it is used here; the
# top-level form exercises bash's "can only `return' from a function or
# sourced script" diagnostic.
run_case C return-toplevel    'return 3; echo "not reached"'
run_case C return-sourced     'source @SANDBOX@/ret3.sh; echo "rc=$?"'
# `return` with no argument returns the status of the LAST command.
run_case C return-noarg       'source @SANDBOX@/retlast.sh; echo "rc=$?"'
# `return 0` from a sourced script exits it successfully.
run_case C return-zero        'source @SANDBOX@/ret0.sh; echo "rc=$?"'

# --- break / continue -------------------------------------------------------
# Outside a loop both builtins are an error in bash and the shell KEEPS
# GOING -- so the case proves that the diagnostic is emitted AND that the
# following command still runs.
run_case C break-toplevel     'break 2; echo "not reached"'
# `break` with no argument defaults to 1.
run_case C break-noarg        'break; echo "not reached"'
run_case C break-one          'break 1; echo "not reached"'
# Inside a loop they control the iteration: `break` leaves after the first
# item, `continue` visits every item.
#
# NOTE: the three cases below need the Phase 3 loop constructs (`for`,
# `while`, `do`, `done`).  Until those exist they fail with
# "for: command not found" / "while: command not found" -- that failure
# measures the MISSING LOOP, not the `break`/`continue` semantics.  They
# are kept here (rather than skipped) because they are the only way to
# observe what `break`/`continue` actually DO, and they turn green as
# soon as the loops land.
run_case C break-in-for       'for i in 1 2 3; do echo $i; break; done'
run_case C continue-in-for    'for i in 1 2 3; do echo $i; continue; done'
run_case C continue-in-while  'i=0; while [ $i -lt 3 ]; do i=$((i+1)); [ $i -eq 2 ] && continue; echo $i; done'
# `continue` with no argument defaults to 1.
run_case C continue-noarg     'continue; echo "not reached"'
run_case C continue-one       'continue 1; echo "not reached"'

# --- local ------------------------------------------------------------------
# `local` is only legal inside a function; at the top level bash reports
# "can only be used in a function" and keeps going.
run_case C local-toplevel     'local x=1; echo "rc=$?"'
# Inside a function `local` scopes the variable: the function sees it, the
# caller does not.
#
# NOTE: unlike `return`, `local` CANNOT be observed through a sourced
# script -- bash rejects it there too ("local: can only be used in a
# function"), because a sourced script is not a function scope.  A
# function DEFINITION is therefore the ONLY precondition that makes
# `local`'s scoping observable, so these two cases need the Phase 3
# function syntax (`f() { ...; }`) and fail with "f(): command not
# found" until it lands.
run_case C local-in-func      'f() { local x=1; echo "in=$x"; }; f; echo "out=[$x]"'
# `local` shadows an outer variable for the duration of the function only.
run_case C local-shadow       'x=outer; f() { local x=inner; echo $x; }; f; echo $x'

# ---------------------------------------------------------------------------
# control flow: loops and functions (Phase 3)
#
# These are regression fences for the engineer's Phase 3 work (loops +
# functions).  wash does NOT implement them yet, so every case below FAILS
# today (a syntax error, or `for`/`while`/.../`f` reported as "command not
# found").  They are kept here -- rather than skipped -- because they are
# the ONLY way to observe what the loop and function constructs actually
# DO, and they turn green the moment Phase 3 lands.  The case text is valid
# bash; the harness runs it under real bash as the oracle and compares
# wash's (currently broken) output against it.
#
# NOTE on top-level errors: `return` / `break` / `continue` / `local` are
# only legal inside a function or a loop, so their top-level forms are
# diagnostic errors (covered above: `return-toplevel` / `break-toplevel` /
# `continue-toplevel` / `local-toplevel`).  `for` / `while` / `until` /
# `case` / a function definition, by contrast, ARE legal at the top level
# in bash -- so they get happy-path coverage below, not error coverage.
# ---------------------------------------------------------------------------

# --- for --------------------------------------------------------------------
# The happy path: every item is visited, in order.
run_case C for-list           'for i in a b c; do echo "[$i]"; done'
run_case C for-numeric        'for i in 1 2 3; do echo "$i"; done'
# `break` leaves the loop after the second item; `continue` skips it.
run_case C break-in-for-2     'for i in 1 2 3 4; do echo $i; [ $i -eq 2 ] && break; done'
run_case C continue-in-for-2  'for i in 1 2 3; do [ $i -eq 2 ] && continue; echo $i; done'

# --- while ------------------------------------------------------------------
run_case C while-list         'i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done'
run_case C break-in-while     'i=0; while [ $i -lt 5 ]; do echo $i; i=$((i+1)); [ $i -eq 2 ] && break; done'
run_case C continue-in-while-2 'i=0; while [ $i -lt 3 ]; do i=$((i+1)); [ $i -eq 2 ] && continue; echo $i; done'

# --- until ------------------------------------------------------------------
run_case C until-list         'i=0; until [ $i -ge 3 ]; do echo $i; i=$((i+1)); done'
run_case C until-break        'until false; do echo once; break; done'
run_case C break-in-until     'i=0; until [ $i -ge 5 ]; do echo $i; i=$((i+1)); [ $i -eq 2 ] && break; done'
run_case C continue-in-until  'i=0; until [ $i -ge 3 ]; do i=$((i+1)); [ $i -eq 2 ] && continue; echo $i; done'

# --- case -------------------------------------------------------------------
run_case C case-default       'case foo in a) echo A;; b) echo B;; *) echo STAR;; esac'
run_case C case-match         'x=foo; case $x in foo) echo FOO;; *) echo OTHER;; esac'
run_case C case-or-pattern    'case bar in a|b) echo AB;; c) echo C;; esac'
# No pattern matches and there is no `*)`: the case runs nothing, rc=0.
run_case C case-no-match      'case zzz in a) echo A;; esac; echo "rc=$?"'

# --- function definition ----------------------------------------------------
run_case C func-def           'f() { echo hi; }; f'
run_case C func-keyword       'function g { echo ghi; }; g'
run_case C func-arg           'f() { echo "1=$1"; }; f hello'
# `return` inside a function returns the given status to the caller.
run_case C return-in-func     'f() { return 3; }; f; echo "rc=$?"'
# `return` with no argument returns the status of the LAST command in the
# function (here `true` -> 0).
run_case C return-in-func-noarg 'f() { true; return; }; f; echo "rc=$?"'
# `local` inside a function scopes the variable to that function (the
# caller's `x` is untouched).
run_case C local-in-func-2    'x=outer; f() { local x=inner; echo "in=$x"; }; f; echo "out=$x"'

###############################################################################
# GROUP D -- extra builtins, measured so that the compatibility matrix
#            (AGENTS.md / README.md) is backed by real observations.
#            Failures here mean "not implemented yet" or "differs in a known
#            way"; they are NOT regressions of the A group.
###############################################################################
# NOTE: the 16 Phase 3 builtins (declare/typeset/readonly, let, ulimit,
# umask, times, kill, trap, dirs/pushd/popd, help, enable, return, break,
# continue, local) used to have one thin case each HERE.  They now have
# comprehensive coverage in GROUP C, so these single cases were removed --
# keeping both would double-count every failure.

# --- printf: known defects (see AGENTS.md "已知缺陷") ----------------------
# `%%` in the MIDDLE of the format string, together with a surplus
# argument, makes wash loop forever (bash prints `%.` and exits).  The
# `timeout` guard in the runner keeps the suite from hanging forever; the
# case still fails because wash's rc (124) differs from bash's rc (0).
run_case D printf-pct-mid-extra 'printf %%. a'
run_case D printf-pct-mid-extra2 'printf x%%y z'
# A numeric conversion with a non-numeric argument: bash reports
# "printf: ff: invalid number" on stderr, wash stays silent.
run_case D printf-hex-invalid 'printf %x ff'
run_case D printf-dec-invalid 'printf %d zz'

# --- job control (Phase 2) --------------------------------------------------
run_case D jobs              'jobs; echo "rc=$?"'
run_case D fg                'fg; echo "rc=$?"'
run_case D bg                'bg; echo "rc=$?"'
run_case D wait              'wait; echo "rc=$?"'
run_case D disown            'disown; echo "rc=$?"'

# --- arrays / coproc (Phase 4) ----------------------------------------------
# 加固（收紧）：原用例只跑 `echo "rc=$?"`，若把内建实现成 no-op（返回 0、不
# 填充数组）即可假绿。追加 `declare -p` 把数组内容纳入逐字节比对，并覆盖
# 多种能证伪 no-op 的情形：mapfile 覆盖 -t 去尾换行 + 默认数组名 MAPFILE；
# readarray 覆盖 -t + 空输入（E=()）。期望输出均由 MSYS2 bash 5.3 实测生成。
run_case D mapfile           'mapfile -t ARR < @SANDBOX@/in.txt; echo "rc=$?"; declare -p ARR; mapfile -t < @SANDBOX@/in.txt; declare -p MAPFILE'
run_case D readarray         'readarray -t ARR < @SANDBOX@/in.txt; echo "rc=$?"; declare -p ARR; readarray -t E < /dev/null; declare -p E'
run_case D coproc            'coproc; echo "rc=$?"'
run_case D array-assign      'ARR=(a b); echo "rc=$?"'

# --- syntax errors from NESTED input sources ----------------------------------
# bash 5.3 用「输入源名」标注解析错误，而不是一律 `-c`：
#   -c          -> `bash: -c: line N: ...`
#   eval        -> `bash: eval: line N: ...`
#   source      -> `<脚本路径>: line N: ...`（不带 shell 名）
#   EXIT trap   -> `bash: exit trap: line N: ...`
#   信号 trap   -> `bash: trap: line N: ...`
# unexpected-token 类还会在第二行回显出错的输入行；EOF 类只有一行，但带
# `on line N` 后缀且行号 = 输入行数 + 1。此前这 5 类上下文全被 `-c:` 覆盖，
# 是 476 条全绿下的真实盲区；以下全部为逐字节断言（非 run_case_sem）。
# 期望值均由 MSYS2 bash 5.3 实测生成。
run_case D synerr-eval-token     "eval '}'"
run_case D synerr-eval-coproc    "eval 'coproc; echo x'"
run_case D synerr-eval-eof       "eval '{ echo hi'"
run_case D synerr-source-token   ". @SANDBOX@/bad_tok.sh"
run_case D synerr-source-eof     ". @SANDBOX@/bad_eof.sh"
run_case D synerr-exit-trap      "trap '}' EXIT; echo ok"
run_case D synerr-exit-trap-eof  "trap '{ echo hi' EXIT; echo ok"
run_case D synerr-sig-trap       "trap '}' USR1; kill -USR1 \$\$; echo after"

# --- completion / key bindings ----------------------------------------------
# compgen -c 不能用于逐字节对拍：它枚举 PATH 下全部命令，实测输出 6132 行且
# 强绑定环境（PATH=/usr/bin 时降到 776 行）；更致命的是单条耗时约 18s，会被
# runner 的 timeout 10 杀掉，oracle 侧产生 rc=124 + 空输出，基准本身失真。
# 改用 compgen -k：只列 22 个 shell 关键字（+ `rc=0` 共 23 行），完全确定、
# 无 PATH / 内建表 / 历史文件绑定，可稳定逐字节比对。
run_case D compgen           'compgen -k; echo "rc=$?"'
run_case D complete           'complete; echo "rc=$?"'
# bind -p 的 497 行 keymap 绑定 readline 版本与 locale collation：实测
# LC_ALL=C 与 zh_CN.UTF-8 下字节数同为 11705B，但 md5 不同、行序不同
# （C 下 `"\C-x\C-r": re-read-init-file` 在 `# redraw-current-line` 之前，
# zh 下相反；vi-* 一批 not-bound 行整体错位），逐字节不可比。改用语义不变量：
# 断言 rc=0、stderr 归一化后精确等于 `bind: warning: line editing not enabled`、
# stdout 恰好 497 行、含关键绑定行 `"\C-a": beginning-of-line`、且含
# `# ... (not bound)` 条目；不得只断言 rc 或「输出非空」。
run_case_sem D bind 'bind -p' '[ "$RC" -eq 0 ] && [ "$(normalize_stderr "$ERR")" = "bind: warning: line editing not enabled" ] && [ "$(wc -l < "$OUT")" -eq 497 ] && grep -qF "\"\C-a\": beginning-of-line" "$OUT" && grep -qF "(not bound)" "$OUT"'

# --- history ----------------------------------------------------------------
run_case D fc                'fc -l; echo "rc=$?"'
run_case D history           'history; echo "rc=$?"'

# --- shell special variables (not builtins, but they change what the
#     builtins print, so they are measured here) -----------------------------
# bash increments SHLVL every time a shell is started; wash does not, which
# is why every `export`/`set` listing needs SHLVL pinned or filtered.
run_case D shlvl-increment   'echo "SHLVL=$SHLVL"'
# bash updates `$_` to the last executed command and drops its export
# attribute; wash never updates it and keeps it exported.
run_case D underscore-update  'X=1; echo "[$_]"'
run_case D underscore-export-status 'X=1; export -p | grep -c "^declare -x _="'

###############################################################################
# Summary
###############################################################################
echo "================================================================="
printf 'GROUP A (core semantics + done builtins) : %3d pass / %3d fail  (%d cases)\n' \
    "${g_pass[A]:-0}" "${g_fail[A]:-0}" "${g_total[A]:-0}"
printf 'GROUP K (KNOWN-DIFF, cannot be equal)   : %3d pass / %3d fail  (%d cases)\n' \
    "${g_pass[K]:-0}" "${g_fail[K]:-0}" "${g_total[K]:-0}"
printf 'GROUP B (fixed defects)                : %3d pass / %3d fail  (%d cases)\n' \
    "${g_pass[B]:-0}" "${g_fail[B]:-0}" "${g_total[B]:-0}"
printf 'GROUP C (third priority builtins)      : %3d pass / %3d fail  (%d cases)\n' \
    "${g_pass[C]:-0}" "${g_fail[C]:-0}" "${g_total[C]:-0}"
printf 'GROUP D (extended / matrix coverage)   : %3d pass / %3d fail  (%d cases)\n' \
    "${g_pass[D]:-0}" "${g_fail[D]:-0}" "${g_total[D]:-0}"
echo "-----------------------------------------------------------------"
echo "PASS=$pass FAIL=$fail TOTAL=$((pass + fail))"
echo "REGRESSION_FAIL=${g_fail[A]:-0}  (group A failures = real regressions)"
echo "KNOWN_DIFF_FAIL=${g_fail[K]:-0}  (group K failures = by-design differences)"
echo "KNOWN_DEFECT_FAIL=${g_fail[B]:-0}  (group B failures = regressions of already-fixed defects)"
if [ "$fail" -eq 0 ]; then
    echo "ALL CASES MATCH BASH"
    exit 0
else
    echo "SOME CASES DIFFER FROM BASH"
    exit 1
fi
