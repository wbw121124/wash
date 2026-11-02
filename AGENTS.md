# AGENTS.md — wash

工作指引，供 AI 代理（与人工）在本仓库继续开发使用。

## 项目目标

`wash` 是一个用 Rust 对 bash 的重新实现（RIIR），交互体验借鉴 fish
（语法高亮、自动建议、历史搜索、智能 Tab 补全、友好报错、作业控制可视化），
但**命令与语法层面要求 100% 与 bash 兼容**。

- 用户已明确：从“常见子集”升级到“**完全 bash 兼容**”。
- 验证方法（oracle）：同一条命令分别跑 MSYS2 bash 与 wash，逐字节比对
  stdout + 退出码。

## 硬性约束（必须严格遵守）

1. **工具链只能用 MSYS2，路径 `D:\msys2`，且 `MSYSTEM=MSYS`**
   （msys 子系统，二进制链接 `msys-2.0.dll`，POSIX 运行时：fork/exec/信号/
   作业控制/PTY 都可用）。**严禁 ucrt64 / mingw64**。
2. Rust 来自 MSYS2 的 `msys/rust`（pacman 安装，约 1.99.1）。
3. **只使用 POSIX 兼容写法，禁止 Win32 控制台 API。** 目标三元组近似
   `x86_64-pc-cygwin` / `x86_64-pc-windows-gnu`，因此 `nix` 等 unix-only
   crate 不可用。通过 `src/posix_shim.c` 的 C FFI 把 fork/execve/pipe/dup2/
   open/waitpid 等路由到真实 msys libc，保证 `O_*` 等常量取 Cygwin 值。
4. `WSL`（`wsl.exe`）仅用于最终人工 POSIX 校验（沙箱网络受限，CI 跑不了）。
   用完后执行 `wsl --shutdown` 释放资源（WSL 启动慢、占用大，不要长期挂着）。
5. **内建命令必须 100% 对齐 GNU bash 手册，严禁“够用就行”的 minimal 实现。**
   凡已实现的内建（`echo` / `printf` / `cd` / `exit` …）都必须配有「逐字节对齐真实
   bash」的测试（见 `tests/bash_diff_*.sh`），否则视为未完成。
6. 必须使用 git，按阶段提交。

### 正确设置 MSYS 环境的方式

```bash
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd /d/project/WorkBuddy/wash && cargo build'
```

把 `MSYSTEM=MSYS` 放在调用的 bash 之前，使 `/etc/profile` 把 `/usr/bin`
排在 `/mingw64/bin` 前面，`cc` 才会解析到 msys gcc。

## 构建与测试

```bash
# 构建（debug）
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd /d/project/WorkBuddy/wash && cargo build'

# Phase 0 oracle（外部命令/管道/重定向，9 项）
./tests/oracle.sh

# Phase 1 oracle（展开系统，20 项）
./tests/oracle_phase1.sh
```

oracle 脚本用 `bash -c`（即 MSYS2 bash 5.3）作为参照，与 wash 的
`./target/debug/wash -c` 输出比对。`$$`（PID）测试已从 oracle 移除，因为
bash 与 wash 是不同进程，PID 天然不同，不可比。

## 架构

```
main.rs        模块声明；处理 -c / -h / --version / 交互
lexer.rs       词法分析：把命令行切成由 WordPart 组成的词（保留引号上下文）
parser.rs      Token 流 -> AST（CommandList -> Pipeline -> SimpleCommand）
               含 name=value / name+=value 赋值识别（as_assignment）
expand.rs      展开系统：brace -> tilde -> 参数/命令/算术 -> 分词 -> glob
executor.rs    展开词 -> 通过 fork/execve 运行；管道、重定向、
               $? 传播、命令替换捕获（capture_command）、内建命令入口
posix.rs       Rust 侧 extern "C" + 安全封装，调用 posix_shim
posix_shim.c   C 垫片，路由 fork/execve/pipe/dup2/open/waitpid/... 到 msys libc
shell.rs       Shell 状态（env/cwd/positional/last_status/self_pid）+ REPL
               入口；`run_interactive` 委托给 `repl::run`，`run_str` 不变
repl.rs         fish 风格交互式行编辑器：termios 原始模式 + ANSI 转义，
               语法高亮、历史 ghost 建议、Tab 补全、多行编辑、历史持久化
error.rs       WashError { message, exit_code } / Result / syntax()
build.rs       编译 posix_shim.c 为 washposix 静态库；从 rustc --print sysroot
               推导 msys gcc，避免误用 PATH 里的 MinGW-w64 gcc
```

