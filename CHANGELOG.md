# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.0] - 2026-11-07

### Added

- **二进制插件系统（wash C ABI）**：`enable -f FILE NAME...` 从共享库加载内建，
  `enable -d NAME...` 热卸载。插件只导出 `wash_plugin_init`，宿主通过
  `WashPluginApi` 函数指针表提供输出、变量读写、内建状态查询；一个插件可注册
  多个内建，并可为每个内建提供 `help` / `short_doc`。C 头文件见
  `include/wash_plugin.h`，示例插件见 `tests/fixtures/`。
- **`enable` 全选项**：`-a`（含 disabled 状态）、`-n`、`-p`（reusable 格式）、
  `-s`（POSIX special builtins）、`-f`、`-d`；`BASH_LOADABLES_PATH` 搜索路径与
  默认 5 段路径。
- **插件安全模型**：ABI 版本双向协商、`struct_size` 前向兼容检查、
  `BASH_LOADABLES_PATH` 白名单（realpath 后父目录必须命中）、拒绝符号链接与
  默认 `.` / 相对路径段、拒绝与静态内建同名的注册。
- **POSIX 动态加载垫片**：`src/posix_shim.c` 的 `wash_dlopen/wash_dlsym/
  wash_dlclose/wash_dlerror`，供 MSYS2（Cygwin）与真 Linux 共用。
- **版本一致性闸门** `tests/check_version.sh`：静态 + 运行时双重校验。
- 新增 `docs/spike-dlopen.md`（两平台 dlopen 技术验证结论）、
  `tests/plugin_contract.sh`（插件 ABI 契约套件）、
  `tests/plugin_second_oracle.sh`（WSL bash 5.3.0 能力等价对拍）。

### Changed

- 版本号升级到 **0.2.0**：`Cargo.toml` 为唯一真源，`--version`、`help` 抬头、
  `$BASH_VERSION`、PS1 `\v` 全部改用 `env!("CARGO_PKG_VERSION")`；两个 `.po` 头
  同步更新。
- `BASH_VERSION` 继续诚实标注 wash 自身版本（`wash 0.2.0`），**不冒充 bash 版本**。
- `enable -s` 修正为 POSIX special builtins（此前是空操作占位）；
  `enable -a` / `-p` 对 disabled 内建输出 `enable -n <name>`。
- 内建名单统一：动态内建同样出现在 `type` / `command` / `builtin` /
  `compgen -b` / `help` / REPL Tab 补全中。

### Compatibility（重要，受控扩展）

- **MSYS2 主 oracle 的 bash 5.3.20 编译时关闭了动态加载**（任何 `enable -f`
  都报 `enable: dynamic loading not available` rc=2），而 wash 按要求
  **全平台启用**插件加载。因此 `-f` / `-d` 及插件加载成功路径
  **无法在该平台逐字节对拍**，其验证改由「wash 自洽 golden 契约套件 +
  WSL 真 Linux bash 5.3.0 第二 oracle 的能力等价行为对拍」完成。
  兼容性矩阵中该能力标 `wash-extension`，**不标 `full`**。
- `enable -a` / `-p` / `-s` 全量列表因内建集合本身与 bash 不同，仍用语义
  不变量与契约套件验证，不逐字节比较。
- 可与 oracle 逐字节对齐的 `enable` 静态行为全部进入 `tests/bash_diff_builtins.sh`
  的 GROUP C；**GROUP A 340 条回归用例一句未改**。
- 插件为进程内加载：与宿主同权限、同崩溃域；插件自身崩溃会拖垮 shell。
  这是二进制插件的固有属性，本期不提供子进程隔离。

## [0.0.1] - 2026-10-31

- 初始引导：外部命令执行、管道、重定向、POSIX FFI 垫片、MSYS2 工具链。
