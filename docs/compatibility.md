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
[`docs/intentional-divergences.md`](intentional-divergences.md)，当前恰好 3 条：

| ID | 例外 | 裁决 |
| ---- | ---- | ---- |
| D-001 | `help -m` 的 IMPLEMENTATION 段用 wash 身份而非 GNU bash 版权段 | 项目自始既有行为；`tests/help_footer.py` 严格替换 + 可逆性校验 |
| D-002 | `help job_spec` / `help 'job_spec [&]'`（单 pattern 精确）命中 `%` 页，rc=0 | 用户裁决：要、全局生效（含非交互 `-c`）、允许破坏脚本兼容 |
| D-003 | `enable -f` 显式路径受 `BASH_LOADABLES_PATH` 白名单约束（oracle 构建未启用动态加载，无可对齐语义） | 2026-11-21 主理人裁决：安全设计优先（白名单 + dlopen 后 dev/ino 复核封 TOCTOU） |

D-002 刻意不进通用别名表：只有"选项解析后恰好 1 个 pattern 且字符串精确命中"才生效，
glob / 多 pattern / 空 pattern / 其余清单显示值 / 别名路径全部仍与 oracle 严格逐字节对拍。
除这 2 条外，任何判定都没有放宽。清单↔页表由 `help_pages.rs::HELP_LIST_MAP` 单一真源绑定，
测试用常量硬断言条目数与用例数。

## 显式延后的内建（有意排期，不是"忘记做"）

| 内建 | 延后原因 | 计划 Phase |
| ---- | ---- | ---- |
| `jobs` / `fg` / `bg` / `wait` | 作业表、jobspec（`kill %N` / `jobs -x`）、stopped 状态、后台独立进程组与 `fg` 前台交接已实现；非交互 `-c` 部分逐字节对齐 | Phase 2 已完成 |
| `disown` | 需要作业表脱离语义；bash `-c` 下与交互模式行为不同，尚未实现 | Phase 2 之后 |
| `mapfile` / `readarray` / `coproc` | 需要协进程双向管道与文件→数组读取 | Phase 4 |
| 数组 `-a` / `-A`（下标展开） | 赋值 `ARR=(a b)`、下标赋值 `a[i]=v`、`declare -A` 关联数组（FNV 桶序遍历）已落地 | Phase 4 已完成 |
| `compgen` / `complete` / `bind` | 补全与键绑定属交互层；Phase 6 已有自研 Tab 补全但未暴露成 bash 风格内建 | Phase 4 之后 |
| `fc` / `history` | 历史由 REPL 持久化，但需以 bash 格式暴露 / 支持范围展开与编辑 | Phase 4 之后 |

## 已知未实现特性（功能缺失，非覆盖空洞）

oracle 实测：`(( ))` 算术命令、**C 风格 `for ((...))`**、`declare -A`、`getopts`、
`trap -l` 均已实现并与 bash 5.3 逐字节对齐（含 OPTIND/OPTARG/静默模式、
完整 64 信号表）。数组下标赋值 `a[i]=v` 与稀疏下标计数也已对齐。

以下三处历史边界已在本轮修复（均有 GROUP A 栅栏）：
- **关联数组遍历顺序**：按 bash 5.3 `assoc.h` 的 `ASSOC_HASH_BUCKETS=1024`
  复刻（FNV-1 桶序 + 头插 + 2048/2049 键 rehash 仿真），`foo/bar/baz/qux`、
  单字符键、2047/2048/2049/5000 键大表全部逐字节一致；`declare -p` 同根因修复。
- **算术自增写回时序**：`$(( x++ ))` 的赋值经命令级 overlay 立即对同一命令行
  后续 `$x` 可见（`echo $(( x++ )) $x` → `1 2`、`(( ++x ))` → `2 2`），跨命令仍
  经 pending_assign 落回 sh.env；前序 `$x` 不回退。
- **C 风格 for 诊断**：段数错（`syntax error: arithmetic expression required` +
  原始 `((...))` 回显）、缺右括号（词法 EOF）、缺 `done` / 不匹配 closer
  （yacc near-token）、省略分号 `for ((...)) do ... done` 合法形态，全部对齐。

此前如实登记的四处窄边界已逐项修复，均有 GROUP A 栅栏（oracle 逐字节对拍）：
- **brace group 作循环 body**：`for NAME in ...; { LIST; }` 与
  `for ((...)); { LIST; }` 现在与 `do ... done` 等价执行；无 `in` 的
  `for NAME; { ...; }` 按 `"$@"` 逐参数迭代。未闭合的 `{` 与空 `{ }`
  分别复刻 bash 的 EOF 消息和 near-`}` 两行诊断。