### 关键设计点

- `WordPart` 携带 `quoted` 标志（来自双引号内则抑制分词与 glob），保住
  引号上下文以便后续正确展开。
- 赋值只识别命令行首部的 `name=value`；值先以不可变借用展开，再统一 mutate
  `sh.env`（避免借用冲突）。
- `execute()` 在每条 pipeline 之后更新 `sh.last_status`，保证 `$?` 跨 `;`
  反映上一条命令的退出码。
- glob 对 msys 盘符路径（`/d/...`）做转换：`/d/...` -> `D:\...` 交给
  `std::fs::read_dir`，但输出保持 msys 形式 `/d/...`。

## 阶段状态

| 阶段 | 内容 | 状态 |
|------|------|------|
| 0 | 引导：exec / 管道 / 重定向 / FFI 垫片 / msys 工具链 | 已提交（v0.0.1），oracle 9/9 |
| 1 | 展开系统：变量、花括号、命令替换、算术、波浪号、brace、range、glob、分词、参数运算 `:- := :+ :? # ## % %% / // ^ ^^ , ,,`（含 echo/printf 内建） | 已完成，oracle 20/20（已提交） |
| 2 | 作业控制：`&`、`jobs`、`fg`、`bg`、`wait`、Ctrl-C/Ctrl-Z 信号处理 | 待开始 |
| 3 | 控制流 `if/for/while/until/case/select`、函数、更多内建 | 待开始 |
| 4 | 高级内建/数组/`[[ ]]`/`(( ))`/`declare`/`mapfile`/`trap`/`eval` 等 | 待开始 |
| 5 | 长尾 100% 兼容：边缘语义、locale、shopt 等 | 待开始 |
| 6 | 交互式 REPL（fish 风格）：语法高亮、历史命令自动建议（ghost text）、Tab 补全、多行编辑、友好报错 | 已完成（自研 termios+ANSI 行编辑器，见 `repl.rs`） |

### Phase 1 残余问题（已解决）

`split unquoted` / `split quoted`（`printf '[%s]' $X`）原为 2 项失败，现已通过
在 `executor.rs` 实现 `echo` / `printf` 内建命令修复：

- 二者在 **fork 出的子进程内**运行（重定向先应用），因此 `echo x > file`、
  `printf ... > file` 能正确写文件，不会退化到父进程导致重定向失效。
- `printf` 正确处理格式串循环复用、反斜杠转义（`\n \t \\ \" \' \a \b \f \r \v \0`、
  八进制 `\nnn`、十六进制 `\xHH`）、以及参数少于转换说明符时使用空串/`0`。
- 同时修正了 `lexer.rs`：`flush_text` 原始终把字面量标记 `quoted=false`，导致
  单/双引号内的字面量被错误地 glob；现按引号上下文设置 `quoted` 标志。
- 修正了 `expand.rs` 的 `glob`：含通配元字符但无匹配时返回字面量（bash 默认，
  nullglob 关闭），而非空结果。

oracle 由 18/20 提升至 **20/20**，Phase 0 维持 **9/9**。这也是 Phase 4
“内建命令” 的前置工作，已提前完成。

### 内建命令：100% bash 兼容（已落地）

用户明确要求：凡实现的内建必须 **100% 对齐 GNU bash 手册**，不可以“够用就行”。

`echo` / `printf` 已升级为完整 bash 兼容实现：

- `echo`:`-n` / `-e` / `-E` 仅识别为前导选项（`--` 在 echo 中是字面量）；`-e` 下的
  `\a \b \c \e \f \n \r \t \v \0NNN`（八进制要求前导 0、`\xHH`、`\uHHHH`、`\UHHHHHHHH`；
  未知 `\X` 保留反斜杠）；默认（无 `-e`）不解释转义、原样输出反斜杠。
