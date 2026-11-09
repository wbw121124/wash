#!/usr/bin/env python3
"""Regenerate the ``help`` topic-page data files from the MSYS2 bash 5.3 oracle.

Run under MSYS2 (the project's authoritative oracle), from any directory::

    MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
        'cd <WASH_REPO> && python3 tools/gen_help_pages.py'

Use ``--check`` to verify both checked-in files still match the oracle without
rewriting them.  Two catalogs are produced::

    src/help_pages.txt           LC_ALL=C
    src/help_pages_zh_CN.txt     LC_ALL=zh_CN.UTF-8

Every record is::

    @@@ <topic name> @@@\\n
    <full help page for that topic>\\n

Topic *names* stay in English in both catalogs (bash's canonical index); only
the page text, synopsis and long description are translated by gettext.

Self-checks performed for each locale:

  * the 77 individual pages concatenate exactly to ``help '*'`` body;
  * ``help -s '*'`` body == 77 ``name: usage`` lines;
  * ``help -m NAME`` == NAME/SYNOPSIS/DESCRIPTION + page body + the oracle's
    own SEE ALSO / IMPLEMENTATION footer (extracted from the oracle, not
    hardcoded -- wash deliberately prints its *own* footer at runtime, see
    ``src/help_pages.rs``);
  * the zh_CN ``help -d '*'`` body is byte-identical to the C one: bash's
    short_doc is NOT gettext-translated, so ``Mode::Desc`` must always use the
    English catalog.

The script also maintains a generated block in ``po/en.po`` and
``po/zh_CN.po``: the 5 banner sentences plus the 77 short synopsis lines of
the **no-argument** ``help`` list.  Their English/ZH display values are taken
from the oracle (bash truncates long synopses differently per locale), and the
column-padding rule in ``src/builtins.rs`` is re-verified byte-for-byte against
both oracle lists on every run.

Structural gate (run in FULL on every invocation, including regeneration).
The old design kept the list and the pages as two independent
hardcoded arrays that only *happened* to have the same order.  Now the list is
the first column of ``HELP_LIST_MAP`` in ``src/help_pages.rs`` and every
assertion below is measured against the oracle, never a self-filled golden::

  * oracle ``help -s '*'`` -> 77 canonical names == second column, term by term;
  * the 77 ``@@@ name @@@`` markers of BOTH checked-in data files == that column;
  * for each canonical name the oracle's full ``help '<name>'`` stdout is
    byte-identical to the stored page body (not merely rc), and rc == 0, i.e.
    every listed row really has one queryable canonical key;
  * the bash-inherent orphan is locked: oracle ``help job_spec`` is rc=1 while
    ``help '%'`` is rc=0, ``ALIASES`` registers no ``job_spec`` alias, and the
    built wash binary reproduces rc=1 / rc=0 with byte-identical stdout/stderr.

Every invocation requires a built wash binary (``$WASH_BIN`` or
``target/debug/wash[.exe]`` / ``target/release/wash[.exe]``); a missing binary
is a hard error, never a skipped assertion.
"""

import os
import re
import shlex
import subprocess
import sys

BASH = "/d/msys2/usr/bin/bash.exe"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

LOCALES = (
    ("C", "src/help_pages.txt"),
    ("zh_CN.UTF-8", "src/help_pages_zh_CN.txt"),
)

HEADER = (
    b"# GNU bash 5.3 help pages, LC_ALL=%s, MSYS2 oracle.\n"
    b"# Format: @@@ <topic> @@@\\n followed by the full page text.\n"
    b"# Regenerate with: python3 tools/gen_help_pages.py (under MSYS2).\n"
)

# bash prints a banner when a single topic name itself contains glob
# metacharacters; strip it in either language before storing the page.
BANNERS = (b"Shell commands matching keyword", "匹配关键字".encode("utf-8"))

# ---- generated PO block for the no-argument `help` list --------------------
LIST_PO_BEGIN = "# --- BEGIN GENERATED help list (tools/gen_help_pages.py) ---"
LIST_PO_END = "# --- END GENERATED help list ---"
LIST_PO_PATHS = ("po/en.po", "po/zh_CN.po")
# Line indices of the 5 banner sentences inside `help`'s 8-line header.
BANNER_IDX = (1, 2, 3, 4, 6)

