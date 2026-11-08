# wash vs GNU bash 双平台性能对比与回退判定（2026-11-08）

> 执行：QA 工程师严过关（software-qa-engineer）｜被测发布点：HEAD `3e7680f`（附注 tag `v0.2.1` 指向该提交）
> 对照基线：2026-11-07（`docs/perf-plan-2026-11-07.md`、`docs/perf-wsl-2026-11-07.md`）
> 本报告是本次任务**唯一**写入仓库的文件；未改 `src/`、未 `git commit`、未 `git push`、未触碰 `stash@{0}` 与任何 tag。
> 最终定性经主理人三轮独立窗口交叉验证后裁定（2026-11-08），详见 **§5 旧/新 release 同窗口 A/B 与源码演进归因**。
>
> **2026-11-09 入库补记**（原文保留不改，此处只登记后续状态）：
> ① 上一段描述的是 11-08 QA 任务当时的状态；后续源码优化已以 `e3ba08d` 提交、本报告单独入库，详见 §10.8。
> ② 文中 `<WASH_REPO>` 为 wash 仓库根目录占位符，仓库内不写入本机绝对路径；`/d/msys2/...` 为 MSYS2 安装目录，`/mnt/d/msys2/...` 为其 WSL 挂载视图，`/var/tmp/...` 为 WSL 内临时构建目录。文中的命令为实测记录模板，复现时把 `<WASH_REPO>` 换成本地仓库根即可。

---

## 0. 结论摘要（双层结论，缺一不可）

### 0.1 主判定：相对 oracle（GNU bash），双平台**无确定性回退**

`3e7680f` 相对 2026-11-07 基线，在 wash/bash 比值上总体改善或持平；所有「比值 >1.00x」或「相对基线差值 >+0.1x」的疑似项，均经沉降后交错复测**证伪**。

| 维度 | MSYS2 跨轮最小 | 11-07 基线 | 差值 | WSL 跨轮最小 | 11-07 基线 | 差值 |
|---|---|---|---|---|---|---|
| 性能 TOTAL | **0.77x** | 0.93x | −0.16x | **0.70–0.83x**（四轮区间，见 §4.8） | 0.84–0.85x | −0.14~−0.15x |
| 启动（50 次） | **0.59x** | 0.72x | −0.13x | **0.43x** | 0.44x | −0.01x |
| 内存 VmRSS | **0.64x** | 0.85x | −0.21x | 1.01x（bench 四轮）/ **0.996x**（专项 10× 探针） | ≈0.98–1.00x | 持平 |

（差值 = 本次 − 基线；wash/bash 比值**越小越好**，负号 = 改善方向）

- MSYS2 三轮 30 个比值**全部 <1.00x**，无一项越线，无一项恶化 >0.1x。
- WSL 四轮出现过 `fork-exec-true` 1.28x / 1.09x / 1.07x、`pipeline` 1.01x、`arith-loop` 1.00x、`cmdsub` 0.98x、`TOTAL` 0.97x/0.91x 等越线/近线读数及 VmRSS 1.01–1.02x，全部由 R1 低噪声窗口、R4 `ROUNDS=9` 加深轮、两次专项 10× 交错探针与 `/proc/loadavg` 证据定性为**外部 CPU 争用噪声 / 页级采样波动 / 基线取值口径差异**（§6）。
- **上述「改善」不得写成「wash 变强」**：§5 的 A/B 证明源码演进未使解释器性能变好，比值改善的主因是测量窗口/机器状态（bash 侧绝对值亦从 16646ms 降到 10317–10423ms）。
- WSL VmRSS 严格字面口径（「任何 >1.00x 且复测确认即回退」）下会被升级为「确定性轻微偏差（+0.01~0.04x，+20~72KB）」；主判定依据主理人给定的 **0.1x 明显恶化阈值**与历史文档承认的平台持平特征，判为「持平 ≈1.00x、无退化」。两种口径并列，不放宽、不掩盖。

### 0.2 补充判定：相对 2026-11-07 旧实现（0.0.1 源码），存在**确定性性能退化（P2 级）**

旧 release（`wash 0.0.1`，正是 11-07 收官基线所用二进制）与新 release（`wash 0.2.0` / `3e7680f`）同窗口交错 A/B 及两轮沉降复测，三个独立窗口一致证实：

| 项 | 20:00 干净 A/B（`ROUNDS=5`） | 20:08 QA 沉降复测（`ROUNDS=9`） | 20:12 主理人无并行窗口（`ROUNDS=9`） | 判定 |
|---|---:|---:|---:|---|
| startup(50次) | 1.51x | 1.570x | **1.521x** | 确定性退化 |
| arith-loop(2000) | 1.13x | 1.247x | **1.167x** | 确定性退化 |
| builtin-echo(500) | 1.21x | 1.231x | **1.203x** | 确定性退化 |
| expand-heavy(1000) | 1.23x | 1.222x | **1.194x** | 确定性退化 |

（比值 = NEW/OLD，>1.00x 表示新实现更慢；三个独立窗口四项全部 >1.10x、方向一致、量级一致）

**定性**：相对 2026-11-07 旧实现，`3e7680f` 的启动 **+51~57%**、纯解释器负载 **+17~25%**，属**确定性性能退化，P2 级**（可观测、需登记跟踪；不阻断发布、不影响语义兼容）。这是 0.0.1→0.2.0 新增初始化/每命令路径（二进制插件系统 `src/plugin.rs` + `wash_dl*` shim、i18n、help、间接展开等功能累积）的**功能代价，不是语义回归缺陷**。严格区分「相对旧 wash」与「相对 GNU bash」：当前 wash 相对 bash 启动仍 **0.59x**、纯解释器 **0.74 / 0.71 / 0.75x**，**不是相对 oracle 的回退**。

### 0.3 跨版本基线口径声明（主理人 synthesis，必须保留）

版本号升到 `0.2.0` 是 2026-11-08 的 P4 提交 `26e90f6`；而 2026-11-07 收官基线（TOTAL 0.93x / 启动 0.72x / 内存 0.85x）正是用 **`0.0.1` 源码的 release** 测的。当前 `3e7680f` 相比它多了整个二进制插件系统与三项加固。因此「与 2026-11-07 基线对照」是**跨源码版本的回归对照，不是同一份代码的 A/B**：差值若在噪声范围内记「持平」，若出现确定性恶化必须说明「可能来自新增代码路径」，**不得直接下结论写成「v0.2.1 回退」**。真正区分「窗口效应 vs 源码演进」的判据是 §5 的同窗口 A/B。

---

## 1. 环境与二进制新鲜度证据

### 1.1 仓库与提交状态（实测，`LC_ALL=C LANG=C`）

| 项 | 实测值 |
|---|---|
| HEAD | `3e7680f`（2026-11-08 15:31:56 +0800）`fix(help): EPIPE-safe output; harden plugin gates; extend enable -f WSL oracle` |
| `git status --porcelain` | 0 行（基准前后均洁净） |
| `git describe --tags --always` | `v0.2.1`；`git rev-list -n1 v0.2.1` = `3e7680f0c6b61eed992aa5a105efe15025475dff` |
| `Cargo.toml:3` | `version = "0.2.0"`（v0.2.1 只是 tag，**未升版本号**） |

因此 `wash --version` 应为 **`wash 0.2.0`**，不得据此误判二进制过期；源码 `src/builtins.rs` 中版本输出为编译期常量 `env!("CARGO_PKG_VERSION")`。

### 1.2 两端 oracle 与工具链

| 项 | MSYS2 / Cygwin | WSL / 真 Linux |
|---|---|---|
| oracle | `/d/msys2/usr/bin/bash.exe` = GNU bash **5.3.20(1)-release (x86_64-pc-cygwin)** | `/usr/local/bin/bash` = GNU bash **5.3.0(1)-release (x86_64-pc-linux-gnu)** |
| 内核 | Windows + MSYS2 | Ubuntu-22.04 / WSL2 / Linux 6.18.54.1-microsoft-standard-WSL2 |
| 工具链 | `/usr/bin/cargo` 1.99.1 / `rustc` 1.99.1（MSYS2 项目构建） | `/usr/bin/cargo` 1.75.0 / `rustc` 1.75.0 |
| locale | `LC_ALL=C LANG=C`（脚本内建 + 外层显式） | 同左 |
| HOME | `mktemp -d` 沙箱（bench.sh 内建） | 同左 |

### 1.3 二进制新鲜度（本次最关键的前置修正）

**重建前的 `target/release/wash.exe` 是无效数据，已实测确认：**

| 二进制 | 重建前 mtime | 重建前 `--version` |
|---|---|---|
| `target/release/wash.exe` | 2026-11-07 17:42:31 | **`wash 0.0.1`** |
| `target/debug/wash.exe` | 2026-11-08 15:21:27 | `wash 0.2.0` |

release 二进制不仅 mtime 早于 `3e7680f`（`src/builtins.rs` 15:20:47），其编译期版本常量还是 `0.0.1` —— 它编译自 `Cargo.toml` 尚为 `0.0.1` 的旧源码。**若直接按 bench.sh 默认 release 跑，得到的是旧实现的数据，属无效测量。**

**重建结果（两端均为 release 对 release）：**

| 平台 | 路径 | 构建命令 | mtime | `--version` |
|---|---|---|---|---|
| MSYS2 | `<WASH_REPO>/target/release/wash.exe` | `MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc 'cd <WASH_REPO> && CARGO_NET_OFFLINE=true cargo build --release --offline'`（rc=0，35.14s） | 2026-11-08 **19:19:12** | `wash 0.2.0` |
| WSL | `/var/tmp/wash-build/target/release/wash` | WSL 副本内 `env -u BASH_ENV -u WSLENV CARGO_NET_OFFLINE=true cargo build --release --offline`（rc=0，26.59s） | 2026-11-08 **19:20:55** | `wash 0.2.0` |