- **`declare -f` / `declare -F`**：按名称排序输出函数定义，`-f NAME` /
  `-F NAME` 支持多名字与缺失名字 rc=1；函数体采用 bash 的固定版式（尾部
  空格、4 空格缩进、`do` / `fi` / `;;` 独立行、`elif` 嵌套为 `else / if /
  fi;`、`for i in "$@";`）。`declare -p` 路径不受影响。
- **循环层级传播**：普通 `for` / `while` 的 `continue N`（N>1）现在与
  C 风格 for 一致地递减并向上传播，不再被内层吞掉（此前
  `continue 2` 在 `while :` 内甚至会死循环）。`continue 0` / 非数字 /
  参数过多三类错误分别复刻 out-of-range(rc 1)、numeric argument
  required(rc 2，终止整条命令列表)、too many arguments(rc 1)。
- `x=1; echo ${!y}` 间接展开经复核本已对齐（既有栅栏），无需修复。
- 此前已修的 `done extra` / `fi zz` near-token 诊断保持不变；closer 后跟
  重定向 / `&` / `&&` / `||` 仍为合法语法。
`[[ ]]` 已实现并与 bash 5.3 逐字节对齐（49 条栅栏见 `tests/bash_diff_builtins.sh`
GROUP A）：单目/双目/文件/数值/字符串/模式/ERE 运算、`BASH_REMATCH` 捕获组、
`!`/`&&`/`||`/分组、`[[ ]]` 内不做词分割，以及畸形表达式的两行诊断（含 yacc
`syntax error near` 行与未闭合 `(` 的词法 EOF）。畸形 token 按 bash 的原始源码
文本渲染：引号、反斜杠转义与 `$'...'` 的 `$` 前缀照原样出现在 conditional 消息中，
yacc `near` 行只取最后一个空白之后的部分；被转义的 `=` 是字面词而非运算符（rc=2）。
`select` 与 `shopt -s nullglob` 已实现并与 bash 5.3 逐字节对齐（44 条 GROUP A
栅栏，另有 18 条 QA 残留栅栏覆盖注释词法/空 body 诊断/readonly 循环变量，
见 `tests/bash_diff_builtins.sh`）：
- **`select NAME [in WORDS]; do BODY; done`**：stderr 打印编号菜单与 `PS3`
  （默认 `#? `，设为空则不显示）；stdin 按 bash 默认 `read` 语义读取（反斜杠
  引用下一字符、`\`+换行续行）到 `REPLY`；非法选择执行 body 且 NAME 为空；
  空行清空 REPLY 并在下一周期重显菜单；EOF 清空 REPLY、向 stdout 写一个裸
  `\n`、不执行 body、rc=1；`break`/`continue N` 与 for 一致传播；NAME 允许
  任意词，非法标识符执行期报 `` `X': not a valid identifier``；`do; done` /
  `do done` 等空 body 与不匹配 closer 按 bash yacc 诊断（near `;` / `done` /
  `fi`）。`{ LIST; }` 亦可作 body。
- **`shopt -s/-u nullglob`**：未匹配的无引号通配展开为零字段，经
  `capture_command` / fork 子进程透传到命令替换；加引号的模式不受影响。
  `shopt` 列宽固定 20，具名查询 rc 仅当全部 on 为 0，支持 bundled 短选项
  （`-qs` / `-oq`）、`-s`/`-u` 无参数列状态子集、`-su` 冲突报错、`-o` 走
  `set -o` 名字校验与 `set ±o` 输出。
- **`#` 注释词法**（QA 复核补齐）：词首 `#`（不在引号内、非词中、非
  `cond`、非重定向 target 位）吃掉到行尾；词中 `a#b` 与引号内 `"a # b"`
  是字面量；`echo hi >#f` 按 bash 报重定向缺 target（near `newline`）。
- **readonly 循环变量**：`for`/`select` 的目标变量为 readonly 时，bash
  在赋值处报 `X: readonly variable`、跳过 body、循环状态 1（select 保留
  菜单/PS3/REPLY 副作用），wash 逐字节一致。
- **`shopt` 60 项全表**：本轮补齐到 MSYS2 bash 5.3 的全部 60 项（默认
  13 ON / 47 OFF），列宽 `%-20s\t%s`、表项顺序（含
  `completion_strip_exe` 在 `complete_fullquote` 之前）、`shopt -p`、
  具名查询 rc、`-s`/`-u`（含 bundled / 冲突 / `-o`）、`BASHOPTS`
  冒号串均与 oracle 逐字节一致；`login_shell` / `restricted_shell`
  是只读伪选项（设置 rc=0 但状态不变）。**语义边界（如实登记）**：
  60 项中仅 3 项有真实语义（`expand_aliases` / `nullglob` /
  `sourcepath`），其余 57 项仅做状态存取与显示，不改变 shell 行为
  （经用户 2026-11-23 裁决：全表显示/状态对齐优先，语义未实现不伪装）。
