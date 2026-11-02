# wash

一个用 Rust 重新实现的 bash：交互体验借鉴 fish（语法高亮、自动建议、历史搜索、
智能 Tab 补全、友好报错、作业控制可视化），但**在命令与语法层面 100% 兼容 bash**。

> 名称 `wash` = **W**indows **A**nother **SH**ell（也读作 “wash your shell”）。

## 为什么

bash 的交互体验长期落后于 fish/zsh，而 fish 又有自己的非 POSIX 语法。
`wash` 想同时拿到两样东西：bash 的脚本兼容性与 fish 的现代交互手感。

## 当前状态

项目处于早期阶段，按阶段推进：

| 阶段 | 内容 | 状态 |
|------|------|------|
| 0 | 引导：外部命令执行、管道、重定向、POSIX FFI 垫片、msys 工具链 | 已完成（oracle 9/9） |
| 1 | 展开系统：变量、花括号展开、命令替换、算术、波浪号、brace/range、glob、分词、参数运算（含 echo/printf 内建） | 已完成（oracle 20/20） |
| 2 | 作业控制：`&` / `jobs` / `fg` / `bg` / `wait` / 信号 | 待开始 |
| 3 | 控制流与函数：`if` / `for` / `while` / `until` / `case` / `select`、函数 | 待开始 |
| 4 | 高级内建与特性：数组、`[[ ]]`、`(( ))`、`declare`、`mapfile`、`trap`、`eval` 等（内建 `printf`/`echo` 已在 Phase 1 提前实现） | 待开始 |
| 5 | 长尾 100% 兼容：边缘语义、locale、shopt 等 | 待开始 |
| 6 | 交互式 REPL（fish 风格）：语法高亮、历史命令自动建议、Tab 补全、多行编辑 | 已完成（自研 termios+ANSI 行编辑器，见 `src/repl.rs`） |

验证方式（oracle 测试）：同一条命令分别跑 MSYS2 bash 与 wash，逐字节比对
stdout 与退出码。

> **内建命令同样要求 100% 对齐 bash 手册**（含 `echo`/`printf` 的完整转义与
> `%b/%q/%d` 等转换），不允许“够用就行”的 minimal 实现；每个内建都要有逐字节
> 对齐真实 bash 的测试。其中 `echo` / `printf` 已完成：由 `tests/bash_diff_echo_printf.sh`
> 对 MSYS2 bash 与 wash 逐字节比对 stdout + 退出码，**100/100 全通过**。交互式 REPL
> 则借鉴 fish 的手感（语法高亮、历史自动建议、Tab 补全、多行编辑），但语义仍归 bash。

## 平台与工具链（重要）

`wash` 运行在 **Windows**，但使用 **MSYS2 的 msys 子系统**（`MSYSTEM=MSYS`，
链接 `msys-2.0.dll`，提供 POSIX 运行时），因此完全走 POSIX 语义，不使用
Win32 控制台 API，也不使用 MinGW/ucrt 工具链（那会丢失 fork/exec/信号/PTY）。

- MSYS2 安装位置：`D:\msys2`
- Rust：`msys/rust`（pacman，约 1.99.1）
- 严禁 ucrt64 / mingw64

### 构建

```bash
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd /d/project/WorkBuddy/wash && cargo build'
```

### 运行

```bash
./target/debug/wash -c "echo hello $USER"
./target/debug/wash            # 进入交互 REPL（fish 风格：语法高亮/ghost 建议/Tab 补全/多行）
```

### 交互式 REPL（Phase 6，fish 风格）

交互层**完全自研**，只基于 msys 的 termios 原始模式 + ANSI 转义码，不依赖任何
Win32 控制台 API、也不使用带 Win32 后端的行编辑器 crate（如 rustyline 的 windows
后端）。仅新增 `unicode-width` 用于多字节字符的光标定位。

- **语法高亮**：命令（绿）/ 选项 `-x`（橙）/ 引号字符串（黄）/ 变量 `$x`（青）/
  运算符与重定向（红），随输入实时着色。