WSL 二进制文件类型：`ELF 64-bit LSB pie executable, x86-64, dynamically linked, BuildID[sha1]=c91ab04aca0017e913c846edf54099160c6f430a`（含 debug_info、not stripped）。

WSL 同步与防坑证据（日志 `wsl-build.log`）：`src/`、`po/`、`include/`、`tests/`、`Cargo.toml`、`build.rs` 从 `/mnt<WASH_REPO>` 复制；**仅在 WSL 副本删除 `Cargo.lock`**（cargo 1.75 不认 lockfile v4，仓库 `Cargo.lock` 未动）；`src/builtins.rs`、`src/main.rs`、`src/expand.rs` 的 sha256 两端逐一相同（builtins.rs = `a9ee5bf02135a1cdadc2396c533e95d174660df701f47dc84c620c1e868f4f46`），证明 `3e7680f` 的 EPIPE-safe 改动已进入 WSL 副本；同步后 `touch` 全部 `src/*.rs` 并删除旧 wash 二进制，**强制 cargo 重编译**，杜绝 DrvFS 保留 mtime 导致跳过改动的坑；构建前 `touch /var/tmp/wash-build/.writable_probe` 成功，排除 ext4 emergency_ro「输出全空」假数据；构建目录固定在 `/var/tmp`（`/tmp` 会被实例回收）。构建完成后 `git status --porcelain` 仍为 0 行。

旧 release 的留存、来源与实际用途（作为 §5 A/B 的 OLD 侧判据）见 **§5.2** 与 **§9.2**。

### 1.4 基准脚本与防陷阱措施

- 基准脚本 `tests/perf/bench.sh` **未作任何修改**，sha256 = `5dcfc728f9b3ab8f7bf7157546d6c33e4c2ca1026782b2c11587290d76809b02`，两端各轮日志均记录该值，保证逐项可比。
- 脚本为三合一：① 工作负载（内部循环 + 单次 `-c`），② 启动 50 次 × ROUNDS 轮交错，③ 内存 `tail -f /dev/null | <bin> -c 'read x'` 阻塞进程读 `/proc/<pid>/status` 的 VmRSS（3 次取最小）。
- **`CASES` 实际 7 条**（`arith-loop` / `builtin-echo` / `fork-exec-true` / `cmdsub` / `subshell` / `pipeline` / `expand-heavy`）；`docs/perf-plan-2026-11-07.md` 写「8 工作负载」为文档笔误，**以脚本为准，按 7 条逐项比**，未因数量差异误判。
- **第 18 行陷阱**：`[ -x "$WASH_BIN" ] || WASH_BIN=".../target/debug/wash.exe"` —— 传入路径不存在会**静默回退到 debug**（Rust 无优化，比值不可比）。两端包装脚本 `msys2-bench.sh` / `wsl-bench.sh` 在每轮开头对被测 release 做 `[ -x ]` 硬核验，并把**绝对路径 + mtime + `--version` + 源码 mtime + bench.sh sha256**（WSL 另加可写探针）写进该轮日志；7 轮日志全部显示 `wash 0.2.0` 与 release 路径，**未发生回退**。
- **`time_run` 丢弃 stdout/stderr 且不检查退出码**：若被测二进制启动失败（缺 dll / 路径错）会被计成「极快」形成假优势。故在 A/B 之前对两侧 release 与旧副本各做一次可见输出 smoke test，断言 stdout 恰为 `PERF_SMOKE_OK` / `2000` 两行、rc=0、stderr 空（§5.2）。
- 所有探针/原始日志/编译产物均落在仓库外临时目录：`/d/msys2/tmp/wash-perf-2026-11-08/` 与 `/var/tmp/wash-perf-logs/`（WSL 侧，双份）；仓库根目录无垃圾文件。

---

## 2. 测量方法与纪律

1. **严格串行**：MSYS2 R1→R2→R3 结束后才启动 WSL R1→R2→R3→R4→专项探针；任何时刻只允许一个基准进程在跑，基准期间不构建、不跑其它 CPU 密集任务。两端构建亦串行（MSYS2 19:19 完成 → WSL 19:20 完成）。**并发纪律的两处例外（导致数据作废）如实登记于 §8 第 10、11 条**。
2. **release 对 release**：两端均使用 1.3 节重建并核验的 release 二进制；§5 的 A/B 两侧同为 release、同优化级别。
3. **同窗口交错 best-of-N**：MSYS2 3 轮 `ROUNDS=5`；WSL 3 轮 `ROUNDS=5` + 1 轮 `ROUNDS=9` 加深轮；A/B 1 轮 `ROUNDS=5` + 沉降复测 `ROUNDS=9`；bash/wash（或 OLD/NEW）交错取最小。
4. **跨轮最小比值**：每轮已 best-of-N，跨轮再取最小（R4 为 best-of-9，表中已标注）。
5. **疑似项沉降复测**：R3 前记录 `/proc/loadavg` 并静置 20s；VmRSS 与 cmdsub 各做一次独立专项 10× 交错探针；启动/纯解释器四项做两轮 `ROUNDS=9` 沉降交错复测（§5.5）。所有疑似项均先复测再定性，禁止凭单次/单轮读数下结论。
6. **原始 stdout 全量落盘**：每轮完整日志同时保存于 `/d/msys2/tmp/wash-perf-2026-11-08/` 与 `/var/tmp/wash-perf-logs/`（WSL 侧），支持断点续跑，不伪造、不补数。
7. 长命令全部后台执行；`wsl.exe -e bash -lc` 统一 `env -u BASH_ENV -u WSLENV` 去掉宿主注入。
8. **每轮回报同时给出两侧绝对 ms/KB 与比值**：跨轮绝对值波动可达 4.5x，只给比值无法复核噪声窗口；本报告所有表格均并列绝对值与比值。

---

## 3. MSYS2 结果（3 轮，`ROUNDS=5`，oracle bash 5.3.20）

被测指纹三轮一致：`<WASH_REPO>/target/release/wash.exe`，mtime 19:19:12，`wash 0.2.0`，`[ -x ]` 通过。

### 3.1 R1（19:21:44–19:24:36，耗时 2m53s）

| workload | bash(ms) | wash(ms) | wash/bash |
|---|---:|---:|---:|
| arith-loop(2000次) | 135 | 100 | 0.74x |
| builtin-echo(500次) | 120 | 85 | 0.71x |
| fork-exec-true(100次) | 3386 | 2746 | 0.81x |
| cmdsub(100次) | 2116 | 1596 | 0.75x |
| subshell(100次) | 1889 | 1463 | 0.77x |
| pipeline(50次) | 2655 | 2078 | 0.78x |
| expand-heavy(1000次) | 122 | 91 | 0.75x |
| **TOTAL(best-of-5)** | **10423** | **8159** | **0.78x** |
| startup(50次) | 3853 | 2291 | 0.59x |
| VmRSS(read x 阻塞) | 8384KB | 5376KB | 0.64x |

### 3.2 R2（19:25:06–19:28:03，耗时 2m57s）

| workload | bash(ms) | wash(ms) | wash/bash |
|---|---:|---:|---:|
| arith-loop(2000次) | 124 | 95 | 0.77x |
| builtin-echo(500次) | 110 | 80 | 0.73x |
| fork-exec-true(100次) | 3284 | 2782 | 0.85x |
| cmdsub(100次) | 2003 | 1437 | 0.72x |
| subshell(100次) | 1849 | 1356 | 0.73x |
| pipeline(50次) | 2814 | 2053 | 0.73x |
| expand-heavy(1000次) | 133 | 93 | 0.70x |
| **TOTAL(best-of-5)** | **10317** | **7896** | **0.77x** |
| startup(50次) | 4051 | 2446 | 0.60x |
| VmRSS(read x 阻塞) | 8384KB | 5440KB | 0.65x |

### 3.3 R3（19:28:25–19:31:26，耗时 3m02s）

| workload | bash(ms) | wash(ms) | wash/bash |
|---|---:|---:|---:|
| arith-loop(2000次) | 127 | 96 | 0.76x |
| builtin-echo(500次) | 111 | 83 | 0.75x |
| fork-exec-true(100次) | 3442 | 3128 | 0.91x |
| cmdsub(100次) | 2026 | 1613 | 0.80x |
| subshell(100次) | 1889 | 1370 | 0.73x |
| pipeline(50次) | 2667 | 2127 | 0.80x |
| expand-heavy(1000次) | 121 | 89 | 0.74x |
| **TOTAL(best-of-5)** | **10383** | **8506** | **0.82x** |
| startup(50次) | 3882 | 2361 | 0.61x |
| VmRSS(read x 阻塞) | 8384KB | 5376KB | 0.64x |

### 3.4 MSYS2 跨轮最小与基线逐项差值

