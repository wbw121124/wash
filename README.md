# wash

一个用 Rust 重写的 shell：**命令与语法逐字节兼容 GNU bash 5.3**，交互体验借鉴 fish。

> 名字 `wash` = **w**bw121124's **A**nother **SH**ell。

## 它解决什么

bash 的交互体验落后于 fish/zsh，而 fish 有自己的非 POSIX 语法。wash 想同时拿到两样东西：
bash 的脚本兼容性，加上 fish 的现代交互手感。非交互 `-c` 的输出以真实 bash 为 oracle
**逐字节比对**，兼容性闸门不因交互改进而放宽。

## 特性

- **bash 5.3 兼容**：外部命令、管道、重定向、展开、控制流、函数、常见内建，逐字节对拍验证。
- **fish 风格交互**（完全自研，不用 Win32 API）：语法高亮、历史 ghost 建议、Tab 补全、多行编辑。
- **更快、更省内存**：同一 MSYS2 环境公平 A/B，启动、7 项负载、VmRSS 全部不劣于 bash（详见 [路线图](docs/roadmap.md)）。
- **二进制插件**：自有 C ABI，可注册内建、读写 shell 变量（详见 [插件](docs/plugins.md)）。
- **双语 help**：C / zh_CN 双 locale，77 个主题完整手册页。

## 快速开始

wash 面向 Linux/POSIX。在 Windows 上通过 MSYS2 的 **msys 子系统**开发（禁用 MinGW/ucrt）。

```bash
# Linux（POSIX 环境）
cargo build --release

# Windows：MSYSTEM=MSYS 放在调用 bash 之前
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd <repo> && cargo build'

# 运行
./target/debug/wash -c 'echo hello $USER'
./target/debug/wash                  # 无参数进入交互 REPL
```

需要 Rust（`msys/rust` 包或 Linux 上的 rustup）、MSYS2 提供 gcc/bash。构建时 `build.rs`
会硬闸绑定 msys gcc，误用其他工具链会直接报错。

## 兼容性现状

wash 处于**进行中**：核心执行与展开系统已对齐，但仍有已知缺口。当前登记的重点：

- 计划内特性已全部实现：`select`、`shopt -s nullglob` 均已与 bash 5.3 逐字节对齐。`[[ ]]`、`declare -A` 关联数组、`getopts`、`trap -l`、`declare -f`/`-F`、`(( ))` 算术命令、C 风格 `for ((...))` 循环、brace group 作循环 body、`continue N` 跨层传播同样对齐（含畸形语法诊断、1024 桶哈希遍历顺序与函数定义版式）。窄边界：`shopt` 全表仅列 wash 实现的 3 项、`select` ≥10 项的多列菜单布局依赖终端宽度，详见[兼容性矩阵](docs/compatibility.md)。
- 有意偏离 bash 仅 3 项（见 [兼容性矩阵](docs/compatibility.md)）。

内建逐项状态、实测依据、已知缺陷一律以 [兼容性矩阵](docs/compatibility.md) 为准——那是
实测结论，不是设计意图。

## 测试

```bash
bash tests/bash_diff_builtins.sh     # 主闸门：全部内建逐字节对拍
bash tests/check_version.sh          # 版本串一致性
bash tests/perf/better_than_bash.sh  # 性能 / 启动 / 内存门禁
```

验证方法与测量纪律见 [测试与验证](docs/testing.md)。

## 文档

| 文档 | 内容 |
| ---- | ---- |
| [架构与平台](docs/architecture.md) | 执行管线、MSYS2/POSIX 工具链、REPL 实现 |
| [兼容性矩阵](docs/compatibility.md) | 内建逐项状态、有意偏离、未实现特性 |
| [路线图与产品目标](docs/roadmap.md) | 阶段状态、性能目标、已知待修 |
| [测试与验证](docs/testing.md) | oracle 对拍方法、套件清单、测量纪律 |
| [二进制插件](docs/plugins.md) | C ABI 契约、编译与加载、安全模型 |
| [版本串](docs/versioning.md) | 版本生成唯一真源与校验 |
| [变更日志](CHANGELOG.md) | 发布历史与兼容边界 |
| [贡献指引](AGENTS.md) | 仓库内开发约定、铁律、实现细节 |

## 许可

待定（参考 bash GPL / fish GPLv2）。
