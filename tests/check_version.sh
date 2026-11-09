#!/usr/bin/env bash
# Release gate: EVERY user-visible version line must equal the string generated
# by tools/wash_version.sh from Cargo.toml + git tag/HEAD.  Nothing here is
# hardcoded, and nothing is skipped: a missing binary, a stale git lookup, a
# stray literal version in src/, or a single differing byte is a hard failure.
#
#   MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc \
#     'cd <WASH_REPO> && bash tests/check_version.sh'
#   WASH_BIN=<copy of release binary> bash tests/check_version.sh
set -u
export LC_ALL=C LANG=C

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

fail=0
check() { # description, expected, actual
    if [ "$2" = "$3" ]; then
        printf 'ok   %s\n' "$1"
    else
        printf 'FAIL %s\n     expected: [%s]\n     actual:   [%s]\n' "$1" "$2" "$3"
        fail=$((fail + 1))
    fi
}
static_ok() { # description, condition-result
    if [ "$2" -eq 0 ]; then
        printf 'ok   %s\n' "$1"
    else
        printf 'FAIL %s\n' "$1"
        fail=$((fail + 1))
    fi
}

# --- the one generator -------------------------------------------------------
VER_SCRIPT="$REPO_ROOT/tools/wash_version.sh"
[ -f "$VER_SCRIPT" ] || { echo 'FAIL tools/wash_version.sh missing'; exit 1; }
FULL="$(bash "$VER_SCRIPT")" || { echo 'FAIL version generator failed (git metadata missing?)'; exit 1; }
BASE="$(bash "$VER_SCRIPT" --base)" || { echo 'FAIL --base failed'; exit 1; }
[ -n "$FULL" ] && [ -n "$BASE" ] || { echo 'FAIL empty generated version'; exit 1; }
echo "generated version: [$FULL]   Cargo.toml base: [$BASE]"

# --- static checks -----------------------------------------------------------
check 'Cargo.lock wash package version' "$BASE" \
    "$(awk '/^name = "wash"$/{found=1} found && /^version = /{gsub(/"/,"",$3); print $3; exit}' Cargo.lock)"

# No source file may bypass the generator.  CARGO_PKG_VERSION is the OLD
# shortcut (it ignores git tags); a literal "wash, version 0.x.y" is equally
# forbidden.  Every version line must come from env!("WASH_VERSION").
if grep -rq 'CARGO_PKG_VERSION' src/; then r=1; else r=0; fi
static_ok 'src/ has no CARGO_PKG_VERSION shortcut' "$r"
if grep -rqE 'wash, version [0-9]|wash，版本 [0-9]|wash [0-9]+\.[0-9]+\.[0-9]' src/; then r=1; else r=0; fi
static_ok 'src/ has no hardcoded wash version literal' "$r"
if grep -rq '0\.2\.0' src/ po/; then r=1; else r=0; fi
static_ok 'src/ and po/ have no stale 0.2.0 literal' "$r"

for f in src/main.rs src/shell.rs src/repl.rs src/builtins.rs src/help_pages.rs; do
    n="$(grep -c 'env!("WASH_VERSION")' "$f")"
    check "$f uses env!(WASH_VERSION)" 1 "$n"
done
grep -q 'cargo:rustc-env=WASH_VERSION' build.rs
static_ok 'build.rs injects WASH_VERSION' "$?"
grep -q 'tools/wash_version.sh' build.rs
static_ok 'build.rs calls the version generator' "$?"

bash "$VER_SCRIPT" --check-po >/dev/null
static_ok 'po headers generated from Cargo.toml' "$?"

if grep -q "\[$BASE\]" CHANGELOG.md; then
    echo 'ok   CHANGELOG.md top release entry'
else
    echo "FAIL CHANGELOG.md has no [$BASE] entry"; fail=$((fail + 1))
fi
if grep -q "发布 $BASE" README.md; then
    echo 'ok   README.md release section'