| 指标 | R1 | R2 | R3 | 跨轮最小 | 11-07 基线 | 差值 |
|---|---:|---:|---:|---:|---:|---:|
| TOTAL | 0.78x | 0.77x | 0.82x | **0.77x** | 0.93x | −0.16x |
| startup(50次) | 0.59x | 0.60x | 0.61x | **0.59x** | 0.72x | −0.13x |
| VmRSS | 0.64x | 0.65x | 0.64x | **0.64x** | 0.85x | −0.21x |
| arith-loop | 0.74x | 0.77x | 0.76x | **0.74x** | 0.87x | −0.13x |
| builtin-echo | 0.71x | 0.73x | 0.75x | **0.71x** | 0.83x | −0.12x |
| fork-exec-true | 0.81x | 0.85x | 0.91x | **0.81x** | 0.97x | −0.16x |
| cmdsub | 0.75x | 0.72x | 0.80x | **0.72x** | 0.93x | −0.21x |
| subshell | 0.77x | 0.73x | 0.73x | **0.73x** | <1.00x | 改善（未定量） |
| pipeline | 0.78x | 0.73x | 0.80x | **0.73x** | <1.00x | 改善（未定量） |
| expand-heavy | 0.75x | 0.70x | 0.74x | **0.70x** | 0.79x | −0.09x |

基线来源：`docs/perf-plan-2026-11-07.md` 第 67–68 行（commit `8461131` 收官）与第 75–81 行「最终基线」。另有 P0-1 绝对值 TOTAL 16646/15293ms、startup 4760/3951ms、VmRSS 10368/8896KB；本次 bash TOTAL 10317–10423ms、wash 7896–8506ms，绝对值显著更低，属**机器负载窗口差异**，按纪律只用同窗口比值，不做绝对值跨窗口比较。

### 3.5 趋势与窗口说明（如实登记，不掩盖）

- **`fork-exec-true` 三轮 0.81→0.85→0.91x 单调上升**：虽仍 <1.00x 且优于基线 0.97x、不构成回退，但趋势如实写出。R1/R2 已证明低值可复现，且 §5 A/B 中 fork/exec 类四项全部 <1.00x（NEW 更快），故该上升趋势与「源码退化」方向相反，属窗口噪声。
- **`fork-exec-true` 0.81x / VmRSS 0.64x 相对基线 0.97x / 0.85x 改善幅度偏大**：历史结论是 fork-exec 受 Cygwin fork 平台下限约束、内存优势主要来自 bash 的 Cygwin runtime 开销，二者不应因源码新增插件系统而大幅变强。结合 bash 侧绝对值同步大幅下降（16646→10317–10423ms），更可能是**窗口/机器状态差异**，不是 wash 变强；§5 A/B 的定性支持这一点。
- **内存绝对值平台对照**：MSYS2 本轮 bash 8384KB / wash 5376KB，相对历史 10368 / 8896KB **两侧都大幅下降**（新增代码不可能让内存变少，显然是窗口/Cygwin runtime 差异）；而 WSL 侧两侧绝对值几乎不变（§4.9）。这是区分「窗口效应」与「真实增长」的关键证据。

---

## 4. WSL 结果（4 轮 + 2 次专项探针，oracle bash 5.3.0）

被测指纹四轮一致：`/var/tmp/wash-build/target/release/wash`，BuildID `c91ab04a…`，mtime 19:20:55，`wash 0.2.0`，`[ -x ]` 通过，可写探针 `PROBE_OK`。

### 4.1 R1（19:32:45–19:33:08，`ROUNDS=5`，39s）

| workload | bash(ms) | wash(ms) | wash/bash |
|---|---:|---:|---:|
| arith-loop(2000次) | 79 | 42 | 0.53x |
| builtin-echo(500次) | 63 | 22 | 0.35x |
| fork-exec-true(100次) | 234 | 173 | 0.74x |
| cmdsub(100次) | 98 | 77 | 0.79x |
| subshell(100次) | 70 | 56 | 0.80x |
| pipeline(50次) | 113 | 92 | 0.81x |
| expand-heavy(1000次) | 17 | 9 | 0.53x |
| **TOTAL(best-of-5)** | **674** | **471** | **0.70x** |
| startup(50次) | 229 | 108 | 0.47x |
| VmRSS(read x 阻塞) | 3176KB | 3236KB | **1.02x** |

### 4.2 R2（19:34:11–19:34:35，`ROUNDS=5`，44s）— 高噪声窗口

| workload | bash(ms) | wash(ms) | wash/bash |
|---|---:|---:|---:|
| arith-loop(2000次) | 85 | 66 | 0.78x |
| builtin-echo(500次) | 31 | 14 | 0.45x |
| fork-exec-true(100次) | 227 | 291 | **1.28x** |
| cmdsub(100次) | 123 | 104 | 0.85x |
| subshell(100次) | 82 | 60 | 0.73x |
| pipeline(50次) | 102 | 100 | 0.98x |
| expand-heavy(1000次) | 15 | 8 | 0.53x |
| **TOTAL(best-of-5)** | **665** | **643** | **0.97x** |
| startup(50次) | 249 | 108 | 0.43x |
| VmRSS(read x 阻塞) | 3180KB | 3240KB | **1.02x** |

### 4.3 R3（19:36:33–19:36:59，`ROUNDS=5`，沉降后，75s）

沉降记录：`/proc/loadavg` 静置前 `1.96 1.27 0.53` → 静置 20s 后 **`4.28 1.90 0.77`（不降反升）**，说明宿主存在不可控外部 CPU 负载，20s 静置未达成理想沉降。该事实本身作为测量局限证据如实记录。

| workload | bash(ms) | wash(ms) | wash/bash |
|---|---:|---:|---:|
| arith-loop(2000次) | 37 | 37 | **1.00x** |
| builtin-echo(500次) | 53 | 21 | 0.40x |
| fork-exec-true(100次) | 188 | 205 | **1.09x** |
| cmdsub(100次) | 92 | 90 | 0.98x |
| subshell(100次) | 91 | 69 | 0.76x |
| pipeline(50次) | 105 | 106 | **1.01x** |
| expand-heavy(1000次) | 20 | 8 | 0.40x |
| **TOTAL(best-of-5)** | **586** | **536** | **0.91x** |
| startup(50次) | 243 | 114 | 0.47x |
| VmRSS(read x 阻塞) | 3180KB | 3252KB | **1.02x** |

### 4.4 R4（19:38:39–19:39:10，`ROUNDS=9` 加深轮，47s）

`loadavg` BEFORE `2.89 0.64 0.21` → AFTER `2.40 0.80 0.28`，较 R3 的 4.28 明显回落。

| workload | bash(ms) | wash(ms) | wash/bash |
|---|---:|---:|---:|
| arith-loop(2000次) | 54 | 18 | 0.33x |
| builtin-echo(500次) | 15 | 8 | 0.53x |
| fork-exec-true(100次) | 127 | 136 | **1.07x** |
| cmdsub(100次) | 86 | 70 | 0.81x |
| subshell(100次) | 69 | 55 | 0.80x |
| pipeline(50次) | 96 | 90 | 0.94x |
| expand-heavy(1000次) | 15 | 8 | 0.53x |
| **TOTAL(best-of-9)** | **462** | **385** | **0.83x** |
| startup(50次) | 231 | 108 | 0.47x |
| VmRSS(read x 阻塞) | 3180KB | 3200KB | **1.01x** |

### 4.5 WSL 跨轮最小与基线逐项差值

| 指标 | R1 | R2 | R3 | R4(9) | 跨轮最小 | 11-07 基线（主口径） | 差值 | 11-07 明细跨轮最佳 | 对明细差值 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| TOTAL | 0.70x | 0.97x | 0.91x | 0.83x | **0.70x** | 0.84–0.85x | −0.14~−0.15x | 0.84–0.85x | −0.14~−0.15x |
| startup(50次) | 0.47x | 0.43x | 0.47x | 0.47x | **0.43x** | 0.44x | −0.01x | 0.44x | −0.01x |
| VmRSS | 1.02x | 1.02x | 1.02x | 1.01x | **1.01x**（专项 MIN **0.996x**） | ≈0.98–1.00x | 持平 | ≈0.98–1.00x | 持平 |
| arith-loop | 0.53x | 0.78x | 1.00x | 0.33x | **0.33x** | 内部语义 0.5–0.8x | 区间内/更优 | 0.56x | −0.23x |
| builtin-echo | 0.35x | 0.45x | 0.40x | 0.53x | **0.35x** | 内部语义 0.5–0.8x | 区间内/更优 | 0.49x | −0.14x |
| fork-exec-true | 0.74x | 1.28x | 1.09x | 1.07x | **0.74x** | ≈1.00x 持平 | 改善 | ≈1.00x | −0.26x |
| cmdsub | 0.79x | 0.85x | 0.98x | 0.81x | **0.79x** | 一致窗口 0.79–0.80x | 持平 | 0.56x | **+0.23x**（该口径不可用于回退判定，见 §6.7） |
| subshell | 0.80x | 0.73x | 0.76x | 0.80x | **0.73x** | 内部语义 0.5–0.8x | 区间内/更优 | 0.69x | +0.04x |
| pipeline | 0.81x | 0.98x | 1.01x | 0.94x | **0.81x** | ≈1.00x 持平 | 改善 | 0.73x | +0.08x |
| expand-heavy | 0.53x | 0.53x | 0.40x | 0.53x | **0.40x** | 内部语义 0.5–0.8x | 区间内/更优 | 0.53x | −0.13x |

基线来源：`docs/perf-wsl-2026-11-07.md` 第 20–36 行（四轮明细与跨轮最佳）、第 40–58 行（结论）。绝对值（历史最低噪声窗口 R1）：bash 单次启动 ~4.6ms、wash ~2.1ms，VmRSS bash ~3184KB / wash ~3168KB。本次 R1 对应值：bash 229/50≈4.58ms、wash 108/50≈2.16ms，VmRSS 3176/3236KB —— 与历史窗口几乎重合，数据可比性好。

### 4.6 专项复测一：VmRSS 10× 交错探针（19:40:49–19:41:29）

