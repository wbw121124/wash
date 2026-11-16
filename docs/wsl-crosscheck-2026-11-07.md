# WSL 交叉校验结论（2026-11-07）

目标：用真 Linux GNU bash 5.3.0（WSL Ubuntu-22.04 自装 `/usr/local/bin/bash`）
交叉校验 wash 的 GROUP A 对拍套件（338 用例），确认 MSYS2 bash 5.3.20 oracle
结论在真 Linux 上成立。

## 结果

**304 / 338 通过；34 个失败全部定性为平台固有限制，非 wash 语义回归。**

- 通过的 304 个覆盖全部纯内存语义：解析、展开（含 `${!arr[@]}`、`${!prefix@}`、
  间接展开）、算术（含 `2 ** -1` 报错）、`( )` 子 shell、语法错误 rc/诊断、
  内建返回值 —— 真 Linux bash 5.3.0 下逐字节一致。
- 同一套件同一 wash.exe 在主 oracle（MSYS2 bash 5.3.20）下 338/338 全绿。

## 34 个失败的三类根因（均非语义回归）

### A. 文件系统视图隔离（约 27 个：cd-*、source-*、redir-fd2、
group-cd-persist、subshell-isolate-cd、exit-in-source）
套件 sandbox/mktemp 落在 WSL 的 `/tmp`，而 wash.exe 是 Windows 进程（经
interop 执行），其 `/tmp` 是 Cygwin 视图（MSYS2 安装目录），两个文件系统
互不可见。凡引用真实路径的用例必然分叉。实测：wash.exe 在 WSL 下
`echo hi > /tmp/wtest` 报成功，文件实际落在 MSYS2 的 `/tmp`。

### B. 环境块差异（3 个：export-p、export-noarg、export-no-name）
bash 侧看到 WSL 进程环境，wash.exe 继承 interop 转换后的 Windows 环境，
`export -p` / `export` 输出必然不同。

### C. Cygwin-on-WSL 进程创建异常（4 个：exec-missing、
alias-noninteractive、set-o、set-plus-o）
- `exec missing`：bash 报 `not found`，wash 报 `File exists`（errno 在
  Cygwin + WSL 内核组合下错乱）。
- `alias-noninteractive`：wash rc=127 正确但**丢失** command-not-found
  诊断（同源：进程创建错误报告路径异常）。
- `set -o`/`set +o`：wash 多输出 `igncr` 项 —— MSYS2 平台特有选项，
  真 Linux bash 没有。

## 运行注意事项

1. **环境污染**：宿主（WorkBuddy）注入 `BASH_ENV`（Windows 路径 shim）+
   `WSLENV=BASH_ENV/u`。跑套件必须 `env -u BASH_ENV -u WSLENV`，否则
   wash（Cygwin 可读该 shim）与 WSL bash（读不到）行为分叉。
2. **WSL ext4 易损坏**：本次 ext4.vhdx 三次进入 emergency read-only
   （目录块校验和错误 / aborted journal）。修复方法：
   ```powershell
   wsl --shutdown
   wsl --mount "C:\Users\wbw\AppData\Local\Packages\CanonicalGroupLimited.Ubuntu22.04LTS_79rhkp1fndgsc\LocalState\ext4.vhdx" --vhd --bare
   wsl -d CachyOS --user root -- e2fsck -fy /dev/sdd   # 256G 那块
   wsl --unmount
   ```
   fs 只读时套件症状极具迷惑性：`mktemp -d` 失败 → `$TMP` 为空 → 全部
   重定向 Permission denied → run_case 两侧全空「假 PASS」（331/338），
   仅 sem 用例现形。**看到 `$TMP` 为空的 Permission denied 先查 fs 只读**。
3. 调用形式（用户约定）：`wsl -e bash -c 'cd <WASH_REPO> && ...'`（`<WASH_REPO>` 为仓库在 WSL 中的挂载路径）；
   bash oracle 用 `/usr/local/bin/bash`（5.3.0；系统 `/usr/bin/bash` 是
   5.1.16 仅参考）。用完 `wsl --shutdown`。

## 结论

WSL 交叉校验证实：wash 的纯语义层在真 Linux bash 5.3.0 上逐字节兼容。
路径/环境/进程创建类用例受 Windows 二进制 × WSL 的固有限制，以主 oracle
（MSYS2，两边同为 Cygwin 进程、共享文件系统视图）为准，已全绿。
