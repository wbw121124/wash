# 架构与平台

## 执行管线

```
词法 lexer.rs   ->  解析 parser.rs  ->  展开 expand.rs  ->  执行 executor.rs
                                         （fork/execve，管道，重定向，命令替换）
        Shell 状态 shell.rs + 交互 repl.rs  |  POSIX 垫片 posix.rs + posix_shim.c
                                            |  二进制插件 plugin.rs + include/wash_plugin.h
```

- `lexer.rs` 把命令行切成由 `WordPart` 组成的词，保留引号上下文（决定后续是否分词/glob）。
  花括号展开在 lexer 出口做 quote removal；`expand.rs` 不再二次去引号。
- `parser.rs` 把 Token 流变成 AST（`CommandList → Pipeline → SimpleCommand`），
  含 `name=value` 赋值识别与复合命令（`if`/`for`/`while`/`case`/group/subshell）。
- `expand.rs` 实现 bash 展开顺序：brace → tilde → 参数/命令/算术 → 分词 → glob。
- `executor.rs` 通过 `posix_shim.c` 的 C FFI 调用真实 msys libc（`fork`/`execve`/
  `waitpid`/`open`/`dup2`/`pipe`/heredoc fd 等），保证 `O_*` 等常量取 Cygwin 值（与 Linux 不同）。
- `shell.rs` 持有 Shell 状态（env/cwd/positional/last_status/变量/数组/历史），
  `run_interactive` 委托给 `repl::run`，`-c` / `run_str` 路径独立。
- `build.rs` 从 `rustc --print sysroot` 推导 msys gcc 并**硬闸**绑定，禁止回退到 PATH 上
  任何其他 gcc；CI 使用 `msys/rust` 包（host `x86_64-pc-cygwin`、sysroot `/usr`），
  绝不复制/符号链接 gcc（重定向会丢失 exec prefix 导致找不到 `cc1`）。

## 平台与工具链（重要）

wash 面向 **Linux/POSIX**。开发期在 Windows 上通过 **MSYS2 的 msys 子系统**
（`MSYSTEM=MSYS`，链接 `msys-2.0.dll`）获得 POSIX 运行时——fork/exec/信号/作业控制/PTY 都可用。

- **只用 MSYS2 的 msys 子系统**：严禁 ucrt64 / mingw64 / `mingw-w64-*` 包（会丢失 POSIX 语义）。
- Rust 来自 `msys/rust`（`pacman -S rust`，约 1.99.1），不用 rustup 的 gnu target。
- 只写 POSIX 兼容代码，禁止 Win32 控制台 API。`nix` 等 unix-only crate 不可用，
  走 `src/posix_shim.c` + `src/posix.rs` 的 C FFI 抽象层。
- MSYS2 bash 5.3.20 是主 oracle（唯一权威）；WSL2 bash 5.3.0 仅做第二 oracle 交叉校验，
  用完 `wsl --shutdown`。

正确设置 MSYS 环境的方式（`MSYSTEM=MSYS` 放在调用 bash 之前，让 `/etc/profile`
把 `/usr/bin` 排在 `/mingw64/bin` 前）：

```bash
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd <repo> && cargo build'
```

> 该路径中的 `/d/msys2` 是 MSYS2 安装位置约定；在自己的机器上替换为实际的
> MSYS2 根目录即可（Linux 上直接 `cargo build`）。

## 交互式 REPL（fish 风格）

交互层**完全自研**，只基于 msys 的 termios 原始模式 + ANSI 转义码，不依赖任何
Win32 控制台 API、也不使用带 Win32 后端的行编辑器 crate（如 rustyline）。仅新增
`unicode-width` 用于多字节字符的显示宽度计算。

- **语法高亮**：命令（绿）/ 选项 `-x`（橙）/ 引号字符串（黄）/ 变量 `$x`（青）/
  运算符与重定向（红），随输入实时着色；对部分输入（未闭合引号、`&`）也健壮。
- **历史自动建议（ghost text）**：若历史中有以当前内容为前缀的命令，剩余部分以灰色
  显示在光标后；Right-Arrow / End 接受补全。
- **Tab 补全**：命令名查 `PATH` 与内建；参数位置补全文件路径；唯一候选直接补全，
  多候选补全公共前缀并列出候选。
- **多行编辑**：未闭合引号 / `(){}[]` 或末尾为 `|` `&` `&&` `||` `|&` 时进入续行
  提示符 `> `，整段作为一条命令提交。
- 历史持久化到 `~/.wash_history`（启动时加载、退出时追加）。
- 非 TTY 输入（管道）自动回退为逐行读取，保证非交互用法正常。

Fish 行为只影响交互层；`-c` / `run_str` 路径完全不受影响。