沿用 bench.sh 的 `mem_kb` 逻辑，10 轮 bash/wash 交错逐次输出（独立脚本，不改基准脚本）；沉降复测时**每侧 3 次采样的原始读数**亦全部记录于此，便于判断采样抖动还是真实占用上升：

| round | bash_kb | wash_kb | ratio |
|---:|---:|---:|---:|
| 1 | 3188 | 3268 | 1.025x |
| 2 | 3212 | 3212 | 1.000x |
| 3 | 3252 | 3240 | 0.996x |
| 4 | 3216 | 3188 | 0.991x |
| 5 | 3220 | 3208 | 0.996x |
| 6 | 3180 | 3168 | 0.996x |
| 7 | 3188 | 3252 | 1.020x |
| 8 | 3188 | 3180 | 0.997x |
| 9 | 3192 | 3228 | 1.011x |
| 10 | 3180 | 3256 | 1.024x |
| **MIN** | **3180** | **3168** | **0.996x** |
| **MEAN** | **3201** | **3220** | **1.006x** |

`valid_pairs=10`。分布 0.991–1.025x，**跨越 1.00x 两侧**，10 次中 4 次 wash 更省；均值 1.006x 与基线 ≈0.98–1.00x 吻合。bench.sh 四轮之所以稳定显示 1.01–1.02x，根因是内存段仅「bash 取 3 次最小 + wash 取 3 次最小」分别取极值、样本过少导致的抽样方向偏差。R4 加深轮亦由 1.02x 回落到 1.01x（差值仅 +20KB），证明差值并非刚性固定。**判定：页级采样波动 + 抽样口径偏差，VmRSS 持平 ≈1.00x、无退化。**

### 4.7 专项复测二：cmdsub 10× 交错探针（19:44:28–19:44:33）

沿用 bench.sh 的 cmdsub 脚本与计时逻辑，10 轮交错逐次输出：

| round | bash_ms | wash_ms | ratio |
|---:|---:|---:|---:|
| 1 | 124 | 92 | 0.74x |
| 2 | 236 | 104 | 0.44x |
| 3 | 273 | 316 | **1.16x** |
| 4 | 1240 | 216 | 0.17x |
| 5 | 389 | 153 | 0.39x |
| 6 | 135 | 96 | 0.71x |
| 7 | 131 | 97 | 0.74x |
| 8 | 180 | 181 | **1.01x** |
| 9 | 222 | 121 | 0.55x |
| 10 | 92 | 84 | 0.91x |
| **MIN（分别取最小）** | **92** | **84** | **0.91x** |

bash 绝对值 92↔1240ms（**13.5x 波动**），单次比值 0.17–1.16x（含 2 次越线）；稳定轮次（1/6/7/10）为 0.71–0.91x。

### 4.8 历史噪声区间对照与 TOTAL 区间表述

- 本次 R1 绝对值与历史最低噪声窗口几乎重合（startup bash 4.58ms vs 4.6ms、wash 2.16ms vs 2.1ms；VmRSS 3176/3236KB vs 3184/3168KB），**但 TOTAL 比值 0.70x vs 历史 0.84–0.85x** —— 机器状态几乎相同而比值不同，恰说明不能把 −0.14x 写成「wash 变强」，必须由 §5 A/B 定性。
- `docs/perf-wsl-2026-11-07.md` 四轮里被标为高噪声的 R2 的 TOTAL 是 **0.65x**，与本次 0.70x 同量级；文档采作基线的 0.84–0.85x 来自 R1/R4。**0.70x 很可能落在历史已观测的噪声区间内**，不只拿 0.84x 对比。
- 跨轮最小 TOTAL 0.70x（R1, best-of-5）与加深轮 0.83x（R4, best-of-9）差距较大；本报告**并列两者 + 全部四轮表格**，判定用「**0.70–0.83x，均优于或持平基线 0.84–0.85x**」的区间表述，不把 0.70x 单独当结论。
- **`fork-exec-true` 1.28x 高于历史 WSL 曾观测的最大 1.12x**，不能只写「历史上出现过持平窗口」带过：R3 1.09x、R4 1.07x 仍 >1.00x，均如实登记为越线；归因于宿主 CPU 争用（loadavg 1.96→4.28、同 ELF 低噪声窗口 R1 0.74x、专项 cmdsub 探针 13.5x 波动），并由 §5 A/B 中 fork/exec 类四项全部 <1.00x 排除源码因素。

### 4.9 平台内存对照

| 平台 | 本次 bash | 本次 wash | 历史 bash | 历史 wash | 解读 |
|---|---:|---:|---:|---:|---|
| MSYS2 / Cygwin | 8384KB | 5376KB | 10368KB | 8896KB | **两侧都大幅下降**，新增代码不可能让内存变少 → 窗口/Cygwin runtime 差异 |
| WSL / 真 Linux | 3176–3180KB | 3200–3256KB | 3184KB | 3168KB | 两侧绝对值几乎不变，仅 wash 侧小幅波动 → 采样/页级波动 |

---

## 5. 旧/新 release 同窗口 A/B 与源码演进归因（本章为最终判据）

### 5.1 目的、跨版本口径与设计

「本次 0.77x / 0.70–0.83x 相对历史 0.93x / 0.84–0.85x 的差值」究竟是测量窗口还是源码演进，不能靠并列两组数字回答。为此做**同窗口 A/B**：OLD = `wash 0.0.1`（2026-11-07 源码 release，正是历史基线所用二进制）作 `BASH_BIN`，NEW = `wash 0.2.0`（`3e7680f`）作 `$1`，`ROUNDS=5` 交错、同为 release、同优化级别、同窗口，绕开 bash 侧噪声，可直接判定源码演进有无性能变化。

**列头含义（防止复核误读）**：bench.sh 打印的 `bash(ms)` 列实为 **OLD-wash**、`wash(ms)` 列实为 **NEW-wash**、`wash/bash` 比值 = **NEW/OLD**。日志 `ab-qa-rerun.log` 第 3 行已明确标注。

因果判定链（主理人给定）：`new/old ≈ 1.00x` → 证明「0.77x vs 0.93x」的差值来自测量窗口/机器状态，**不是**源码演进带来的性能变化；`new/old` 显著偏离 1.00x（>0.1x）→ 才需把差异归因到新增插件系统/加固代码，并做沉降复测。

### 5.2 前置证据与旧副本来源

| 证据 | 内容 | 结果 |
|---|---|---|
| `smoke-msys2.log`（658B, 19:52:28） | MSYS2 新 release `-c 'echo PERF_SMOKE_OK; i=0; while [ $i -lt 2000 ]; do i=$((i+1)); done; echo $i'` | stdout 恰 `PERF_SMOKE_OK`/`2000` 两行、rc=0、stderr 0 字节 → PASS |
| `smoke-wsl.log`（644B, 19:52:51） | WSL ELF 同上 | 同上 → PASS |
| `old-release-verify.log`（837B, 19:52:53） | 旧副本 `--version` 与 `-c` smoke | `--version` 恰 `wash 0.0.1` rc=0；`-c` 两行、rc=0、stderr 0 → PASS |
| 主理人侧 `old-release-version.log` / `smoke-old-release.log` | `wash 0.0.1` rc=0；stdout hex `504552465f534d4f4b455f4f4b0a323030300a`，stderr `.err` 文件 0 字节 | PASS |
| PRECHECK（写进 `ab-qa-rerun.log` 第 16–24 行） | 旧副本对 bench.sh 全部 7 个负载逐个 `-c` 的 rc | **7/7 rc=0**，排除「旧副本跑不动脚本被 `time_run` 记成极快」的假优势 |

**旧 release 副本来源与实际用途**（修正早期草稿「本次未使用」的过时表述）：cargo 重建前遗留的旧 bin 产物 `target/release/deps/wash-2219c1d80d1f9544.exe`（mtime 2026-11-07 17:42:31、size 2203901、link count 1；新的 `wash-d3838d8bee837364.exe` 与 `target/release/wash.exe` 互为 hardlink），由主理人 `cp -p` 留存为 `/d/msys2/tmp/wash-perf-2026-11-08/wash-0.0.1-release.exe`（mtime 2026-11-07 17:42:31.636708900、size 2203901、sha256 `9527ce0153bd814b29028e299aa1213f461100ddf01e19b895dd82f8d5e63ebc`）。它**实际用于 §5 A/B 的 OLD 侧（作 `BASH_BIN`）与两轮沉降复测的 OLD 侧**，是本章因果判定的核心判据，而非「未使用」。

**指纹**：OLD `wash 0.0.1` / sha256 `9527ce01…`；NEW `wash 0.2.0` / sha256 `9d97de6c65fa09cc78f41ffe8c42a1907f97f07a2b8645e6af31c7f1a85182cd`；bench.sh sha256 `5dcfc728…`。

**首跑作废**：`ab-old-vs-new.log`（1708B, 19:56:34）被 QA 与主理人 `lead-ab.sh` 两个进程并发写同一路径而污染（表格、日志头碎片、两套头尾标记交错），**已作废、不作任何数据**。详见 §8 第 10 条。

### 5.3 第一窗口：干净 A/B 完整表格（20:00:14–20:02:51，`ROUNDS=5`）

日志 `ab-qa-rerun.log`（45 行，唯一头尾、无碎片、`END bench.sh (rc=0)`）、原始 stdout `ab-qa-rerun-raw.log`。窗口干净：主理人 `lead-ab.sh` 19:54:39 结束后，19:59:04 用 `ps -ef`（含 WSL 侧）确认无任何 `bench.sh`/`wash.exe`/`cargo` 残留，20:00:14 才开跑。

