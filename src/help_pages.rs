//! GNU bash 5.3 `help` topic pages, captured byte-for-byte from the MSYS2
//! bash 5.3.20 oracle (see `src/help_pages.txt` for `LC_ALL=C` and
//! `src/help_pages_zh_CN.txt` for `LC_ALL=zh_CN.UTF-8`).
//!
//! These records are **documentation**, not a list of capabilities wash has
//! implemented.  bash's own list documents reserved words, `job_spec`,
//! `variables`, and builtins wash has not ported yet; `help NAME` prints the
//! same manual page bash prints.  The no-argument `help` list is derived from
//! `HELP_LIST_MAP` below -- the single ordered table that binds each displayed
//! synopsis to the canonical lookup key of its page.  `builtins.rs` holds no
//! second copy of that data.
//!
//! Locale handling matches the oracle: all 77 canonical topic *names* stay
//! English in both catalogs (they are bash's index keys), while page bodies,
//! synopses and long descriptions are gettext-translated.  One exception is
//! `help -d`: bash uses the builtin's `short_doc`, which is **not** translated,
//! so `Mode::Desc` always renders the English catalog's description.
//!
//! Intentional divergences from the oracle are enumerated in
//! `docs/intentional-divergences.md` and registered by the test suites.  This
//! `help` subsystem owns two of them: D-001 (the `help -m` wash-identity
//! IMPLEMENTATION footer, see `man_footer`) and D-002 (the `job_spec`
//! single-pattern lookup, see `JOB_SPEC_DIVERGENCES`).  Every other `help`
//! path stays byte-for-byte identical to the oracle.

use std::collections::HashSet;
use std::sync::OnceLock;

const DATA_EN: &str = include_str!("help_pages.txt");
const DATA_ZH_CN: &str = include_str!("help_pages_zh_CN.txt");

/// Is the current locale a Chinese one?  `i18n::locale()` returns the
/// normalized name (`zh_CN`, `zh`, `C`, `en_US`, ...).
fn is_zh() -> bool {
    crate::i18n::locale().starts_with("zh")
}

/// The man-page footer wash prints for `help -m`.
///
/// Unlike the page bodies, this block is **not** copied from bash: it is the
/// one place where wash states its own identity instead of reproducing bash's
/// IMPLEMENTATION stanza.  Consequently `help -m` is the only `help` path that
/// is intentionally *not* byte-identical to the oracle; `tests/bash_diff_help.sh`
/// verifies it against the oracle output with only this block substituted, and
/// additionally proves the substitution is reversible (no other byte differs).
fn man_footer() -> String {
    let version = env!("WASH_VERSION");
    let head = "\nSEE ALSO\n    bash(1)\n\nIMPLEMENTATION\n";
    if is_zh() {
        format!(
            "{head}    wash，版本 {version}\n    \
             以 GNU bash 5.3 语义为目标的 Rust 重新实现。\n    \
             许可证：MIT OR Apache-2.0\n\n"
        )
    } else {
        format!(
            "{head}    wash, version {version}\n    \
             A Rust reimplementation targeting GNU bash 5.3 semantics.\n    \
             License: MIT OR Apache-2.0\n\n"
        )
    }
}

#[derive(Clone, Copy)]
pub struct Page {
    pub name: &'static str,
    pub usage: &'static str,
    pub desc: &'static str,
    /// `help -d` prints bash's untranslated `short_doc` (English) even under a
    /// Chinese locale, so every page also carries the English description.
    pub desc_en: &'static str,
    /// Full page, starting with `name: usage` and ending in a newline.
    pub body: &'static str,
    /// `body` without its first line (the DESCRIPTION text for `-m`).
    pub rest: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Full,
    Short,
    Desc,
    Man,
}

