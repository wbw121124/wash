# P0 技术验证：两平台 dlopen 能力（2026-11-07）

二进制插件系统实施前的 spike。探针全部放在**仓库外**临时目录
（MSYS2 `/d/msys2/tmp/wash-spike/`、WSL `/var/tmp/wash-spike/`），
本文件保存结论与可复现命令，仓库内不留临时探针。

## 结论速览

| 验证项 | MSYS2（Cygwin target） | WSL（真 Linux） |
|---|---|---|
| `dlopen/dlsym/dlclose/dlerror` | ✅ 全链路通过 | ✅ 全链路通过 |
| 共享库后缀 | `.dll` 与 `.so` **都能**被 dlopen | `.so` ✅ |
| `-ldl` | **不需要**（加与不加都链接/运行成功） | **不需要**（glibc 2.35 已并入 libc） |
| 插件引用宿主可执行文件符号 | ❌ 链接 DLL 时即 `undefined reference`；`--export-dynamic` 不支持 PE+ | 未测（不需） |
| bash-ABI loadable 插件 | 不适用（无 bash 开发头） | ✅ 可编译、可加载、可卸载 |

**关键决策（spike 驱动）**：
1. **ABI 必须走 vtable**：Cygwin 下插件无法反向解析宿主可执行文件的符号，
   因此插件只导出 `wash_plugin_init`，宿主把函数指针表传进去；
   宿主无需 `-rdynamic`/`--export-all-symbols`。
2. **`build.rs` 不 emit `cargo:rustc-link-lib=dl`**：两平台默认链接已提供 dl*。
   先按「不加」实现；若 MSYS2 最终链接报 `undefined reference to dlopen`，
   再按 `CARGO_CFG_TARGET_OS == "cygwin"` 分支加回（不使用 `cfg!(unix)`，
   该 target 下为假）。
3. 契约套件统一用 `.so` 后缀（`$SUF=so`），MSYS2 实测可 dlopen。
4. WSL 第二 oracle 技术可行：bash 5.3.0 自带开发头与示例 .so，
   自编 bash-ABI 插件的「加载→调用→卸载」链路已实测通过。

## MSYS2 探针

```bash
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc '
cd /d/msys2/tmp/wash-spike
GCC="$(rustc --print sysroot)/bin/gcc.exe"     # = /usr/bin/gcc.exe (GCC 16.2.0)
$GCC -shared -fPIC -o libprobe.dll dl_probe.c
$GCC -shared -fPIC -o libprobe.so  dl_probe.c
$GCC -o dlhost      dl_host.c -ldl   && ./dlhost ./libprobe.dll
$GCC -o dlhost_nodl dl_host.c        && ./dlhost_nodl ./libprobe.so
$GCC -shared -fPIC -o libref.dll dl_ref.c      # 引用宿主符号 wash_host_fn
'
```
`dl_probe.c` 导出 `probe_add`/`probe_msg`；`dl_host.c` 依次
`dlopen(RTLD_NOW|RTLD_LOCAL)` → `dlerror` → `dlsym` ×2 → 调用 → `dlclose`。

实测输出（两种后缀、有无 `-ldl` 均相同）：
```
DLOPEN_OK
DLSYM_OK add(2,3)=5
MSG=PROBE_OK
DLCLOSE=0
```
宿主符号反解探针（`dl_ref.c` 引用 `extern int wash_host_fn(void)`）结果：
```
/usr/.../ld: dl_ref.c:(.text+0x9): undefined reference to `wash_host_fn'
/usr/.../ld: warning: --export-dynamic is not supported for PE+ targets
```

## WSL 探针

```bash
wsl -e bash -c '
cd /var/tmp/wash-spike
gcc -shared -fPIC -o libprobe.so dl_probe.c
gcc -o dlhost dl_host.c -ldl  && ./dlhost ./libprobe.so
gcc -o dlhost_nodl dl_host.c  && ./dlhost_nodl ./libprobe.so
gcc -fPIC -shared \
  -I/usr/local/include/bash \
  -I/usr/local/include/bash/include \
  -I/usr/local/include/bash/builtins \
  -o bash_hello.so bash_hello.c
LC_ALL=C /usr/local/bin/bash -c "enable -f ./bash_hello.so hello; hello; enable -d hello"
'
```
`bash_hello.c` 为最小 bash loadable 模板（`struct builtin hello_struct` +
`hello_builtin_load/unload` + ` hello_builtin(WORD_LIST*)` 打印
`HELLO_FROM_PLUGIN`）。**三个 `-I` 缺一不可**（`stdc.h` 在 `include/`、
`bashgetopt.h`/`common.h` 在 `builtins/`）。实测：
```
load_rc=0
HELLO_FROM_PLUGIN
call_rc=0
builtin
unload_rc=0
```

## 复现用最小源码

`dl_probe.c`
```c
int probe_add(int a, int b) { return a + b; }
const char *probe_msg(void) { return "PROBE_OK"; }
```

`dl_host.c`（节选）
```c
#include <dlfcn.h>
#include <stdio.h>
void *h = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
void *p = dlsym(h, "probe_add");        /* 先 dlerror() 清错，再判 dlerror() */
int (*add)(int,int) = (int (*)(int,int))p;
printf("%d\n", add(2,3));
dlclose(h);
```

`bash_hello.c`（节选）
```c
#include "builtins.h"
#include "shell.h"
#include "bashgetopt.h"
#include "common.h"
int hello_builtin(WORD_LIST *list) { puts("HELLO_FROM_PLUGIN"); return EXECUTION_SUCCESS; }
char *hello_doc[] = { "hello from plugin", (char *)NULL };
struct builtin hello_struct = { "hello", hello_builtin, BUILTIN_ENABLED,
                                hello_doc, "hello", 0 };
int hello_builtin_load(void) { return 1; }
void hello_builtin_unload(void) { }
```