| workload | OLD(ms) | NEW(ms) | NEW/OLD |
|---|---:|---:|---:|
| arith-loop(2000次) | 98 | 111 | **1.13x** |
| builtin-echo(500次) | 81 | 98 | **1.21x** |
| fork-exec-true(100次) | 2890 | 2620 | 0.91x |
| cmdsub(100次) | 1636 | 1429 | 0.87x |
| subshell(100次) | 1540 | 1350 | 0.88x |
| pipeline(50次) | 2161 | 1979 | 0.92x |
| expand-heavy(1000次) | 74 | 91 | **1.23x** |
| **TOTAL(best-of-5)** | **8480** | **7678** | **0.91x** |
| startup(50次) | 1577 | 2386 | **1.51x** |
| VmRSS | 5376KB | 5440KB | **1.01x** |

### 5.4 三层分层解读（TOTAL 0.91x 不等于「解释器性能持平」）

`TOTAL 0.91x`「未过 0.1x」**不等于**「解释器性能持平」。拆开看，实质是三层混合结果：

1. **纯解释器负载三项全部 >1.10x**：`arith-loop` 1.13x、`builtin-echo` 1.21x、`expand-heavy` 1.23x —— NEW 慢 **13–23%**，方向一致；
2. **fork/exec 类四项全部 <1.00x**：`fork-exec-true` 0.91x、`cmdsub` 0.87x、`subshell` 0.88x、`pipeline` 0.92x —— NEW 快 **8–13%**，可能受益于旧 release 之后落地的 cmdsub 解析缓存等优化；
3. **TOTAL 0.91x 只是被 fork 类大数值加权主导后的混合结果，掩盖了纯解释器负载的退化**（fork 类绝对值 1350–2890ms 远大于纯解释器 74–111ms）。再加 **startup 1.51x**，纵向 A/B 的实质结论是：**源码演进让「启动 + 纯解释器」变慢、「进程类」变快，混合后 TOTAL 略快**。

`VmRSS 1.01x`（+64KB）≈1.00x：按因果链「A/B 无差异 → 波动」，WSL 侧 bench 四轮 1.01–1.02x 归为采样/页级波动，**不是**新增插件系统的常驻开销；与 §4.6 专项 10× 探针（0.991–1.025x、MIN 0.996x、MEAN 1.006x）一致。

### 5.5 三轮交叉验证（判据的核心）

| 项 | 20:00 干净 A/B（`ROUNDS=5`） | 20:08 QA 沉降复测（`ROUNDS=9`） | 20:12 主理人无并行窗口（`ROUNDS=9`） |
|---|---:|---:|---:|
| startup(50次) | 1.51x | 1.570x | **1.521x** |
| arith-loop(2000) | 1.13x | 1.247x | **1.167x** |
| builtin-echo(500) | 1.21x | 1.231x | **1.203x** |
| expand-heavy(1000) | 1.23x | 1.222x | **1.194x** |

- **第二窗口（20:08）**：`settle-ab-probe.log`（78 行，唯一头尾、rc=0, 20:09:51），独立脚本 `settle-ab-probe.sh` 在仓库外、未改 bench.sh；一轮同时覆盖 4 项、OLD/NEW 交错 `ROUNDS=9`、固定先 OLD 后 NEW（与 bench.sh 一致）、`LC_ALL=C` + HOME 沙箱；开跑前 `CONCURRENT_BENCH_PROCS=0`；loadavg 静置前 `2.63 3.26 3.38` → 20s 后 `1.85 3.03 3.30`（有效沉降）。跨轮最小（各侧 best-of-9）：startup 1571/2467ms=**1.570x**、arith 77/96ms=**1.247x**、echo 65/80ms=**1.231x**、expand 72/88ms=**1.222x**；逐轮比值 startup 1.244–1.823x（mean 1.479）、arith 1.140–2.500x（mean 1.382）、echo 1.224–1.292x（mean 1.252）、expand 0.556–1.278x（mean 1.162）。两个单点异常如实记录：arith 第 4 轮 NEW=205ms → 2.500x；expand 第 3 轮 OLD=178ms → 0.556x（唯一 <1.00x 的一轮，OLD 侧单点抖动）；剔除后其余 8 轮方向不变。**该窗口与主理人独立探针并行（§8 第 11 条），不是完全独占窗口，不作为最终无并行判据。**
- **第三窗口（20:12，主理人亲自跑，QA 完全静止、无并行）**：`lead-settle-probe.log`（48 行，loadavg 全程 `0.00`，`ROUNDS=9` 交错）：startup **1.521x**(1545/2350ms)、arith **1.167x**(78/91ms)、echo **1.203x**(64/77ms)、expand **1.194x**(72/86ms)。据主理人回报，此轮为**最终无并行判据**。
- **三个独立窗口、四项全部 >1.10x、方向一致、量级一致 → 退化信号被证实，不是噪声。**

### 5.6 因果判定链结论：窗口效应 vs 源码演进

- **「0.77x vs 0.93x」的差值主因是测量窗口，不是源码演进让 wash 变强**：A/B 的 TOTAL 0.91x 显示 NEW 相对 OLD 仅略快且分层后纯解释器反而变慢，无法解释 −0.16x 的相对 bash 改善；两侧绝对值同步下降（bash 16646→10317–10423ms）是窗口效应的直接证据。§3.5 中 `fork-exec-true` 0.81x、VmRSS 0.64x 的「异常改善」随之定性为窗口效应，**不得写成「性能大幅提升」**。
- **但源码演进确实带来确定性退化**：启动与纯解释器三项在三个窗口一致 >1.10x，按因果链归因到 0.0.1→0.2.0 新增初始化/每命令路径，而非窗口噪声。因此不能一句「≈1.00x 全是窗口效应」带过：**性能/内存的总体差值属窗口，启动与纯解释器的退化属源码**，两者分层并存。
- **VmRSS 归因裁决**：A/B 1.01x ≈1.00x → 归为 WSL 侧测量/环境差异（采样/页级波动），**未能归因到新增代码开销**。

### 5.7 覆盖限制

WSL 侧旧 release 在重建时已被删除，**A/B 只能在 MSYS2 侧做**。报告必须写明：不能把 MSYS2 的 A/B 结论无条件外推到 WSL；它仅作为「源码演进是否改变性能」的旁证，配合两平台相对 bash 同向的变化来解释窗口效应。WSL 侧无纵向 A/B，故 §0.2 的退化幅度（启动 +51~57%、纯解释器 +17~25%）**仅在 MSYS2 侧成立**。

### 5.8 最终定性与四点说明

**判定：相对 2026-11-07 旧实现（`0.0.1` 源码）的确定性性能退化，P2 级**（可观测、需登记跟踪；不阻断发布、不影响语义兼容）。四点说明：

1. **功能代价，不是语义回归缺陷**：属 0.0.1→0.2.0 新增初始化/每命令路径（二进制插件系统 `src/plugin.rs` + `wash_dl*` shim、i18n、help、间接展开等功能累积）带来的开销，不改变命令语义的逐字节兼容性。
2. **严格区分「相对旧 wash」与「相对 GNU bash」**：当前 wash 相对 bash 启动仍 **0.59x**、纯解释器 **0.74 / 0.71 / 0.75x**，**不是相对 oracle 的回退**；§0.1 的「双平台无确定性回退」结论依然成立。
3. **双平台相对 bash 的差值主因是测量窗口**，不得写成「wash 变强」；历史高噪声窗口（WSL R2 TOTAL 0.65x）与本次 0.70x 同量级，0.70x 落在历史已观测噪声区间内。
4. **VmRSS 1.01x ≈ 持平**，配合专项 10× 探针归为采样/页级波动，无退化、无新增代码常驻开销。

---

## 6. 回退判定（含复测证伪/证实过程）

### 6.1 判定口径（主理人给定）

> 比值 >1.00x **或** 相对基线恶化明显（>0.1x）**且经复测确认** → 判定确定性回退并上报；否则记为噪声/持平。
> 本机噪声极大，一切结论必须同窗口 bash/wash 交错 best-of-≥5 + 跨轮复测；任何「疑似回退」必须沉降后交错复测证伪/证实，不得直接写进结论。

### 6.2 MSYS2 R3 `fork-exec-true` 0.91x / TOTAL 0.82x —— 证伪

R3 是三轮最高点，主因 `fork-exec-true` 0.81→0.85→0.91x 单调上升（§3.5 已如实登记趋势）。但仍 <1.00x、明显优于基线 0.97x / 0.93x；R1/R2 已证明 0.81x / 0.78x、0.77x 可复现，未形成持续恶化趋势；§5 A/B 中 fork/exec 类四项全部 <1.00x，与「源码退化」方向相反。**判定：噪声，不构成疑似回退。**

### 6.3 WSL R2 `fork-exec-true` 1.28x / TOTAL 0.97x —— 证伪

同一 ELF、同一脚本、相隔 1 分钟：bash 侧绝对值几乎不变（TOTAL 674→665、startup 229→249、VmRSS 3176→3180），wash 侧内部语义负载稳定（echo 22→14、expand 9→8、cmdsub 77→104、subshell 56→60），但 `fork-exec-true` wash 173→291ms（+68%）使 TOTAL wash 471→643（+36%）。R1 同 ELF 给出 0.74x/0.70x；R4 best-of-9 绝对值 127/136ms 远低于 R2 的 227/291ms；跨轮最小 0.74x/0.70x。**判定：外部 CPU 争用噪声，证伪；1.28x 高于历史最大 1.12x 的越线事实如实保留（§4.8）。**

