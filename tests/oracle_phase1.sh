#!/usr/bin/env bash
# Phase 1 oracle: compare wash against MSYS2 bash for the expansion system.
# Commands are single-quoted so the OUTER bash does not pre-expand $.
set -u
WASH="./target/debug/wash"

# each entry: "description | command"
tests=(
  "simple var|X=hello; echo \$X"
  "braced var|X=hello; echo \${X}"
  "default unset|echo \${Y:-fallback}"
  "default set|X=yes; echo \${Y:-fallback}"
  "var length|X=hello; echo \${#X}"
  "substring|X=hello; echo \${X:1:3}"
  "strip prefix short|X=hello; echo \${X#he}"
  "strip prefix long|X=hello; echo \${X##he}"
  "strip suffix short|X=hello; echo \${X%lo}"
  "strip suffix long|X=hello; echo \${X%%l*}"
  "command sub|X=hi; echo \$(echo \$X)"
  "backtick|X=hi; echo \`echo \$X\`"
  "arithmetic|X=2; echo \$((X*3+1))"
  "brace|X=1; echo a{b,c}d"
  "range|echo {1..3}"
  "tilde|echo ~/wash_test_tilde"
  "glob|echo /d/project/WorkBuddy/wash/Cargo.*"
  "split unquoted|X='a b c'; printf '[%s]' \$X; echo"
  "split quoted|X='a b c'; printf '[%s]' \"\$X\"; echo"
  "exit code|false; echo \$?"
)

pass=0
fail=0
for t in "${tests[@]}"; {
  desc="${t%%|*}"
  cmd="${t#*|}"
  bash_out=$(bash -c "$cmd" 2>/dev/null)
  wash_out=$($WASH -c "$cmd" 2>/dev/null)
  if [ "$bash_out" = "$wash_out" ]; then
    echo "PASS: $desc"
    pass=$((pass+1))
  else
    echo "FAIL: $desc"
    echo "   bash: [$bash_out]"
    echo "   wash: [$wash_out]"
    fail=$((fail+1))
  fi
}
echo "----"
echo "oracle phase1: $pass passed, $fail failed"