- **`select` ≥10 项多列菜单**：本轮补齐 `ls -C` 式列数重算
  （`maxcols = COLUMNS / cellw`、`nrows = ceil(n/maxcols)`、
  `ncols = ceil(n/nrows)`、退化单列），编号按列主序、首列宽度按
  `digits(nrows)`、列间贪心 tab（tabstop 8）+ 空格补齐；`COLUMNS`
  为只读 shell 变量，缺省/0/非法按 80。10～30 项 × 多列宽度的
  130 项矩阵与 oracle 逐字节一致。此前 ≤9 项单列行为不变。

其余已对齐：if/for/while/until/case、函数、eval、mapfile、local、`>|`、
`exec 3>&-`、`declare -A`、`getopts`、`trap -l`、`(( ))`、数组下标赋值、
`${!pre*}`、`$(( ))`、heredoc/herestring、未闭合 `$(`、重定向逐行部分执行等。


## backlog 残留（oracle 实测确认，未伪装对齐）

### 本轮已修复（2026-11-23 / 11-24）

以下项已修复并逐字节对齐 oracle（除另有说明者，均已纳入门禁）：

- **`cmdsub` 内部语法错误上抛**：`src/lexer.rs::validate_cmdsub_body` 在顶层
  lex 期递归验证 `$(...)` body，错误经 `take_cmdsub_syntax_error` 上抛到
  `run_str`：rc=127、两行诊断、内部 token 后缀 ` while looking for matching
  \`)'`、EOF 形态归一为 near `)'`；错误行之前的完整命令行仍执行。
- **`case ;&` / `;;&` fallthrough**：lexer 最长匹配专用分号 token、
  parser `CaseTerminator` 记录每段终止符、executor 状态机执行
  （`;&` 跳过 pattern 匹配直接跑下一段 body、`;;&` 重新匹配、rc 取最后 body）。
  顶层非法 `;&` / `;;&` 报 near-token。
- **`jobs -x`（jobspec→pid 替换执行）**：`run_job_x_command` 把可解析的 `%N`
  替换为对应 job pid 后 exec；**  无法解析的 `%xxx` 保留原词照常执行、rc=0**
  （oracle 实测 `jobs -x echo %99` 输出 `%99` rc=0——这与 `jobs %99` 报
  `no such job` 是两套不同语义，切勿混用）。
- **`cmdsub` 输出内嵌 NUL**（11-24，commit `11985a9`）：`capture_command`
  收集出口丢弃 NUL 并告警，每个 substitution 只告警一次；不经过 cmdsub 的
  裸字节路径（`printf 'a\0b' | od`）仍保留 NUL，与 byteval 保真契约不冲突。
- **`${arr[@]:-x}` / `${@:+x}` 整表判定**（11-24，commit `96ea521`）：
  `src/expand.rs::array_word_op` 对数组 `@`/`*` 的 `-`/`:-`/`+`/`:+` 按整体
  判定——无冒号形式看元素数、带冒号形式看 IFS 连接后的值是否为空；生效时
  替代词作为单个字段产出（加引号 1 词、未加引号按 IFS 分词）。故 `a=("")`
  套 `:-` 默认值而 `a=("" "")` 不套。索引数组与关联数组两处分支均已覆盖。
- **`2>&x` 重定向目标整词**（11-24，commit `329f647`）：lexer `read_dup_target`
  改为读取完整 shell 词（引号 / `$var` / `${}` / `$()` / 算术 / glob / `{x}`），
  不再只吞数字前缀——`2>&1x` 现在整词歧义报错而非拆成 fd 1 + 参数；
  非数字目标回显展开后的单字段（`x=abc; 2>&$x` → `abc: ambiguous redirect`）；
  `2>&` 到 EOF 报两行 `syntax error near unexpected token`；`N>&""` / 变量空值
  分别按 Bad file descriptor / ambiguous redirect 对齐。18 条 GROUP A 栅栏。
- **`PIPESTATUS` 特殊数组**（11-25，commit `10daf6b`）：`execute_pipeline`
  在每次命令/管道完成后写 `sh.arrays["PIPESTATUS"]`——单命令单元素 `[rc]`、
  多段管道各段原始码（不受 pipefail 调整）；`&` 不写数组、`wait` 写
  `[wait rc]`；fork 时子 Shell `arrays.clone()` 天然满足 cmdsub/子 shell/管道段
  "继承快照、不回传父"语义；不在启动时预置（首命令前 `declare -p` 报
  not found，与 oracle 一致）。`${PIPESTATUS[@]}`/`[*]`/`[i]`/`${#…[@]}`/
  切片/`declare -p`/`unset` 全部由现有数组机制自动生效。24 条 GROUP A 栅栏。

### 仍未修复（如实登记）