### 6.4 WSL R3 `arith-loop` 1.00x / `fork-exec-true` 1.09x / `pipeline` 1.01x / `cmdsub` 0.98x / TOTAL 0.91x —— 证伪

沉降阶段 loadavg 1.96→4.28 不降反升，证明外部负载未平息；R1 低噪声窗口与 R4 加深轮均不复现这些越线；跨轮最小分别 0.33x / 0.74x / 0.81x / 0.79x / 0.70x，全部显著 <1.00x 且优于基线。**判定：噪声，证伪。**

### 6.5 WSL R4 `fork-exec-true` 1.07x —— 证伪（且不掩盖平台特征）

历史 `docs/perf-wsl-2026-11-07.md` 已明确：`fork-exec-true`/`pipeline` ≈1.00x 持平，噪声窗口会出现 1.12x / 1.02x，是「平台真实特征，不掩盖」。本次 R4 的 1.07x 与历史 R3 的 1.12x、R4 的 1.01x 同类；跨轮最小 0.74x，且 R4 绝对值（bash 127 / wash 136ms）为四轮最低。**判定：平台持平特征 + 噪声，证伪。**

### 6.6 WSL VmRSS 1.01–1.02x（四轮 bench）—— 证伪

见 §4.6 与 §5.4：专项 10× 探针分布 0.991–1.025x、MIN 0.996x、MEAN 1.006x；R4 回落 1.01x（+20KB）；A/B 1.01x ≈1.00x。**判定：页级采样波动 + 抽样口径偏差，VmRSS 持平 ≈1.00x、无退化。**

> **口径说明（不放宽）**：若采用「任何 >1.00x 且复测确认即回退」的最严格字面口径，此项应升级为「确定性轻微偏差（+0.01~0.04x，+20~72KB）」。主判定依据 0.1x 明显恶化阈值与历史平台特征判为「持平」。两种口径并列。

### 6.7 WSL `cmdsub` 对明细基线 +0.23x —— 证伪（超 0.1x 阈值项）

核对 `docs/perf-wsl-2026-11-07.md` 明细时发现：其逐项「跨轮最佳」`cmdsub` = 0.56x，本次跨轮最小 0.79x，机械相减为 **+0.23x > 0.1x**，故追加一次固定设计的专项 10× 交错复测（§4.7），不靠文字解释放过、不反复测到满意为止：

1. 历史 0.56x **来自该文档明确标注的 R2「高噪声」窗口**（同轮 TOTAL 0.65x、fork-exec 0.66x、startup 0.23x 均为四轮异常低值）；把被标注为高噪声的单点最小值混入基线，本身是「挑最有利单点」的方法论缺陷。**主口径 = 一致窗口 0.79–0.80x + TOTAL 0.84–0.85x；0.56x 口径的 +0.23x 差值并列列出，但标注「该口径不可用于回退判定」。**
2. 历史 R1/R3/R4 的 cmdsub 为 **0.79 / 0.79 / 0.80x**，与本次四轮 **0.79 / 0.85 / 0.98 / 0.81x** 一致。
3. 专项探针显示 bash 绝对值 92↔1240ms（13.5x 波动）、单次比值 0.17–1.16x（含 2 次越线），稳定轮 0.71–0.91x；`MIN(分别取最小)` 0.91x 仍 <1.00x。
4. 本次四轮 cmdsub **全部 <1.00x**，绝对值 bash 86–123ms / wash 70–104ms 正常；主理人指定基线「内部语义负载稳定 0.5–0.8x」，0.79x 落在区间内。
5. `3e7680f` 的三项加固不触碰 `cmdsub` 热路径（`parse_cmdsub_cached` 等）。

**判定：基线取值口径差异（历史异常低值） + 噪声，证伪，不构成确定性回退。** 列为 P3 观察项：历史逐项「跨轮最佳」明细含高噪声窗口读数，不宜直接作为逐项门禁基线。

### 6.8 相对旧实现的退化 —— **证实**（唯一被证实的项）

见 §5：三个独立窗口四项全部 >1.10x，方向一致、量级一致 → **证实**「相对 2026-11-07 旧实现（`0.0.1` 源码）的确定性性能退化」，P2 级。此项**不是相对 GNU bash 的回退**，与 §0.1 主判定并存不矛盾。

### 6.9 最终判定汇总

| 疑似项 | 触发阈值 | 复测手段 | 结论 |
|---|---|---|---|
| MSYS2 R3 fork 0.91x / TOTAL 0.82x | 无（<1.00x） | R1/R2 跨轮最小、§5 A/B fork 类 <1.00x | 噪声，证伪 |
| WSL R2 fork 1.28x / TOTAL 0.97x | >1.00x | R1 同 ELF 低噪声窗口、R4 best-of-9、跨轮最小 | 噪声，证伪（越线事实保留） |
| WSL R3 arith 1.00x / fork 1.09x / pipeline 1.01x / cmdsub 0.98x / TOTAL 0.91x | >1.00x 或近线 | 沉降 20s + loadavg、R1、R4、跨轮最小 | 噪声，证伪 |
| WSL R4 fork 1.07x | >1.00x | 跨轮最小、历史平台特征 | 平台持平特征 + 噪声，证伪 |
| WSL VmRSS 1.01–1.02x（连续 4 轮） | >1.00x | 专项 10× VmRSS 探针、R4 加深轮、§5 A/B 1.01x | 页级波动 + 抽样偏差，证伪（持平） |
| WSL cmdsub 对明细 +0.23x | >+0.1x | 专项 10× cmdsub 探针、历史轮次比对 | 基线口径差异 + 噪声，证伪 |
| **相对旧实现的 startup/纯解释器退化** | **>1.10x（A/B）** | **20:00 A/B + 20:08 沉降 + 20:12 无并行，三轮交叉验证** | **证实：确定性性能退化，P2 级** |

**结论（双层）**：相对 oracle（GNU bash），双平台**无确定性回退**；相对 2026-11-07 旧实现（`0.0.1` 源码），**存在确定性性能退化（P2 级）**，属功能代价、非语义回归。

---

## 7. P0–P4 风险分级

| 级别 | 结论 | 明细 |
|---|---|---|
| **P0**（阻断发布） | **无** | 无功能/语义/性能阻断项 |
| **P1**（高，需修复） | **无** | 无相对 oracle 的证实回退 |
| **P2**（中，需跟进） | **1 项** | **相对 2026-11-07 旧实现（`0.0.1` 源码）的确定性性能退化**：启动 +51~57%、纯解释器 +17~25%，三个独立窗口（含 loadavg 0.00 无并行窗口）一致证实，详见 §5。属新增插件系统/i18n/help/间接展开等功能累积的**功能代价，非语义回归缺陷**；不阻断发布、不影响逐字节兼容；**不是相对 GNU bash 的回退**（相对 bash 启动仍 0.59x、纯解释器 0.74/0.71/0.75x）。建议登记跟踪：若后续对启动/纯解释器路径做优化，以 §5 的 A/B 与沉降复测为回归判据；WSL 侧因旧 release 已删无法纵向 A/B，不得外推。 |
| **P3**（低，观察项） | 4 项 | ① WSL VmRSS 在 bench.sh 3 次取最小口径下稳定显示 1.01–1.02x（+20~72KB），已由 10× 探针 + A/B 1.01x 定性为页级波动；若后续门禁采用「任何 >1.00x 即回退」最严格口径，此项会被判为确定性轻微偏差。② 本机噪声极大（WSL cmdsub 单次 bash 92↔1240ms、13.5x；loadavg 1.96→4.28），单轮/单次读数绝不可用于门禁，建议 CI 固定 `ROUNDS≥5` 且跨轮 ≥2、取跨轮最小。③ `docs/perf-wsl-2026-11-07.md` 逐项「跨轮最佳」明细含高噪声窗口异常低值（R2 cmdsub 0.56x 等），作为逐项门禁基线会误报，建议改用稳定轮次中位数或标注噪声窗口来源。④ **并发纪律两处例外**（§8 第 10、11 条）：两个进程写同一日志路径会造成数据作废，建议后续基准统一「唯一文件名 + 独立 raw 文件再拼接」并做开跑前并发检查。 |
| **P4**（信息/无需动作） | 4 项 | ① `Cargo.toml` 仍 `version="0.2.0"` 而 tag 为 `v0.2.1`（已知状态，非缺陷，勿据此判二进制过期）。② 重建前 `target/release/wash.exe` 为 `wash 0.0.1` 的过期产物，已重建；bench.sh 第 18 行静默回退 debug 陷阱已由每轮 `[ -x ]` 硬核验规避；`time_run` 吞 rc 的风险已由 smoke + PRECHECK 规避。③ `3e7680f` 三项加固本身未在 wash/bash 比值上产生负向影响（相对 oracle 全指标仍 <1.00x）；但 0.0.1→0.2.0 的整体功能累积带来 §5 所述的 P2 级纵向退化，二者不矛盾。④ WSL 实例在基准结束后已被自动空闲回收（报告完成时 `wsl.exe --list --running` 输出「没有正在运行的分发」），但 `wsl.exe -e bash -lc` 仍可按需拉起并访问 `/var/tmp/wash-build`；全程**未执行 `wsl --shutdown`**，由主理人统一收尾。 |

---

## 8. 测量局限说明

