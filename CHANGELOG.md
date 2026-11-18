# Changelog

All notable changes to this project are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed（有意偏离，用户 2026-11-10 裁决）

- **新增单一「有意偏离清单」** `docs/intentional-divergences.md`，登记 wash 相对
  MSYS2 bash 5.3.20 oracle 的全部 **2 条**有意逐字节例外：既有的 `help -m`
  IMPLEMENTATION footer（D-001，wash 身份段）与本次新增的 `help job_spec` 可查
  （D-002）。任何新增 / 删除偏离必须同步该清单、代码真源、生成脚本静态锁、
  `tests/bash_diff_help.sh` 的固定用例计数与下方登记行。
- **`help job_spec` / `help 'job_spec [&]'` 现在可查（D-002）**：用户明确要求
  「无参清单第 2 项列出却查不到」这一 UX 痛点必须修好，**全局生效（含非交互
  `-c`）**，明确允许为此破坏脚本兼容，并否决了「仅交互模式」方案。这是本轮
  **唯一新增**的有意逐字节兼容例外。仅当选项解析后的 pattern 列表恰好为 1 个、
  且字符串精确等于两个登记值时，wash 返回 `%` 页（rc=0、stderr 空、Full / Short /
  Desc / Man 四种模式与 C / zh_CN 两个 locale 的输出逐字节等于 wash 自己的
  `help '%'`）；oracle 侧仍 rc=1。实现走 `src/help_pages.rs::JOB_SPEC_DIVERGENCES`
  + `divergence_page()` 的**单-pattern 精确通道**，不进通用 `ALIASES` /
  `lookup()`，因此 glob / 多 pattern / 空 pattern / 别名等一切其它路径仍逐字节一致。
- **闸门只收紧不放宽**：`tests/bash_diff_help.sh` 把旧「wash 必须 rc=1」的 6 条
  用例改成 `tests/help_divergence.py` 的双侧精确偏离断言（oracle rc + stdout +
  stderr GOLDEN 含中文 `第 1 行：` 前缀、wash rc=0 + stderr 空 + stdout 逐字节
  等于 wash `help '%'`、GOLDEN sha256 自检、canonical 链式证明），新增 14×2 条
  「偏离不扩散」严格对拍，并硬断言偏离用例数 26 / 严格用例数 457 / 清单恰好 2 条。
  `tools/gen_help_pages.py --check` 的 4 条旧静态锁换成「oracle `help job_spec`
  仍 rc=1」+「wash 2×2×4 组合精确偏离」+「`ALIASES`=3 且偏离表恰好 2 项」，
  原有全部结构断言保留。

## [0.2.1] - 2026-11-09

### Added

- **「比 bash 更好」回归门禁** `tests/perf/better_than_bash.sh`：把产品目标
  「性能更高 / 速度更快 / 内存占用更小」固化为三项可复测的硬判定
  （相对 MSYS2 bash 5.3：TOTAL ≤ 1.00x、startup ≤ 0.60x、VmRSS ≤ 0.80x）。
  两端同目录 `cp` 成 `links=1` 独立副本、路径对称性校验、3 条负载的
  rc + stdout sha256 双侧逐字节一致与输出行数硬闸、版本串与生成值一致硬闸、
  ROUNDS 轮交错取最小；工具/二进制/输出缺失、对称性越界一律 FATAL，不 skip。
  2026-11-09 两个独立窗口基线：TOTAL **0.81x / 0.81x**、startup
  **0.42x / 0.38x**、VmRSS **0.65x / 0.65x**，7 项负载全部 < 1.00x。
  UX 维度（语法高亮 / ghost 建议 / Tab 补全 / 多行编辑）由 `tests/e2e/` 覆盖；
  非交互 `-c` 的逐字节兼容闸门不放宽。
- **`[[ ... ]]` 条件命令已实现并与 bash 5.3 逐字节对齐**：新增 `src/cond.rs`
  （递归下降解析 + 求值），词法层新增 `COND_START`/`COND_END`/`COND_OP`/
  `COND_NUMBER`/`COND_NEWLINE` 与 `PST_REGEXP` 状态（`=~` 右侧的 `|`/`(` 按
  bash `parse_matched_pair` 规则处理），执行器把 `=~` 捕获组写入 `BASH_REMATCH`。
  覆盖单目/双目/文件/数值/字符串/模式/ERE 运算、`!`/`&&`/`||`/分组、`[[ ]]`
  内不做词分割，以及畸形表达式**两行诊断**（含 yacc `syntax error near` 行与
  未闭合 `(` 的词法 EOF 行）。`tests/bash_diff_builtins.sh` GROUP A 新增 44 条
  逐字节栅栏（rc + stdout + 仅剥 shell 名前缀的 stderr），A 组 590 → 634。

### Added