- **历史自动建议（ghost text）**：输入时若历史中有以当前内容为前缀的命令，剩余部分
  以灰色显示在光标后；Right-Arrow / End 接受补全。
- **Tab 补全**：命令名查 `PATH` 与内建；参数位置补全文件路径；唯一候选直接补全，
  多候选补全公共前缀并列出候选。
- **多行编辑**：未闭合引号 / `(){}[]` 或末尾为 `|` `&` `&&` `||` `|&` 时进入续行
  提示符 `> `，整段作为一条命令提交。
- 历史持久化到 `~/.wash_history`（启动时加载、退出时追加）。
- 仅 `run_interactive` 被改写，`-c` / `run_str` 路径保持不变（oracle 9/9 + 20/20）。

### 测试

```bash
./tests/oracle.sh          # Phase 0：外部命令 / 管道 / 重定向
./tests/oracle_phase1.sh   # Phase 1：展开系统
./tests/bash_diff_echo_printf.sh   # echo / printf 逐字节对齐 bash
./tests/bash_diff_builtins.sh      # 全部内建逐字节对齐 bash（见下方矩阵）
```

## 内建兼容性矩阵

**这份矩阵是实测结论，不是设计意图。** 每一行的状态都由
`tests/bash_diff_builtins.sh`（MSYS2 bash 5.3 ↔ wash 逐字节比对 stdout +
stderr + 退出码）跑出来，或由针对该内建的定向探针实测得到。**没实测过的一律
不写 `full`。**

状态定义：`full` = 与 bash 5.3 逐字节一致（实测通过）；`partial` = 已实现但
存在可观测差异（备注写明差在哪）；`deferred` = 尚未实现（备注写明为什么延后 +
计划在哪个 Phase 落地）。