else
    echo "FAIL README.md has no 发布 $BASE section"; fail=$((fail + 1))
fi

# help header must keep the "wash, version ..." comma form: the i18n suite
# extracts builtin names with ^[a-z_][a-z0-9_]*$ and "wash," is not a token.
# The header is rendered into a buffer and written via help_emit() (the
# EPIPE-safe writer), so match the literal plus that writer, not println!.
if grep -q 'wash, version {}' src/builtins.rs && grep -q 'fn help_emit' src/builtins.rs; then
    echo 'ok   help header keeps comma form (EPIPE-safe writer)'
else
    echo 'FAIL help header changed (i18n builtin-name extraction coupling)'
    fail=$((fail + 1))
fi

# --- runtime checks ----------------------------------------------------------
WASH="${WASH_BIN:-}"
if [ -z "$WASH" ]; then
    for c in "$REPO_ROOT/target/debug/wash.exe" "$REPO_ROOT/target/debug/wash" \
             "$REPO_ROOT/target/release/wash.exe" "$REPO_ROOT/target/release/wash"; do
        if [ -x "$c" ]; then WASH="$c"; break; fi
    done
fi
if [ -z "$WASH" ] || [ ! -x "$WASH" ]; then
    echo 'FAIL no built binary found (run cargo build first)'; exit 1
fi
echo "runtime binary: $WASH"

# Hard gate before any assertion: a stale binary (old version) must abort, not
# produce 20 mismatches that could be waved away.
WASH_VER="$("$WASH" --version 2>&1)"
if [ "$WASH_VER" != "wash $FULL" ]; then
    printf 'FAIL stale/wrong binary: expected [wash %s] got [%s]\n' "$FULL" "$WASH_VER"
    exit 1
fi
echo 'ok   binary version hard gate'

check "--version"                "wash $FULL"          "$("$WASH" --version)"
check '$BASH_VERSION'            "wash $FULL"          "$("$WASH" -c 'echo "$BASH_VERSION"')"
# `set` prints the whole variable table; GROUP A can only assert a semantic
# invariant there (bash's variable set is genuinely different), so the exact
# wash version line in that path is verified HERE, not left as a soft "exists".
check 'set output BASH_VERSION'  "BASH_VERSION='wash $FULL'" \
    "$("$WASH" -c 'set' | grep '^BASH_VERSION=')"
check 'help first line'          "wash, version $FULL" "$("$WASH" -c 'help' | head -1)"
check 'help first line (zh_CN)'  "wash, version $FULL" "$(LC_ALL=zh_CN.UTF-8 LANG=zh_CN.UTF-8 "$WASH" -c 'help' | head -1)"

# `help -m` states wash's identity in IMPLEMENTATION.  The full footer is
# byte-compared against the oracle by tests/bash_diff_help.sh + help_footer.py;
# here we additionally require the version line itself to be the generated one
# in both locales (no hardcoded golden).
check 'help -m IMPLEMENTATION version'      "    wash, version $FULL" \
    "$("$WASH" -c 'help -m cd' | grep -F '    wash, version ')"
check 'help -m IMPLEMENTATION version (zh)' "    wash，版本 $FULL" \
    "$(LC_ALL=zh_CN.UTF-8 LANG=zh_CN.UTF-8 "$WASH" -c 'help -m cd' | grep -F '    wash，版本 ')"

# `help | head -1` used to make println! panic and print
# "failed printing to stdout: Broken pipe" plus a backtrace on stderr.
# MSYS2 bash 5.3.20 produces an EMPTY stderr there; assert it byte-for-byte
# so the P4 noise cannot silently come back.
check 'help | head -1 stderr' '' \
    "$({ "$WASH" -c 'help' | head -1 >/dev/null; } 2>&1)"

echo '-----------------------------------------'
if [ "$fail" -ne 0 ]; then
    echo "VERSION CHECK FAILED ($fail mismatches)"
    exit 1
fi
echo 'ALL VERSION STRINGS MATCH'
