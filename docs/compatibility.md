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

## 裸字节保真（byteval 契约）

`LC_ALL=C` 下 bash 的变量是裸字节序列，`read -n` 可在多字节字符中间截断。Rust 的
`String` 必须是合法 UTF-8，因此 wash 用 `src/byteval.rs` 做**可逆**编码：ASCII
原样，高位字节 `0x80..=0xFF` 映射为哨兵 `U+E100` + PUA `U+E000..=U+E07F`；
源码里的字面 `U+E100` 双写为 `U+E100 U+E100`。`decode_raw` 见 `ESC ESC` 还原
一个字面哨兵，见 `ESC PUA` 还原一个高位字节，落单 PUA 原样输出。

**纪律**（改动相关路径前必读）：

| 类别 | 规则 | 站点 |
| ---- | ---- | ---- |
| 源码文本 ingress | `escape_literal`（双写字面哨兵） | `-c` 参数、继承环境变量 |
| 文件文本 ingress | `encode_raw`（`read_file` 已编码，调用方**不得**再 `escape_literal`） | 脚本文件、`source`、`run_script_in_shell` |
| 原始字节 ingress | `encode_raw`（逐字节编码） | `read -n`/`-N`、`printf -v`、命令替换 `read_all`、`mapfile`、shebang 解释器路径 |
| 按字节解释 | 先 `decode_raw` | `${#v}` / substring / pattern substitution / 审计输出 |
| 普通 egress | `decode_raw` | `echo` / `printf` / argv / env / path / heredoc |
| 诊断 egress | 按内建分流 | `cd` 与 command-not-found 用 `$'...'`（`diag_text`）；`source`/`type` 按 `%s` 裸字节写（`safe_eprint_bytes!`）；exec 类诊断（`bad interpreter` / `Is a directory` / `cannot execute binary` / errno）用 `exec_fail_bytes!` 写 `decode_raw(display)` |

禁止 `String::from_utf8_unchecked`（UB）与 `from_utf8_lossy` / `to_string_lossy`
处理可能含非法 UTF-8 的字节（永久失真）。`read_fields` 必须吃**原始** `input_bytes`，
传已编码串的 `.as_bytes()` 会二次编码。逐字节栅栏见 `tests/bash_diff_builtins.sh`
GROUP A 的 byteval / ANSI-C / from_utf8_lossy 用例（含字面哨兵五条 ingress、
stderr 诊断不泄漏编码串、`$'\u...'` 的 locale 回退、裸 0xff 在脚本/printf/shebang/命令名的保真）。

### `from_utf8_lossy` 站点清零（非交互 `-c` 可达路径）

oracle（MSYS2 bash 5.3, `LC_ALL=C`）实测原始字节 `0xff` 在下列站点必须原样保真，
修复前一律失真为 `U+FFFD`（`ef bf bd`）或 `U+00FF`（`c3 bf`）：

| 站点 | 修复前 | 修复后 |
| ---- | ---- | ---- |
| `posix::read_file`（脚本/`source` 源码） | `from_utf8_lossy` | `encode_raw`，调用方不再 `escape_literal` |
| `executor::shebang_interpreter` | `from_utf8_lossy` | `encode_raw`；execve 前 `decode_raw`，诊断裸字节写 |
| `builtins::printf` 非法/缺失格式字符 | `from_utf8_lossy` / `as char` | `safe_eprint_bytes!` 写 `bytes` 原始切片 |
| `executor::exec_classified` errno/目录/二进制诊断 | `format!` + `to_string_lossy` 的 `display` | `exec_fail_bytes!` 写 `decode_raw(display)` |

**残留边界（不在 `-c` 对拍门内，如实登记）**：`posix::strftime_local` 仍用
`from_utf8_lossy`。它只喂交互 REPL 的 PS1 `\d/\t/\T/\A/\@`，格式串是硬编码 ASCII，
本平台 locale 输出只有 ASCII 或合法 UTF-8，用户非法字节无法经此进入；改动显示路径
（同时被 `visible_width` 消费）风险大于收益，故保留并在此登记。

### ANSI-C quoting `$'...'` 与字符分类 locale

bash 对 `$'\uHHHH'`/`$'\UHHHHHHHH'` 的解释取决于 `MB_CUR_MAX`（字符分类 locale），
不是消息 locale。实测（MSYS2 bash 5.3）：单字节 locale（`C`/`POSIX`）下码点 `<=0xFF`
输出单字节、`>0xFF` 回退字面 `\uXXXX`（`>0xFFFF` 则 `\UXXXXXXXX`）；多字节
（`*.UTF-8`）下按 Unicode 字符输出。`\xNN`/`\NNN` 恒为裸字节（不受 locale 影响）；
任何转义产生 NUL 会截断其后全部内容（bash 用 C 字符串保存）。实现位于
`lexer::read_ansi_c` + `i18n::is_utf8_ctype`（优先级 `LC_ALL`>`LC_CTYPE`>`LANG`，
空值视为未设置，默认单字节）；lexer 新生成的 `U+E100` 必须双写以免与紧邻 PUA 碰撞。

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