- **`declare -A` 关联数组与数组下标赋值**：此前 `a[0]=x` 被误判为命令
  （`command not found`），`declare -A` 静默吞掉 `-A` 标志。新增独立的
  `Shell::assoc_arrays`（FNV-1 哈希、16 桶、桶内头插，复刻 bash `${!m[@]}` 的
  遍历顺序）与 `array_set_indices`（稀疏数组计数）；lexer/parser 让
  `name[subscript]=value`（含 `m[$k]=v`）识别为赋值词；executor 对关联数组用
  字符串键、对索引数组用算术下标。支持 `declare -A m=([k]=v)` 复合字面量与
  `declare -p m` 的 `([k]="v" )` 格式。已知窄边界：多字符键哈希冲突时 bash
  内部 rehash 时机导致顺序与朴素 FNV 桶序不同，如实登记不假 PASS。
- **`getopts` 内建**：实现 OPTIND/OPTARG、前导冒号静默模式、非法选项与缺参数
  诊断、`--`/非选项终止、bundled 选项（`-ab`）跨调用续扫，与 bash 5.3 逐字节对齐。
- **`trap -l` 信号列表**：复用现有 64 信号表打印器，支持 `-l`/`-lp`/`-l -p`，
  与 bash 逐字节一致。
- **`(( ))` 算术命令**：新增 `Token::ArithCmd` 与 `Compound::Arith`，复用现有
  算术求值器；退出码按表达式结果 0/非 0 为 1/0，赋值写回 shell 变量。
  已知边界：C 风格 `for ((i=0;i<3;i++))` 循环属独立语法，尚未支持。

### Fixed

- **review P1-3：`largest_parseable_prefix` 的 O(n²) 语法错恢复**：2000 行脚本
  尾部语法错从 ~39s 降到 ~0.23s（bash 0.25s）。整串已知解析失败，改为从末行
  往前找首个可解析前缀即停，语义与原来的前向扫描完全等价。
- **review P1-5：命令替换不继承位置参数**：`capture_command` 新增 `positional` /
  `dollar_zero` 参数并由 `ExpandCtx` 两处调用点透传；`set -- aa bb; echo "$(echo $1)"`
  从空变为 `aa`，`set -u` 下也不再误报 unbound。新增 2 条 GROUP A 栅栏。
- **`[[ ]]` 畸形诊断的 quoted token 渲染**：此前 `WordPart` 只记录「是否 quoted」，
  丢失引号类型与原始源码文本，导致 `[[ a "=" a ]]` 输出 `unexpected token \`='`
  而不是 bash 的 `` `"="'``。词法层为 cond 词新增 `Token::CondWord(parts, raw)`，
  随词携带 `chars[word_start..i]` 原始文本；conditional 消息用 raw `yytext`（`$'...'`
  去掉 `$` 前缀），yacc `syntax error near` 行取最后一个空白之后的尾部；被转义的
  运算符（`\=`）不再被当成运算符（rc=2 对齐）。新增 5 条 GROUP A 栅栏，A 组
  634 → 639，全量 880 → 885。
- **Cargo 版本号补齐并与 git tag 对齐**：`Cargo.toml` / `Cargo.lock` 升到 **0.2.1**
  （此前 `v0.2.1` tag 已打，清单却仍写 0.2.0，所有版本行因此输出旧版本）；
  `po/en.po` / `po/zh_CN.po` 的 `Project-Id-Version` 由生成脚本同步。
- **版本行改为生成式**：新增 `tools/wash_version.sh`，从 `Cargo.toml` 的 `version`
  与构建时 git 元数据生成版本串 —— HEAD 有精确 tag 时输出 `<version>`（且 tag 必须与
  清单一致），无 tag 时输出 `<version>-<HEAD hash 前 7 位>`。`build.rs` 把结果注入
  `WASH_VERSION`；`--version`、`$BASH_VERSION`、`help` 抬头、`help -m` 的
  IMPLEMENTATION 段、PS1 `\v` 全部改用 `env!("WASH_VERSION")`。清单落后于最近 tag、
  tag 与清单不一致、git 元数据缺失一律硬失败，绝不静默回退。
- **`help` 清单显示值与可查规范名合并为单一真源并加三层闸门**：`src/help_pages.rs` 新增 77 项 `HELP_LIST_MAP`（显示值 ↔ 规范名），`builtins.rs` 删掉独立的 `HELP_TOPICS`；首次访问 `all()` 时硬断言 77↔77 长度/顺序/集合全等且两列无重复，不符直接 panic；`tools/gen_help_pages.py --check` 新增 oracle 实测的规范名/数据标记/逐页 stdout/rc/静态 `ALIASES` 断言，`tests/bash_diff_help.sh` 新增 77×2 条清单显示值对拍与 `job_spec`/`%` 孤例用例。所有 `help` 输出字节不变；bash 自身 `help job_spec` 就是 rc=1，**禁止为它补别名**。
- **消除版本行的假 PASS / skip**：`tests/bash_diff_help.sh` 的无参清单不再用
  `help | tail -n +2` 跳过版本行，改由 `tests/help_banner.py` 把 oracle 首行替换为
  生成值后做**完整逐字节对拍**（含可逆性校验）；`tests/help_footer.py`、
  `tests/plugin_contract.sh`、`tests/plugin_second_oracle.sh` 中硬编码的 0.2.0
  golden/硬闸全部改为生成值；`tests/check_version.sh` 重写为静态 + 运行时严格闸门，
  禁止任何 `CARGO_PKG_VERSION` 快捷路径或硬编码版本字面量。

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