# The single source of the list -> page binding, and the alias table that must
# never grow a `job_spec` entry.  Both live in src/help_pages.rs.
HELP_MAP_MARKER = "pub(crate) const HELP_LIST_MAP: &[(&str, &str)] = &["
ALIASES_MARKER = "const ALIASES: &[(&str, &str)] = &["


def read_help_map():
    """Parse HELP_LIST_MAP from src/help_pages.rs -> (display column, name column, source)."""
    path = os.path.join(ROOT, "src", "help_pages.rs")
    with open(path, encoding="utf-8") as f:
        src = f.read()
    assert HELP_MAP_MARKER in src, "HELP_LIST_MAP table missing from src/help_pages.rs"
    block = src.split(HELP_MAP_MARKER, 1)[1].split("\n];", 1)[0]
    pairs = re.findall(r'\(\s*r#"(.*?)"#,\s*"([^"]*)"\s*\)', block)
    assert len(pairs) == 77, ("HELP_LIST_MAP must hold 77 pairs", len(pairs))
    displays = [p[0] for p in pairs]
    names = [p[1] for p in pairs]
    assert len(set(displays)) == 77, "duplicate display value in HELP_LIST_MAP"
    assert len(set(names)) == 77, "duplicate canonical name in HELP_LIST_MAP"
    return displays, names, src


def read_alias_pairs(src):
    """Parse the ALIASES table; used to forbid a `job_spec` alias statically."""
    assert ALIASES_MARKER in src, "ALIASES table missing from src/help_pages.rs"
    block = src.split(ALIASES_MARKER, 1)[1].split("\n];", 1)[0]
    pairs = re.findall(r'\(\s*"([^"]*)"\s*,\s*"([^"]*)"\s*\)', block)
    assert pairs, "ALIASES table is empty or unparsable"
    return pairs


def read_data_pages(relpath):
    """Read the checked-in data file -> (77 marker names, 77 full page bodies)."""
    with open(os.path.join(ROOT, relpath), "rb") as f:
        data = f.read()
    pos = 0
    while data.startswith(b"#", pos):
        eol = data.find(b"\n", pos)
        assert eol >= 0, (relpath, "unterminated header comment")
        pos = eol + 1
    names, bodies = [], []
    while data.startswith(b"@@@ ", pos):
        eol = data.find(b"\n", pos)
        assert eol >= 0, (relpath, "unterminated page marker")
        assert data[eol - 4:eol] == b" @@@", (relpath, "bad marker suffix")
        names.append(data[pos + 4:eol - 4].decode("utf-8"))
        nxt = data.find(b"\n@@@ ", eol + 1)
        if nxt < 0:
            body = data[eol + 1:]
            pos = len(data)
        else:
            body = data[eol + 1:nxt + 1]
            pos = nxt + 1
        assert body.endswith(b"\n"), (relpath, names[-1], "page body must end with LF")
        bodies.append(body)
    assert pos == len(data), (relpath, "trailing bytes after last page")
    assert len(names) == 77, (relpath, "expected 77 pages", len(names))
    return names, bodies


def normalize_stderr(data):
    """Strip only the shell-name/line prefix, exactly like the diff test suite."""
    text = data.decode("utf-8", "replace")
    return re.sub(
        r"^\S*(?:bash|wash)(?:\.exe)?: (?:line \d+: |第 \d+ 行：)?",
        "", text,
    )


def wash_help(wash, name, env):
    """Run `help '<name>'` in the built wash binary."""
    proc = subprocess.run(
        [wash, "-c", "help " + shlex.quote(name)],
        capture_output=True,
        env=env,
    )
    return proc.returncode, proc.stdout, proc.stderr


