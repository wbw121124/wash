// 本地化（i18n）支持。
//
// 参照 fish 的做法：源码里用 `_("...")` 宏标记可翻译字符串，译文放在
// `po/<locale>.po` 里，运行时按当前 locale 查表；**查不到译文就回退到原文**
// （也就是 msgid 本身），这与 gettext 的行为一致。
//
// 之所以不直接绑定 MSYS2 的 libintl，是因为那需要在构建期用 msgfmt 把 .po
// 编译成 .mo（会牵动 build.rs），而且 .mo 的加载路径在 MSYS2 下并不稳定。
// 这里改成在运行时直接解析 .po：目录小、启动开销可以忽略，而且完全不需要
// 额外的 FFI，也就不会破坏 posix 层的抽象边界。
//
// locale 的探测顺序与 POSIX / gettext 一致：
//     LC_ALL > LC_MESSAGES > LANG > LC_CTYPE
// 其中 `LC_ALL` / `LC_MESSAGES` 只影响消息文本，`LANG` / `LC_CTYPE` 还会影响
// 字符集与排序。wash 目前只需要消息文本这一类。

use std::collections::HashMap;
use std::sync::OnceLock;

/// 英文 catalog（默认语言，译文与原文一致，实际回退到 msgid）。
const CATALOG_EN: &str = include_str!("../po/en.po");
/// 简体中文 catalog。
const CATALOG_ZH_CN: &str = include_str!("../po/zh_CN.po");

/// 当前生效的 locale（不含编码后缀），例如 `zh_CN`；`C` / `POSIX` 表示英文。
fn current_locale() -> &'static str {
    static LOCALE: OnceLock<String> = OnceLock::new();
    LOCALE.get_or_init(|| {
        let raw = ["LC_ALL", "LC_MESSAGES", "LANG", "LC_CTYPE"]
            .iter()
            .find_map(|k| std::env::var(k).ok())
            .filter(|v| !v.is_empty())
            // POSIX 规定未设置 / 空值等同于 "C"（即英文）。
            .unwrap_or_else(|| "C".to_string());
        normalize_locale(&raw)
    })
}

/// 把一个 locale 名规范化成 `语言[_地区]` 的形式：`zh_CN.UTF-8` → `zh_CN`，
/// `en_US.utf8` → `en_US`，`C` / `POSIX` 保持原样（表示英文）。
fn normalize_locale(raw: &str) -> String {
    // 去掉编码后缀（`.UTF-8`）与修饰符（`@euro`）。
    let mut name = raw.split('@').next().unwrap_or(raw);
    if let Some(dot) = name.find('.') {
        name = &name[..dot];
    }
    if name.is_empty() {
        return "C".to_string();
    }
    // 归一化地区部分的大小写：`zh_cn` → `zh_CN`。
    let mut parts = name.split('_');
    let lang = parts.next().unwrap_or("").to_ascii_lowercase();
    let mut out = lang;
    if let Some(region) = parts.next() {
        out.push('_');
        out.push_str(&region.to_ascii_uppercase());
    }
    out
}

/// 载入的 catalog：msgid → msgstr。
fn catalog() -> &'static HashMap<String, String> {
    static CATALOG: OnceLock<HashMap<String, String>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let loc = current_locale();
        // 先按 `语言_地区` 精确匹配，再退回 `语言` 匹配。
        let source = match loc {
            "zh_CN" => Some(CATALOG_ZH_CN),
            "en" | "C" | "POSIX" => Some(CATALOG_EN),
            other => {
                let lang = other.split('_').next().unwrap_or("");
                match lang {
                    "zh" => Some(CATALOG_ZH_CN),
                    "en" => Some(CATALOG_EN),
                    _ => None,
                }
            }
        };
        match source {
            Some(text) => parse_po(text),
            // 没有对应的 catalog：全部回退到原文。
            None => HashMap::new(),
        }
    })
}

/// 初始化本地化环境（shell 启动时调用一次）。
///
/// catalog 本身是懒加载的（第一次 `translate` 时才载入），这里提前把 locale
/// 探测出来，让启动阶段的诊断也能用上正确的语言。
pub fn init() {
    let _ = current_locale();
    let _ = catalog();
}

/// 翻译 `msgid`：catalog 里有译文就返回译文，否则回退到原文。
pub fn translate(msgid: &str) -> String {
    match catalog().get(msgid) {
        Some(msgstr) if !msgstr.is_empty() => msgstr.clone(),
        _ => msgid.to_string(),
    }
}