- `printf`：`[-v var]` + 格式串转义；`%s %d %i %o %u %x %X %e %E %f %g %G
  %a %A %c %b %q %%`；字段宽度/精度（含 `*`）；标志 `- + 空格 # 0`；整数 base-0
  解析（`010`→8、`0x10`→16，溢出截断到目标宽度）；浮点经 C FFI 用 `long double`
  格式化（与 MSYS2 bash 同一 libc，逐字节一致）；`%b` 解释参数转义、`%q` 可重入引用、
  `-v` 在父进程写入变量；格式串循环复用；参数不足时数值→0、字符串→空。
- **架构约束**：不带 `-v` 的 `echo` / `printf` 在 fork 后的子进程、重定向生效之后
  执行（`echo x > file` 正确落盘）；只有 `cd` / `exit` / `printf -v` 在父进程执行。
- 验证：`tests/bash_diff_echo_printf.sh` 对 MSYS2 bash 与 wash 逐字节比对
  stdout + 退出码，**100/100 全通过**；回归 oracle 维持 **9/9 + 20/20**。

## 内建兼容性矩阵

**这份矩阵是实测结论，不是设计意图。** 每一行的状态都由
`tests/bash_diff_builtins.sh`（MSYS2 bash 5.3 ↔ wash 逐字节比对 stdout +
stderr + 退出码）跑出来，或由针对该内建的定向探针实测得到。**没实测过的一律
不写 `full`。**

状态定义：

| 状态 | 含义 |
|------|------|
| `full` | 与 bash 5.3 逐字节一致（stdout + stderr + 退出码），实测通过 |
| `partial` | 已实现，但与 bash 存在可观测差异（备注里写明差在哪） |
| `deferred` | 尚未实现；备注里写明为什么延后 + 计划在哪个 Phase 落地 |

### 状态型 / 流程型内建

