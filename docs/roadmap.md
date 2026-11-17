# 路线图与产品目标

wash 分阶段推进。命令/语法层面以 GNU bash 5.3 逐字节兼容为硬闸门，交互体验借鉴 fish。
**兼容性闸门任何判定都不放宽**——"比 bash 更好"不能以牺牲兼容为代价。

## 阶段状态

| 阶段 | 内容 | 状态 |
| ---- | ---- | ---- |
| 0 | 引导：外部命令、管道、重定向、POSIX FFI 垫片、MSYS2 工具链 | 已完成（oracle 9/9） |
| 1 | 展开系统：变量、花括号（含 range）、命令替换、算术、波浪号、glob、分词、参数运算 | 已完成（oracle 20/20） |
| 2 | 作业控制：`&` / `jobs` / `fg` / `bg` / `wait` / 信号 | 待开始 |
| 3 | 控制流与函数：`if` / `for` / `while` / `until` / `case`、函数 | 基本完成（`select` 未实现） |
| 4 | 高级内建与特性：数组、`[[ ]]`（已完成，44 条逐字节栅栏）、`(( ))` 算术命令、`declare`、`mapfile`、`trap` 等 | 进行中/部分，见 [兼容性矩阵](compatibility.md) |
| 5 | 长尾 100% 兼容：边缘语义、locale、shopt 等 | 进行中/部分 |
| 6 | 交互式 REPL（fish 风格） | 已完成，见 [架构](architecture.md) |
| 7 | 二进制插件系统（wash C ABI）与 0.2.0 发布 | 已完成，见 [插件](plugins.md) |

## 产品目标：比 bash 更好

在非交互 `-c` 语义 100% 逐字节兼容之外，wash 的一等目标是在三个维度上超过 GNU bash 5.3：

| 维度 | 现状（两个独立窗口，MSYS2 公平 A/B） | 守护 |
| ---- | ---- | ---- |
| 性能更高 / 速度更快 | 7 项负载 0.58–0.98x（全部更快），TOTAL 0.81x；启动 50 次 0.38–0.42x | `tests/perf/better_than_bash.sh` |
| 内存占用更小 | 阻塞进程 VmRSS 0.65x（约 5.3 MB vs bash 8.1 MB） | 同上 |
| UX 更友好 | 语法高亮 / 历史 ghost 建议 / Tab 补全 / 多行编辑 | `tests/e2e/` pty 套件 |

门禁用两端同目录独立副本（`links=1`）、路径对称性校验、3 条负载的
rc + stdout sha256 双侧逐字节一致与输出行数硬闸、版本串与生成值一致硬闸、
ROUNDS 轮交错取最小；阈值 TOTAL ≤ 1.00x / startup ≤ 0.60x / VmRSS ≤ 0.80x。
工具缺失、二进制缺失、版本串不匹配、对称性越界、负载没真跑起来一律 **FATAL**，不静默跳过。

编译层优化已基本吃满：MSYS2 启动约 94% 是 Cygwin 进程/DLL 地板。算法级组合改造
（`dash_flags()` 去重、无通配符跳过 glob、展开期分配、启动期建表）实测收益落在
1–3% 噪声带，结论是"无显著收益"——不许用噪声读数宣称优化。测量纪律与历史窗口见
`docs/perf-compare-2026-11-08.md` §10。

## 已知待修（实测复现）

详见 `.workbuddy/review.md`（HEAD `1f22a37` 全仓审查）与项目日志。已修复：arith 三元
运算符、arith 深递归护栏（bash 自身在 MSYS2 上该深度即 SIGSEGV，wash 的目标是确定性
报错而非复刻崩溃）、parser 复合命令深度、REPL `cursor_pos` 字节/字符混用、round 7
复合命令未闭合诊断（`if true; then` 等六关键字 + `>;` near-token + 家族优先级）。
剩余项见 review.md 的 P1/P2 清单。

内建级未实现特性（功能缺失，非覆盖空洞）见 [兼容性矩阵](compatibility.md)。