- `2>&x` 的残余窄边界（在 23 条 GROUP A 栅栏之外）：fd==1 的空目标回显
  保留原始引号种类——`1>&''` bash 报 `''`、wash 报 `""`；`1>&"$x"`（x 空）
  bash 报 `"$x"`、wash 报 `$x`。`raw_word_text` 的 AST 不保留单双引号形态，
  改动面超出本轮收益，如实登记。源词含引号/变量/算术但展开为数字
  （`2>&"9"` / `x=9; 2>&$x` 等）在后续修复中已对齐（回显 fd 号）。
- `${PIPESTATUS[@]:-none}`：已随 `PIPESTATUS` 实现对齐（见上）。
- `$HOME` 缺省回退（MSYS2 平台边界）：判据必须从 MSYS2 bash 内部调
  `env -u HOME`（从 Git Bash 调会造出"bash HOME 为空"的跨运行时假象）。
  原生口径实测：unset HOME 时 bash 回退 passwd 项 `/home/wbw`、wash 为空；
  空 HOME 值时 bash 保持空、wash 被 MSYS2 runtime 转成 `/c/Users/wbw`。
  两者都属进程环境/runtime 行为，不通过 getpwuid 回填伪装对齐。
- 算术变量值递归链深度：wash 50 层护栏（防栈溢出）以内与 bash 对齐，
  51 层以上 bash 仍可求值而 wash 报 recursion exceeded。MSYS2 bash 5.3 的
  算术**嵌套**在 1022 层成功、1023 层确定性报错（不 SIGSEGV），wash 的
  200 层嵌套护栏同样低于该阈值；两者都是"防栈溢出的保守上限"，不是
  bash 兼容阈值，勿据注释声称对齐。
- 后台作业 `command` 文本：Phase 1（raw span @ `&` + normalizer）已覆盖
  简单 pipeline 的引号/赋值前缀/重定向空格/字面变量；残余窄边界——复合命令
  括号规范化（bash `( sleep 5 )`）、if/for/while 多行 pretty-print、
  ANSI-C `$'...'` 剥 `$` 并解码、`$( )` 内层空白压缩、`foo && bar &`
  bash 后台化整个 AND-OR 而 wash 只后台化最后一段 pipeline（执行语义
  独立缺口）——均如实登记。持久作业号 jobno 与状态码已对齐。
- **Phase 2 作业控制的 PTY-only 边界**：非交互 `-c` 下 bash 不跟踪 stopped
  状态，wash 已按 oracle 对齐；stopped 状态、后台独立进程组、`fg` 的
  `tcsetpgrp` 前台交接仅在 PTY 下可观测，以 `tests/e2e/` PTY 套件验证，
  不做 `-c` 逐字节断言。
- **`PIPESTATUS` 数组本身未实现**（全源码零命中）：bash 在每条管道后把各段
  退出码写入 `PIPESTATUS`（`true | false` → `0 1`），wash 该变量完全不存在，
  `${PIPESTATUS[@]}` 恒为空。属独立缺口，非 `:-` 默认值一项可覆盖。
- **默认值/替代词内的二次展开**：`${v:-$x}` 在 bash 下把 `$x` 展开后再作
  默认值（x=VAL → `VAL`），wash 的标量 `apply_scalar_op` 与数组
  `array_word_op` 都直接回显参数原文（→ `$x`）；命令替换 / 算术替换
  同理不展开。为保持标量与数组路径一致，本轮未改。
- **复合赋值中的稀疏下标**：`a=([0]=x [3]=y)` bash 产出 `x y`，wash 把
  `[0]=x` / `[3]=y` 当字面元素（且下标记为 0/1）；`declare -p` 也随之
  不同。属 `a[i]=v` 单独赋值路径之外的复合赋值解析缺口，本轮未改。
- **数组模式替换 `${arr[@]/pat/repl}`**：bash 对数组逐元素替换
  （`false|true|false; echo "${PIPESTATUS[@]/1/9}"` → `9 0 9`），wash
  完全不替换（普通数组 `arr=(1 0)` 同样不替换）；属通用数组模式替换
  缺口，非 PIPESTATUS 特有，本轮未改。
- **管道取反 `! pipeline`**：bash `!` 只取反 `$?`、PIPESTATUS 保留各段
  原始码；wash Parser::Pipeline 无 negate 字段，`!` 被当外部命令
  （`!: command not found`）。属解析层独立缺口，本轮未改。
- **`declare -p` 未定义变量**：bash 报 `declare: X: not found` rc=1，
  wash 输出 `declare -- X` rc=0（`declare -p TOTALLY_UNDEFINED` 同样
  复现，通用行为非 PIPESTATUS 特有），本轮未改。

详细根因存档见 `AGENTS.md`「内建兼容性矩阵 → 已修复缺陷的根因存档」。