1. **本机噪声极大且不可控**：WSL `cmdsub` 单次 bash 92↔1240ms（13.5x）、`fork-exec-true` wash 173↔291ms；R3 沉降期间 loadavg 1.96→4.28 不降反升。两端虽总体串行、基准期间不构建，但 Windows 宿主侧后台负载无法排除。因此**只看同窗口比值 + 跨轮最小**，单轮读数不用于判定。
2. **绝对值不可跨窗口比较**：本次 MSYS2 bash TOTAL 10317–10423ms vs 历史 P0-1 的 16646ms；WSL R1 与历史最低噪声窗口接近但 R2/R3 差异明显。所有结论均基于比值。
3. **两端 oracle/平台不同，不可混算**：MSYS2 为 Cygwin bash 5.3.20（进程创建/DLL 地板占启动 ~94%），WSL 为真 Linux bash 5.3.0（fork+exec 便宜）。`fork-exec-true`/`pipeline` 的平台特征差异属真实特征，不掩盖。
4. **bench.sh 内存段样本过少**：每轮仅 3 次且 bash/wash **分别取最小**，会产生抽样方向偏差（本次稳定显示 1.01–1.02x，10× 探针显示真实均值 1.006x）。已用专项探针补足，未修改脚本。
5. **CASES 数量**：计划文档写「8 工作负载」，脚本实际 7 条；按 7 条逐项比，未因数量差异误判。
6. **WSL 二进制含 debug_info / not stripped**：会影响 VmRSS 绝对值（不影响比值方向结论）；与历史构建方式可能不同，故内存绝对值只作参考。
7. **跨轮最小混用 best-of-5 与 best-of-9**：R4 为加深轮（`ROUNDS=9`），表中已单独标注；按「取跨轮最小比值」口径一并纳入，同时用 0.70–0.83x 区间表述（§4.8）。
8. **跨源码版本对照**：11-07 基线用 `0.0.1` 源码 release，当前为 `3e7680f`；「与基线对照」不是同一份代码的 A/B，差值判定依赖 §5 的同窗口 A/B 归因（§0.3）。
9. **覆盖限制**：WSL 侧旧 release 重建时已删，A/B 仅 MSYS2 可做；§0.2 的退化幅度不外推到 WSL（§5.7）。
10. **并发污染（一）—— `ab-old-vs-new.log` 作废**：19:52 的 smoke 与主理人首轮 A/B 时间重叠数秒，且 QA 的 A/B 首跑与主理人 `lead-ab.sh` **两个进程并发写同一路径** `ab-old-vs-new.log`，导致该文件混入两份基准输出与两套头尾标记（表格、日志头碎片、`END bench.sh` 与 `END A/B` 交错），**已整体作废、不作任何数据**；该轮前两项 `arith 1.24x` / `builtin-echo 1.29x` 亦不据以下结论。根因经 `lead-ab.sh` 第 9 行 `LOG="$D/ab-old-vs-new.log"` 与时间戳（主理人 19:54:39 写完 END、QA 19:53:47–19:56:34）确认。
11. **并发污染（二）—— 20:08 沉降窗口不独占**：20:08 QA 的 `settle-ab-probe.sh` 与主理人独立探针**并行、同写 `settle-ab-probe.log`**，主理人该轮输出被覆盖未保留。故 **20:08 窗口不得写成「完全独占窗口」**；其数据仅作交叉验证之一，**最终无并行判据以 20:12 主理人第三窗口（`lead-settle-probe.log`，loadavg 全程 `0.00`、无并行）为准**（§5.5）。
12. **smoke 属事后补做，存在时序缺口**：两侧 release 与旧副本的 smoke test 在全部基准轮次结束之后（19:52）才补做，**不是轮次前置**；R1–R4 各轮日志 `rc=0` + END 时间戳 + 负载耗时明显分化（如 R1 100/2746/2078ms）只能作间接佐证。`--version` 成功仅证明进程能启动，不证明 `-c` 的解析/arith/循环路径可用，故此缺口如实登记，不伪装成前置。
13. **本次仅性能/启动/内存对比，未跑语义回归闸门**（GROUP A/C/D、CLI、echo_printf 等），也未做逐字节兼容性验证；性能结论不能替代语义正确性结论。
14. **流程缺口三项（如实披露，不掩饰也不夸大）**：① 报告 `docs/perf-compare-2026-11-08.md` 在主理人「A/B 日志存在前不要创建」的指令送达前已提前创建（19:47），早期草稿缺 A/B 归因段且 §9.2 曾写「旧副本本次未使用」的过时表述，现已按最终裁决修订；② 任务 #4 曾被误标 completed 后改回 in_progress；③ 见第 12 条 smoke 时序缺口。
15. **未改 `src/`、未 commit、未 push**；仓库除本报告外无改动（`git status --porcelain` 仅新增本文件）。

---

## 9. 附录

### 9.1 关键命令

```bash
# 核验（LC_ALL=C LANG=C）
cd <WASH_REPO> && git status --porcelain && git log -1 --oneline

# MSYS2 release 重建（严禁裸 cargo / ucrt64 / mingw64）
MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
  'cd <WASH_REPO> && CARGO_NET_OFFLINE=true cargo build --release --offline'

# MSYS2 每轮（R1/R2/R3）
MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
  'ROUNDS=5 bash /d/msys2/tmp/wash-perf-2026-11-08/msys2-bench.sh R1'

# WSL 每轮（R1/R2/R3 ROUNDS=5；R4 ROUNDS=9）
wsl.exe -e bash -lc "env -u BASH_ENV -u WSLENV ROUNDS=5 bash /mnt/d/msys2/tmp/wash-perf-2026-11-08/wsl-bench.sh R1"

# WSL 专项探针
wsl.exe -e bash -lc "env -u BASH_ENV -u WSLENV bash /mnt/d/msys2/tmp/wash-perf-2026-11-08/wsl-rss-probe.sh"
wsl.exe -e bash -lc "env -u BASH_ENV -u WSLENV bash /mnt/d/msys2/tmp/wash-perf-2026-11-08/wsl-cmdsub-probe.sh"

# 前置证据：两侧 release smoke 与旧副本验证
MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
  "bash /d/msys2/tmp/wash-perf-2026-11-08/smoke-run.sh <WASH_REPO>/target/release/wash.exe /d/msys2/tmp/wash-perf-2026-11-08/smoke-msys2.log MSYS2 '<note>'"
wsl.exe -e bash -lc "env -u BASH_ENV -u WSLENV bash /mnt/d/msys2/tmp/wash-perf-2026-11-08/smoke-run.sh /var/tmp/wash-build/target/release/wash /mnt/d/msys2/tmp/wash-perf-2026-11-08/smoke-wsl.log WSL '<note>'"
MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc "bash /d/msys2/tmp/wash-perf-2026-11-08/old-verify.sh"

# 旧/新 release 同窗口 A/B（唯一干净轮，raw 独立落盘再拼接）
MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc \
  "cd <WASH_REPO> && ROUNDS=5 BASH_BIN=/d/msys2/tmp/wash-perf-2026-11-08/wash-0.0.1-release.exe bash tests/perf/bench.sh <WASH_REPO>/target/release/wash.exe"

# 四项沉降交错复测（ROUNDS=9，独立脚本，不改 bench.sh）
MSYSTEM=MSYS /d/msys2/usr/bin/bash.exe -lc "bash /d/msys2/tmp/wash-perf-2026-11-08/settle-ab-probe.sh"
```

包装/探针脚本（均在仓库外临时目录，含 `[ -x ]` 硬核验与指纹记录）：`msys2-bench.sh`、`wsl-bench.sh`、`wsl-build.sh`、`wsl-rss-probe.sh`、`wsl-cmdsub-probe.sh`、`smoke-run.sh`、`old-verify.sh`、`settle-ab-probe.sh`；主理人侧 `lead-prep.sh`、`lead-ab.sh`。

### 9.2 原始日志清单（含前置证据，双份落盘）

- **Windows 侧 `/d/msys2/tmp/wash-perf-2026-11-08/`**：
  - 构建：`build-msys2.log`、`wsl-build.log`
  - MSYS2 三轮：`msys2-R1.log`、`msys2-R2.log`、`msys2-R3.log`（含各轮 driver 日志）
  - WSL 四轮：`wsl-R1.log`、`wsl-R2.log`、`wsl-R3.log`、`wsl-R4.log`
  - 专项探针：`wsl-rss-probe.log`、`wsl-cmdsub-probe.log`
  - **前置证据**：`smoke-msys2.log`(658B, 19:52:28)、`smoke-wsl.log`(644B, 19:52:51)、`old-release-verify.log`(837B, 19:52:53)；主理人侧 `old-release-version.log`、`smoke-old-release.log`、`smoke-msys2.err`、`smoke-old-release.err`（后两者 0 字节）
  - **A/B 与沉降复测**：`ab-old-vs-new.log`(1708B, **并发污染已作废**)、`ab-qa-rerun.log`(2551B, 45 行)、`ab-qa-rerun-raw.log`(898B, 19 行)、`settle-ab-probe.log`(78 行)、`settle-ab-probe.sh`；主理人第三窗口 `lead-settle-probe.log`(48 行, loadavg 0.00)
  - **旧 release 副本 `wash-0.0.1-release.exe`**：来源 `target/release/deps/wash-2219c1d80d1f9544.exe`（mtime 2026-11-07 17:42:31.636708900、size 2203901、link count 1），由主理人 `cp -p` 留存，sha256 `9527ce0153bd814b29028e299aa1213f461100ddf01e19b895dd82f8d5e63ebc`。**实际用途**：§5 A/B 与两轮沉降复测的 OLD 侧（作 `BASH_BIN`），是「窗口效应 vs 源码演进」因果判定的核心判据；先经 `--version=wash 0.0.1`（rc=0）、`-c` smoke（stdout 恰两行、rc=0、stderr 空）与 7 个负载 rc 全 0 的 PRECHECK 三重硬核验。**早期草稿中「非本次任务产出、本次未使用、未改动」的表述已过时，此处更正为上述实际来源与用途。**
