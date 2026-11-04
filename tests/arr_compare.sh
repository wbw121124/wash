#!/bin/bash
# Compare wash vs bash on a set of array / background cases.
W=/d/project/WorkBuddy/wash/target/debug/wash.exe
B=/d/msys2/usr/bin/bash.exe

run_case() {
  local desc="$1"; shift
  local script="$1"
  printf '### %s\n' "$desc"
  printf '  script: %s\n' "$script"
  printf '  wash : '
  "$W" -c "$script" 2>&1 | tr '\n' '~'
  printf '\n'
  printf '  bash : '
  "$B" -c "$script" 2>&1 | tr '\n' '~'
  printf '\n'
}

run_case 'index'        'a=(1 2 3); echo "${a[0]} ${a[1]} ${a[2]}"'
run_case 'at'           'a=(1 2 3); printf "[%s]" "${a[@]}"; echo'
run_case 'star'         'a=(1 2 3); printf "[%s]" "${a[*]}"; echo'
run_case 'count'        'a=(x y); echo "${#a[@]}"'
run_case 'scalar-elem'  'a=(foo bar); echo "$a"'
run_case 'empty-elem'   'a=(); echo "n=${#a[@]}"'
run_case 'quoted-elems' 'a=("x y" z); printf "[%s]" "${a[@]}"; echo'
run_case 'declare-a'    'declare -a b; declare -p b'
run_case 'declare-a2'   'a=(1 2); declare -p a'
run_case 'set-output'   'a=(1 2); set | grep "^a="'
run_case 'append'       'a=(1 2); a+=(3); printf "[%s]" "${a[@]}"; echo'
run_case 'unset-idx'    'a=(1 2); echo "[${a[5]}]"'
run_case 'arr-len'      'a=(abc); echo "${#a[0]}"'
run_case 'bg'           'sleep 0.1 & echo "started"'
run_case 'bg-pid'       'sleep 0.1 & echo "pid=$!"'
run_case 'bg-then-echo' 'sleep 0.1 & wait; echo done'
run_case 'bg-list'      'sleep 0.1 & jobs'
