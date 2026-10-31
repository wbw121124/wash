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
```

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
