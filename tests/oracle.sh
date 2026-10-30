#!/usr/bin/env bash
# Phase 0 oracle harness: compare wash against MSYS2 bash (the reference).
# Run from the msys shell:  MSYSTEM=MSYS bash -lc 'cd /d/project/WorkBuddy/wash && bash tests/oracle.sh'
set -u
WASH="./target/debug/wash"

cmds=(
  "echo hello world"
  "echo a b c | cat"
  "printf '%s\n' one two"
  "echo out1 > /tmp/wt_out.txt; cat /tmp/wt_out.txt"
  "ls /no/such/path 2> /tmp/wt_err.txt; echo err_done"
  "echo 'quoted   spaces'"
  "echo mix\"quote\"end"
  "echo a > /tmp/wt_a.txt; echo b >> /tmp/wt_a.txt; cat /tmp/wt_a.txt"
)

pass=0
fail=0
for c in "${cmds[@]}"; do
  bash_out=$(bash -c "$c" 2>/dev/null; echo "RC=$?")
  wash_out=$($WASH -c "$c" 2>/dev/null; echo "RC=$?")
  if [ "$bash_out" = "$wash_out" ]; then
    echo "PASS: $c"
    pass=$((pass+1))
  else
    echo "FAIL: $c"
    echo "   bash: [$bash_out]"
    echo "   wash: [$wash_out]"
    fail=$((fail+1))
  fi
done

# exit-code propagation
$WASH -c "exit 3"; rc=$?
if [ "$rc" = "3" ]; then echo "PASS: exit-code propagation (wash exit 3 -> $rc)"; pass=$((pass+1)); else echo "FAIL: exit-code (got $rc, want 3)"; fail=$((fail+1)); fi

echo "----"
echo "oracle: $pass passed, $fail failed"