def find_wash_bin():
    """Locate the built wash binary.  A missing binary is a hard error, not a skip."""
    candidates = [os.environ.get("WASH_BIN")]
    for rel in ("target/debug/wash.exe", "target/debug/wash",
                "target/release/wash.exe", "target/release/wash"):
        candidates.append(os.path.join(ROOT, rel))
    for cand in candidates:
        if cand and os.path.isfile(cand) and os.access(cand, os.X_OK):
            return cand
    raise SystemExit(
        "FATAL: built wash binary not found; run cargo build first or set WASH_BIN "
        "(the job_spec orphan gate must actually execute, never be skipped)"
    )


def check_bindings():
    """Oracle-backed structural gate: list display values <-> canonical page keys."""
    displays, names, hp_src = read_help_map()
    env = env_for("C")

    # 1. oracle `help -s '*'` names == HELP_LIST_MAP second column, term by term.
    oracle_names = topic_names(env)
    assert oracle_names == names, (
        "HELP_LIST_MAP canonical column != oracle `help -s '*'` names",
        [(i, a, b) for i, (a, b) in enumerate(zip(oracle_names, names)) if a != b][:5],
    )
    print("VERIFIED canonical names: 77/77 == oracle `help -s '*'`")

    # 2. checked-in data markers == second column (both catalogs).
    en_names, en_bodies = read_data_pages("src/help_pages.txt")
    zh_names, _ = read_data_pages("src/help_pages_zh_CN.txt")
    assert en_names == names, "src/help_pages.txt markers != HELP_LIST_MAP second column"
    assert zh_names == names, "src/help_pages_zh_CN.txt markers != HELP_LIST_MAP second column"
    print("VERIFIED data markers: 77/77 == HELP_LIST_MAP (C + zh_CN catalogs)")

    # 3. per canonical name: full oracle stdout == stored body, byte-for-byte.
    #    `help "$@" -- <name>` keeps the name one literal argv element, exactly
    #    like `help '<name>'`.  A name that is itself a glob (only `[[ ... ]]`
    #    today) makes bash print a keyword banner; strip it, but first prove the
    #    removed prefix is exactly that banner so nothing else can be dropped.
    for i, name in enumerate(names):
        rc, out, err = help_raw(["--", name], env)
        assert rc == 0 and not err, (
            "listed topic is not queryable in oracle", name, rc, err
        )
        body = strip_banner(out)
        if body != out:
            prefix = out[:len(out) - len(body)]
            want = (b"Shell commands matching keyword `"
                    + name.encode("utf-8") + b"'\n\n")
            assert prefix == want, ("unexpected glob banner", name, prefix, want)
        assert body == en_bodies[i], (
            "oracle full page != stored body", name, len(body), len(en_bodies[i])
        )
    print("VERIFIED listed-is-queryable: 77/77 names rc=0, full stdout == data body byte-for-byte")

    # 4. Lock bash's inherent orphan on BOTH sides.  The list shows
    #    `job_spec [&]` but its key is `%`; bash itself rejects `help job_spec`.
    assert displays[1] == "job_spec [&]", ("orphan row moved", displays[1])
    assert names[1] == "%", ("orphan key is not %", names[1])
    assert "job_spec" not in names, "no canonical key may be job_spec"
    aliases = read_alias_pairs(hp_src)
    assert all(a != "job_spec" and t != "job_spec" for a, t in aliases), \
        "wash must NOT register a job_spec alias (byte-for-byte compatibility)"
    print("VERIFIED static: ALIASES has no job_spec entry (%d aliases)" % len(aliases))

    rc, out, err = help_raw(["--", "job_spec"], env)
    assert rc == 1 and not out, ("oracle `help job_spec` must be rc=1", rc, out)
    assert b"job_spec" in err, ("oracle job_spec diagnostic lost", err)
    rc_pct, out_pct, err_pct = help_raw(["--", "%"], env)
    assert rc_pct == 0 and not err_pct, ("oracle `help '%'` must be rc=0", rc_pct, err_pct)
    assert out_pct == en_bodies[1], "oracle `help '%'` page != stored % page"

    # Wash side is checked on EVERY run, never conditionally skipped: the same
    # rc + stdout + normalized stderr must hold for the orphan and its real key.
    wash = find_wash_bin()
    wrc, wout, werr = wash_help(wash, "job_spec", env)
    assert wrc == 1 and wout == out, (
        "wash `help job_spec` must be rc=1 with empty stdout", wrc, wout
    )
    assert normalize_stderr(werr) == normalize_stderr(err), (
        "wash job_spec stderr != oracle", werr, err
    )
    wrc2, wout2, werr2 = wash_help(wash, "%", env)
    assert wrc2 == 0 and not werr2 and wout2 == out_pct, (
        "wash `help '%'` must be rc=0 with the oracle page", wrc2, werr2
    )
    print("VERIFIED wash: job_spec rc=1 / '%%' rc=0 match oracle (%s)"
          % os.path.basename(wash))
    print("VERIFIED orphan: job_spec is a bash-inherent rc=1 anomaly, '%' is the real key")