| 内建 | 状态 | 备注（实测依据） |
|------|------|------------------|
| `:` | deferred | 未实现，报 `wash: :: command not found`（rc=127）。计划在 Phase 3 批次内补齐（`true` / `false` 已在更早的批次完成并判定为 `full`，本项是仅剩的遗漏） |
| `true` / `false` | full | 退出码与 stdout 均一致（GROUP C `true` / `false` 通过） |
| `echo` | full | `-n` / `-e` / `-E` / `--` 字面量 / 全部转义序列逐字节一致（`tests/bash_diff_echo_printf.sh` 100/100） |
| `printf` | full | 全部转换说明符逐字节一致（`%s %d %i %o %u %x %X %e %E %f %g %G %a %A %c %b %q %%`、字段宽度/精度/`*`、`-v`），**25 条定向探针全通过**。两个历史缺陷已由 `f59ea30` 修复并复测：①`printf %%. a`（`%%` 在格式串中间 + 多余参数）不再挂起，输出 `%.` 与 bash 一致；②`printf %x ff` 非法数字现在会在 stderr 报 `printf: ff: invalid number`（stdout 仍为 `0`，rc=1），与 bash 一致 |
| `cd` | full | 绝对/相对/逻辑(`/L`)/物理(`/P`)/`CDPATH`/`-`/`--`/空串/不存在/失败后 `PWD` 不变，全部一致。含 bash 的 `cd: too many arguments`（rc=2，`PWD` 不变） |
| `exit` | full | 无参/`42`/`300`/`-1`/被 source 的脚本内 `exit` 一致；非法参数 `exit abc` 时 bash 报 `exit: abc: numeric argument required`、置 `$?=2`、**继续执行后续命令**，wash 已照此对齐 |
| `export` | partial | `-p` / 无参 / `NAME=val` / `-n` / 赋值 / 非法标识符报错，格式与 bash 一致（GROUP A `export-p`/`export-noarg`/`export-no-name` 逐字节通过）。**差异**：①`export -f`（导出函数）wash 报 rc=2，bash 是 rc=1；②wash 不维护 bash 的 `BASH_*` / `BASHOPTS` / `BASH_VERSINFO` 等内部变量（这是 shell 内部变量集合差异，不是 `export` 本身的缺陷）；③`$_` 的导出属性处理与 bash 不同（bash 在命令执行后取消 `$_` 的导出属性，wash 保留） |
| `unset` | full | `-v` / 裸变量 / 多变量 / 不存在变量 / `-f` / 无参，rc 与报错一致 |
| `set` | partial | 选项处理（`-o` / `+o` / `-e` / `-f` / **`-n`（noexec）** / `--` / 位置参数 / 非法选项报错）与 bash **逐字节一致**（`set -o`、`set +o` 全表比对通过；`set -n` 的 4 条 GROUP A 用例通过）。**差异**：`set` 无参时 bash 打印**整个变量表**，含 `BASH` / `BASHOPTS` / `BASH_ARGC` / `BASH_ARGV` / `BASH_CMDS` / `BASH_LINENO` / `BASH_SOURCE` / `BASH_VERSINFO` / `BASH_VERSION` / `DIRSTACK` / `EUID` / `GROUPS` / `HOSTTYPE` / `MACHTYPE` / `OSTYPE` / `PIPESTATUS` / `PPID` / `RANDOM` / `SECONDS` / `SHELLOPTS` / `UID` 等内部变量，wash 只有自己的变量表，**变量集合必然不同**，无法逐字节全等（已隔离到 GROUP K `set-noargs`） |
| `alias` | full | 无参列出 / `-p` / 定义 / 未定义别名报错 / 非法名报错 / 别名展开，一致 |
| `unalias` | full | `-a` / 删单个 / 不存在 / **无参打印用法行**（`unalias: usage: unalias [-a] name [name ...]`，rc=2），一致。注意：`unalias` 无参**不是**列出全部别名（那是 `alias` 无参的行为） |
| `source` / `.` | full | 基本 / `.` 形式 / 带参数 / 不存在 / 无参 / 退出码透传 / `$0` 语义（被 source 的文件里 `$0` 仍是 shell 自己的调用路径，bash 默认模式与 `--posix` 模式均如此）/ 查找顺序（无斜杠时**先查 `PATH`** 再回落当前目录），一致。差异仅在 `$0` 的**字面值**（两个二进制路径不同，见 GROUP K `source-zero-raw`） |
| `eval` | full | 副作用 / 执行命令 / 空串 / 无参 / `eval 'exit 3'` 退出整 shell / 多词，一致 |
| `exec` | full | `-a name` / 替换当前进程 / 命令不存在 / 无参，一致 |
| `test` / `[` | full | 数值比较（`-eq -ne -gt -lt -ge -le`）、字符串比较（`=` `!=`）、`-z -n -f -d -e -r -w -x -s -L -h -b -c -p -S`、`!`、`-a`、`-o`、`-nt -ot -ef`、`[` `]` 形式、参数个数错误（`test: too many arguments`）、缺右括号（``` [: missing `]' ```）、`-ef` 语法错误信息、八进制数值（`010 -eq 10`），全部逐字节一致（GROUP C 28 条通过 + 18 条定向探针） |
| `type` | full | 无参（`echo is a shell builtin`）、`-t`（`builtin`/`file`/无输出+rc=1）、`-a`（列出全部同名路径）、`-f`（`ls is /usr/bin/ls`）、`-P`（只给路径），全部逐字节一致 |
| `command` | full | `-v`（内建名 / 不存在时 rc=1）、`-V`（`echo is a shell builtin`）、`-p`（清 PATH 后执行）、`-a`（非法选项 rc=2 + `command: -a: invalid option`）、无参直接执行命令，全部逐字节一致 |
| `builtin` | full | 执行内建（`builtin echo hi`）、非内建（`builtin: nosuchbuiltin: not a shell builtin`，rc=1）、无参列出内建清单，全部逐字节一致 |
| `hash` | deferred | 未实现（rc=127）。Phase 3 批次内补齐 |
| `read` | deferred | 未实现（rc=127）。Phase 3 批次内补齐 |
| `pwd` | full | `pwd` / `-L` / `-P` 三种形式逐字节一致 |
| `shift` | full | 基本位移 / 连续位移 / 计数按**十进制**解析（`shift 2`）/ 越界返回 rc=1，一致 |

### 声明型 / 杂项内建

