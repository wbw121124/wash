#!/usr/bin/env bash
# Release gate: every user-visible version string must agree with Cargo.toml.
#
#   MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc \
#     'cd /d/project/WorkBuddy/wash && bash tests/check_version.sh'
#
# This is a REAL gate, not a documentation note: static checks (Cargo.lock,
# env!("CARGO_PKG_VERSION") presence, .po headers, CHANGELOG/README) plus
# runtime checks (--version, $BASH_VERSION, `help` first line). Any mismatch
# exits 1. A missing binary is an error (build first), never a silent skip.
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

VER="$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)"
if [ -z "$VER" ]; then
    echo "FAIL Cargo.toml: cannot read version"; exit 1
fi
echo "wash version from Cargo.toml: $VER"

# --- static checks -----------------------------------------------------------
lock_ver="$(awk '/^name = "wash"$/{found=1} found && /^version = /{gsub(/"/,"",$3); print $3; exit}' Cargo.lock)"
check "Cargo.lock wash package version" "$VER" "$lock_ver"

for f in src/main.rs src/builtins.rs src/shell.rs src/repl.rs; do
    if grep -q 'env!("CARGO_PKG_VERSION")' "$f"; then
        printf 'ok   %s uses env!('"'"'CARGO_PKG_VERSION'"'"')\n' "$f"
    else
        printf 'FAIL %s has no env!("CARGO_PKG_VERSION")\n' "$f"
        fail=$((fail + 1))
    fi
done

for f in po/en.po po/zh_CN.po; do
    if grep -q "Project-Id-Version: wash $VER" "$f"; then
        printf 'ok   %s header\n' "$f"
    else
        printf 'FAIL %s header does not say wash %s\n' "$f" "$VER"
        fail=$((fail + 1))
    fi
done

if grep -q "\[$VER\]" CHANGELOG.md; then
    echo 'ok   CHANGELOG.md top release entry'
else
    echo "FAIL CHANGELOG.md has no [$VER] entry"; fail=$((fail + 1))
fi
if grep -q "发布 $VER" README.md; then
    echo 'ok   README.md release section'
else
    echo "FAIL README.md has no 发布 $VER section"; fail=$((fail + 1))
fi

# help header must keep the "wash, version ..." comma form: the i18n suite
# extracts builtin names with ^[a-z_][a-z0-9_]*$ and "wash," is not a token.
# The header is now rendered into a buffer and written via help_emit() (the
# EPIPE-safe writer), so match the literal plus that writer, not println!.
if grep -q 'wash, version {}' src/builtins.rs && grep -q 'fn help_emit' src/builtins.rs; then
    echo 'ok   help header keeps comma form (EPIPE-safe writer)'
else
    echo 'FAIL help header changed (i18n builtin-name extraction coupling)'
    fail=$((fail + 1))
fi

# --- runtime checks ----------------------------------------------------------
WASH=''
for c in "$REPO_ROOT/target/debug/wash.exe" "$REPO_ROOT/target/debug/wash" \
         "$REPO_ROOT/target/release/wash.exe" "$REPO_ROOT/target/release/wash"; do
    if [ -x "$c" ]; then WASH="$c"; break; fi
done
if [ -z "$WASH" ]; then
    echo 'FAIL no built binary found (run cargo build first)'; exit 1
fi
echo "runtime binary: $WASH"

check "--version"          "wash $VER"              "$("$WASH" --version)"
check '$BASH_VERSION'      "wash $VER"              "$("$WASH" -c 'echo "$BASH_VERSION"')"
check 'help first line'    "wash, version $VER"     "$("$WASH" -c 'help' | head -1)"
# `help | head -1` used to make println! panic and print
# "failed printing to stdout: Broken pipe" plus a backtrace on stderr.
# MSYS2 bash 5.3.20 produces an EMPTY stderr there; assert it byte-for-byte
# so the P4 noise cannot silently come back.
check 'help | head -1 stderr' ''                  "$({ "$WASH" -c 'help' | head -1 >/dev/null; } 2>&1)"

echo '-----------------------------------------'
if [ "$fail" -ne 0 ]; then
    echo "VERSION CHECK FAILED ($fail mismatches)"
    exit 1
fi
echo 'ALL VERSION STRINGS MATCH'