/// 把译文里的 `%s` / `%d` 占位符按出现顺序替换成 `args` 里的值。
///
/// gettext 的译文用 printf 风格的占位符；Rust 的 `format!` 要求格式串必须是
/// 编译期的字面量，而译文是运行期才知道的，所以这里自己做替换（占位符按出现
/// 顺序依次对应 `args`）。
pub fn format_msg(msgid: &str, args: &[String]) -> String {
    let msg = translate(msgid);
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg.as_str();
    let mut next = 0usize;
    while let Some(pos) = rest.find('%') {
        let tail = &rest[pos..];
        let len = if tail.starts_with("%s") || tail.starts_with("%d") {
            2
        } else {
            1
        };
        out.push_str(&rest[..pos]);
        if len == 2 {
            match args.get(next) {
                Some(a) => out.push_str(a),
                None => out.push_str(&rest[pos..pos + len]),
            }
            next += 1;
        } else {
            // 孤立的 `%`（例如 "100%"）原样保留。
            out.push('%');
        }
        rest = &rest[pos + len..];
    }
    out.push_str(rest);
    out
}

/// 当前 locale 名（不含编码后缀），供 `locale` 之类的诊断使用。
pub fn locale() -> &'static str {
    current_locale()
}

/// fish 风格的翻译宏：`tr!("text")`。
///
/// fish 用的是 C/C++ 的 `_()` 宏，但 Rust 把 `_` 保留为通配符标识符，不能
/// 当宏名，所以这里用 gettext-rs 的 `tr!` 约定：语义完全一样，调用点写成
/// `tr!("...")`，msgid 在源码里一目了然。
#[macro_export]
macro_rules! tr {
    ($msgid:expr) => {
        $crate::i18n::translate($msgid)
    };
}

// ---------------------------------------------------------------------------
// .po 解析
// ---------------------------------------------------------------------------

/// 解析一个 .po 文件，返回 msgid → msgstr 的映射。
///
/// 只支持 gettext 用到的最基本的语法：`msgid` / `msgstr` 及其续行（多行
/// 字符串），以及 `#` 注释。空 msgid（文件头）被忽略。
fn parse_po(src: &str) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    // 0 = 尚未开始，1 = 正在读 msgid，2 = 正在读 msgstr
    let mut state = 0u8;
    let mut msgid = String::new();
    let mut msgstr = String::new();

    for raw in src.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("msgid ") {
            // 上一个条目结束，先落表。
            store_entry(&mut map, &msgid, &msgstr);
            msgid.clear();
            msgstr.clear();
            state = 1;
            if let Some(s) = parse_quoted(rest) {
                msgid.push_str(&s);
            }
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            msgstr.clear();
            state = 2;
            if let Some(s) = parse_quoted(rest) {
                msgstr.push_str(&s);
            }
        } else if line.starts_with('"') {
            // 续行：接在上一个 msgid / msgstr 后面。
            if let Some(s) = parse_quoted(line) {
                match state {
                    1 => msgid.push_str(&s),
                    2 => msgstr.push_str(&s),
                    _ => {}
                }
            }
        }
        // 其它关键字（msgctxt、msgid_plural 等）本项目用不到，忽略。
    }
    store_entry(&mut map, &msgid, &msgstr);
    map
}

/// 把一个条目写进表里（空 msgid 是文件头，空 msgstr 表示没有译文）。
fn store_entry(map: &mut HashMap<String, String>, msgid: &str, msgstr: &str) {
    if msgid.is_empty() || msgstr.is_empty() {
        return;
    }
    map.insert(msgid.to_string(), msgstr.to_string());
}

/// 解析一个 .po 里的双引号字符串（去掉引号并还原转义）。
fn parse_quoted(s: &str) -> Option<String> {
    let s = s.trim();
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    Some(unescape(inner))
}

/// 还原 .po 字符串里的转义序列。
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                // 未知的转义：按原样保留反斜杠。
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_normalization() {
        assert_eq!(normalize_locale("zh_CN.UTF-8"), "zh_CN");
        assert_eq!(normalize_locale("en_US.utf8"), "en_US");
        assert_eq!(normalize_locale("C"), "C");
        assert_eq!(normalize_locale("POSIX"), "POSIX");
        assert_eq!(normalize_locale("zh_CN@euro"), "zh_CN");
    }

    #[test]
    fn po_parsing() {
        let src = "msgid \"\"\nmsgstr \"Content-Type: text/plain; charset=UTF-8\\n\"\n\n\
                   msgid \"Hello\"\nmsgstr \"你好\"\n\n\
                   msgid \"multi\"\n\"line one\\n\"\n\"line two\"\nmsgstr \"多行\"\n";
        let map = parse_po(src);
        assert_eq!(map.get("Hello").map(String::as_str), Some("你好"));
        assert_eq!(map.get("multi").map(String::as_str), Some("多行"));
        // 文件头（空 msgid）不进入表。
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn unknown_msgid_falls_back() {
        assert_eq!(translate("__no_such_msgid__"), "__no_such_msgid__");
    }
}
