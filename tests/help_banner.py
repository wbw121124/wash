#!/usr/bin/env python3
"""Strict, skip-free comparison of the no-argument `help` version banner.

The old suite ran ``help | tail -n +2`` on BOTH shells, i.e. it deleted the
version line before comparing.  That is a real skip, not a byte comparison.
This verifier instead:

  1. extracts the oracle's first line from the ORACLE bytes themselves (no
     oracle version text is hardcoded);
  2. substitutes ``wash, version <generated>`` -- the string produced by
     ``tools/wash_version.sh`` from Cargo.toml + git tag/HEAD;
  3. requires the resulting byte stream to equal wash's COMPLETE stdout, with
     nothing dropped from either side;
  4. requires wash's first line to be exactly that generated version line;
  5. requires the substitution to be reversible;
  6. forbids any GNU bash version string surviving in wash's output.

Any other differing byte is a hard failure.  Usage::

    python3 tests/help_banner.py <oracle_stdout> <wash_stdout> <version>
"""

import sys


def first_diff(a, b):
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return i
    return min(len(a), len(b))


def main():
    if len(sys.argv) != 4:
        print("usage: help_banner.py <oracle> <wash> <version>")
        return 2
    with open(sys.argv[1], "rb") as f:
        oracle = f.read()
    with open(sys.argv[2], "rb") as f:
        wash = f.read()
    try:
        want = b"wash, version " + sys.argv[3].encode("ascii") + b"\n"
    except UnicodeEncodeError:
        print("version must be ASCII")
        return 2

    nl = oracle.find(b"\n")
    if nl < 0:
        print("oracle output has no first line")
        return 1
    oracle_line = oracle[:nl + 1]
    if not oracle_line.strip():
        print("oracle first line is empty")
        return 1
    if oracle_line == want:
        print("oracle first line already equals wash's version line: identity check is vacuous")
        return 1

    if not wash.startswith(want):
        i = first_diff(want, wash[:len(want)])
        print("wash banner is not the generated version line at byte %d" % i)
        print("  expected: %r" % want)
        print("  wash    : %r" % wash[:nl + 1])
        return 1

    expected = want + oracle[nl + 1:]
    if expected != wash:
        i = first_diff(expected, wash)
        print("substituted oracle != wash at byte %d (expected %dB, got %dB)"
              % (i, len(expected), len(wash)))
        print("  expected: %r" % expected[max(0, i - 60):i + 60])
        print("  wash    : %r" % wash[max(0, i - 60):i + 60])
        return 1

    restored = oracle_line + wash[len(want):]
    if restored != oracle:
        i = first_diff(restored, oracle)
        print("reverse substitution != oracle at byte %d" % i)
        return 1
    if b"GNU bash, version" in wash:
        print("wash output still carries the GNU bash version banner")
        return 1

    print("OK: full byte-identical help output, version banner = %r" % want.rstrip())
    return 0


if __name__ == "__main__":
    sys.exit(main())
