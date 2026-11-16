# 内建兼容性矩阵

**这份矩阵是实测结论，不是设计意图。** 每一行的状态都由 `tests/bash_diff_builtins.sh`
（MSYS2 bash 5.3 ↔ wash 逐字节比对 stdout + stderr + 退出码）跑出来，或由针对该内建的
定向探针实测得到。**没实测过的一律不写 `full`。**

状态定义：`full` = 与 bash 5.3 逐字节一致（实测通过）；`partial` = 已实现但存在可观测
差异（备注写明差在哪）；`deferred` = 尚未实现（备注写明为什么延后 + 计划在哪个 Phase 落地）；
`wash-extension` = wash 自有扩展，不参与逐字节对拍。

| 内建 | 状态 | 备注（实测依据） |
| ---- | ---- | ---- |
| `:` | full | 无参与带参数（`: foo`）均与 bash 一致 |
| `true` / `false` | full | 退出码与 stdout 均一致 |
| `echo` | full | `-n` / `-e` / `-E` / `--` 字面量 / 全部转义序列逐字节一致（`tests/bash_diff_echo_printf.sh` 100/100）；stdout 写失败（如 `> /dev/full`）报 `echo: write error: <strerror>` rc=1，亦已对齐 |
| `printf` | full | 全部转换说明符逐字节一致（`%s %d %i %o %u %x %X %e %E %f %g %G %a %A %c %b %q %%`、字段宽度/精度/`*`、`-v`），25 条定向探针全通过。历史缺陷（`%%` 死循环、非法数字静默、空格式串误报 usage、写错误静默）均已修复并复测 |
| `cd` | full | 绝对/相对/逻辑(`/L`)/物理(`/P`)/`CDPATH`/`-`/`--`/空串/不存在/失败后 `PWD` 不变，全部一致（含 bash 的 `cd: too many arguments`，rc=2） |
| `exit` | full | 无参/`42`/`300`/`-1`/被 source 的脚本内 `exit` 一致；`exit abc` 时 bash 报 `exit: abc: numeric argument required`、置 `$?=2`、**继续执行后续命令**，wash 已对齐 |
| `export` | partial | `-p` / 无参 / `NAME=val` / `-n` / 非法标识符报错，格式逐字节一致。**差异**：①`export -f`（导出函数）wash 报 `export: -f: not supported`（rc=2），bash 是 `export: f: not a function`（rc=1）；②wash 不维护 bash 的 `BASH_*` / `BASHOPTS` / `BASH_VERSINFO` 等内部变量（shell 内部变量集合差异）；③`$_` 的导出属性处理与 bash 不同 |
| `unset` | full | `-v` / 裸变量 / 多变量 / 不存在变量 / `-f` / 无参，rc 与报错一致 |
| `set` | partial | 选项处理（`-o` / `+o` / `-e` / `-f` / `-n`（noexec）/ `--` / 位置参数 / 非法选项报错）与 bash **逐字节一致**（`set -o`、`set +o` 全表比对通过）。**差异**：`set` 无参时 bash 打印整个变量表（含 `BASH_*` / `PIPESTATUS` / `RANDOM` / `SECONDS` 等内部变量），wash 只有自己的变量表，集合必然不同 |
| `alias` | full | 无参列出 / `-p` / 定义 / 未定义别名报错 / 非法名报错 / 别名展开，一致 |
| `unalias` | full | `-a` / 删单个 / 不存在 / **无参打印用法行**（rc=2），一致。注意 `unalias` 无参**不是**列出全部别名 |
| `source` / `.` | full | 基本 / `.` 形式 / 带参数 / 不存在 / 无参 / 退出码透传 / `$0` 语义 / 查找顺序（无斜杠时**先查 `PATH`** 再回落当前目录）/ 目录（`<cmd>: <path>: is a directory` rc=1），一致。差异仅在 `$0` 的**字面值**（两个二进制路径不同，GROUP K 改断言语义不变量） |
| `eval` | full | 副作用 / 执行命令 / 空串 / 无参 / `eval 'exit 3'` 退出整 shell / 多词，一致 |
| `exec` | full | `-a name` / 替换当前进程 / 命令不存在（`<path>: No such file or directory` rc=127，shell 终止）/ 无参，一致；ENOEXEC 四类（坏解释器 / 二进制 / 目录 / 纯文本脚本回退）均逐字节对齐 |
| `test` / `[` | full | 数值比较、字符串比较、`-z -n -f -d -e -r -w -x -s -L -h -b -c -p -S`、`!`、`-a`、`-o`、`-nt -ot -ef`、`[` `]` 形式、参数个数错误、缺右括号、八进制数值，全部逐字节一致 |
| `type` | full | 无参 / `-t` / `-a` / `-f` / `-P`，全部逐字节一致 |
| `command` | full | `-v` / `-V` / `-p` / `-a`（非法选项 rc=2）/ 无参直接执行，全部逐字节一致 |
| `builtin` | full | 执行内建 / 非内建报错（rc=1）/ 无参列出内建清单，逐字节一致 |
| `hash` | full | 无参 / `-t <name>` / `-l` / `-r` / `-p <path> <name>` 五种用法逐字节一致 |
| `read` | full | `-r`/`-n`/`-N`/`-d`/`-p`/`REPLY`/EOF rc=1/多变量/`IFS` 分隔均逐字节一致（含前缀赋值 `IFS=: read` 与空 `IFS=`）；`-n`/`-N` 按**裸字节**计数，不做 UTF-8 字符边界重编码 |
| `pwd` | full | `pwd` / `-L` / `-P` 三种形式逐字节一致 |
| `shift` | full | 基本位移 / 连续位移 / 计数按**十进制**解析 / 越界 rc=1，一致 |
| `return` | partial | 已实现；sourced 脚本/函数内 `return n` 正常（rc=n）。**差异**：函数/source 外调用 bash 报中文（rc=2），wash 报英文（rc=1） |
| `break` / `continue` | partial | 已实现；循环内正常。差异：循环外调用 bash 报中文（rc=0+stderr），wash 报英文（rc=1） |
| `local` | partial | 已实现；函数内声明局部变量。差异：函数外调用 bash 报中文（rc=1），wash 报英文（rc=1） |
| `declare` / `typeset` / `readonly` | partial | 已实现：赋值/`declare -i`/`declare -p`/`readonly` 可用。差异：数组下标展开 `${arr[@]}` 待补；`declare -p` 变量集合与 bash 不同（身份差异） |
| `let` | partial | 已实现；`let expr` 算术赋值与 bash 一致。基础算术已对齐；`-p` 等选项待补 |
| `ulimit` | full | 选项解析（非法选项 usage rc=2）、十进制回绕、超范围 `limit out of range`、`invalid number` / `invalid hex number`、软硬限制查询均逐字节一致；setrlimit 成败取决于平台资源上限，诊断文案已对齐 |
| `umask` / `times` | full | `umask`(`-S`/`-p`)/`times` 与 bash 逐字节一致（数值属运行时） |
| `kill` | partial | **发信号路径已覆盖**：`kill -0 $$` rc=0、`kill -TERM $$` rc=143、`kill -TERM <子进程>` 后 `wait` 报 143；`kill -l 9` / `kill -l KILL` 一致；信号 0 探活、不存在进程 `kill: (PID) - No such process`、无参 usage rc=2、非法 pid 文案、`kill -l 0/130` 均已对齐。**剩余差异**：`kill -l` 全量清单格式不同 |
| `trap` | partial | 已实现；无参列出已设陷阱与 bash 一致。差异：信号说明符解析/`-p`/`-l`/`-P` 覆盖不完整 |
| `dirs` / `pushd` / `popd` | partial | 已实现；`dirs`/`pushd dir` 与 bash 逐字节一致。差异：`popd` 空栈 wash 报英文（rc=1，bash 中文 rc=1，rc 已对齐） |
| `help` | partial | 77 主题完整页 + 双 locale 清单逐字节对齐；正文/集合仍待对齐 bash（见有意偏离清单） |
| `enable` | partial | `-a` / `-n` / `-p` / `-s` 静态语义、`-z` 用法错误、未知名报错按 oracle 实测对齐。差异：全量清单内建集合与 bash 不同；`-f`/`-d` 是 `wash-extension` |
| 二进制插件（wash C ABI） | wash-extension | `enable -f/-d`、`BASH_LOADABLES_PATH` 白名单、ABI 协商、多内建注册与热卸载、变量读写；由 golden 契约验证，见 [插件](plugins.md) |