def display_width(s):
    """Mirror src/builtins.rs::display_width: ASCII 1 column, everything else 2."""
    return sum(1 if ord(c) < 128 else 2 for c in s)


def fetch_help_list(locale):
    """Full no-argument `help` output, split into lines."""
    proc = subprocess.run([BASH, "-c", "help"], capture_output=True, env=env_for(locale))
    assert proc.returncode == 0 and not proc.stderr, (locale, proc.returncode, proc.stderr)
    lines = proc.stdout.decode("utf-8").splitlines()
    assert len(lines) == 47, (locale, len(lines))
    assert lines[5] == "" and lines[7] == "", (locale, lines[:8])
    return lines


def split_list_rows(lines):
    """Split the 39 two-column rows into the 77 entries (column-major)."""
    body = lines[8:]
    assert len(body) == 39
    left, right = [], []
    for row in body:
        assert row.startswith(" ")
        m = re.search(r" {2,}", row[1:])
        if m:
            left.append(row[1:1 + m.start()])
            r = row[1 + m.end():].rstrip()
            if r:
                right.append(r)
        else:
            left.append(row[1:])
    entries = left + right
    assert len(left) == 39 and len(right) == 38 and len(entries) == 77, \
        (len(left), len(right), len(entries))
    return entries, body


def rebuild_list(entries, body, locale):
    """Re-apply the Rust column renderer and require an exact byte match."""
    nrow = (len(entries) + 1) // 2
    rebuilt = []
    for r in range(nrow):
        l = entries[r]
        rr = entries[nrow + r] if nrow + r < len(entries) else None
        if rr is None:
            rebuilt.append(" " + l)
        else:
            w = display_width(l)
            # `... >` entries are truncated by bash; a wide char straddling the
            # boundary is still billed as at least 37 columns.
            eff = max(w, 37) if l.endswith(">") else w
            rebuilt.append(" " + l + " " * (1 + max(0, 39 - eff)) + rr)
    assert rebuilt == body, (locale, "column reconstruction differs from oracle")
    return rebuilt