/// GNU bash 5.3 `help` 无参数清单的 77 个**显示值** ↔ 77 个**可查规范名**。
///
/// 这是整个 `help` 子系统的**单一真源**，两列都不允许在别处再抄一份：
///
///   * 第一列：`LC_ALL=C` 下清单的显示文本（含 bash 自己截断的 `...>`），
///     同时是 `builtins.rs` 里 `tr!` 的 gettext msgid。无参清单与 po 生成段
///     都由这一列派生，zh_CN 译文由 `tools/gen_help_pages.py` 从 oracle 抓取。
///   * 第二列：`help NAME` 真正用来查找的规范名，取自 oracle `help -s '*'`
///     的 77 个键，也就是 `src/help_pages.txt` / `_zh_CN.txt` 的
///     `@@@ <name> @@@`。数组顺序即 bash 内部清单顺序（列优先分半排版）。
///
/// **清单显示值 ≠ `help NAME` 的查找键。** 77 条显示值大多是带参数/省略号的
/// 短摘要，把它们原样当 NAME 查不到（oracle 实测 72/77 查不到）；`help NAME`
/// 只认第二列的规范名，外加 `ALIASES` 里的三个短拼写（`[[` / `((` / `{`）。
/// 换言之「清单列出了它」并不保证「能用显示值查到它」，这是 bash 5.3 的
/// 既定行为；除下面登记的唯一有意偏离外，wash 原样复现。
///
/// **有意偏离 D-002（用户 2026-11-10 裁决，见 `docs/intentional-divergences.md`）**：
/// 第 2 项 `job_spec [&]` 的规范名是 `%`（不是 `job_spec`），oracle bash 5.3
/// 自身 `help job_spec` 就是 rc=1。用户要求 wash 提供「列出即可查」的 UX
/// 增强，且**全局生效（含非交互 `-c`）**，明确允许为此破坏脚本兼容，并否决
/// 了「仅交互模式」方案。这是本次唯一新增的有意逐字节兼容例外。
///
/// 实现刻意**不**放进 `ALIASES` / `lookup()`：那条路径是每个 pattern 都过
/// 一遍的共享通道，注册别名会让 `help job_spec cd` 等多 pattern 组合静默多
/// 输出 `%` 页。改由 `JOB_SPEC_DIVERGENCES` + `divergence_page()` 提供
/// **单 pattern 精确匹配**通道：仅当选项解析后的 pattern 列表恰好为 1 个、
/// 且字符串精确等于表内登记值时命中；glob / 多 pattern / 空 pattern 一律
/// 不参与，`[&]` 也不会被当 fnmatch 字符类展开。
/// `tools/gen_help_pages.py --check` 与 `tests/bash_diff_help.sh` 双向锁死
/// 该表的条目数与逐字节偏离形状。
///
/// 77 ↔ 77 的一一对应（长度、顺序、集合全等）由 `all_en()` 首次初始化时的
/// `verify_list_binding` 硬断言保证：映射表第 i 项的规范名必须恰好等于
/// `all_en()[i].name`，且第一列与第二列都无重复。任何不符直接 panic，
/// **绝不静默回退**到「顺序恰好相同」的隐式对齐。
pub(crate) const HELP_LIST_MAP: &[(&str, &str)] = &[
    (r#"! PIPELINE"#, "!"),
    (r#"job_spec [&]"#, "%"),
    (r#"(( expression ))"#, "(( ... ))"),
    (r#". [-p path] filename [arguments]"#, "."),
    (r#":"#, ":"),
    (r#"[ arg... ]"#, "["),
    (r#"[[ expression ]]"#, "[[ ... ]]"),
    (r#"alias [-p] [name[=value] ... ]"#, "alias"),
    (r#"bg [job_spec ...]"#, "bg"),
    (r#"bind [-lpsvPSVX] [-m keymap] [-f file>"#, "bind"),
    (r#"break [n]"#, "break"),
    (r#"builtin [shell-builtin [arg ...]]"#, "builtin"),
    (r#"caller [expr]"#, "caller"),
    (r#"case WORD in [PATTERN [| PATTERN]...)>"#, "case"),
    (r#"cd [-L|[-P [-e]]] [-@] [dir]"#, "cd"),
    (r#"command [-pVv] command [arg ...]"#, "command"),
    (r#"compgen [-V varname] [-abcdefgjksuv] >"#, "compgen"),
    (r#"complete [-abcdefgjksuv] [-pr] [-DEI]>"#, "complete"),
    (r#"compopt [-o|+o option] [-DEI] [name .>"#, "compopt"),
    (r#"continue [n]"#, "continue"),
    (r#"coproc [NAME] command [redirections]"#, "coproc"),
    (r#"declare [-aAfFgiIlnrtux] [name[=value>"#, "declare"),
    (r#"dirs [-clpv] [+N] [-N]"#, "dirs"),
    (r#"disown [-h] [-ar] [jobspec ... | pid >"#, "disown"),
    (r#"echo [-neE] [arg ...]"#, "echo"),
    (r#"enable [-a] [-dnps] [-f filename] [na>"#, "enable"),
    (r#"eval [arg ...]"#, "eval"),
    (r#"exec [-cl] [-a name] [command [argume>"#, "exec"),
    (r#"exit [n]"#, "exit"),
    (r#"export [-fn] [name[=value] ...] or ex>"#, "export"),
    (r#"false"#, "false"),
    (r#"fc [-e ename] [-lnr] [first] [last] o>"#, "fc"),
    (r#"fg [job_spec]"#, "fg"),
    (r#"for NAME [in WORDS ... ] ; do COMMAND>"#, "for"),
    (r#"for (( exp1; exp2; exp3 )); do COMMAN>"#, "for (("),
    (r#"function name { COMMANDS ; } or name >"#, "function"),
    (r#"getopts optstring name [arg ...]"#, "getopts"),
    (r#"hash [-lr] [-p pathname] [-dt] [name >"#, "hash"),
    (r#"help [-dms] [pattern ...]"#, "help"),
    (r#"history [-c] [-d offset] [n] or hist>"#, "history"),
    (r#"if COMMANDS; then COMMANDS; [ elif C>"#, "if"),
    (r#"jobs [-lnprs] [jobspec ...] or jobs >"#, "jobs"),
    (r#"kill [-s sigspec | -n signum | -sigs>"#, "kill"),
    (r#"let arg [arg ...]"#, "let"),
    (r#"local [option] name[=value] ..."#, "local"),
    (r#"logout [n]"#, "logout"),
    (r#"mapfile [-d delim] [-n count] [-O or>"#, "mapfile"),
    (r#"popd [-n] [+N | -N]"#, "popd"),
    (r#"printf [-v var] format [arguments]"#, "printf"),
    (r#"pushd [-n] [+N | -N | dir]"#, "pushd"),
    (r#"pwd [-LPW]"#, "pwd"),
    (r#"read [-Eers] [-a array] [-d delim] [>"#, "read"),
    (r#"readarray [-d delim] [-n count] [-O >"#, "readarray"),
    (r#"readonly [-aAf] [name[=value] ...] o>"#, "readonly"),
    (r#"return [n]"#, "return"),
    (r#"select NAME [in WORDS ... ;] do COMM>"#, "select"),
    (r#"set [-abefhkmnptuvxBCEHPT] [-o optio>"#, "set"),
    (r#"shift [n]"#, "shift"),
    (r#"shopt [-pqsu] [-o] [optname ...]"#, "shopt"),
    (r#"source [-p path] filename [arguments>"#, "source"),
    (r#"suspend [-f]"#, "suspend"),
    (r#"test [expr]"#, "test"),
    (r#"time [-p] pipeline"#, "time"),
    (r#"times"#, "times"),
    (r#"trap [-Plp] [[action] signal_spec ..>"#, "trap"),
    (r#"true"#, "true"),
    (r#"type [-afptP] name [name ...]"#, "type"),
    (r#"typeset [-aAfFgiIlnrtux] name[=value>"#, "typeset"),
    (r#"ulimit [-SHabcdefiklmnpqrstuvxPRT] [>"#, "ulimit"),
    (r#"umask [-p] [-S] [mode]"#, "umask"),
    (r#"unalias [-a] name [name ...]"#, "unalias"),
    (r#"unset [-f] [-v] [-n] [name ...]"#, "unset"),
    (r#"until COMMANDS; do COMMANDS-2; done"#, "until"),
    (r#"variables - Names and meanings of so>"#, "variables"),
    (r#"wait [-fn] [-p var] [id ...]"#, "wait"),
    (r#"while COMMANDS; do COMMANDS-2; done"#, "while"),
    (r#"{ COMMANDS ; }"#, "{ ... }"),
];

/// 一次性硬绑定校验：`HELP_LIST_MAP` ↔ 77 页规范名。
///
/// 在 `all_en()` 首次初始化（也就是第一次调用 `all()`）时执行。任何
/// 长度 / 顺序 / 集合不一致、任何重复条目都直接 panic：这两份数据不允许
/// 靠「顺序恰好相同」隐式对齐，也不允许缺页后悄悄回退到旧行为。
fn verify_list_binding(pages: &[Page]) {
    assert_eq!(HELP_LIST_MAP.len(), 77, "help list map must hold 77 entries");
    assert_eq!(pages.len(), 77, "help page catalog must hold 77 topics");
    assert_eq!(
        HELP_LIST_MAP.len(),
        pages.len(),
        "help list map / page catalog lengths differ"
    );
    let mut displays: HashSet<&str> = HashSet::with_capacity(77);
    let mut names: HashSet<&str> = HashSet::with_capacity(77);
    for (i, (display, canonical)) in HELP_LIST_MAP.iter().enumerate() {
        assert!(
            displays.insert(*display),
            "duplicate help list display value at index {}: {:?}",
            i,
            display
        );
        assert!(
            names.insert(*canonical),
            "duplicate canonical topic name at index {}: {:?}",
            i,
            canonical
        );
        assert_eq!(
            *canonical, pages[i].name,
            "help list -> page binding differs at index {}: map says {:?}, \
             page says {:?}, list display value is {:?}",
            i, canonical, pages[i].name, display
        );
    }
}

static PAGES_EN: OnceLock<Vec<Page>> = OnceLock::new();
static PAGES_ZH_CN: OnceLock<Vec<Page>> = OnceLock::new();

/// Pages for the current locale.
pub fn all() -> &'static [Page] {
    if is_zh() {
        PAGES_ZH_CN
            .get_or_init(|| {
                parse(DATA_ZH_CN, Some(all_en()))
                    .unwrap_or_else(|e| panic!("help page data invalid: {e}"))
            })
            .as_slice()
    } else {
        all_en()
    }
}

fn all_en() -> &'static [Page] {
    PAGES_EN
        .get_or_init(|| {
            let pages = parse(DATA_EN, None).unwrap_or_else(|e| {
                // 数据漂移时在首次初始化处快速失败：错误消息带 1-based
                // 行号与具体原因（review #24），绝不静默回退、也绝不产生
                // "byte index N is not a char boundary" 这类不可读 panic。
                panic!("help page data invalid: {e}")
            });
            // One-time structural gate: the 77 displayed synopses and the 77
            // parsed page names must be the SAME ordered 77-key set.  A failure
            // here is a hard panic on the first `help` call, never a fallback.
            verify_list_binding(&pages);
            pages
        })
        .as_slice()
}

fn parse(data: &'static str, en: Option<&'static [Page]>) -> Result<Vec<Page>, String> {
    let mut src = data;
    let mut line = 1usize;
    while let Some(rest) = src.strip_prefix('#') {
        match rest.find('\n') {
            Some(eol) => {
                src = &rest[eol + 1..];
                line += 1;
            }
            None => {
                return Err(format!(
                    "help page data line {line}: comment line has no newline"
                ))
            }
        }
    }
    let mut pages: Vec<Page> = Vec::with_capacity(77);
    while !src.is_empty() {
        if !src.starts_with("@@@ ") {
            let head = src.split('\n').next().unwrap_or("");
            return Err(format!(
                "help page data line {line}: expected `@@@ NAME @@@' topic marker, found {head:?}"
            ));
        }
        // 前缀 `@@@ ` 是 ASCII，长度 4 处必为字符边界。
        let after = &src[4..];
        let eol = after.find('\n').ok_or_else(|| {
            format!("help page data line {line}: topic marker line has no newline")
        })?;
        let marker = &after[..eol];
        let name = marker
            .strip_suffix(" @@@")
            .ok_or_else(|| format!("help page data line {line}: topic marker must end with ` @@@'"))?;
        if name.is_empty() {
            return Err(format!("help page data line {line}: empty topic name"));
        }
        let tail = &after[eol + 1..];
        line += 1;
        let (body, next) = match tail.find("\n@@@ ") {
            Some(p) => (&tail[..p + 1], &tail[p + 1..]),
            None => (tail, ""),
        };
        let first_end = body.find('\n').ok_or_else(|| {
            format!("help page data line {line}: topic `{name}' has no synopsis line")
        })?;
        let first = &body[..first_end];
        let rest = &body[first_end + 1..];
        // usage：synopsis 必须形如 `<name>: <usage>`。用 strip_prefix 定位，
        // 不再按 `name.len() + 2` 做裸字节偏移（多字节名称下会切在非字符
        // 边界上 panic，见 review #24）。
        let usage = first
            .strip_prefix(name)
            .and_then(|r| r.strip_prefix(": "))
            .ok_or_else(|| {
                format!(
                    "help page data line {line}: synopsis of topic `{name}' must start with `{name}: '"
                )
            })?;
        let desc_line = rest.split('\n').next().unwrap_or("");
        let desc = desc_line.strip_prefix("    ").unwrap_or(desc_line);
        let desc_en = match en {
            Some(english) => {
                let counterpart = english.get(pages.len()).ok_or_else(|| {
                    format!("help page data line {line}: more topics than the English catalog")
                })?;
                if counterpart.name != name {
                    return Err(format!(
                        "help page data line {line}: catalog topic order differs: \
                         expected `{}', found `{name}'",
                        counterpart.name
                    ));
                }
                counterpart.desc
            }
            None => desc,
        };
        pages.push(Page {
            name,
            usage,
            desc,
            desc_en,
            body,
            rest,
        });
        // 推进行号计数：body 内的换行数即这段数据占用的行数。
        line += body.matches('\n').count();
        src = next;
    }
    if pages.len() != 77 {
        return Err(format!(
            "help page data must hold 77 topics, found {}",
            pages.len()
        ));
    }
    Ok(pages)
}

/// bash accepts these short spellings in addition to the canonical topic name.
/// Verified against the oracle: `help [[` -> `[[ ... ]]`, `help ((` ->
/// `(( ... ))`, `help {` -> `{ ... }`.  `help }` etc. do not resolve (rc=1).
///
/// **Keep this table free of `job_spec`.**  The D-002 divergence is deliberately
/// NOT an alias: `lookup()` is shared by glob and multi-pattern paths, so an
/// alias here would make `help job_spec cd` silently emit the `%` page too.
/// It lives in `JOB_SPEC_DIVERGENCES` below and is reachable only through
/// `divergence_page()`, which requires exactly one pattern.
/// `tools/gen_help_pages.py --check` asserts this table holds exactly 3 entries
/// and that the divergence table holds exactly the 2 enumerated entries.
const ALIASES: &[(&str, &str)] = &[
    ("[[", "[[ ... ]]"),
    ("((", "(( ... ))"),
    ("{", "{ ... }"),
];

/// 单一「有意偏离清单」的代码侧真源（人类可读版见
/// `docs/intentional-divergences.md` 的 D-002 条目）。
///
/// 每行是 (精确 pattern, 目标规范名)。**只有**这两个字符串会在 `help` 的
/// pattern 列表恰好为 1 个时重定向到 `%` 页；大小写、空白、glob 展开、
/// 多 pattern、空 pattern 一律不参与。`tools/gen_help_pages.py --check`
/// 静态断言本表恰好等于这两项；`tests/bash_diff_help.sh` 以固定用例数
/// 断言偏离面不再扩散。
pub(crate) const JOB_SPEC_DIVERGENCES: &[(&str, &str)] = &[
    ("job_spec", "%"),
    ("job_spec [&]", "%"),
];

/// 单-pattern 精确偏离通道：命中返回目标页，否则 `None`。
///
/// 刻意**不放进** `lookup()`：别名表与 glob 集合是「每个 pattern 都过一遍」的
/// 共享路径，一旦在那里注册 `job_spec`，`help job_spec cd` / `help cd job_spec`
/// 之类组合会多输出 `%` 页。这里只由 `builtins.rs::builtin_help` 在选项解析
/// 之后、glob banner 与失败诊断之前调用，保证偏离面封闭为「1 个 pattern +
/// 精确字符串」。找不到目标页直接 panic，绝不静默回退到原行为。
pub fn divergence_page(patterns: &[&str]) -> Option<&'static Page> {
    if patterns.len() != 1 {
        return None;
    }
    let mut target: Option<&'static str> = None;
    for (pattern, canonical) in JOB_SPEC_DIVERGENCES {
        if *pattern == patterns[0] {
            target = Some(*canonical);
            break;
        }
    }
    let target = target?;
    Some(
        all()
            .iter()
            .find(|page| page.name == target)
            .expect("job_spec divergence target is not a real help page"),
    )
}

pub fn lookup(pattern: &str) -> Vec<&'static Page> {
    let pages = all();
    if let Some(p) = pages.iter().find(|p| p.name == pattern) {
        return vec![p];
    }
    if let Some((_, target)) = ALIASES.iter().find(|(a, _)| *a == pattern) {
        if let Some(p) = pages.iter().find(|p| p.name == *target) {
            return vec![p];
        }
    }
    if pattern.is_empty() {
        // bash treats an empty pattern as "all topics", without the glob banner.
        return pages.iter().collect();
    }
    let pat = pattern.as_bytes();
    pages
        .iter()
        .filter(|p| glob_match(pat, p.name.as_bytes()))
        .collect()
}

pub fn render(page: &Page, mode: Mode) -> String {
    match mode {
        Mode::Full => page.body.to_string(),
        Mode::Short => format!("{}: {}\n", page.name, page.usage),
        Mode::Desc => format!("{} - {}\n", page.name, page.desc_en),
        Mode::Man => format!(
            "NAME\n    {} - {}\n\nSYNOPSIS\n    {}\n\nDESCRIPTION\n{}{}",
            page.name, page.desc, page.usage, page.rest, man_footer()
        ),
    }
}

/// bash's `glob_pattern_p`: `*`, `?`, or a *closed* bracket expression.
/// An unclosed `[` (e.g. the literal topic name `[`) is not a glob, which is
/// why `help [` prints no "Shell commands matching keyword" banner.
pub fn is_glob(pat: &str) -> bool {
    let b = pat.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'*' | b'?' => return true,
            b'[' => {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'!' || b[j] == b'^') {
                    j += 1;
                }
                if j < b.len() && b[j] == b']' {
                    j += 1;
                }
                while j < b.len() && b[j] != b']' {
                    j += 1;
                }
                if j < b.len() {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Match a glob pattern against a topic name (byte-wise; all topic names are
/// ASCII).  Supports `*`, `?`, `[...]` (with `!`/`^` negation and ranges) and
/// backslash escapes; an unclosed `[` is a literal.
fn glob_match(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_si = 0usize;
    while si < s.len() {
        let mut advanced = false;
        if pi < p.len() {
            match p[pi] {
                b'*' => {
                    star = Some(pi);
                    star_si = si;
                    pi += 1;
                    advanced = true;
                }
                b'?' => {
                    pi += 1;
                    si += 1;
                    advanced = true;
                }
                b'[' => match class_matches(p, pi, s[si]) {
                    Some((true, next)) => {
                        pi = next;
                        si += 1;
                        advanced = true;
                    }
                    Some((false, _)) => {}
                    None if s[si] == b'[' => {
                        pi += 1;
                        si += 1;
                        advanced = true;
                    }
                    None => {}
                },
                b'\\' if pi + 1 < p.len() => {
                    if p[pi + 1] == s[si] {
                        pi += 2;
                        si += 1;
                        advanced = true;
                    }
                }
                c => {
                    if c == s[si] {
                        pi += 1;
                        si += 1;
                        advanced = true;
                    }
                }
            }
        }
        if advanced {
            continue;
        }
        if let Some(st) = star {
            star_si += 1;
            si = star_si;
            pi = st + 1;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Returns (matches, index just past the closing `]`), or None for an
/// unclosed bracket expression.
fn class_matches(p: &[u8], start: usize, c: u8) -> Option<(bool, usize)> {
    let mut j = start + 1;
    let mut negate = false;
    if j < p.len() && (p[j] == b'!' || p[j] == b'^') {
        negate = true;
        j += 1;
    }
    let mut matched = false;
    let mut first = true;
    while j < p.len() {
        if p[j] == b']' && !first {
            return Some((matched != negate, j + 1));
        }
        first = false;
        if j + 2 < p.len() && p[j + 1] == b'-' && p[j + 2] != b']' {
            if c >= p[j] && c <= p[j + 2] {
                matched = true;
            }
            j += 3;
        } else {
            if c == p[j] {
                matched = true;
            }
            j += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// review #25：77↔77 静态锁必须在 `cargo test` 时**立即**执行，
    /// 而不是推迟到运行时第一次 `help` 调用（那会让漂移穿过所有普通
    /// 测试、只在交互式使用时炸出来）。
    #[test]
    fn list_binding_holds_at_test_time() {
        let pages = all_en();
        assert_eq!(pages.len(), 77);
        verify_list_binding(pages);
    }

    /// zh_CN 目录必须与英文目录绑定同一组 77 个规范名（顺序一致）。
    #[test]
    fn zh_catalog_binds_the_same_77_names() {
        let en = all_en();
        let zh = all();
        assert_eq!(zh.len(), 77);
        for (i, p) in zh.iter().enumerate() {
            assert_eq!(
                p.name, en[i].name,
                "zh_CN catalog name/order drift at index {i}"
            );
        }
    }

    /// 别名表与 D-002 偏离表的目标页必须真实存在，否则偏离通道的
    /// `expect` 会在运行时炸出（这里把它变成测试期失败）。
    #[test]
    fn alias_and_divergence_targets_resolve() {
        let pages = all_en();
        for (alias, target) in ALIASES {
            assert!(
                pages.iter().any(|p| p.name == *target),
                "alias `{alias}' targets missing page `{target}'"
            );
        }
        for (pattern, target) in JOB_SPEC_DIVERGENCES {
            assert!(
                pages.iter().any(|p| p.name == *target),
                "divergence `{pattern}' targets missing page `{target}'"
            );
        }
    }

    /// review #24：数据漂移必须得到**带行号的确定性错误**，而不是
    /// `expect` panic 或非字符边界切片 panic。
    #[test]
    fn parse_errors_are_located_and_deterministic() {
        // `Page` 没有 Debug，这里不能用 unwrap_err，用 match 显式拆。
        let expect_err = |r: Result<Vec<Page>, String>| match r {
            Err(e) => e,
            Ok(_) => panic!("expected a located parse error, got Ok"),
        };
        let e = expect_err(parse("@@@ broken\nbody\n", None));
        assert!(e.contains("line 1"), "error must carry a line number: {e}");
        let e = expect_err(parse("#comment\n@@@ x\nbody without suffix\n", None));
        assert!(e.contains("line 2"), "error must carry a line number: {e}");
        // 多字节数据漂移不得触发 char-boundary panic。
        let e = expect_err(parse("@@@ 中文 @@@\n中文-缺少冒号\nbody\n", None));
        assert!(e.contains("`中文'"), "error must name the topic: {e}");
    }
}