| 内建 | 状态 | 备注（实测依据） |
|------|------|------------------|
| `:` | full | 无参与带参数（`: foo`）均与 bash 一致 |
| `true` / `false` | full | 退出码与 stdout 均一致 |
| `echo` | full | `-n` / `-e` / `-E` / `--` 字面量 / 全部转义序列逐字节一致（`tests/bash_diff_echo_printf.sh` 100/100） |
| `printf` | full | 全部转换说明符逐字节一致（`%s %d %i %o %u %x %X %e %E %f %g %G %a %A %c %b %q %%`、字段宽度/精度/`*`、`-v`），25 条定向探针全通过。两个历史缺陷（`%%` 死循环、非法数字静默）已修复并复测 |
| `cd` | full | 绝对/相对/逻辑(`/L`)/物理(`/P`)/`CDPATH`/`-`/`--`/空串/不存在/失败后 `PWD` 不变，全部一致（含 bash 的 `cd: too many arguments`，rc=2） |
| `exit` | full | 无参/`42`/`300`/`-1`/被 source 的脚本内 `exit` 一致；`exit abc` 时 bash 报 `exit: abc: numeric argument required`、置 `$?=2`、**继续执行后续命令**，wash 已对齐 |
| `export` | partial | `-p` / 无参 / `NAME=val` / `-n` / 非法标识符报错，格式逐字节一致。**差异**：①`export -f`（导出函数）wash 报 `export: -f: not supported`（rc=2），bash 是 `export: f: not a function`（rc=1）；②wash 不维护 bash 的 `BASH_*` / `BASHOPTS` / `BASH_VERSINFO` 等内部变量（shell 内部变量集合差异）；③`$_` 的导出属性处理与 bash 不同 |
| `unset` | full | `-v` / 裸变量 / 多变量 / 不存在变量 / `-f` / 无参，rc 与报错一致 |
| `set` | partial | 选项处理（`-o` / `+o` / `-e` / `-f` / `-n`（noexec）/ `--` / 位置参数 / 非法选项报错）与 bash **逐字节一致**（`set -o`、`set +o` 全表比对通过）。**差异**：`set` 无参时 bash 打印**整个变量表**（含 `BASH` / `BASHOPTS` / `BASH_ARGC` / `BASH_ARGV` / `BASH_CMDS` / `BASH_LINENO` / `BASH_SOURCE` / `BASH_VERSINFO` / `BASH_VERSION` / `DIRSTACK` / `EUID` / `GROUPS` / `HOSTTYPE` / `MACHTYPE` / `OSTYPE` / `PIPESTATUS` / `PPID` / `RANDOM` / `SECONDS` / `SHELLOPTS` / `UID` 等内部变量），wash 只有自己的变量表，**变量集合必然不同**，无法逐字节全等 |
| `alias` | full | 无参列出 / `-p` / 定义 / 未定义别名报错 / 非法名报错 / 别名展开，一致 |
| `unalias` | full | `-a` / 删单个 / 不存在 / **无参打印用法行**（`unalias: usage: unalias [-a] name [name ...]`，rc=2），一致。注意 `unalias` 无参**不是**列出全部别名（那是 `alias` 无参的行为） |
| `source` / `.` | full | 基本 / `.` 形式 / 带参数 / 不存在 / 无参 / 退出码透传 / `$0` 语义（被 source 的文件里 `$0` 仍是 shell 自己的调用路径，bash 默认与 `--posix` 模式均如此）/ 查找顺序（无斜杠时**先查 `PATH`** 再回落当前目录），一致。差异仅在 `$0` 的**字面值**（两个二进制路径不同） |
| `eval` | full | 副作用 / 执行命令 / 空串 / 无参 / `eval 'exit 3'` 退出整 shell / 多词，一致 |
| `exec` | full | `-a name` / 替换当前进程 / 命令不存在 / 无参，一致 |
| `test` / `[` | full | 数值比较、字符串比较、`-z -n -f -d -e -r -w -x -s -L -h -b -c -p -S`、`!`、`-a`、`-o`、`-nt -ot -ef`、`[` `]` 形式、参数个数错误、缺右括号、`-ef` 语法错误信息、八进制数值，全部逐字节一致 |
| `type` | full | 无参 / `-t` / `-a` / `-f` / `-P`，全部逐字节一致 |
| `command` | full | `-v` / `-V` / `-p` / `-a`（非法选项 rc=2）/ 无参直接执行，全部逐字节一致 |
| `builtin` | full | 执行内建 / 非内建报错（rc=1）/ 无参列出内建清单，逐字节一致 |
| `hash` | full | 无参 / `-t <name>` / `-l` / `-r` / `-p <path> <name>` 五种用法逐字节一致 |
| `read` | partial | 基础读取、`-r`、`-n`、`-d`、`-p`、`REPLY`、EOF rc=1、多变量、`IFS` 作为普通语句时按 `IFS` 分隔，均逐字节一致。**一处差异**：`IFS` 作为**前缀赋值**时（`IFS=: read a b`）bash 会把它应用到本次分词（输入无 `:` 则整行是一个字段 `a=[a b c] b=[]`），wash 仍按全局 `IFS` 分词（`a=[a] b=[b c]`）——wash 的 `read` 看不到前缀赋值的临时 `IFS` |
| `pwd` | full | `pwd` / `-L` / `-P` 三种形式逐字节一致 |
| `shift` | full | 基本位移 / 连续位移 / 计数按**十进制**解析（`shift 2`）/ 越界 rc=1，一致 |
| `return` | deferred | 未实现（rc=127）。需配合函数（Phase 3）落地 |
| `break` / `continue` | deferred | 未实现（rc=127）。需配合循环（Phase 3）落地 |
| `local` | deferred | 未实现（rc=127）。需配合函数（Phase 3）落地 |
| `declare` / `typeset` / `readonly` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `let` | deferred | 未实现（rc=127）。Phase 4 落地（算术展开已在 Phase 1 完成，`let` 只是语法糖） |
| `ulimit` / `umask` / `times` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `kill` | partial | **发信号路径已覆盖**：`kill -0 $$` rc=0、`kill -TERM $$` rc=143、`kill -TERM <子进程>` 后 `wait` 报 143，均与 bash 一致；`kill -l 9` / `kill -l KILL` 一致。**三处差异**：①`kill -l` 全量清单格式不同（bash ` 1) SIGHUP\t...` 带序号 + `SIG` 前缀 vs wash `HUP INT ...`）；②`kill` 无参时 bash 打印自己的用法行（rc=2），wash 转发给 Cygwin 外部 `/usr/bin/kill` 打印其帮助文本（rc=1）；③非法信号名 / 不存在进程的错误文案与 bash 不同（`kill: unknown signal: NOSUCHSIG` vs `kill: NOSUCHSIG: invalid signal specification`；`kill: 999999: No such process` 缺 bash 的括号） |
| `trap` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `dirs` / `pushd` / `popd` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `help` / `enable` | deferred | 未实现（rc=127）。Phase 4 落地 |

