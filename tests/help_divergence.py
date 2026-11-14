#!/usr/bin/env python3
"""Strict verifier for the D-002 intentional divergence (`help job_spec`).

This helper does NOT relax any byte comparison.  For one (locale, pattern,
option-mode) case it proves, with exact bytes:

  * oracle side: rc is exactly 1 (or 0 when the case appends `echo rc=$?`),
    stdout is exactly the measured GOLDEN (empty for `job_spec`, one glob
    banner line for `job_spec [&]`), and stderr -- after stripping ONLY the
    shell-name prefix, keeping `line 1: ` / `第 1 行：` -- is exactly the
    measured GOLDEN diagnostic;
  * wash side: rc is exactly 0, stderr is exactly empty, and stdout is
    byte-for-byte equal to wash's OWN `help '<opts> %'` output for the same
    mode/locale/suffix;
  * that canonical `%` page is itself still oracle-identical: exact byte
    equality for Full/Short/Desc, and `tests/help_footer.py`'s strict
    reversible wash-identity footer substitution for Man;
  * the divergence is real (wash stdout != oracle stdout).

The GOLDEN byte strings are independent of both binaries: they were measured
from MSYS2 bash 5.3.20 and are re-verified here by their sha256 digests, so a
transcription or encoding drift fails loudly instead of turning into a fake
PASS.

Usage::

    python3 tests/help_divergence.py <loc:C|ZH> <pattern> <mode:plain|short|desc|man> \\
        <suffix:0|1> <version> \\
        <oracle_rc> <oracle_out> <oracle_err> \\
        <wash_rc> <wash_out> <wash_err> \\
        <canon_oracle_rc> <canon_oracle_out> <canon_oracle_err> \\
        <canon_wash_rc> <canon_wash_out> <canon_wash_err>

See `docs/intentional-divergences.md` (the single divergence list).
"""

import contextlib
import hashlib
import io
import os
import re
import sys

# Importing the sibling verifier must not leave a __pycache__ inside the repo.
sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import help_footer  # noqa: E402  (reuse the existing strict Man verifier)

# ---- GOLDEN oracle bytes (measured from MSYS2 bash 5.3.20) -----------------
GOLDEN_STDOUT = {
    ("C", "job_spec"): b"",
    ("C", "job_spec [&]"): b"Shell commands matching keyword `job_spec [&]'\n\n",
    ("ZH", "job_spec"): b"",
    ("ZH", "job_spec [&]"):
        '匹配关键字 "job_spec [&]" 的 shell 命令\n\n'.encode("utf-8"),
}

GOLDEN_STDERR = {
    ("C", "job_spec"): (
        "line 1: help: no help topics match `job_spec'.  Try `help help' or "
        "`man -k job_spec' or `info job_spec'.\n"
    ).encode("utf-8"),
    ("C", "job_spec [&]"): (
        "line 1: help: no help topics match `job_spec [&]'.  Try `help help' or "
        "`man -k job_spec [&]' or `info job_spec [&]'.\n"
    ).encode("utf-8"),
    ("ZH", "job_spec"): (
        '第 1 行：help: 没有与 "job_spec" 匹配的帮助主题。尝试使用 '
        '"help help" 或 "man -k job_spec" 或 "info job_spec"。\n'
    ).encode("utf-8"),
    ("ZH", "job_spec [&]"): (
        '第 1 行：help: 没有与 "job_spec [&]" 匹配的帮助主题。尝试使用 '
        '"help help" 或 "man -k job_spec [&]" 或 "info job_spec [&]"。\n'
    ).encode("utf-8"),
}

# sha256 of the four GOLDEN stdout / stderr values above, measured 2026-11-10.
# A hard self-check so no GOLDEN can silently drift.
GOLDEN_STDOUT_SHA = {
    ("C", "job_spec"):
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ("C", "job_spec [&]"):
        "aa4fdfc28b8644fa135d74a432e852f79d865b3979ef563da34a3cb769e1a92a",
    ("ZH", "job_spec"):
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    ("ZH", "job_spec [&]"):
        "7519d8ec25268a2ceab16fa49a0620959edb9043dea956c17b325657a4a05eb0",
}
GOLDEN_STDERR_SHA = {
    ("C", "job_spec"):
        "7af8d35fc3d1bf249ddebb7734cb4ba396097a225eb9b3334fa5fe68b58b6544",
    ("C", "job_spec [&]"):
        "dc984e3246afb01e56bc62e85c44687a0df90062147437bd1085b297b62d710e",
    ("ZH", "job_spec"):
        "5a55e1a9f08c7b50267bb6a098be93f68df34e9ef9e8fa6876f9a17cbfadf0ec",
    ("ZH", "job_spec [&]"):
        "733f11788bfdaaa9680209bde3acb08ec6960b8a9ba42a46eeb11f74f06501da",
}

# Strip ONLY the shell-name prefix, keeping the `line 1: ` / `第 1 行：`
# locale marker so it is part of the exact assertion (not normalized away).
SHELL_PREFIX = re.compile(rb"^\S*bash(?:\.exe)?: ")


def strip_shell(data):
    """Remove the shell-name prefix from raw oracle stderr bytes."""
    return SHELL_PREFIX.sub(b"", data)


def read_bytes(path):
    with open(path, "rb") as f:
        return f.read()


def check(cond, msg):
    if not cond:
        raise AssertionError(msg)


def first_diff(a, b):
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return i
    return min(len(a), len(b))


