# 二进制插件（wash C ABI）

插件是导出 `wash_plugin_init` 的共享库（MSYS2 与 Linux 均用 `.so` 后缀）。宿主通过
`WashPluginApi` 函数指针表向插件提供 stdout/stderr 输出、shell 变量读写与内建状态查询；
一个插件可一次注册多个内建，每个内建可带 `help` / `short_doc`。
C 头文件是 `include/wash_plugin.h`，可编译示例见 `tests/fixtures/`。

## 编译与加载

```bash
# 编译示例插件（MSYS2 用 /usr/bin/gcc.exe，Linux 用 gcc）
gcc -shared -fPIC -Iinclude -o /path/to/plugins/plugin_hello.so \
    tests/fixtures/plugin_hello.c

# 从 BASH_LOADABLES_PATH 白名单目录加载，调用，再热卸载
BASH_LOADABLES_PATH=/path/to/plugins ./target/debug/wash -c \
  'enable -f plugin_hello.so plhello; plhello; enable -d plhello'
```

## 契约与安全模型

- 搜索路径：未设 `BASH_LOADABLES_PATH` 时用 bash 5.3 的默认 5 段路径并丢弃
  `.` / 空段 / 相对段；文件名不含 `/` 时按该路径搜索，含 `/` 时 realpath 后的
  父目录也必须命中白名单；符号链接一律拒绝。
- ABI 版本双向协商：版本或 `struct_size` 不符时立即拒绝并卸载，不调用任何插件函数。
- `enable -a/-p/-s` 的列表语义、`type` / `compgen -b` / `help` / Tab 补全中的
  动态内建联动，由 `tests/plugin_contract.sh` 做 golden 契约验证。
- **安全模型**：插件与宿主同权限、同崩溃域（进程内加载），插件自身崩溃会拖垮 shell；
  只加载可信目录中的可信插件。本期不提供子进程隔离。
- **不与 bash 官方 loadable `.so` 二进制兼容**——这是 wash 自有扩展，不参与 MSYS2
  逐字节对拍，矩阵中标 `wash-extension`。

MSYS2 主 oracle 的 bash 5.3.20 编译时关闭了动态加载（`enable -f/-d` 报
"dynamic loading not available"），故扩展能力由插件契约套件与 WSL 第二 oracle 验证。
两平台动态加载能力的技术验证见 `docs/spike-dlopen.md`。
