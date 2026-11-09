# wash

一个用 Rust 重新实现的 bash：交互体验借鉴 fish（语法高亮、自动建议、历史搜索、
智能 Tab 补全、友好报错、作业控制可视化），但**在命令与语法层面 100% 兼容 bash**。

> 名称 `wash` = **w**bw121124's **A**nother **SH**ell。

## 为什么

bash 的交互体验长期落后于 fish/zsh，而 fish 又有自己的非 POSIX 语法。
`wash` 想同时拿到两样东西：bash 的脚本兼容性与 fish 的现代交互手感。

## 当前状态

项目处于早期阶段，按阶段推进：

| 阶段 | 内容                                                                                                                          | 状态                                                   |
| ---- | ----------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| 0    | 引导：外部命令执行、管道、重定向、POSIX FFI 垫片、msys 工具链                                                                 | 已完成（oracle 9/9）                                   |
| 1    | 展开系统：变量、花括号展开、命令替换、算术、波浪号、brace/range、glob、分词、参数运算（含 echo/printf 内建）                  | 已完成（oracle 20/20）                                 |
| 2    | 作业控制：`&` / `jobs` / `fg` / `bg` / `wait` / 信号                                                                          | 待开始                                                 |
| 3    | 控制流与函数：`if` / `for` / `while` / `until` / `case` / `select`、函数                                                      | 待开始                                                 |
| 4    | 高级内建与特性：数组、`[[ ]]`、`(( ))`、`declare`、`mapfile`、`trap`、`eval` 等（内建 `printf`/`echo` 已在 Phase 1 提前实现） | 进行中/部分（数组赋值已落地；`declare -a`/索引/`mapfile` 待补）                                                 |
| 5    | 长尾 100% 兼容：边缘语义、locale、shopt 等（locale 维度测试见 `tests/bash_diff_i18n.sh`）                                                                                    | 进行中/部分（`help` 头部已本地化（zh_CN 下出中文、C 下出英文）；内建正文/用法摘要与内建集合待内建实现对齐后增强；`tests/bash_diff_i18n.sh` 的「轴 2 结构」用例 FAIL 源于内建集合差异，属预期（进行中），非本地化缺失）                                                 |
| 6    | 交互式 REPL（fish 风格）：语法高亮、历史命令自动建议、Tab 补全、多行编辑                                                      | 已完成（自研 termios+ANSI 行编辑器，见 `src/repl.rs`） |
| 7    | 二进制插件系统（wash C ABI）与 0.2.0 发布                                                                                          | 已完成（v0.2.0，见下方「二进制插件」）               |

验证方式（oracle 测试）：同一条命令分别跑 MSYS2 bash 与 wash，逐字节比对
stdout 与退出码。

> **内建命令同样要求 100% 对齐 bash 手册**（含 `echo`/`printf` 的完整转义与
> `%b/%q/%d` 等转换），不允许“够用就行”的 minimal 实现；每个内建都要有逐字节
> 对齐真实 bash 的测试。其中 `echo` / `printf` 已完成：由 `tests/bash_diff_echo_printf.sh`
> 对 MSYS2 bash 与 wash 逐字节比对 stdout + 退出码，**100/100 全通过**。交互式 REPL
> 则借鉴 fish 的手感（语法高亮、历史自动建议、Tab 补全、多行编辑），但语义仍归 bash。

## 平台与工具链（重要）

`wash` 运行在 **Linux**，但开发时使用 **Windows 下的 MSYS2 的 msys 子系统**（`MSYSTEM=MSYS`，
链接 `msys-2.0.dll`，提供 POSIX 运行时），因此完全走 POSIX 语义，不使用
Win32 控制台 API，也不使用 MinGW/ucrt 工具链（那会丢失 fork/exec/信号/PTY）。