def main():
    if len(sys.argv) != 18:
        print("usage: help_divergence.py <loc> <pattern> <mode> <suffix> <version> "
              "<o_rc> <o_out> <o_err> <w_rc> <w_out> <w_err> "
              "<co_rc> <co_out> <co_err> <cw_rc> <cw_out> <cw_err>")
        return 2

    (loc, pattern, mode, suffix, version,
     o_rc_s, o_out_p, o_err_p,
     w_rc_s, w_out_p, w_err_p,
     co_rc_s, co_out_p, co_err_p,
     cw_rc_s, cw_out_p, cw_err_p) = sys.argv[1:]

    try:
        check(loc in ("C", "ZH"), "locale must be C or ZH: %r" % loc)
        check(pattern in ("job_spec", "job_spec [&]"),
              "pattern is not in the divergence list: %r" % pattern)
        check(mode in ("plain", "short", "desc", "man"),
              "unknown mode: %r" % mode)
        check(suffix in ("0", "1"), "suffix must be 0 or 1: %r" % suffix)
        check(not (suffix == "1" and mode == "man"),
              "the echo-suffix is only used with plain mode")
        check(bool(version), "empty generated version")

        key = (loc, pattern)
        gold_out = GOLDEN_STDOUT[key]
        gold_err = GOLDEN_STDERR[key]
        # GOLDEN integrity: independent of the binaries under test.
        check(hashlib.sha256(gold_out).hexdigest() == GOLDEN_STDOUT_SHA[key],
              "GOLDEN stdout sha256 drifted for %r" % (key,))
        check(hashlib.sha256(gold_err).hexdigest() == GOLDEN_STDERR_SHA[key],
              "GOLDEN stderr sha256 drifted for %r" % (key,))

        o_rc, w_rc = int(o_rc_s), int(w_rc_s)
        co_rc, cw_rc = int(co_rc_s), int(cw_rc_s)
        o_out, o_err = read_bytes(o_out_p), read_bytes(o_err_p)
        w_out, w_err = read_bytes(w_out_p), read_bytes(w_err_p)
        co_out, co_err = read_bytes(co_out_p), read_bytes(co_err_p)
        cw_out, cw_err = read_bytes(cw_out_p), read_bytes(cw_err_p)

        has_echo = suffix == "1"
        want_o_rc = 0 if has_echo else 1
        want_o_out = gold_out + (b"rc=1\n" if has_echo else b"")

        # --- oracle side: exact rc + stdout + stderr -----------------------
        check(o_rc == want_o_rc,
              "oracle rc=%d, expected %d" % (o_rc, want_o_rc))
        check(o_out == want_o_out,
              "oracle stdout differs at byte %d (got %dB, want %dB): %r"
              % (first_diff(o_out, want_o_out), len(o_out), len(want_o_out),
                 o_out[:200]))
        o_err_n = strip_shell(o_err)
        check(o_err_n == gold_err,
              "oracle stderr (shell prefix stripped) differs at byte %d: "
              "got %r want %r"
              % (first_diff(o_err_n, gold_err), o_err_n[:240], gold_err[:240]))
        check(b"job_spec" in o_err,
              "oracle diagnostic lost the `job_spec` token: %r" % o_err[:240])
        if loc == "ZH":
            check("第 1 行：".encode("utf-8") in o_err,
                  "zh_CN oracle stderr lost the `第 1 行：` prefix")
            check(b"line 1: " not in o_err,
                  "zh_CN oracle stderr fell back to the English `line 1: ` prefix")

        # --- wash side: exact rc=0 + empty stderr + stdout == wash `help '%'` -
        check(w_rc == 0, "wash rc=%d, expected 0" % w_rc)
        check(w_err == b"", "wash stderr is not empty: %r" % w_err[:240])
        check(w_out == cw_out,
              "wash stdout != wash canonical `help '%%'` at byte %d "
              "(%dB vs %dB)"
              % (first_diff(w_out, cw_out), len(w_out), len(cw_out)))
        check(w_out != o_out,
              "no real divergence: wash stdout equals oracle stdout")

        # --- canonical `%` page still oracle-identical ---------------------
        check(cw_rc == 0, "wash canonical rc=%d, expected 0" % cw_rc)
        check(cw_err == b"", "wash canonical stderr is not empty")
        check(co_rc == 0, "oracle canonical rc=%d, expected 0" % co_rc)
        check(co_err == b"", "oracle canonical stderr is not empty")
        if mode == "man":
            # Reuse the existing D-001 verifier: strict footer substitution,
            # reversibility, equal footer counts, pinned generated version.
            saved = sys.argv
            buf = io.StringIO()
            try:
                sys.argv = ["help_footer.py", co_out_p, cw_out_p, loc, version]
                with contextlib.redirect_stdout(buf):
                    frc = help_footer.main()
            finally:
                sys.argv = saved
            check(frc == 0,
                  "canonical `help -m '%%'` footer check failed: %s"
                  % buf.getvalue().strip())
        else:
            check(co_out == cw_out,
                  "canonical `help '%%'` differs from oracle at byte %d "
                  "(%dB vs %dB)"
                  % (first_diff(co_out, cw_out), len(co_out), len(cw_out)))

        print("OK: D-002 loc=%s pattern=%r mode=%s echo=%s "
              "oracle(rc=%d out=%dB err=%dB) wash(rc=0 out=%dB err=0B) "
              "== wash canonical `%%` page"
              % (loc, pattern, mode, suffix, o_rc, len(o_out), len(o_err),
                 len(w_out)))
        return 0
    except (AssertionError, OSError, ValueError) as exc:
        print("FAIL: D-002 loc=%s pattern=%r mode=%s echo=%s :: %s"
              % (loc, pattern, mode, suffix, exc))
        return 1


if __name__ == "__main__":
    sys.exit(main())