| 内建 | 状态 | 备注（实测依据） |
|------|------|------------------|
| `return` | deferred | 未实现（rc=127）。需配合函数（Phase 3）落地 |
| `break` | deferred | 未实现（rc=127）。需配合循环（Phase 3）落地 |
| `continue` | deferred | 未实现（rc=127）。需配合循环（Phase 3）落地 |
| `local` | deferred | 未实现（rc=127）。需配合函数（Phase 3）落地 |
| `declare` / `typeset` / `readonly` | deferred | 未实现（rc=127）。Phase 4 高级内建批次内落地 |
| `let` | deferred | 未实现（rc=127）。Phase 4 落地（算术展开已在 Phase 1 完成，`let` 只是语法糖） |
| `ulimit` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `umask` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `times` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `kill` | partial | 已实现，且**发信号路径已覆盖**：`kill -0 $$`（存在性检查）rc=0、`kill -TERM $$`（真发信号，shell 被 SIGTERM 终止）rc=143、`kill -TERM <子进程>` 后 `wait` 报 143，与 bash 一致；`kill -l 9`（`KILL`）、`kill -l KILL`（`9`）也一致。**三处差异**：①`kill -l` 全量清单格式不同（bash ` 1) SIGHUP\t 2) SIGINT\t...` 带序号 + `SIG` 前缀 vs wash `HUP INT QUIT ...`）；②`kill` 无参时 bash 打印自己的用法行 `kill: usage: kill [-s sigspec \| -n signum \| -sigspec] pid \| jobspec ... or kill -l [sigspec]`（rc=2），wash 转发给了 Cygwin 外部 `/usr/bin/kill`，打印的是外部工具的帮助文本（rc=1）；③`kill -NOSUCHSIG 12345` bash 报 `kill: NOSUCHSIG: invalid signal specification`，wash 报 `kill: unknown signal: NOSUCHSIG` + `Try \`kill --help' ...`；`kill -TERM 999999` bash 报 `kill: (999999) - No such process`（带括号），wash 报 `kill: 999999: No such process`（缺括号） |
| `trap` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `dirs` / `pushd` / `popd` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `help` | deferred | 未实现（rc=127）。Phase 4 落地 |
| `enable` | deferred | 未实现（rc=127）。Phase 4 落地 |

### 本阶段显式延后的内建（不属于「忘记做」，是有意排期）

| 内建 | 延后原因 | 计划 Phase |
|------|----------|-----------|
| `jobs` / `fg` / `bg` / `wait` / `disown` | 需要完整的作业控制：后台进程组、`SIGTSTP`/`SIGCONT` 处理、`$!` 与 `jobs` 表。wash 当前的执行模型是「每条命令 fork/exec」，尚无作业表与进程组管理 | **Phase 2**（作业控制） |
| `mapfile` / `readarray` / `coproc` / 数组 `-a` / `-A` | 需要真正的数组类型（`declare -a` 语义、数组下标展开、`${arr[@]}`）与协进程双向管道。wash 目前只有标量变量，且没有协进程管道机制 | **Phase 4**（高级内建 / 数组） |
| `compgen` / `complete` / `bind` | 补全与键绑定属于交互式 REPL 层。wash 的 Phase 6 REPL 已有自研 Tab 补全（命令名查 `PATH` + 文件路径补全），但那是 REPL 内部逻辑，尚未暴露成 bash 风格的 `complete` / `bind` 内建 | **Phase 4** 之后（与 REPL 补全对接） |
| `fc` / `history` | 命令历史由 REPL 持久化到 `~/.wash_history`，但 `history` 内建需要把历史列表以 bash 格式暴露，`fc` 还需要历史范围展开与编辑。两者都与 REPL 的历史存储格式强耦合 | **Phase 4** 之后（与 REPL 历史对接） |

### 已知缺陷（实测复现，待修）

**当前无已复现待修的缺陷。** 上一轮记录的 `printf` 两处缺陷（`%%` 死循环、
非法数字静默）已由 `f59ea30` 修复并复测通过，现已从本表移除；对应的回归
用例保留在 `tests/bash_diff_builtins.sh` 的 GROUP D（`printf-pct-mid-extra` /
`printf-pct-mid-extra2` / `printf-hex-invalid` / `printf-dec-invalid`），
一旦缺陷复发会立刻变红。

> **这两个缺陷当初为什么没被发现**：`tests/bash_diff_echo_printf.sh` 的 100/100
> 用例里，唯一涉及 `%%` 的用例是 `printf "100%%\n"`——**格式串以 `%%` 结尾且
> 没有多余参数**，恰好落在死循环触发条件之外。补上「`%%` 位于格式串中间 +
> 带多余参数」的用例即可暴露。

