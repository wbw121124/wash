# AGENTS.md

本项目是 wash（wbw121124's advanced shell）。仓库 `.github/workflows/build.yml` 构建 Linux / Windows(mingw64 / mingw32 / ucrt64) 产物并发布 GitHub Releases。

## 提交前必须完成的步骤（Commit 流程）

每次 commit/push 前必须按顺序完成：

1. **同步文档**：实现若有改动，同步更新 `docs/index.md`（语言规范）等相关文档，确保与实际行为一致。
2. **本地三环境测试**：在项目根目录用 MSYS2 bash 执行
   `bash scripts/test-all.sh`（或单独 `bash scripts/test-all.sh msys|mingw64|ucrt64`）。
   三个环境必须全部 **PASS** 才算通过。
3. **检查 diff**：`git status` / `git diff` 确认只包含本次意图的改动，无残留构建产物（build/ 已在 .gitignore）。
4. **push 使用代理**：
   ```
   git -c http.proxy=http://127.0.0.1:7890 -c https.proxy=http://127.0.0.1:7890 push
   ```

## 本地开发环境（Windows + MSYS2）

- 所有 shell 命令必须用 MSYS2 bash 执行：`G:\msys64\usr\bin\bash.exe -lc '...'`
- 环境变量（`MSYSTEM`、`PATH`、`CMAKE_CXX_COMPILER` 等）必须在 **bash 内部** export，
  pwsh 设置的环境变量不会传入 bash。
- 三个测试环境：
  - `msys`    -> `MSYSTEM=MSYS`    `/usr/bin/g++`  链接 msys-2.0.dll
  - `mingw64` -> `MSYSTEM=MINGW64` `/mingw64/bin/g++`
  - `ucrt64`  -> `MSYSTEM=UCRT64`  `/ucrt64/bin/g++`
- 构建目录：`build/local/<env>`，每次测试脚本会重建，保证干净。
- 本机只有 `/usr/bin/cmake`。`CMakeLists.txt` 对 MSYS2 做了特殊处理（见下）。

### 已知工具链坑

- **mingw64 的 gcc 必须 ≥ 16.2.0**：15.2.0 与新版 mingw-w64 头不匹配，
  任何用到 `stdout`/`stderr` 的程序都会链接失败（`undefined reference to __imp___acrt_iob_func`）。
  升级：`pacman -S mingw-w64-x86_64-gcc mingw-w64-x86_64-gcc-libs mingw-w64-x86_64-gcc-libgfortran mingw-w64-x86_64-crt mingw-w64-x86_64-headers`
- **MSYS2 下 Intl 查找**：`find_package(Intl)` 在 msys cmake 下会命中 `/usr/lib`（MSYS 的 libintl），
  导致 mingw64/ucrt64 产物错误链接 msys 运行时。`CMakeLists.txt` 在
  `WASH_MINGW_ENV` 分支跳过 `find_package`，用 `NO_DEFAULT_PATH` 手动查找 `<prefix>/lib`。
- **i18n 编码**：`src/main.cpp` 必须调用 `bind_textdomain_codeset("wash", "UTF-8")`，
  否则 mingw64 下的 libintl 会按 Windows ANSI 代码页输出 GBK（msys 环境不受影响）。

## 待办

- `wash.1` man page 尚未创建（见 `docs/plan.md` 第 9.2 节），是 GPL 分发文档要求的一部分。