def list_po_fragments():
    """Return {po path: generated block} after verifying every assumption."""
    c_lines = fetch_help_list("C")
    zh_lines = fetch_help_list("zh_CN.UTF-8")
    c_entries, c_body = split_list_rows(c_lines)
    zh_entries, zh_body = split_list_rows(zh_lines)
    rebuild_list(c_entries, c_body, "C")
    rebuild_list(zh_entries, zh_body, "zh_CN.UTF-8")

    # The msgids must be exactly the strings `tr!` looks up at runtime: the
    # first column of HELP_LIST_MAP, the single source of the no-argument list.
    # The old HELP_TOPICS constant in builtins.rs was a second independent copy
    # of this data and has been deleted; the assertion below keeps the column
    # locked to the C oracle list with exactly the old strength.
    displays, _, _ = read_help_map()
    assert displays == c_entries, \
        "HELP_LIST_MAP display column no longer matches the C oracle list"
    assert len(set(c_entries)) == 77, "duplicate msgid in help list"
    assert any(ord(ch) > 127 for ch in "".join(zh_entries + [zh_lines[1]])), \
        "zh_CN oracle list is not localized"
    print("VERIFIED help list: 77 entries, both locales reproduce the oracle "
          "column layout byte-for-byte")

    def fragment(strs_lines, strs_entries):
        pairs = [(c_lines[i], strs_lines[i]) for i in BANNER_IDX]
        pairs += list(zip(c_entries, strs_entries))
        assert len(pairs) == 82

        def esc(s):
            # .po strings must stay on one line; i18n::unescape understands
            # both escapes (and the ZH banner contains ASCII `"` quotes).
            assert "\n" not in s and "\r" not in s, s
            return s.replace("\\", "\\\\").replace('"', '\\"')

        out = [LIST_PO_BEGIN,
               "# Generated from the MSYS2 bash 5.3 oracle: msgid = LC_ALL=C text,",
               "# msgstr = the same line under zh_CN.UTF-8 (en.po mirrors msgid).",
               "# Truncated `...>` entries are bash's own display values, not full",
               "# sentences, so the ZH side is extracted rather than translated.",
               "# Do not edit by hand -- run: python3 tools/gen_help_pages.py",
               ""]
        for mid, mstr in pairs:
            out += ['msgid "%s"' % esc(mid), 'msgstr "%s"' % esc(mstr), ""]
        out.append(LIST_PO_END)
        return "\n".join(out) + "\n"

    return {
        "po/en.po": fragment(c_lines, c_entries),
        "po/zh_CN.po": fragment(zh_lines, zh_entries),
    }


def sync_po(relpath, fragment, check):
    """Insert/replace the generated block, or just verify it under --check."""
    path = os.path.join(ROOT, relpath)
    with open(path, encoding="utf-8") as f:
        text = f.read()
    if LIST_PO_BEGIN in text:
        pre, rest = text.split(LIST_PO_BEGIN, 1)
        assert LIST_PO_END in rest, (relpath, "unterminated generated block")
        _, post = rest.split(LIST_PO_END, 1)
        post = post.lstrip("\n")
        new = pre + fragment + post
    else:
        new = text.rstrip("\n") + "\n\n" + fragment
    if check:
        if new != text:
            print("MISMATCH: %s generated help-list block is stale" % relpath)
            return False
        print("OK: %s help-list block matches the oracle" % relpath)
        return True
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(new)
    print("WROTE %s help-list block (%d bytes)" % (relpath, len(fragment)))
    return True


def env_for(locale):
    return dict(os.environ, LC_ALL=locale, LANG=locale)


def help_raw(args, env):
    """Invoke `help "$@"` so every topic name stays one literal argv element."""
    proc = subprocess.run(
        [BASH, "-c", 'help "$@"', "bash"] + list(args),
        capture_output=True,
        env=env,
    )
    return proc.returncode, proc.stdout, proc.stderr


def strip_banner(page):
    for marker in BANNERS:
        if page.startswith(marker):
            _, sep, page = page.partition(b"\n\n")
            assert sep, marker
    return page


def topic_names(env):
    rc, out, err = help_raw(["-s", "*"], env)
    assert rc == 0 and not err, (rc, err)
    lines = out.decode("utf-8").splitlines()
    assert lines[1] == "", lines[:2]
    names = [line.split(": ", 1)[0] for line in lines[2:] if ": " in line]
    assert len(names) == 77, "expected 77 topics, got %d" % len(names)
    return names


