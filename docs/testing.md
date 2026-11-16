# 测试与验证

wash 的核心验证方法是 **oracle 逐字节对拍**：同一条命令分别跑 MSYS2 bash 5.3 与
wash，比较 rc + 原始 stdout 字节 + stderr 字节（stderr 只归一化 shell 名前缀）。
`LC_ALL=C` 是强制的——MSYS2 bash 默认中文 locale，不强制英文就无法比较诊断文案。

主 oracle = MSYS2 bash 5.3.20 `/d/msys2/usr/bin/bash.exe`（唯一权威）。
第二 oracle = WSL2 bash 5.3.0，仅交叉校验，用后 `wsl --shutdown`。
与二进制身份绑定的量（`BASH_VERSION` / `EUID` / `PPID` / 自身路径）不冒充 bash，
用例改断言语义不变量（GROUP K）。

## 测试套件

| 脚本 | 覆盖 |
| ---- | ---- |
| `tests/oracle.sh` | Phase 0：外部命令 / 管道 / 重定向 |
| `tests/oracle_phase1.sh` | Phase 1：展开系统 |
| `tests/bash_diff_builtins.sh` | 主套件，全部内建的逐字节对拍（GROUP A/K/B/C/D） |
| `tests/bash_diff_echo_printf.sh` | echo / printf 全转义与转换说明符 |
| `tests/bash_diff_cli.sh` | CLI 参数形态 |
| `tests/bash_diff_help.sh` | help：C / zh_CN 双 locale、77 主题、清单、glob、错误路径 + D-002 偏离 |
| `tests/bash_diff_i18n.sh` | 本地化维度 |
| `tests/plugin_contract.sh` | 二进制插件 ABI 契约（MSYS2 / WSL 各跑） |
| `tests/plugin_second_oracle.sh` | WSL 第二 oracle 能力等价对拍 |
| `tests/check_version.sh` | 版本串生成与一致性（静态 + 运行时） |
| `tests/e2e/` | 交互式 REPL 的 pty 套件 |
| `tests/perf/better_than_bash.sh` | 性能 / 启动 / 内存门禁（"比 bash 更好"） |
| `tools/gen_help_pages.py` | 从 oracle 重生成 help 页数据 + po 清单译文（`--check` 校验新鲜度） |
| `tools/wash_version.sh` | 版本串唯一生成规则；`--check-po` / `--sync-po` |

主套件按 GROUP 隔离运行：`ONLY_GROUP=A|K|B|C|D`。改用例须同步脚本头部计数与
`run_case A` / `run_case_sem A` 三方自洽。全量回归约 7 分钟，后台跑。
语义套件默认跑 `target/debug/wash.exe`——**改源码必须重建 debug**；release 另用
`WASH_BIN=<release 副本>` 再跑一遍。

## 运行示例

```bash
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd <repo> && bash tests/bash_diff_builtins.sh'
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd <repo> && bash tests/check_version.sh'
MSYSTEM=MSYS "/d/msys2/usr/bin/bash.exe" -lc 'cd <repo> && bash tests/perf/better_than_bash.sh target/release/wash.exe'
```

## 测量纪律（性能门禁）

- 两端必须用同目录 `cp` 出的 `links=1` 副本（cargo 输出的二进制是 links=2 硬链接，
  在该路径执行稳定慢 24–35%）；`stat -c '%h'` 确认。
- 跨睡眠/挂起、并行重负载、对称性越界的窗口一律作废。不许用 1–3% 噪声读数宣称优化。
- 启动前置硬闸：`--version` 必须 rc=0 且 stdout 非空。
- `python -O` 是解释器开关，必须在脚本名之前（`python3 -O script.py --check`）。
- debug 二进制 sha 不可重复（PE/PDB 非确定性元数据）→ 禁止用二进制 sha 当代码身份凭据；
  用源码 sha 清单，二进制只在"开跑前 vs 跑完后"自比相同。

详细历史窗口与实测报告见 `docs/perf-compare-2026-11-08.md`、`docs/perf-plan-2026-11-07.md`、
`docs/perf-wsl-2026-11-07.md`。
