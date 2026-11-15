#!/usr/bin/env bash
# The ONE place wash's version rule lives.  Everything user-visible (--version,
# $BASH_VERSION, help banner, help -m IMPLEMENTATION, PS1 \v) is generated from
# this string, and every test gate compares against it.  No source file and no
# test is allowed to hardcode a version number.
#
#   tools/wash_version.sh             -> full version, e.g. 0.2.1-751d26d
#   tools/wash_version.sh --base      -> Cargo.toml version, e.g. 0.2.1
#   tools/wash_version.sh --sync-po   -> rewrite po/en.po + po/zh_CN.po headers
#   tools/wash_version.sh --check-po  -> verify those headers (exit 1 on drift)
#
# Rule (fail loudly, never invent a version):
#   * base = `version = "..."` in Cargo.toml
#   * git metadata comes from WASH_GIT_DIR, defaulting to this repo.  A WSL
#     build tree without .git MUST set WASH_GIT_DIR to the source repo; a
#     missing/unreadable git dir is a hard error, not a silent fallback.
#   * HEAD has an exact tag       -> that tag (minus leading v) MUST equal base,
#                                   output = base
#   * HEAD has no tag             -> output = base-<first 7 chars of HEAD hash>
#   * the most recent reachable tag must never be NEWER than base.  This catches
#     "tagged a release but forgot to bump Cargo.toml" even at an untagged HEAD
#     (exactly the mistake this script was written to prevent).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GIT_DIR="${WASH_GIT_DIR:-$ROOT}"

die() { printf 'wash_version: %s\n' "$*" >&2; exit 1; }

base="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"
[ -n "$base" ] || die "cannot read version from $ROOT/Cargo.toml"
case "$base" in
    *[!0-9A-Za-z.+-]*|'') die "invalid version in Cargo.toml: [$base]" ;;
esac

git_at() { git -c "safe.directory=$GIT_DIR" -C "$GIT_DIR" "$@"; }
git_at rev-parse --git-dir >/dev/null 2>&1 \
    || die "no git metadata at [$GIT_DIR]; set WASH_GIT_DIR to the source repo"
head_hash="$(git_at rev-parse HEAD)" || die "cannot resolve HEAD in [$GIT_DIR]"
short="${head_hash:0:7}"
[ -n "$short" ] || die "empty HEAD hash"

exact_tags="$(git_at tag --points-at HEAD)" || die "cannot list tags at HEAD"
latest_tag="$(git_at describe --tags --abbrev=0 2>/dev/null || true)"

if [ -n "$exact_tags" ]; then
    hit=''
    while IFS= read -r t; do
        # Use an explicit `if` so the loop body always exits 0.  The old
        # `[ ... ] && hit="$t"` left a non-zero status on the last iteration
        # when the tag did not match, and under `set -e` that silently aborted
        # the script with no output (exactly the failure mode this repo forbids).
        if [ "${t#v}" = "$base" ]; then
            hit="$t"
        fi
    done <<< "$exact_tags"
    [ -n "$hit" ] \
        || die "HEAD is tagged [$exact_tags] but Cargo.toml says [$base]; bump Cargo.toml to match the tag"
    full="$base"
else
    full="$base-$short"
fi

if [ -n "$latest_tag" ]; then
    latest="${latest_tag#v}"
    if [ "$latest" != "$base" ]; then
        newer="$(printf '%s\n%s\n' "$latest" "$base" | LC_ALL=C sort -V | tail -1)"
        [ "$newer" != "$latest" ] \
            || die "latest tag [$latest_tag] is newer than Cargo.toml [$base]; bump Cargo.toml"
    fi
fi

case "${1:-}" in
    '') printf '%s\n' "$full" ;;
    --base) printf '%s\n' "$base" ;;
    --sync-po)
        python3 - "$ROOT" "$base" <<'PY'
import pathlib, sys

root, base = pathlib.Path(sys.argv[1]), sys.argv[2]
want = '"Project-Id-Version: wash %s\\n"\n' % base
for rel in ("po/en.po", "po/zh_CN.po"):
    path = root / rel
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    hits = [i for i, line in enumerate(lines)
            if line.startswith('"Project-Id-Version: wash ')]
    if len(hits) != 1:
        raise SystemExit("wash_version: %s has %d Project-Id-Version headers"
                         % (rel, len(hits)))
    lines[hits[0]] = want
    path.write_text("".join(lines), encoding="utf-8")
    print("synced %s -> wash %s" % (rel, base))
PY
        ;;
    --check-po)
        rc=0
        want="\"Project-Id-Version: wash $base\\n\""
        for f in "$ROOT/po/en.po" "$ROOT/po/zh_CN.po"; do
            got="$(grep -m1 '^"Project-Id-Version:' "$f" || true)"
            if [ "$got" != "$want" ]; then
                printf 'wash_version: %s header mismatch: want [%s] got [%s]\n' \
                    "$f" "$want" "$got" >&2
                rc=1
            fi
        done
        exit "$rc"
        ;;
    *) die "unknown option: $1" ;;
esac
