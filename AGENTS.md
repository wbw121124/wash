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
| 6 | 交互式 REPL（fish 风格）：语法高亮、历史命令自动建议（ghost text）、Tab 补全、多行编辑、友好报错 | 待开始 |

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

## 参考源码

- bash 5.3：`https://ftp.gnu.org/gnu/bash/bash-5.3.tar.gz`
- fish 4.9.3：`https://github.com/fish-shell/fish-shell/releases/download/4.9.3/fish-4.9.3.tar.xz`

实现 bash 语义时以 bash 5.3 源码（`expand.c`、`lib/sh`、`builtins/`）为准。

## 提交约定

- 按阶段提交，commit message 用 `feat: Phase N ...` 风格（英文，简洁）。
- 已提交：`40caffc feat: Phase 0 bootstrap ...`。
- Phase 1 已提交（含展开系统与 echo/printf 内建，oracle 9/9 + 20/20）。