- **WSL 侧 `/var/tmp/wash-perf-logs/`**：`wsl-build.log`、`wsl-R1.log`–`wsl-R4.log`、`wsl-rss-probe.log`、`wsl-cmdsub-probe.log`，以及 `ab-qa-rerun.log`、`ab-qa-rerun-raw.log`、`old-release-verify.log`、`smoke-msys2.log`、`smoke-wsl.log`、`settle-ab-probe.log` 的镜像（共 12+ 个文件，双份落盘，支持断点续跑）。

### 9.3 WSL 实例状态

Ubuntu-22.04 由本次任务启动（基准期间 `wsl.exe --list --verbose` 显示 `Ubuntu-22.04 Running / VERSION 2`）。**报告完成时实测** `wsl.exe --list --running` 输出「**没有正在运行的分发**」—— 实例已被 WSL 自动空闲回收；但紧接着 `wsl.exe -e bash -lc` 仍成功拉起并读到 `/var/tmp/wash-build/target/release/wash`（mtime 19:20:55、`wash 0.2.0`），证明 `/var/tmp` 下的构建产物与日志未随实例回收丢失、可按需重启实例复核。按纪律**全程未执行 `wsl --shutdown`**，由主理人统一收尾。

---

## 10. 追加复核（22:00–22:40）：「能否优化到 <1x」与 A/B 测量伪影

> 由「能优化到 <1x 吗？」的追问触发，独立复核 §5 / §7 的 P2 结论。
> **§0–§9 原文一律不改写**：本节只登记新增证据与待裁决修正建议，原结论保留可追溯。

### 10.1 先分清「<1x」的两层口径

1. **相对 GNU bash**：§0.1 已全部 <1x（MSYS2 TOTAL 0.77x / startup 0.59x；WSL 0.70–0.83x），但 §5 明确其差值主因是测量窗口，**不得写成「wash 变强」**。
2. **相对 11-07 旧 wash（A/B）**：§0.2 / §5 报 startup 1.51–1.57x、纯解释器 1.13–1.25x（>1x，判 P2）。本节针对这一组复核。

### 10.2 新发现的混淆变量：cargo 输出路径（硬链接）稳定偏慢 24–35%

- §9.1 的 A/B 命令（第 548 行）：OLD 侧 = `/d/msys2/tmp/.../wash-0.0.1-release.exe`（`cp -p` 独立文件，links=1）；NEW 侧 = `target/release/wash.exe`（cargo 输出，`stat` 实测 **links=2**，与 `target/release/deps/wash-*.exe` 共享 inode `6755399441732861`）。**两端文件性质不同，此前未登记为混淆变量。**
- 同一份二进制内容，在 cargo 硬链接路径执行 vs `cp` 成独立文件执行，三个独立窗口稳定差异：

  | 指标 | cargo 路径 / OLD | 同内容副本 / OLD |
  |---|---|---|
  | `--version`(50次) | 1.241x / 1.268x / 1.264x | 0.992x / 0.995x / 0.993x |
  | startup(50次) | 1.348x / 1.361x / 1.292x | 1.005x / 1.007x / 1.005x |

- 六路对照关键行（ROUNDS=3、顺序奇偶轮换）：`version old=1437 base=1428 opt=1784 old2=1430 base2=1518 opt2=1427`、`startup old=1437 base=1451 opt=1937 old2=1447 base2=1445 opt2=1444`。`opt2` 与 `opt` **内容完全相同**（同一文件 cp），比值却从 1.35x 回到 1.005x。
- 根因未最终定性（Windows 实时扫描 / Prefetch 路径缓存 / Cygwin 对 cargo 硬链接输出的处理均可能），但**偏差可复现、与源码无关、量级与 §5 的退化量级同阶**。

### 10.3 公平条件复测：P2 退化未复现

三个二进制全部 `cp` 成同目录、同方式、同一时刻创建的独立副本（`ab-old.exe` / `ab-base.exe` / `ab-opt.exe`，links 均为 1），三方交错、顺序奇偶轮换、ROUNDS=5 × 2 组：

- BASE(`3e7680f` 原样)/OLD：version 0.995–1.004x、startup 1.000–1.019x、arith 1.000–1.027x、echo 0.954–1.000x、expand 1.014–1.029x。
- 官方 `bench.sh` ROUNDS=5（`BASH_BIN=ab-old.exe`，`$1=ab-base.exe`）：七项 0.95–1.02x、**TOTAL 0.99x、startup 1.00x、VmRSS 1.00x**。
- 与 §5「三个窗口四项全部 >1.10x」直接矛盾 → **§5 的 P2「确定性退化」在剔除路径混淆后未复现，因果判定需复核**。建议把 P2 降级为「测量方法学待复核项」并保留原数据，不删改、不掩盖。本节不擅自改写 §5 / §7 结论，待裁决。

### 10.4 实际做的优化（语义不变，17 行 diff，未提交）

| 文件 | 改动 |
|---|---|
| `Cargo.toml` | `[profile.release]` 由 `lto = false` 改为 `lto = true` + `codegen-units = 1` + `strip = true` |
| `src/i18n.rs` | `init()` 去掉 `catalog()` 预解析，改由 `translate()` 首次调用懒加载 |
| `src/shell.rs` | `BASH_VERSION` 由运行时 `format!` 改编译期 `concat!` |
| `src/builtins.rs` | `run_builtin` 动态内建查表加「空注册表直接跳过」快速路径 |

产物：文件 2,280,977 → 1,066,496 B（**−53%**），`size` text 1,683,269 → 1,039,325（**−38%**），data/bss 基本不变，DLL 依赖三项不变；`wash 0.2.0`；六项 `-c` 冒烟（解析 / 算术 / 展开 / 命令替换 / 管道 / 子 shell）全 rc=0。

### 10.5 优化后的比值与「<1x」裁决

- 三方副本探针 `OPT/OLD` 跨组最小：version **0.987x**、startup **0.995x**、arith **0.974x**、echo **0.938x**、expand **0.971x**；`OPT/BASE` 0.944–1.000x。
- 官方 `bench.sh` ROUNDS=5（`$1=ab-opt.exe`）：七项 0.95–1.01x、**TOTAL 0.99x、startup 1.01x、VmRSS 0.99x**。
- **裁决：不能宣称「确定性 <1x」**。跨轮最小口径下五项 ≤1.00x，但官方 best-of-5 口径 TOTAL 0.99x / startup 1.01x，差值 1–6% 落在本机噪声量级（§8 第 1 条）。编译层面收益已基本吃满；要得到可门禁的稳定 <1x，需算法级改造，且 MSYS2 启动约 94% 是 Cygwin 进程创建 / DLL 地板，wash 源码可压缩空间有限。**公平条件下本来就是 ≈1.00x，不存在一个「从 1.5x 压到 <1x」的真实缺口。**

### 10.6 语义回归（逐字节对拍）

- **纪律修正**：语义套件默认走 `target/debug/wash.exe`，而上一轮报告 §8 第 13 条登记「未跑语义闸门」、本轮初次回归也误测了未重建的 debug。本轮先 `cargo build` 重建 debug（9.52s，含新源码），**并额外**用 `WASH_BIN` 指向优化后 release 副本再跑一遍全量闸门。
- 新源码 debug：GROUP A 340/340、K 3/3、B 0、C 193/193、D 29/29 → **565/565**；echo/printf **100/100**；CLI **22/22**；`check_version` 全项 ok；`plugin_contract` **35/35**；`plugin_second_oracle` **64/64**（WSL oracle rc=0）。
- 优化后 release 副本：全量闸门 **565/565**（rc=0）。
- `bash_diff_i18n.sh` 轴 2 `i18n-help-structure` 仍 FAIL（help 内建清单未对齐 bash）→ 已用**改动前** `wash-baseline-3e7680f.exe` 复跑，FAIL 完全相同，属既有状态（脚本 NOTE 亦注明为内建实现进度问题），与本次改动无关；轴 1 本地化 PASS。
- 收尾：`wsl.exe --shutdown`（rc=0）。

### 10.7 新增测量纪律（建议写入长期约定）

1. A/B 两端必须**同目录、同方式创建独立副本**，`stat -c '%h'` 确认 links=1；禁止把 cargo 输出路径（links=2 硬链接）与复制副本直接对比。
2. 纵向 A/B 开跑前先做**路径对称性校验**：同一份二进制放到两条待测路径，空负载比值应 ≈1.00x，否则该窗口数据作废。
3. 改了源码必须重建 debug（语义套件默认路径）；性能产物是 release 时，须额外用 `WASH_BIN` 指向 release 副本复跑闸门。

### 10.8 版本控制状态

- **复核/优化阶段（§10.1–§10.7 记录的是当时状态）**：`HEAD=3e7680f`、tags `v0.0.1 / v0.2.0 / v0.2.1` 未动，4 个优化文件未提交、未 push。
- **2026-11-09 收尾**：4 个优化文件以 **`e3ba08d`** `perf(release): enable LTO/strip and trim startup hot paths` 提交（author `wbw121124`，`commit-msg` 钩子自动追加 `Co-authored-by: workbuddy`），**未 push**；本报告单独提交入库，不与源码混在同一 commit；`target/release/` 下留存 `wash-old-0.0.1.exe`、`wash-baseline-3e7680f.exe` 两个对照副本（被 gitignore）。

---

*报告生成/修订：2026-11-08｜全部数据来自实测原始日志，未 skip、未 defer、未伪造、未补数；并发作废数据明确标注、未用于结论。*
