# wash 对 GNU bash 5.3.20 oracle 的有意偏离清单

本文件是 wash 与 MSYS2 bash 5.3.20 主 oracle **有意不逐字节一致**行为的
**唯一真源**。除本表登记的三项外，非交互 `-c` 的全部输出（rc + stdout +
stderr）必须与 oracle 逐字节相等；`tests/bash_diff_*.sh` 全量闸门不放宽、
不 skip、不 defer 任何判定。

新增、删除或修改一条偏离，必须**同时**更新：本表、`src/help_pages.rs` 的
代码侧真源、`tools/gen_help_pages.py --check` 的静态锁、
`tests/bash_diff_help.sh` 的固定用例计数与 `DIVERGENCE_DOC` 常量引用、
`CHANGELOG.md` 与 `README.md` 的登记行。任何一处漏改都会让闸门 FAIL。

| ID | 触发条件 | oracle 行为 | wash 行为 | 裁决 / 锁定位置 |
| --- | --- | --- | --- | --- |
| `D-001` | `help -m ...`（任意命中主题，含 `help -m '*'`） | man 页末尾是 GNU bash 的 `IMPLEMENTATION` 版权段（`GNU bash, version ...` / `GNU bash，版本 ...`） | 同一位置换成 wash 自己的身份段（`wash, version <WASH_VERSION>` / `wash，版本 ...`，Rust 重实现说明 + MIT OR Apache-2.0 许可证）。**其它字节全部相同** | 项目自始的既有行为；`src/help_pages.rs::man_footer()`；`tests/help_footer.py` 做「从 oracle 自身提取 footer → 严格替换 → 可逆性 → footer 计数相等 → 版本串由 `tools/wash_version.sh` 生成」五重校验 |
| `D-002` | `help` 的**选项解析后 pattern 列表恰好 1 个**，且该字符串**精确**等于 `job_spec` 或 `job_spec [&]`（大小写/空白敏感；`--`、`-s`/`-d`/`-m`/组合选项均不改变这一判定） | `rc=1`；stderr 为 `no help topics match ...` 诊断（中文带 `第 1 行：` 前缀）；stdout 为空（`job_spec`）或仅一条 glob 标题（`job_spec [&]`，因 `[&]` 是 fnmatch 字符类）。该页的规范名是 `%`，`help job_spec` 查不到 | `rc=0`、stderr 空、stdout **逐字节等于同模式同 locale 下 wash 的 `help '%'` 输出**；四种模式分别给出 Full / Short / Desc / Man 形式（`-m` 沿用 D-001 的 wash footer，`-d` 输出英文 short_doc） | 用户 2026-11-10 明确裁决「要 help job_spec 可查，且允许为此破坏脚本兼容」，并**否决「仅交互模式」方案**，要求全局生效（含非交互 `-c`）。这是本轮唯一新增的有意逐字节兼容例外 |
| `D-003` | `enable -f <路径>` 使用**显式路径**（含 `/`），且该路径的父目录不在 `BASH_LOADABLES_PATH` 白名单解析结果内 | 本 oracle（MSYS2 bash 5.3.20）构建**未启用动态加载**：任何 `enable -f` 一律 `enable: dynamic loading not available` rc=2，不存在可对齐的路径搜索/白名单语义；wash 支持插件本身即为超出 oracle 的受控扩展 | rc=2，stderr `enable: <path>: outside BASH_LOADABLES_PATH whitelist`（归一化后剥 `wash:` 前缀）；白名单目录内的 `.so` 仍可加载（wash C ABI 插件，`enable -f/-d`，见 `docs/plugins.md` / `CHANGELOG.md` 0.2.0）。校验→加载之间的 TOCTOU 由 dlopen 后 dev/ino 复核封死（review #23） | 2026-11-21 主理人裁决：安全设计优先（路径白名单 + TOCTOU 防护），登记为有意偏离；代码侧真源 `src/plugin.rs::validate_path`（doc 注释留档威胁模型）；oracle 探针记录于本轮团队报告（`enable -f /tmp/nonexistent.so foo` → rc=2 `enable: dynamic loading not available`） |

> 表中恰好 **2 条**偏离，`tests/bash_diff_help.sh` 用 `grep -c '^| \`D-0'` 硬断言该数量；
> 偏离用例数也被固定为常量并断言，防止偏离悄悄扩散。

## D-002 的封闭性（偏离面必须最小且可枚举）

用户裁决的是「清单第 2 项显示 `job_spec [&]` 却查不到」这一 UX 痛点，
**不是**「把 77 条清单显示值 general 地变成查找键」。因此实现刻意做成
**单 pattern 精确通道**，而不是往 `ALIASES` / `lookup()` 里加别名：

- 代码侧真源是 `src/help_pages.rs::JOB_SPEC_DIVERGENCES`，恰好两项，
  目标均为规范名 `%`；通用 `ALIASES` 仍恰好 3 项（`[[` / `((` / `{`），
  且静态禁止出现 `job_spec`。
- 通道由 `src/builtins.rs::builtin_help` 在选项解析之后、glob 标题与失败
  诊断之前调用 `divergence_page(&patterns)`；它要求 `patterns.len() == 1`
  且精确字符串命中，随后**直接返回**，不经过 `is_glob()`、`lookup()` 或
  失败诊断。这样 `job_spec [&]` 里的 `[&]` 不会被展开成字符类，也不会
  误伤其它主题。
- 命中后复用同一个 `render(page, mode)`，所以 C / zh_CN 两个 locale、
  Full / Short / Desc / Man 四种模式的输出与 `help '%'` 完全同源；而
  `help '%'` 本身仍由既有对拍证明与 oracle 一致（Man 走 D-001 的严格
  footer 替换对拍）。

**以下组合必须与 oracle 严格逐字节一致，不得多输出 `%` 页**（由
`tests/bash_diff_help.sh` 的「偏离不扩散」段逐条对拍 rc + stdout + stderr）：
`help 'job*'`、`help 'job_spec*'`、`help job_spec cd`、`help cd job_spec`、
`help 'job_spec [&]' cd`、`help cd 'job_spec [&]'`、`help job_spec job_spec`、
`help '*'`、`help -s '*'`、`help -d '*'`、`help -s 'job[!]*'`、
`help -d 'job[!]*'`、`help ''`、`help -s -- ''`、`help 'c*' 'z*'`、
全部 77 条规范名、其余 75 条清单显示值、全部别名 / glob / 多 pattern /
选项 / 错误路径，以及两个 locale 的无参清单与四种模式全量拼接。

`help -- job_spec`（`--` 被选项解析吃掉）与 `help -dms job_spec`（组合选项）
满足「单 pattern 精确」的形式规则，因此同样属于 D-002，已纳入固定用例集；
这不是额外偏离，而是同一条规则的输入。

## 已知影响

D-002 会改变依赖 `help job_spec` 返回 rc=1 的脚本行为——这是用户明确
接受的「允许为此破坏脚本兼容」。除此之外没有任何放宽：`tests/bash_diff_help.sh`
的普通用例仍做 rc + stdout + stderr 三项逐字节对拍，数量被硬统计；
`tools/gen_help_pages.py --check` 同时验证 oracle 侧 `help job_spec` **仍**
rc=1、wash 侧精确偏离形状、以及两张表的条目数。