- 开发时 MSYS2 安装位置：`D:\msys2`
- Rust：`msys/rust`（pacman，约 1.99.1）
- 严禁 ucrt64 / mingw64
- 严禁为 Windows/MSYS 编写专门的代码（除了把 MSYS2 的 POSIX 映射到 rust 中），必须**全部** Linux/POSIX

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
./tests/bash_diff_cli.sh           # CLI 参数形态
./tests/bash_diff_help.sh          # help：C / zh_CN 双 locale，77 主题完整页 / -s -d -m /
                                   # 无参清单（抬头 5 句 + 77 条摘要）/ glob / 错误路径
                                   # 77 条清单显示值原样当 NAME 对拍 + job_spec/% 孤例锁定；清单↔规范名由 HELP_LIST_MAP 单一真源绑定
                                   # `-m` 的 wash 身份段由 tests/help_footer.py 做严格替换对拍
./tools/gen_help_pages.py          # 从 MSYS2 oracle 重生成两份 help 页数据 + po 清单译文段
                                   # （--check 校验新鲜度，含双 locale 双栏排版逐字节重建）
./tests/plugin_contract.sh         # 二进制插件 ABI 契约（MSYS2 / WSL 各跑）
./tests/plugin_second_oracle.sh    # WSL bash 5.3.0 能力等价对拍
./tests/check_version.sh           # 版本串生成与一致性闸门（静态 + 运行时）
./tools/wash_version.sh            # 版本串唯一生成规则（Cargo.toml + git tag/HEAD）
```

## 发布 0.2.1

**所有用户可见的版本行都由 `tools/wash_version.sh` 生成**，唯一真源是
`Cargo.toml` 的 `version` 加上构建时的 git 元数据：

- HEAD 恰好有 tag：该 tag（去掉 `v`）**必须**等于 `Cargo.toml` 版本，输出 `<version>`；
- HEAD 没有 tag：输出 `<version>-<HEAD hash 前 7 位>`；
- 最近可达 tag 比 `Cargo.toml` 更新（即「打了 tag 却忘了改 Cargo.toml」）、tag 与清单不一致、
  或 git 元数据缺失：构建与闸门**直接报错**，绝不静默回退到旧版本。

`build.rs` 调该脚本并把结果注入 `WASH_VERSION`；`--version`、`$BASH_VERSION`、
`help` 抬头、`help -m` 的 IMPLEMENTATION 段、PS1 `\v` 全部使用 `env!("WASH_VERSION")`，
源码与测试中不含任何硬编码版本号。两个 `.po` 头由 `tools/wash_version.sh --sync-po`
生成，`--check-po` 校验新鲜度。`tests/check_version.sh` 做静态 + 运行时严格校验；
`tests/bash_diff_help.sh` 把 oracle 的版本行替换为生成值后做**完整逐字节对拍**，
不再用 `tail -n +2` 跳过任何一行。

## 发布 0.2.0

0.2.0 引入二进制插件系统并完成一次完整发布；当时的版本号机制见
`CHANGELOG.md`，现行机制见上节。升级与兼容边界详见 `CHANGELOG.md`；
两平台动态加载能力的技术验证见 `docs/spike-dlopen.md`。

**兼容性说明**：MSYS2 主 oracle 的 bash 5.3.20 编译时关闭了动态加载，而 wash
全平台启用插件，因此 `enable -f/-d` 属受控扩展，矩阵中标 `wash-extension`，
由插件契约套件与 WSL 第二 oracle 验证，不参与 MSYS2 逐字节对拍。

## 二进制插件（wash C ABI）

插件是导出 `wash_plugin_init` 的共享库（MSYS2 与 Linux 均可用 `.so` 后缀）。
宿主通过 `WashPluginApi` 函数指针表向插件提供 stdout/stderr 输出、shell 变量
读写与内建状态查询；插件可以一次注册多个内建，每个内建可带 `help` / `short_doc`。
C 头文件是 `include/wash_plugin.h`，可编译示例见 `tests/fixtures/`。

```bash
# 编译示例插件（MSYS2 用 /usr/bin/gcc.exe，Linux 用 gcc）
gcc -shared -fPIC -Iinclude -o /path/to/plugins/plugin_hello.so \
    tests/fixtures/plugin_hello.c

# 从 BASH_LOADABLES_PATH 白名单目录加载，调用，再热卸载
BASH_LOADABLES_PATH=/path/to/plugins ./target/debug/wash -c \
  'enable -f plugin_hello.so plhello; plhello; enable -d plhello'