## 有意偏离（唯一真源）

非交互 `-c` 输出默认**逐字节一致**，全部有意例外的单一清单是
[`docs/intentional-divergences.md`](intentional-divergences.md)，当前恰好 2 条：

| ID | 例外 | 裁决 |
| ---- | ---- | ---- |
| D-001 | `help -m` 的 IMPLEMENTATION 段用 wash 身份而非 GNU bash 版权段 | 项目自始既有行为；`tests/help_footer.py` 严格替换 + 可逆性校验 |
| D-002 | `help job_spec` / `help 'job_spec [&]'`（单 pattern 精确）命中 `%` 页，rc=0 | 用户裁决：要、全局生效（含非交互 `-c`）、允许破坏脚本兼容 |

D-002 刻意不进通用别名表：只有"选项解析后恰好 1 个 pattern 且字符串精确命中"才生效，
glob / 多 pattern / 空 pattern / 其余清单显示值 / 别名路径全部仍与 oracle 严格逐字节对拍。
除这 2 条外，任何判定都没有放宽。清单↔页表由 `help_pages.rs::HELP_LIST_MAP` 单一真源绑定，
测试用常量硬断言条目数与用例数。

## 显式延后的内建（有意排期，不是"忘记做"）

| 内建 | 延后原因 | 计划 Phase |
| ---- | ---- | ---- |
| `jobs` / `fg` / `bg` / `wait` / `disown` | 需要完整作业控制（后台进程组、`SIGTSTP`/`SIGCONT`、`$!` 与 jobs 表）；当前执行模型是每条命令 fork/exec | Phase 2 |
| `mapfile` / `readarray` / `coproc` | 需要协进程双向管道与文件→数组读取 | Phase 4 |
| 数组 `-a` / `-A`（下标展开） | 赋值 `ARR=(a b)` 已落地；`declare -a`/`-A`、`${arr[@]}` 待补 | Phase 4 |
| `compgen` / `complete` / `bind` | 补全与键绑定属交互层；Phase 6 已有自研 Tab 补全但未暴露成 bash 风格内建 | Phase 4 之后 |
| `fc` / `history` | 历史由 REPL 持久化，但需以 bash 格式暴露 / 支持范围展开与编辑 | Phase 4 之后 |

## 已知未实现特性（功能缺失，非覆盖空洞）

oracle 实测：`[[ ]]` → `command not found`；`(( ))` 算术命令 → `command not found`
（但 `$(( ))` **展开**已实现对齐）；heredoc `<<`/`<<-`/herestring `<<<` 曾为 rc=2
（后续已实现）；`select` → 语法错误；`shopt -s nullglob` → `invalid shell option name`；
`declare -A`、`getopts`、`trap -l` → **静默 rc=0 但功能未生效**（最危险，勿把它当成功）。
已对齐：if/for/while/until/case、函数、eval、mapfile、local、`>|`、`exec 3>&-`、
`${!pre*}`、`$(( ))`、heredoc/herestring、未闭合 `$(`、重定向逐行部分执行等。

详细根因存档见 `AGENTS.md`「内建兼容性矩阵 → 已修复缺陷的根因存档」。