### 已修复缺陷的根因存档（供回归参考）

`printf` 死循环的根因（`src/builtins.rs` 的 `printf_format()`）：`has_conv`
预扫描判断格式串是否含转换说明符时，把 `%%` 之后的字符也当成了转换说明符
起点 → `has_conv=true`；而真正消费参数的 `parse_printf_spec()` 对 `%%` 不消费
参数 → `ai` 永不前进 → `loop` 里 `ai < args.len()` 恒成立 → 无限循环。
**旁证**：`printf %%%% a`（`%%` 在末尾）不挂起，`printf x%%y`（无多余参数）
也不挂起，`printf %s %%. a`（`%s` 已消费掉参数）同样不挂起——与该根因完全吻合。

## 参考源码

- bash 5.3：`https://ftp.gnu.org/gnu/bash/bash-5.3.tar.gz`
- fish 4.9.3：`https://github.com/fish-shell/fish-shell/releases/download/4.9.3/fish-4.9.3.tar.xz`

实现 bash 语义时以 bash 5.3 源码（`expand.c`、`lib/sh`、`builtins/`）为准。

## 提交约定

- 按阶段提交，commit message 用 `feat: Phase N ...` 风格（英文，简洁）。
- 已提交：`40caffc feat: Phase 0 bootstrap ...`。
- Phase 1 已提交（含展开系统与 echo/printf 内建，oracle 9/9 + 20/20）。

## 阶段 6：交互式 REPL（fish 风格）实现说明

交互式 REPL 完全自研，**不使用任何 Win32 控制台 API、不使用 rustyline 等带
Win32 后端的行编辑器 crate**。底层只用 `posix_shim.c` 提供的 msys termios：
`wash_enable_raw` / `wash_disable_raw`（关闭 `ECHO|ICANON|ISIG`，`VMIN=1/VTIME=0`，
`IXON/OPOST` 关闭，`CS8`）做原始终端；stdin 逐字节读取，方向键等用 ANSI CSI
序列（`\x1b[...`）解析。仅新增 `unicode-width` 一个依赖，用于多字节字符（CJK/emoji）
的显示宽度计算以正确放置光标。

`run_interactive`（shell.rs）仅做委托，所有交互逻辑在 `repl.rs`，`-c` / `run_str`
路径完全未动，oracle 9/9 + 20/20 不受影响。

四项 fish 特性：
1. **语法高亮**：`classify()` 对当前行做流式分词着色（命令=绿、flag/`-x`=橙、
   引号字符串=黄、`$var`/命令替换=青、运算符/重定向=红）。分类维度对齐
   `lexer.rs` 的 `WordPart` 模型；对**部分输入**（未闭合引号、`&` 等 lexer 会
   拒绝的内容）也健壮不崩溃。
2. **历史自动建议（ghost text）**：从最新历史往前找第一条以当前输入为前缀的行，
   在光标后用灰色渲染剩余部分；Right-Arrow / End 接受补全。
3. **Tab 补全**：命令名查 `PATH` 可执行文件 + 已知内建；当前 token 含 `/` 或处于
   参数位置时做文件路径补全。唯一候选直接补全（目录补 `/`，文件补空格），
   多个候选补全公共前缀并列出候选。
4. **多行编辑**：逻辑行存在未闭合引号、未闭合 `(){}[]`、或末尾为 operator
   （`|` `&` `&&` `||` `|&`）时，显示续行提示符 `> ` 继续输入；整段缓冲作为一条
   命令提交。当前物理行内可正常编辑（左右/Home/End/退格/删除）。

其他键位：可打印字符、Backspace、Left/Right、Home/End、Enter（提交）、
Ctrl-C（取消当前行）、Ctrl-D（行空则 EOF 退出，否则前向删除）、Ctrl-L（清屏）、
Up/Down（历史上下，与 ghost 建议分离）。历史持久化到 `~/.wash_history`（退出时
追加，启动时加载；路径集中在 `history_file()` 便于修改）。

非 TTY 输入（管道，如冒烟测试）自动回退为逐行读取，保证非交互用法正常。