### 本阶段显式延后的内建（有意排期，不是「忘记做」）

| 内建 | 延后原因 | 计划 Phase |
|------|----------|-----------|
| `jobs` / `fg` / `bg` / `wait` / `disown` | 需要完整的作业控制：后台进程组、`SIGTSTP`/`SIGCONT` 处理、`$!` 与 `jobs` 表。wash 当前的执行模型是「每条命令 fork/exec」，尚无作业表与进程组管理 | **Phase 2**（作业控制） |
| `mapfile` / `readarray` / `coproc` / 数组 `-a` / `-A` | 需要真正的数组类型（`declare -a` 语义、数组下标展开、`${arr[@]}`）与协进程双向管道。wash 目前只有标量变量，也没有协进程管道机制 | **Phase 4**（高级内建 / 数组） |
| `compgen` / `complete` / `bind` | 补全与键绑定属于交互式 REPL 层。Phase 6 REPL 已有自研 Tab 补全（命令名查 `PATH` + 文件路径补全），但尚未暴露成 bash 风格的 `complete` / `bind` 内建 | **Phase 4** 之后（与 REPL 补全对接） |
| `fc` / `history` | 命令历史由 REPL 持久化到 `~/.wash_history`，但 `history` 内建需把历史列表以 bash 格式暴露，`fc` 还需历史范围展开与编辑——两者都与 REPL 的历史存储格式强耦合 | **Phase 4** 之后（与 REPL 历史对接） |

### 已知缺陷（实测复现，待修）

**当前无已复现待修的缺陷。** 上一轮记录的 `printf` 两处缺陷（`%%` 死循环、
非法数字静默）已修复并复测通过，已从本表移除；对应回归用例保留在
`tests/bash_diff_builtins.sh` 的 GROUP D，缺陷复发会立刻变红。

详细的根因分析见 `AGENTS.md` 的「内建兼容性矩阵 → 已修复缺陷的根因存档」一节。

## 架构概览

```
词法 lexer.rs   ->  解析 parser.rs  ->  展开 expand.rs  ->  执行 executor.rs
                                         （fork/execve，管道，重定向，命令替换）
        Shell 状态 shell.rs + 交互 repl.rs  |  POSIX 垫片 posix.rs + posix_shim.c
```

- `lexer.rs` 把命令行切成由 `WordPart` 组成的词，保留引号上下文（决定后续
  是否分词 / glob）。
- `expand.rs` 实现 bash 展开顺序：brace → tilde → 参数/命令/算术 → 分词 → glob。
- `executor.rs` 通过 `posix_shim.c` 的 C FFI 调用真实 msys libc，保证 `O_*`
  等常量取 Cygwin 值（与 Linux 不同）。
- `build.rs` 从 `rustc --print sysroot` 推导 msys gcc，避免误用 MinGW-w64 gcc。

## 参考

- bash 5.3 源码：`https://ftp.gnu.org/gnu/bash/bash-5.3.tar.gz`
- fish 4.9.3 源码：`https://github.com/fish-shell/fish-shell/releases/download/4.9.3/fish-4.9.3.tar.xz`

实现 bash 语义时以 bash 5.3 源码（`expand.c`、`lib/sh`、`builtins/`）为准。

## 许可证

待定（参考 bash GPL / fish GPLv2）。