def build(locale, relpath):
    env = env_for(locale)
    names = topic_names(env)
    pages = []
    for name in names:
        rc, page, err = help_raw(["--", name], env)
        assert rc == 0 and not err, (name, rc, err)
        page = strip_banner(page)
        assert page.endswith(b"\n"), (name, page[-40:])
        first = page.split(b"\n", 1)[0]
        assert first.startswith(name.encode("utf-8") + b": "), (name, first)
        pages.append((name, page))

    # 1. full pages concatenate to `help '*'` body.
    rc, allout, allerr = help_raw(["*"], env)
    assert rc == 0 and not allerr
    _, sep, body = allout.partition(b"\n\n")
    assert sep
    joined = b"".join(page for _, page in pages)
    assert joined == body, (locale, len(joined), len(body))

    # 2. -s derivation.
    rc, sout, serr = help_raw(["-s", "*"], env)
    assert rc == 0 and not serr
    _, sep, sbody = sout.partition(b"\n\n")
    assert sep
    short = b"".join(name.encode() + b": " + page.split(b"\n", 1)[0].split(b": ", 1)[1] + b"\n"
                     for name, page in pages)
    assert short == sbody, (locale, "-s", len(short), len(sbody))

    # 3. -m derivation, with the oracle footer taken from the oracle itself.
    rc, mprobe, merr = help_raw(["-m", "--", "cd"], env)
    assert rc == 0 and not merr
    mprobe = strip_banner(mprobe)
    footer_at = mprobe.rfind(b"\nSEE ALSO\n")
    assert footer_at >= 0
    oracle_footer = mprobe[footer_at:]
    cd_page = dict(pages)["cd"]
    head, _, cd_rest = cd_page.partition(b"\n")
    cd_desc = cd_rest.split(b"\n", 1)[0].strip()
    cd_prefix = (b"NAME\n    cd - " + cd_desc
                 + b"\n\nSYNOPSIS\n    " + head.split(b": ", 1)[1]
                 + b"\n\nDESCRIPTION\n")
    assert mprobe == cd_prefix + cd_rest + oracle_footer

    rc, mout, merr = help_raw(["-m", "*"], env)
    assert rc == 0 and not merr
    _, sep, mbody = mout.partition(b"\n\n")
    assert sep
    man = []
    for name, page in pages:
        first, _, rest = page.partition(b"\n")
        usage = first.split(b": ", 1)[1]
        desc = rest.split(b"\n", 1)[0].strip()
        prefix = (b"NAME\n    " + name.encode() + b" - " + desc
                  + b"\n\nSYNOPSIS\n    " + usage + b"\n\nDESCRIPTION\n")
        man.append(prefix + rest + oracle_footer)
    assert b"".join(man) == mbody, (locale, "-m")

    # 4. -d body is always English (short_doc is not gettext-translated).
    rc, dout, derr = help_raw(["-d", "*"], env)
    assert rc == 0 and not derr
    _, sep, dbody = dout.partition(b"\n\n")
    assert sep
    print("VERIFIED %-12s 77 pages / %dB / -s -d -m derivations" % (locale, len(joined)))
    return bytes(HEADER % locale.encode()), pages, dbody


def blob_for(header, pages):
    blob = bytearray(header)
    for name, page in pages:
        blob += b"@@@ " + name.encode("utf-8") + b" @@@\n"
        blob += page
    return bytes(blob)


def main():
    check = "--check" in sys.argv[1:]
    results = []
    d_bodies = {}
    for locale, relpath in LOCALES:
        header, pages, dbody = build(locale, relpath)
        d_bodies[locale] = dbody
        results.append((relpath, blob_for(header, pages)))
    assert d_bodies["C"] == d_bodies["zh_CN.UTF-8"], \
        "-d short_doc unexpectedly differs between locales"
    print("VERIFIED -d body is byte-identical in C and zh_CN (short_doc not translated)")

    ok = True
    for relpath, blob in results:
        path = os.path.join(ROOT, relpath)
        if check:
            with open(path, "rb") as f:
                current = f.read()
            if current != blob:
                print("MISMATCH: %s is stale (%d vs %d bytes)" % (relpath, len(current), len(blob)))
                ok = False
            else:
                print("OK: %s matches the oracle (%d bytes)" % (relpath, len(blob)))
        else:
            with open(path, "wb") as f:
                f.write(blob)
            print("WROTE %s (%d bytes)" % (relpath, len(blob)))

    for relpath, fragment in list_po_fragments().items():
        if not sync_po(relpath, fragment, check):
            ok = False
    # Structural gate runs LAST so that regeneration mode first refreshes the
    # checked-in data files and then verifies them against the oracle.  It is
    # never skipped: a missing wash binary is a hard error.
    check_bindings()
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