```

- 搜索路径：未设 `BASH_LOADABLES_PATH` 时用 bash 5.3 的默认 5 段路径并丢弃
  `.` / 空段 / 相对段；文件名不含 `/` 时按该路径搜索，含 `/` 时 realpath 后的
  父目录也必须命中白名单；符号链接一律拒绝。
- ABI 版本双向协商：版本或 `struct_size` 不符时立即拒绝并卸载，不调用任何插件函数。
- `enable -a/-p/-s` 的列表语义、`type` / `compgen -b` / `help` / Tab 补全中的
  动态内建联动，由 `tests/plugin_contract.sh` 做 golden 契约验证。
- **安全模型**：插件与宿主同权限、同崩溃域（进程内加载），插件自身崩溃会拖垮
  shell；只加载可信目录中的可信插件。本期不提供子进程隔离。

## 内建兼容性矩阵

**这份矩阵是实测结论，不是设计意图。** 每一行的状态都由
`tests/bash_diff_builtins.sh`（MSYS2 bash 5.3 ↔ wash 逐字节比对 stdout +
stderr + 退出码）跑出来，或由针对该内建的定向探针实测得到。**没实测过的一律
不写 `full`。**

状态定义：`full` = 与 bash 5.3 逐字节一致（实测通过）；`partial` = 已实现但
存在可观测差异（备注写明差在哪）；`deferred` = 尚未实现（备注写明为什么延后 +
计划在哪个 Phase 落地）。

| 内建                               | 状态     | 备注（实测依据）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| ---------------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `:`                                | full     | 无参与带参数（`: foo`）均与 bash 一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `true` / `false`                   | full     | 退出码与 stdout 均一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `echo`                             | full     | `-n` / `-e` / `-E` / `--` 字面量 / 全部转义序列逐字节一致（`tests/bash_diff_echo_printf.sh` 100/100）                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `printf`                           | full     | 全部转换说明符逐字节一致（`%s %d %i %o %u %x %X %e %E %f %g %G %a %A %c %b %q %%`、字段宽度/精度/`*`、`-v`），25 条定向探针全通过。两个历史缺陷（`%%` 死循环、非法数字静默）已修复并复测                                                                                                                                                                                                                                                                                                                                                                                        |
| `cd`                               | full     | 绝对/相对/逻辑(`/L`)/物理(`/P`)/`CDPATH`/`-`/`--`/空串/不存在/失败后 `PWD` 不变，全部一致（含 bash 的 `cd: too many arguments`，rc=2）                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `exit`                             | full     | 无参/`42`/`300`/`-1`/被 source 的脚本内 `exit` 一致；`exit abc` 时 bash 报 `exit: abc: numeric argument required`、置 `$?=2`、**继续执行后续命令**，wash 已对齐                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `export`                           | partial  | `-p` / 无参 / `NAME=val` / `-n` / 非法标识符报错，格式逐字节一致。**差异**：①`export -f`（导出函数）wash 报 `export: -f: not supported`（rc=2），bash 是 `export: f: not a function`（rc=1）；②wash 不维护 bash 的 `BASH_*` / `BASHOPTS` / `BASH_VERSINFO` 等内部变量（shell 内部变量集合差异）；③`$_` 的导出属性处理与 bash 不同                                                                                                                                                                                                                                               |
| `unset`                            | full     | `-v` / 裸变量 / 多变量 / 不存在变量 / `-f` / 无参，rc 与报错一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `set`                              | partial  | 选项处理（`-o` / `+o` / `-e` / `-f` / `-n`（noexec）/ `--` / 位置参数 / 非法选项报错）与 bash **逐字节一致**（`set -o`、`set +o` 全表比对通过）。**差异**：`set` 无参时 bash 打印**整个变量表**（含 `BASH` / `BASHOPTS` / `BASH_ARGC` / `BASH_ARGV` / `BASH_CMDS` / `BASH_LINENO` / `BASH_SOURCE` / `BASH_VERSINFO` / `BASH_VERSION` / `DIRSTACK` / `EUID` / `GROUPS` / `HOSTTYPE` / `MACHTYPE` / `OSTYPE` / `PIPESTATUS` / `PPID` / `RANDOM` / `SECONDS` / `SHELLOPTS` / `UID` 等内部变量），wash 只有自己的变量表，**变量集合必然不同**，无法逐字节全等                       |
| `alias`                            | full     | 无参列出 / `-p` / 定义 / 未定义别名报错 / 非法名报错 / 别名展开，一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `unalias`                          | full     | `-a` / 删单个 / 不存在 / **无参打印用法行**（`unalias: usage: unalias [-a] name [name ...]`，rc=2），一致。注意 `unalias` 无参**不是**列出全部别名（那是 `alias` 无参的行为）                                                                                                                                                                                                                                                                                                                                                                                                   |
| `source` / `.`                     | full     | 基本 / `.` 形式 / 带参数 / 不存在 / 无参 / 退出码透传 / `$0` 语义（被 source 的文件里 `$0` 仍是 shell 自己的调用路径，bash 默认与 `--posix` 模式均如此）/ 查找顺序（无斜杠时**先查 `PATH`** 再回落当前目录），一致。差异仅在 `$0` 的**字面值**（两个二进制路径不同）                                                                                                                                                                                                                                                                                                            |
| `eval`                             | full     | 副作用 / 执行命令 / 空串 / 无参 / `eval 'exit 3'` 退出整 shell / 多词，一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `exec`                             | full     | `-a name` / 替换当前进程 / 命令不存在 / 无参，一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `test` / `[`                       | full     | 数值比较、字符串比较、`-z -n -f -d -e -r -w -x -s -L -h -b -c -p -S`、`!`、`-a`、`-o`、`-nt -ot -ef`、`[` `]` 形式、参数个数错误、缺右括号、`-ef` 语法错误信息、八进制数值，全部逐字节一致                                                                                                                                                                                                                                                                                                                                                                                      |
| `type`                             | full     | 无参 / `-t` / `-a` / `-f` / `-P`，全部逐字节一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `command`                          | full     | `-v` / `-V` / `-p` / `-a`（非法选项 rc=2）/ 无参直接执行，全部逐字节一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `builtin`                          | full     | 执行内建 / 非内建报错（rc=1）/ 无参列出内建清单，逐字节一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `hash`                             | full     | 无参 / `-t <name>` / `-l` / `-r` / `-p <path> <name>` 五种用法逐字节一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `read`                             | full     | `-r`/`-n`/`-d`/`-p`/`REPLY`/EOF rc=1/多变量/`IFS` 分隔均逐字节一致（GROUP C `read-*` 全通过，含 `read-ifs-prefix`/`read-ifs-empty`）                                                                                                                                                                                                                                       |
| `pwd`                              | full     | `pwd` / `-L` / `-P` 三种形式逐字节一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `shift`                            | full     | 基本位移 / 连续位移 / 计数按**十进制**解析（`shift 2`）/ 越界 rc=1，一致                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `return`                           | partial  | 已实现；sourced 脚本/函数内 `return n` 正常（rc=n）。**差异**：函数/source 外调用 bash 报中文（rc=2），wash 报英文（rc=1）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `break` / `continue`               | partial  | 已实现；循环内正常跳出/继续。差异：循环外调用 bash 报中文（rc=0+stderr），wash 报英文（rc=1）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `local`                            | partial  | 已实现；函数内声明局部变量。差异：函数外调用 bash 报中文（rc=1），wash 报英文（rc=1，rc 已对齐）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| `declare` / `typeset` / `readonly` | partial  | 已实现：赋值/`declare -i`/`declare -p`/`readonly` 可用。差异：数组下标展开 `${arr[@]}` 待补；`declare -p` 变量集合与 bash 不同（身份差异）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `let`                              | partial  | 已实现；`let expr` 算术赋值与 bash 一致。基础算术已对齐；`-p` 等选项待补                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `ulimit`                           | deferred | 未实现（rc=127）。Phase 4 落地 |
| `umask` / `times`                  | full     | `umask`(`-S`/`-p`)/`times` 与 bash 逐字节一致（数值属运行时）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `kill`                             | partial  | **发信号路径已覆盖**：`kill -0 $$` rc=0、`kill -TERM $$` rc=143、`kill -TERM <子进程>` 后 `wait` 报 143，均与 bash 一致；`kill -l 9` / `kill -l KILL` 一致。**三处差异**：①`kill -l` 全量清单格式不同（bash ` 1) SIGHUP\t...` 带序号 + `SIG` 前缀 vs wash `HUP INT ...`）；②`kill` 无参时 bash 打印自己的用法行（rc=2），wash 转发给 Cygwin 外部 `/usr/bin/kill` 打印其帮助文本（rc=1）；③非法信号名 / 不存在进程的错误文案与 bash 不同（`kill: unknown signal: NOSUCHSIG` vs `kill: NOSUCHSIG: invalid signal specification`；`kill: 999999: No such process` 缺 bash 的括号） |
| `trap`                             | partial  | 已实现；`trap` 无参列出已设陷阱与 bash 一致。差异：信号说明符解析/`-p`/`-l`/`-P` 覆盖不完整                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `dirs` / `pushd` / `popd`          | partial  | 已实现；`dirs`/`pushd dir` 与 bash 逐字节一致。差异：`popd` 空栈 wash 报英文（rc=1，bash 中文 rc=1，rc 已对齐）                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `help`                             | partial  | 已实现并注册为内建（commit 57ec1c7，rc=0），头部 3 句已本地化（zh_CN 中文/C 英文）；内建正文/集合待对齐 bash，i18n「轴 2 结构」用例 FAIL 属预期（进行中）。详情见 AGENTS.md `help` 行 |
| `enable`                           | partial  | `-a` / `-n` / `-p` / `-s` 的静态语义、`-z` 用法错误、未知名报错按 oracle 实测对齐（GROUP C 逐字节对拍）；**差异**：①全量清单内建集合与 bash 不同（bash 有 `jobs`/`mapfile`/`bind`/`fc`/`history` 等 wash 未实现的内建）；②`-f`/`-d` 及插件加载是下述 `wash-extension` 能力，MSYS2 主 oracle 编译为「dynamic loading not available」（rc=2），无法逐字节对拍 |
| 二进制插件（wash C ABI）           | wash-extension | `enable -f/-d`、`BASH_LOADABLES_PATH` 白名单、ABI 协商、多内建注册与热卸载、变量读写；MSYS2 与 WSL 由 `tests/plugin_contract.sh` golden 契约验证，WSL bash 5.3.0 由 `tests/plugin_second_oracle.sh` 做能力等价对拍。**不与 bash 官方 loadable .so 二进制兼容**，不标 `full` |

### 本阶段显式延后的内建（有意排期，不是「忘记做」）

| 内建                                                  | 延后原因                                                                                                                                                   | 计划 Phase                           |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------ |
| `jobs` / `fg` / `bg` / `wait` / `disown`              | 需要完整的作业控制：后台进程组、`SIGTSTP`/`SIGCONT` 处理、`$!` 与 `jobs` 表。wash 当前的执行模型是「每条命令 fork/exec」，尚无作业表与进程组管理           | **Phase 2**（作业控制）              |
| `mapfile` / `readarray` / `coproc` | 需要协进程双向管道与文件→数组读取；wash 尚无协进程管道机制                                                                                    | **Phase 4**（高级内建 / 数组）       |
| 数组 `-a` / `-A`（索引 / 展开）     | 数组**赋值** `ARR=(a b)` 已通过 GROUP D `array-assign`（partial）；`declare -a`/`-A`、下标展开 `${arr[@]}` 待补                                      | **Phase 4**（高级内建 / 数组）       |
| `compgen` / `complete` / `bind`                       | 补全与键绑定属于交互式 REPL 层。Phase 6 REPL 已有自研 Tab 补全（命令名查 `PATH` + 文件路径补全），但尚未暴露成 bash 风格的 `complete` / `bind` 内建        | **Phase 4** 之后（与 REPL 补全对接） |
| `fc` / `history`                                      | 命令历史由 REPL 持久化到 `~/.wash_history`，但 `history` 内建需把历史列表以 bash 格式暴露，`fc` 还需历史范围展开与编辑——两者都与 REPL 的历史存储格式强耦合 | **Phase 4** 之后（与 REPL 历史对接） |

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
                                            |  二进制插件 plugin.rs + include/wash_plugin.h
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
