// Fish-style interactive REPL for wash.
//
// This is a from-scratch line editor built on **termios raw mode + ANSI escape
// codes** (see `posix_shim.c` / `posix.rs`). It deliberately avoids any Win32
// console API and any line-editor crate with a Win32 backend — the msys/cygwin
// runtime gives us a real PTY, so raw byte reads + ANSI is all we need. The
// `-c` non-interactive path in `shell::run_str` is never touched.
//
// Features (fish-like):
//   1. Syntax highlighting — a streaming classifier (token categories mirror
//      `lexer.rs`'s WordPart model: command / flag / quoted-string / variable /
//      operator) colors the line as you type. It is robust to *partial* input
//      (unclosed quotes, `&` which the lexer rejects) and never crashes.
//   2. History autosuggestion (ghost text) — the first history line that
//      STARTS WITH the current input is shown dim/gray after the cursor.
//      Accept it with Right-Arrow or End.
//   3. Tab completion — command names from PATH (and known builtins), plus
//      file/dir paths in the current token; common prefix is completed and an
//      ambiguous list is shown below the input.
//   4. Multi-line editing — when the logical line has an unclosed quote or
//      bracket, or ends with an operator (`|` `&` `&&` `||` `|&`), a secondary
//      prompt is shown and input continues on the next physical line; the whole
//      buffer is submitted as one command.
//
// When stdin is not a TTY (e.g. piped input in the smoke test) we fall back to
// the plain line reader so non-interactive usage keeps working.

#![allow(dead_code)]

use crate::i18n;
use crate::posix;
use crate::shell::Shell;
use crate::tr;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthChar;

// Prompts. Single place to change them.
const PRIMARY_PROMPT: &str = "wash$ ";
const CONT_PROMPT: &str = "> ";

// SGR color codes (256-color, widely supported on msys PTYs).
const C_CMD: &str = "\x1b[38;5;76m"; // command name: green
const C_OPT: &str = "\x1b[38;5;214m"; // flags/options: orange
const C_STR: &str = "\x1b[38;5;221m"; // quoted strings: yellow
const C_VAR: &str = "\x1b[38;5;81m"; // variables: cyan
const C_OP: &str = "\x1b[38;5;203m"; // operators/redirects: red
const C_RST: &str = "\x1b[0m"; // reset
const C_GHOST: &str = "\x1b[38;5;245m"; // ghost suggestion: gray

// 控制字符的可见替代形式（readline/fish 风格）：以 `^X` 两格呈现。
// 0x01..0x1A -> ^A..^Z，0x1C..0x1F -> ^[ ^\ ^] ^^，0x7F -> ^?。
// 这样未绑定的控制键（如 Ctrl+Z）就不会以终端的 `▒` 乱码出现，而是显示成 `^Z`。
fn caret_letter(c: char) -> char {
    if c == '\u{7f}' {
        '?'
    } else {
        (b'A' + ((c as u8).wrapping_sub(1))) as char
    }
}

fn is_ctrl(c: char) -> bool {
    (c as u32) < 0x20 || c == '\u{7f}'
}

/// 一个字符在屏幕上的显示宽度：控制字符以 `^X` 两格呈现，其余走 Unicode 宽度。
fn disp_width(ch: char) -> usize {
    if is_ctrl(ch) {
        2
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(0)
    }
}

/// 打印一段文本，把其中的控制字符转义成 `^X`（其余原样输出）。
fn print_escaped(s: &str) {
    for ch in s.chars() {
        if is_ctrl(ch) {
            print!("^{}", caret_letter(ch));
        } else {
            print!("{}", ch);
        }
    }
}

/// 计算一段文本在屏幕上的可见宽度，跳过 ANSI/OSC 转义序列（`\e[..m`、`\e]..<BEL>` 等）。
/// 用于按 $PS1 这种带颜色的提示符正确计算光标列。
fn visible_width(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut w = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            // 跳过转义序列：CSI(`\e[`) / OSC(`\e]`) 等，直到字母或 BEL(0x07)/ST(0x9c) 终结。
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !bytes[i].is_ascii_alphabetic() && bytes[i] != b'~' {
                    i += 1;
                }
                i += 1; // 终结字母
            } else if i < bytes.len() && bytes[i] == b']' {
                i += 1;
                while i < bytes.len() && bytes[i] != 0x07 && bytes[i] != 0x9c {
                    i += 1;
                }
                i += 1; // BEL/ST
            } else {
                // 单个 ESC 后跟其它（如 `\e` 裸转义），跳过该字节。
                if i < bytes.len() {
                    i += 1;
                }
            }
            continue;
        }
        // 把字节还原成 char 计算宽度（非 UTF-8 字节按 1 处理）。
        let ch = if b < 0x80 {
            b as char
        } else {
            // 粗略：跳过多字节序列首字节之外部分，按 1 计宽。
            let len = if b >> 5 == 0b110 { 2 } else if b >> 4 == 0b1110 { 3 } else if b >> 3 == 0b11110 { 4 } else { 1 };
            let s = std::str::from_utf8(&bytes[i..i + len.min(bytes.len() - i)]).ok();
            i += len.min(bytes.len() - i).saturating_sub(1);
            match s.and_then(|s| s.chars().next()) {
                Some(c) => {
                    w += disp_width(c);
                    i += 1;
                    continue;
                }
                None => {
                    i += 1;
                    w += 1;
                    continue;
                }
            }
        };
        w += disp_width(ch);
        i += 1;
    }
    w
}

/// 本次会话已提交的命令序号（PS1 的 \! / \#）。bash 中二者分别对应历史
/// 编号与命令编号，通常一致；这里用同一个计数器近似，在 submit() 里递增。
static COMMAND_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 由 UTF-8 首字节判定字符序列长度（1..=4）；非法首字节按 1 处理。
fn utf8_seq_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// 把 bytes[i] 处的**完整 UTF-8 字符**压入 out，返回消耗的字节数。
/// ASCII 直接压入；多字节用 from_utf8 还原后整体 push_str，避免逐字节
/// `out.push(b as char)` 把中文拆成 Latin-1 乱码。非法序列才走有损回退
/// （合法 UTF-8 的 PS1 不会触发，因此不会产生 U+FFFD 或 Latin-1 残留）。
fn push_utf8_char(out: &mut String, bytes: &[u8], i: usize) -> usize {
    let b = bytes[i];
    let len = utf8_seq_len(b);
    if len == 1 {
        out.push(b as char);
        return 1;
    }
    let end = (i + len).min(bytes.len());
    match std::str::from_utf8(&bytes[i..end]) {
        Ok(s) => {
            out.push_str(s);
            len
        }
        Err(_) => {
            out.push(b as char); // 有损回退：合法 UTF-8 PS1 不触发
            1
        }
    }
}

/// 跳过一个 bash 命令替换 `$(...)`，返回配对右括号之后的字节索引。
/// 括号配对考虑单/双引号、反斜杠转义与嵌套；未闭合时返回 bytes.len()。
///
/// 安全约束：**只扫描、绝不执行**。父 bash 的 git-aware PS1 常含
/// `$(__git_ps1 " (%s)")` 之类，wash 没有 bash 函数体，执行只会报错，
/// 更会把提示符变成任意命令执行的入口。这里统一折叠为空字符串，
/// 因此 `$(` / `__git_ps1` 等代码不会作为字面文本泄漏到提示符。
fn skip_command_substitution(bytes: &[u8], open: usize) -> usize {
    let mut i = open + 1; // open 指向 '('
    let mut depth = 1usize;
    let mut quote: u8 = 0; // 0 表示不在引号内
    while i < bytes.len() {
        let b = bytes[i];
        if quote != 0 {
            if quote == b'"' && b == b'\\' {
                i += 2; // 双引号内反斜杠转义下一个字符
                continue;
            }
            if b == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }
        match b {
            b'\\' => i += 2,
            b'\'' | b'"' => {
                quote = b;
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    bytes.len()
}

/// 展开 bash 风格提示符模板（支持常见转义与 $VAR 变量展开），用于读取 $PS1/$PS2。
///
/// UTF-8 安全：模板里的多字节字符按完整 UTF-8 序列压入（见 push_utf8_char）。
/// bash PS1 语义：`$?` 展开为上一条命令退出码（Shell::last_status，与 wash
/// 自身状态同源）；`$(...)` 命令替换整体折叠为空、绝不执行（见
/// skip_command_substitution）。其余 `\u \h \w \W \$ \v \! \[ \]` 等转义与
/// `$NAME` 变量展开保持不变。
fn render_prompt(template: &str, shell: &Shell) -> String {
    let mut out = String::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() {
            let n = bytes[i + 1];
            match n {
                b'u' => out.push_str(&user_name(shell)),
                b'h' => out.push_str(host_name().split('.').next().unwrap_or("")),
                b'w' => out.push_str(cwd(shell)),
                b'W' => {
                    let w = cwd(shell);
                    let base = w.rsplit('/').next().unwrap_or(&w);
                    let base = base.rsplit('\\').next().unwrap_or(base);
                    out.push_str(base);
                }
                b'$' => out.push_str(if euid_is_root() { "#" } else { "$" }),
                b'n' => out.push('\n'),
                b'e' | b'E' => out.push('\u{1b}'),
                b'a' => out.push('\u{7}'), // bell：BEL（用于终止 OSC 标题序列）
                b'H' => out.push_str(&host_name()),
                b'd' => out.push_str(&posix::strftime_local("%a %b %d")),
                b't' => out.push_str(&posix::strftime_local("%H:%M:%S")),
                b'T' => out.push_str(&posix::strftime_local("%I:%M:%S")),
                b'A' => out.push_str(&posix::strftime_local("%H:%M")),
                b'@' => out.push_str(&posix::strftime_local("%I:%M %p")),
                b'r' => out.push('\r'),
                // \v 用 wash 自身版本串，绝不冒充 BASH_VERSION。
                b'v' => out.push_str(env!("WASH_VERSION")),
                b'!' | b'#' => {
                    use std::sync::atomic::Ordering;
                    out.push_str(&(COMMAND_SEQ.load(Ordering::Relaxed) + 1).to_string());
                }
                b'[' | b']' => {} // 非打印标记：丢弃
                b'\\' => out.push('\\'),
                _ => {
                    out.push('\\');
                    out.push(n as char);
                }
            }
            i += 2;
        } else {
            // bash PS1 语义（优先级高于普通文本拷贝）：
            //   $?     上一条命令退出码，与 Shell::last_status 同源；
            //   $(...) 命令替换：括号配对扫描后整体折叠为空，绝不执行；
            //   $NAME  变量展开（保持既有行为，仅 $NAME 形式）。
            if b == b'$' && i + 1 < bytes.len() {
                let next = bytes[i + 1];
                if next == b'?' {
                    out.push_str(&shell.last_status.to_string());
                    i += 2;
                    continue;
                }
                if next == b'(' {
                    i = skip_command_substitution(bytes, i + 1);
                    continue;
                }
                if next.is_ascii_alphabetic() {
                    let start = i + 1;
                    let mut end = start;
                    while end < bytes.len()
                        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                    {
                        end += 1;
                    }
                    let name = std::str::from_utf8(&bytes[start..end]).unwrap_or("");
                    if let Some(v) = shell.env.get(name) {
                        out.push_str(v);
                    }
                    i = end;
                    continue;
                }
            }
            // 普通文本（含 ANSI 序列）：UTF-8 安全地整体拷贝，绝不逐字节拆。
            i += push_utf8_char(&mut out, bytes, i);
        }
    }
    out
}

fn user_name(_shell: &Shell) -> String {
    // 真实用户名走环境变量，拿不到时回退。返回 String 而非 Box::leak，避免每次
    // 渲染提示符都泄漏一块内存（code-review P3-#36）。
    if let Some(u) = std::env::var_os("USER") {
        u.to_string_lossy().into_owned()
    } else if let Some(u) = std::env::var_os("USERNAME") {
        u.to_string_lossy().into_owned()
    } else {
        "wash".to_string()
    }
}

fn host_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "wash".to_string())
}

fn cwd(shell: &Shell) -> &str {
    if shell.cwd.is_empty() {
        "."
    } else {
        &shell.cwd
    }
}

fn euid_is_root() -> bool {
    posix::effective_uid() == 0
}

// ---------------------------------------------------------------------------
// Key input
// ---------------------------------------------------------------------------

#[derive(PartialEq)]
enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    CtrlC,
    CtrlD,
    CtrlL,
    CtrlV,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Esc,
    Eof,
    Unknown,
}

fn read_key() -> Key {
    let b = match posix::read_byte() {
        Some(b) => b,
        None => return Key::Eof,
    };
    // A bare NUL byte has no meaning as a keystroke (and would otherwise be
    // inserted into the line as a literal NUL). Drop it.
    if b == 0 {
        return Key::Unknown;
    }
    match b {
        0x03 => Key::CtrlC,
        0x0c => Key::CtrlL,
        0x16 => Key::CtrlV,
        0x04 => Key::CtrlD,
        0x0d | 0x0a => Key::Enter,
        0x7f | 0x08 => Key::Backspace,
        0x09 => Key::Tab,
        0x1b => {
            // ESC: lone Esc (cancel) or the start of a CSI/SS3 sequence.
            if !posix::poll_readable(0, 50) {
                return Key::Esc;
            }
            let b2 = match posix::read_byte() {
                Some(b) => b,
                None => return Key::Esc,
            };
            if b2 == b'[' {
                let mut seq = Vec::new();
                loop {
                    let c = match posix::read_byte_timeout(50) {
                        Some(c) => c,
                        None => break,
                    };
                    seq.push(c);
                    if c.is_ascii_alphabetic() || c == b'~' {
                        break;
                    }
                }
                parse_csi(&seq)
            } else if b2 == b'O' {
                match posix::read_byte() {
                    Some(b'H') => Key::Home,
                    Some(b'F') => Key::End,
                    _ => Key::Unknown,
                }
            } else {
                Key::Unknown
            }
        }
        _ => Key::Char(decode_utf8(b)),
    }
}

fn parse_csi(seq: &[u8]) -> Key {
    if seq.is_empty() {
        return Key::Unknown;
    }
    match seq[seq.len() - 1] {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'~' => {
            let num: String = seq[..seq.len() - 1].iter().map(|c| *c as char).collect();
            match num.as_str() {
                "1" | "7" => Key::Home,
                "4" | "8" => Key::End,
                _ => Key::Unknown,
            }
        }
        _ => Key::Unknown,
    }
}

fn decode_utf8(first: u8) -> char {
    if first < 0x80 {
        return first as char;
    }
    let len = if first & 0xE0 == 0xC0 {
        2
    } else if first & 0xF0 == 0xE0 {
        3
    } else if first & 0xF8 == 0xF0 {
        4
    } else {
        return '\u{FFFD}';
    };
    let mut buf = [first, 0, 0, 0];
    for k in 1..len {
        match posix::read_byte() {
            Some(b) => buf[k] = b,
            None => return '\u{FFFD}',
        }
    }
    std::str::from_utf8(&buf[..len])
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or('\u{FFFD}')
}

// ---------------------------------------------------------------------------
// Editor state
// ---------------------------------------------------------------------------

struct Editor {
    buffer: String,
    cursor: usize, // char index into buffer
    history: Vec<String>,
    hist_pos: Option<usize>, // Some(i) while navigating, None when editing fresh
    draft: String,           // saved buffer when history navigation began
    // (显示名, 预置的本地化描述, 完整路径) 列表，显示在输入下方。
    // 预置描述非空时直接用（如内建的 "Builtin"）；为空时按完整路径在渲染
    // 阶段延迟计算描述，避免 Tab 扫全盘时对上千候选逐个 stat 卡死。
    completions: Option<Vec<(String, String, String)>>,
    last_total: usize,       // physical lines rendered last time (for clearing)
    literal_next: bool,      // 下一个键按字面插入（Ctrl+V 后的「引号」模式）
}

impl Editor {
    fn new(history: Vec<String>) -> Self {
        Editor {
            buffer: String::new(),
            cursor: 0,
            history,
            hist_pos: None,
            draft: String::new(),
            completions: None,
            last_total: 0,
            literal_next: false,
        }
    }

    // --- low-level buffer helpers ------------------------------------------

    fn char_count(&self) -> usize {
        self.buffer.chars().count()
    }

    fn insert(&mut self, ch: char) {
        self.hist_pos = None;
        self.completions = None;
        let byte = char_to_byte(&self.buffer, self.cursor);
        self.buffer.insert(byte, ch);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.hist_pos = None;
        self.completions = None;
        self.cursor -= 1;
        let byte = char_to_byte(&self.buffer, self.cursor);
        let len = self.buffer[byte..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        self.buffer.drain(byte..byte + len);
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.char_count() {
            return;
        }
        self.hist_pos = None;
        self.completions = None;
        let byte = char_to_byte(&self.buffer, self.cursor);
        let len = self.buffer[byte..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        self.buffer.drain(byte..byte + len);
    }

    fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.completions = None;
    }

    fn move_right(&mut self) {
        if self.cursor < self.char_count() {
            self.cursor += 1;
        }
        self.completions = None;
    }

    fn move_home(&mut self) {
        let byte = char_to_byte(&self.buffer, self.cursor);
        let prefix = &self.buffer[..byte];
        let start = match prefix.rfind('\n') {
            Some(p) => p + 1,
            None => 0,
        };
        self.cursor = byte_to_char(&self.buffer, start);
        self.completions = None;
    }

    fn move_end(&mut self) {
        let byte = char_to_byte(&self.buffer, self.cursor);
        let rest = &self.buffer[byte..];
        let end = match rest.find('\n') {
            Some(off) => byte + off,
            None => self.buffer.len(),
        };
        self.cursor = byte_to_char(&self.buffer, end);
        self.completions = None;
    }

    fn cancel_line(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.hist_pos = None;
        self.completions = None;
    }

    // --- history navigation ------------------------------------------------

    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.hist_pos.is_none() {
            self.draft = self.buffer.clone();
            self.hist_pos = Some(self.history.len() - 1);
        } else {
            let p = self.hist_pos.unwrap();
            if p > 0 {
                self.hist_pos = Some(p - 1);
            }
        }
        self.buffer = self.history[self.hist_pos.unwrap()].clone();
        self.cursor = self.char_count();
        self.completions = None;
    }

    fn history_next(&mut self) {
        if let Some(p) = self.hist_pos {
            if p + 1 < self.history.len() {
                self.hist_pos = Some(p + 1);
                self.buffer = self.history[p + 1].clone();
            } else {
                self.hist_pos = None;
                self.buffer = std::mem::take(&mut self.draft);
            }
            self.cursor = self.char_count();
            self.completions = None;
        }
    }

    // --- ghost (history autosuggestion) ------------------------------------

    fn ghost_suggestion(&self) -> String {
        let buf = self.buffer.as_str();
        if buf.is_empty() {
            return String::new();
        }
        // Search most-recent first for a line that starts with the current input.
        for h in self.history.iter().rev() {
            if h.starts_with(buf) && h.len() > buf.len() {
                return h[buf.len()..].to_string();
            }
        }
        String::new()
    }

    fn accept_ghost(&mut self) {
        let g = self.ghost_suggestion();
        if !g.is_empty() {
            self.buffer.push_str(&g);
            self.cursor = self.char_count();
            self.completions = None;
        }
    }

    // --- completion --------------------------------------------------------

    fn current_word(&self) -> (usize, String) {
        let cur_byte = char_to_byte(&self.buffer, self.cursor);
        let bytes = self.buffer.as_bytes();
        let mut start = cur_byte;
        while start > 0 {
            let b = bytes[start - 1];
            if b == b' ' || b == b'\t' || matches!(b, b'|' | b'&' | b';' | b'<' | b'>') {
                break;
            }
            start -= 1;
        }
        let word_start = byte_to_char(&self.buffer, start);
        (word_start, self.buffer[start..cur_byte].to_string())
    }

    fn complete(&mut self) {
        // 只在行尾触发补全，保持简单。
        if self.cursor != self.char_count() {
            self.completions = None;
            return;
        }
        let (word_start, word) = self.current_word();
        let prefix = &self.buffer[..char_to_byte(&self.buffer, word_start)];
        let is_cmd = word_start == 0 || is_command_position(prefix);
        let cands = if is_cmd && !word.contains('/') {
            complete_command(&word)
        } else {
            complete_file(&word)
        };
        if cands.is_empty() {
            self.completions = None;
            return;
        }
        // 只取名字列表用于公共前缀与替换计算（描述不参与替换）。
        let names: Vec<String> = cands.iter().map(|(n, _, _)| n.clone()).collect();
        if cands.len() == 1 {
            let mut rep = cands[0].0.clone();
            if !rep.ends_with('/') {
                rep.push(' ');
            }
            self.apply_replacement(word_start, &word, &rep);
            self.completions = None;
        } else {
            let cp = common_prefix(&names);
            if cp.len() > word.len() {
                self.apply_replacement(word_start, &word, &cp);
            }
            self.completions = Some(cands);
        }
    }

    fn apply_replacement(&mut self, word_start: usize, word: &str, rep: &str) {
        let ws_byte = char_to_byte(&self.buffer, word_start);
        let we_byte = char_to_byte(&self.buffer, word_start + word.chars().count());
        self.buffer.replace_range(ws_byte..we_byte, rep);
        self.cursor = byte_to_char(&self.buffer, ws_byte + rep.len());
        self.completions = None;
    }

    // --- rendering ---------------------------------------------------------

    fn render(&mut self, shell: &Shell) {
        // 读取 $PS1/$PS2（带 bash 风格转义与 $VAR 展开）；没有则回退内置常量。
        let prompt0 = render_prompt(
            shell.env.get("PS1").map(|s| s.as_str()).unwrap_or(PRIMARY_PROMPT),
            shell,
        );
        let prompt_cont = render_prompt(
            shell.env.get("PS2").map(|s| s.as_str()).unwrap_or(CONT_PROMPT),
            shell,
        );
        // 多行提示符（含 \n）时，首行输入前的「额外物理行数」与首段可见宽度。
        let prompt0_last = prompt0.rsplit('\n').next().unwrap_or("");
        let prompt_extra = prompt0.matches('\n').count();

        let lines: Vec<&str> = self.buffer.split('\n').collect();
        let total_phys = lines.len();
        let (cur_line, cur_col) = self.cursor_pos(&lines);
        let ghost = if self.cursor == self.char_count() && self.hist_pos.is_none() {
            self.ghost_suggestion()
        } else {
            String::new()
        };

        // 一次「清到屏尾」清除光标之下整块（输入行 + 补全候选），比逐行清更稳健，
        // 不会残留候选列表。多行提示符的首段在光标之上，属静态文本，无需重清。
        if self.last_total > 0 {
            print!("\x1b[J");
        }

        // Draw the input block.
        let last_idx = total_phys - 1;
        for (idx, line) in lines.iter().enumerate() {
            let prompt = if idx == 0 { &prompt0 } else { &prompt_cont };
            print!("\r\x1b[K{}", prompt);
            self.print_highlighted(line);
            if idx == last_idx && ! ghost.is_empty() {
                print!("{}{}{}", C_GHOST, ghost, C_RST);
            }
            if idx != last_idx {
                print!("\r\n");
            }
        }

        // 绘制补全候选列表：仅对「实际可见」的候选（前 page_limit() 行）计算类型/
        // 大小描述，避免 Tab 扫全盘时对上千文件逐个 stat 导致卡死。
        let cand_lines = match &self.completions {
            Some(c) => {
                let limit = page_limit();
                let shown = c.len().min(limit);
                for (name, preset, path) in c.iter().take(limit) {
                    // 内建等候选已算好本地化描述（tr!("Builtin")），直接沿用；
                    // 其余按完整路径在渲染阶段计算，且只对这一屏可见的候
                    // 选执行，避免 Tab 扫全盘时上千次 stat 卡死。
                    let desc = if !preset.is_empty() {
                        preset.clone()
                    } else if !path.is_empty() {
                        describe_path(path)
                    } else {
                        String::new()
                    };
                    let line = if desc.is_empty() {
                        name.clone()
                    } else {
                        format!("{} ({})", name, desc)
                    };
                    print!("\r\n\x1b[K{}", line);
                }
                let mut lines = shown;
                if c.len() > limit {
                    // 状态行：`rows 1 to {limit} of {total}`（按 locale 本地化）。
                    let status = i18n::format_msg(
                        "rows %d to %d of %d",
                        &[1.to_string(), limit.to_string(), c.len().to_string()],
                    );
                    print!("\r\n\x1b[K{}", status);
                    lines += 1;
                }
                lines
            }
            None => 0,
        };

        // 把光标定位回输入块内的正确位置。
        let lines_from_end = (total_phys - 1 - cur_line) + cand_lines;
        if lines_from_end > 0 {
            print!("\x1b[{}A", lines_from_end);
        }
        // 宽度口径统一：base_col 由 visible_width 计算（跳过 ANSI/OSC，按 UTF-8
        // 字符显示宽度累加，中文 2 格）；下方 \x1b[NG 直接用 abs_col+1，二者
        // 同源。render_prompt 修好非 ASCII 拆字节后，中文 PS1 宽度仍为 6。
        let base_col = if cur_line == 0 {
            visible_width(prompt0_last)
        } else {
            visible_width(&prompt_cont)
        };
        let abs_col = base_col + cur_col;
        print!("\r\x1b[{}G", abs_col + 1);

        io::stdout().flush().ok();
        self.last_total = total_phys + cand_lines + prompt_extra;
    }

    fn clear_screen(&mut self) {
        print!("\x1b[2J\x1b[H");
        io::stdout().flush().ok();
        self.last_total = 0;
    }

    fn cursor_pos(&self, lines: &[&str]) -> (usize, usize) {
        // `cursor` is a char index, while `char_indices()` yields byte offsets.
        // Convert once up front so every comparison/subtraction below stays in
        // the byte domain; otherwise multi-byte characters (CJK) shift the
        // caret to the wrong line/column.
        let cbyte = char_to_byte(&self.buffer, self.cursor);
        let mut line_idx = 0;
        let mut line_start = 0;
        for (i, ch) in self.buffer.char_indices() {
            if i >= cbyte {
                break;
            }
            if ch == '\n' {
                line_idx += 1;
                line_start = i + 1;
            }
        }
        let line = lines.get(line_idx).copied().unwrap_or("");
        let offset = cbyte - line_start;
        let mut col = 0;
        for (j, ch) in line.char_indices() {
            if j >= offset {
                break;
            }
            col += disp_width(ch);
        }
        (line_idx, col)
    }

    fn prompt_width(&self, line_idx: usize) -> usize {
        if line_idx == 0 {
            PRIMARY_PROMPT.chars().count()
        } else {
            CONT_PROMPT.chars().count()
        }
    }

    fn print_highlighted(&self, line: &str) {
        let spans = classify(line);
        let chars: Vec<char> = line.chars().collect();
        for (start, end, color) in spans {
            let seg: String = chars[start..end].iter().collect();
            // 控制字符以 `^X` 两格呈现（readline/fish 风格），而不是让终端渲染成
            // `▒` 乱码；颜色序列本身不经过转义，仍是裸 ANSI。
            match color {
                Some(c) => {
                    print!("{}", c);
                    print_escaped(&seg);
                    print!("{}", C_RST);
                }
                None => print_escaped(&seg),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Char-index <-> byte-index helpers (the buffer is UTF-8; cursor is char-based)
// ---------------------------------------------------------------------------

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn byte_to_char(s: &str, byte_idx: usize) -> usize {
    s.char_indices()
        .take_while(|(b, _)| *b < byte_idx)
        .count()
}

// ---------------------------------------------------------------------------
// Syntax highlighting classifier
//
// Mirrors the token categories used by `lexer.rs` (command word, option/flag,
// quoted string, variable, operator/redirect) but is a *streaming* classifier
// so it works on partial, not-yet-valid input. Returns contiguous,
// non-overlapping spans over the whole line. `None` color = default.
// ---------------------------------------------------------------------------

fn classify(line: &str) -> Vec<(usize, usize, Option<&'static str>)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut spans: Vec<(usize, usize, Option<&'static str>)> = Vec::new();
    let mut i = 0usize;
    let mut expecting_cmd = true;

    while i < n {
        let c = chars[i];
        match c {
            '\'' => {
                let start = i;
                i += 1;
                while i < n && chars[i] != '\'' {
                    i += 1;
                }
                let end = if i < n { i + 1 } else { n };
                spans.push((start, end, Some(C_STR)));
                if i < n {
                    i += 1;
                }
            }
            '"' => {
                let open = i;
                spans.push((open, open + 1, Some(C_STR)));
                i += 1;
                let mut bodys = i;
                while i < n && chars[i] != '"' {
                    if chars[i] == '$' {
                        if i > bodys {
                            spans.push((bodys, i, Some(C_STR)));
                        }
                        let (vend, _) = read_var(&chars, i);
                        spans.push((i, vend, Some(C_VAR)));
                        i = vend;
                        bodys = i;
                        continue;
                    }
                    i += 1;
                }
                if i > bodys {
                    spans.push((bodys, i, Some(C_STR)));
                }
                if i < n {
                    spans.push((i, i + 1, Some(C_STR)));
                    i += 1;
                }
            }
            '$' => {
                let (end, _) = read_var(&chars, i);
                spans.push((i, end, Some(C_VAR)));
                i = end;
                expecting_cmd = false;
            }
            '|' | ';' | '&' | '>' | '<' => {
                let start = i;
                if (c == '&' || c == '|') && i + 1 < n && chars[i + 1] == c {
                    i += 2;
                } else if c == '|' && i + 1 < n && chars[i + 1] == '&' {
                    i += 2;
                } else if (c == '>' || c == '<') && i + 1 < n && chars[i + 1] == c {
                    i += 2;
                } else {
                    i += 1;
                }
                spans.push((start, i, Some(C_OP)));
                expecting_cmd = matches!(c, '|' | ';' | '&');
            }
            ' ' | '\t' => {
                let start = i;
                while i < n && (chars[i] == ' ' || chars[i] == '\t') {
                    i += 1;
                }
                spans.push((start, i, None));
            }
            _ => {
                let start = i;
                while i < n
                    && !matches!(
                        chars[i],
                        ' ' | '\t' | '\'' | '"' | '$' | '|' | ';' | '&' | '>' | '<'
                    )
                {
                    i += 1;
                }
                let end = i;
                let word: String = chars[start..end].iter().collect();
                let color = if expecting_cmd {
                    Some(C_CMD)
                } else if word.starts_with('-') {
                    Some(C_OPT)
                } else {
                    None
                };
                spans.push((start, end, color));
                expecting_cmd = false;
            }
        }
    }
    spans
}

// Returns (byte_end, var_name) for a `$...` starting at `i` (i points at '$').
fn read_var(chars: &[char], i: usize) -> (usize, String) {
    let mut j = i + 1;
    if j < chars.len() && chars[j] == '(' {
        if j + 1 < chars.len() && chars[j + 1] == '(' {
            // $(( ... ))
            let mut depth = 1;
            j += 2;
            while j < chars.len() {
                if chars[j] == '(' {
                    depth += 1;
                } else if chars[j] == ')' {
                    depth -= 1;
                    if depth == 0 {
                        j += 1;
                        if j < chars.len() && chars[j] == ')' {
                            j += 1;
                        }
                        break;
                    }
                }
                j += 1;
            }
            (j, String::new())
        } else {
            // $( ... )
            let mut depth = 1;
            j += 1;
            while j < chars.len() {
                if chars[j] == '(' {
                    depth += 1;
                } else if chars[j] == ')' {
                    depth -= 1;
                    if depth == 0 {
                        j += 1;
                        break;
                    }
                }
                j += 1;
            }
            (j, String::new())
        }
    } else if j < chars.len() && chars[j] == '{' {
        let mut j2 = j + 1;
        while j2 < chars.len() && chars[j2] != '}' {
            j2 += 1;
        }
        if j2 < chars.len() {
            j2 += 1;
        }
        (j2, String::new())
    } else {
        let mut name = String::new();
        if j < chars.len() && (chars[j].is_ascii_alphabetic() || chars[j] == '_') {
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                name.push(chars[j]);
                j += 1;
            }
        } else if j < chars.len() {
            name.push(chars[j]);
            j += 1;
        }
        (j, name)
    }
}

// ---------------------------------------------------------------------------
// Multi-line continuation detection
// ---------------------------------------------------------------------------

fn is_incomplete(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut depth = 0i32;
    for &c in &chars {
        if in_single {
            if c == '\'' {
                in_single = false;
            }
        } else if in_double {
            if c == '"' {
                in_double = false;
            }
        } else {
            match c {
                '\'' => in_single = true,
                '"' => in_double = true,
                '(' | '{' | '[' => depth += 1,
                ')' | '}' | ']' => depth -= 1,
                _ => {}
            }
        }
    }
    if in_single || in_double || depth > 0 {
        return true;
    }
    let t = line.trim_end();
    // 管道（`|` / `||` / `|&`）与逻辑与（`&&`）结尾需要续行；结尾的单个
    // `&` 是后台作业（如 `sleep 2 &`），bash 直接执行，不是续行。
    t.ends_with('|') || t.ends_with("|&") || t.ends_with("&&")
}

// ---------------------------------------------------------------------------
// Completion helpers
// ---------------------------------------------------------------------------

fn is_command_position(prefix: &str) -> bool {
    let t = prefix.trim_end();
    if t.is_empty() {
        return true;
    }
    t.ends_with('|') || t.ends_with('&') || t.ends_with(';') || t.ends_with('(') || t.ends_with('{')
}

/// 补全候选数量上限：超过则截断，避免 Tab 在 PATH 巨大（如 System32 五千余文件）
/// 时一次性生成 / 列出海量候选导致卡死。
const COMPLETE_CAP: usize = 1000;

fn complete_command(word: &str) -> Vec<(String, String, String)> {
    let mut cands: Vec<(String, String, String)> = Vec::new();
    // 统一名单：静态内建表 + 动态插件内建（用全局快照，免给补全函数加 sh 参数）。
    // 内建命令标注为 Builtin（按当前 locale 本地化）。
    for b in crate::builtins::BUILTINS {
        if b.starts_with(word) {
            cands.push(((*b).to_string(), tr!("Builtin"), String::new()));
        }
    }
    let plugin_state = crate::plugin::current_state();
    for name in plugin_state.dynamic.keys() {
        if name.starts_with(word) {
            cands.push((name.clone(), tr!("Builtin"), String::new()));
        }
    }
    // PATH 里的可执行文件：只收集名字（不在此处 stat——否则 System32 下数千文件的
    // access() 风暴会让 Tab 卡死）。类型/大小描述延迟到 render 时只对可见候选计算。
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(word) {
                    // 只记名字与完整路径，不在此处 stat；描述延迟到 render
                    // 阶段只对可见候选按完整路径计算，避免 Tab 扫全盘卡死。
                    let full = e.path().to_string_lossy().into_owned();
                    cands.push((name, String::new(), full));
                }
                if cands.len() >= COMPLETE_CAP {
                    break;
                }
            }
            if cands.len() >= COMPLETE_CAP {
                break;
            }
        }
    }
    // 同名候选去重，保留先入者（内建优先于 PATH 里的同名可执行）。
    cands.sort_by(|a, b| a.0.cmp(&b.0));
    cands.dedup_by(|a, b| a.0 == b.0);
    cands
}

fn complete_file(word: &str) -> Vec<(String, String, String)> {
    let (dirpath, prefix) = if let Some(slash) = word.rfind('/') {
        (&word[..slash + 1], &word[slash + 1..])
    } else {
        (".", word)
    };
    let mut cands: Vec<(String, String, String)> = Vec::new();
    let Ok(entries) = fs::read_dir(dirpath) else {
        return cands;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(prefix) {
            let is_dir = e.path().is_dir();
            let mut rep = if word.contains('/') {
                format!("{}{}", &word[..word.rfind('/').unwrap() + 1], name)
            } else {
                name.clone()
            };
            // 目录补一个尾部 `/` 方便继续补全；类型/大小描述延迟到 render 计算。
            if is_dir {
                rep.push('/');
            }
            // 完整路径用于 render 阶段按可见候选 stat，避免扫全盘卡死。
            let full = e.path().to_string_lossy().into_owned();
            cands.push((rep, String::new(), full));
        }
        if cands.len() >= COMPLETE_CAP {
            break;
        }
    }
    cands.sort_by(|a, b| a.0.cmp(&b.0));
    cands
}

// ---------------------------------------------------------------------------
// 补全描述（fish 风格，带类型与大小，并按 locale 本地化）
// ---------------------------------------------------------------------------

/// 把字节数格式化成 fish 风格的大小串（如 `34kB` / `1.5MB` / `2.0GB`）。
/// 单位符号（B/kB/MB/GB）是技术记号，不进 po，跨 locale 保持一致。
fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{}kB", bytes / KB)
    } else {
        format!("{}B", bytes)
    }
}

/// 探测一个路径的类型，返回本地化的补全描述（与 fish 的 `(Executable, 34kB)`
/// 风格一致）：符号链接 → `Symlink`，目录 → `Directory`，可执行文件 →
/// `Executable, %s`，普通文件 → `File, %s`（%s 为大小串）。
///
/// 可执行性统一走 wash 的 posix 抽象层（`posix::access(..., X_OK)`），不依赖
/// unix-only 的权限位 API —— MSYS2 的 target 是 windows-gnu，`cfg(unix)` 为假。
fn describe_path(path: &str) -> String {
    // 符号链接优先判定（symlink_metadata 不跟随）。
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return tr!("Symlink");
        }
    }
    let Ok(meta) = fs::metadata(path) else {
        return String::new();
    };
    if meta.is_dir() {
        return tr!("Directory");
    }
    if meta.is_file() {
        let size = human_size(meta.len());
        if posix::access(path, posix::X_OK) == 0 {
            return i18n::format_msg("Executable, %s", &[size]);
        }
        return i18n::format_msg("File, %s", &[size]);
    }
    String::new()
}

/// 补全候选每屏最多显示的行数（为输入行与状态行各留一行）。优先读 `LINES`
/// 环境变量，拿不到时回退到 20。
fn page_limit() -> usize {
    std::env::var("LINES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n.saturating_sub(2))
        .filter(|&n| n >= 1)
        .unwrap_or(20)
}

fn common_prefix(words: &[String]) -> String {
    if words.is_empty() {
        return String::new();
    }
    let first = &words[0];
    let mut end = first.chars().count();
    for w in &words[1..] {
        let max = end.min(w.chars().count());
        let mut i = 0;
        for (a, b) in first.chars().zip(w.chars()) {
            if a != b {
                break;
            }
            i += 1;
        }
        end = end.min(i).min(max);
        if end == 0 {
            break;
        }
    }
    // 按字符数截取，绝不在多字节字符中间切片（修复 P0-2：中文文件名 panic）。
    first.chars().take(end).collect()
}

// ---------------------------------------------------------------------------
// History persistence
// ---------------------------------------------------------------------------

fn history_file() -> PathBuf {
    // Single place to change the history file location.
    if let Some(home) = std::env::var_os("HOME") {
        Path::new(&home).join(".wash_history")
    } else {
        PathBuf::from(".wash_history")
    }
}

fn load_history(p: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(s) = fs::read_to_string(p) {
        for line in s.lines() {
            if !line.is_empty() {
                v.push(line.to_string());
            }
        }
    }
    v
}

fn save_history(p: &Path, hist: &[String]) {
    // Append only entries not already in the file (so repeated launches don't
    // duplicate), matching the "~/.wash_history append on exit" requirement.
    let existing: Vec<String> = fs::read_to_string(p)
        .map(|s| s.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    let existing_set: std::collections::HashSet<&str> = existing.iter().map(|s| s.as_str()).collect();
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(p) {
        for h in hist {
            if !existing_set.contains(h.as_str()) {
                let _ = writeln!(f, "{}", h);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Submit a finished logical line
// ---------------------------------------------------------------------------

fn submit(editor: &mut Editor, shell: &mut Shell) {
    // 提交前先清掉光标之下（输入行 + 补全候选）的全部内容，避免命令输出前
    // 屏幕上残留一堆补全候选。
    if editor.last_total > 0 {
        print!("\x1b[J");
        io::stdout().flush().ok();
    }
    let line = std::mem::take(&mut editor.buffer);
    editor.cursor = 0;
    editor.hist_pos = None;
    editor.completions = None;
    editor.last_total = 0;

    let line = line.trim_end_matches('\n').to_string();
    // 每提交一行就推进命令序号（PS1 的 \! / \#）。
    COMMAND_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !line.trim().is_empty() {
        editor.history.push(line.clone());
    }

    // Run the command in cooked mode so child programs get a normal terminal.
    posix::disable_raw_mode();
    println!();
    io::stdout().flush().ok();
    shell.run_str(&line);
    posix::enable_raw_mode();
}

// ---------------------------------------------------------------------------
// Fallback for non-TTY input (piped / smoke tests)
// ---------------------------------------------------------------------------

fn run_piped(shell: &mut Shell) {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }
        shell.run_str(line);
        // `exit` 内建会把标志置位；此处检测并停止读取后续输入（对齐交互模式）。
        if shell.exit_requested.is_some() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

pub fn run(shell: &mut Shell) {
    let hist_path = history_file();
    let history = load_history(&hist_path);
    let mut editor = Editor::new(history);

    // Non-interactive (piped) input: use the plain line reader.
    if !posix::isatty(0) || !posix::enable_raw_mode() {
        run_piped(shell);
        return;
    }

    editor.render(shell);
    loop {
        // 前台命令运行期间（cooked 模式、ISIG 打开）若收到 SIGINT 等信号，
        // C handler 只置了标志；这里统一处理：派发对应 trap，丢弃当前输入
        // 行，回到新行提示符，shell 保持存活且历史不丢（P1-1 / P1-2）。
        // raw 模式下 ISIG 关闭，Ctrl-C 是字节 0x03，仍由下面的 Key::CtrlC
        // 分支处理，不走这里。
        let sig = posix::take_pending_signal();
        if sig > 0 {
            // 让 trap 命令的输出走正常的 cooked 终端。
            posix::disable_raw_mode();
            crate::builtins::dispatch_signal_trap(shell, sig);
            io::stdout().flush().ok();
            println!();
            io::stdout().flush().ok();
            posix::enable_raw_mode();
            // `trap 'exit' INT`（以及任何在 trap 里执行 `exit` 的动作）会在
            // dispatch_signal_trap 里置位 exit_requested。必须在下面的 continue
            // 之前检查：该 continue 会跳过循环末尾的统一退出检查，导致 Ctrl-C
            // 之后 REPL 不退出（P1 第二组）。命中时直接 break，交给后面的
            // disable_raw_mode / EXIT trap / save_history 干净收尾，且不再
            // 重画一个多余提示符。
            if shell.exit_requested.is_some() {
                break;
            }
            editor.cancel_line();
            editor.last_total = 0;
            save_history(&hist_path, &editor.history);
            editor.render(shell);
            continue;
        }
        // Ctrl+V 之后的那个键按字面插入：直接读一个裸字节，绕过控制键映射，
        // 这样 Ctrl+V 后跟 Ctrl+C 会插入 0x03 而非取消整行（对齐 readline 行为）。
        let key = if editor.literal_next {
            editor.literal_next = false;
            match posix::read_byte() {
                Some(b) => Key::Char(decode_utf8(b)),
                None => Key::Eof,
            }
        } else {
            read_key()
        };
        match key {
            Key::Eof => break,
            Key::CtrlC => {
                // 清掉光标之下（输入行 + 补全候选）的全部内容，再打印 ^C 并换行，
                // 由随后的 render() 在下一行重绘提示符。
                if editor.last_total > 0 {
                    print!("\x1b[J");
                }
                print!("\r\x1b[K^C\n");
                io::stdout().flush().ok();
                editor.cancel_line();
                editor.last_total = 0;
                // bash 的 readline 在提示符处按 Ctrl-C 仍会产生 SIGINT 并派发
                // 已安装的 INT trap。raw 模式下 ISIG 关闭、0x03 作为字节到达
                // 这里（不走 signal handler），所以手动派发一次；补一个回车让
                // trap 的输出从行首开始、成为独立输出行。
                print!("\r");
                io::stdout().flush().ok();
                crate::builtins::dispatch_signal_trap(shell, 2);
            }
            Key::CtrlV => {
                editor.literal_next = true;
            }
            Key::CtrlD => {
                if editor.buffer.is_empty() {
                    if shell.options.ignoreeof {
                        // bash: `set -o ignoreeof` 后空缓冲的 Ctrl-D 不退出，
                        // 打印提示并继续（bash 需连续多次才真正退出）。
                        print!("\r\x1b[K\nUse \"exit\" to leave the shell.\n");
                        io::stdout().flush().ok();
                        editor.last_total = 0;
                    } else {
                        break;
                    }
                } else {
                    editor.delete_forward();
                }
            }
            Key::CtrlL => editor.clear_screen(),
            Key::Enter => {
                let trimmed_empty = !editor.buffer.contains('\n') && editor.buffer.trim().is_empty();
                if trimmed_empty {
                    // 空行：换行后在下一行重绘提示符。
                    print!("\r\x1b[K\n");
                    io::stdout().flush().ok();
                } else {
                    let cur_empty = {
                        let after = match editor.buffer.rfind('\n') {
                            Some(p) => p + 1,
                            None => 0,
                        };
                        editor.buffer[after..].trim().is_empty()
                    };
                    if editor.buffer.contains('\n') && cur_empty && !is_incomplete(&editor.buffer) {
                        submit(&mut editor, shell);
                    } else if is_incomplete(&editor.buffer) {
                        editor.buffer.push('\n');
                        editor.cursor = editor.char_count();
                        editor.completions = None;
                    } else {
                        submit(&mut editor, shell);
                    }
                }
            }
            Key::Char(ch) => editor.insert(ch),
            Key::Backspace => editor.backspace(),
            Key::Tab => editor.complete(),
            Key::Left => editor.move_left(),
            Key::Right => {
                if editor.cursor == editor.char_count() {
                    editor.accept_ghost();
                } else {
                    editor.move_right();
                }
            }
            Key::Home => editor.move_home(),
            Key::End => {
                if editor.cursor == editor.char_count() {
                    editor.accept_ghost();
                } else {
                    editor.move_end();
                }
            }
            Key::Up => editor.history_prev(),
            Key::Down => editor.history_next(),
            Key::Esc | Key::Unknown => {
                // 退出 Tab 补全候选列表（不提交、不清空输入）。
                editor.completions = None;
            }
        }

        // `exit` 内建会把标志置位；此处检测并干净退出（P1-#20）。
        if shell.exit_requested.is_some() {
            break;
        }

        editor.render(shell);
    }

    posix::disable_raw_mode();
    // 退出前触发 EXIT trap（与 main.rs 的 -c 路径对称）。run_str 会在开头
    // 清空 exit_requested，因此先保存、派发后恢复，免得 main 读不到显式
    // `exit N` 的退出码。
    let saved_exit = shell.exit_requested;
    if let Some(action) = shell.traps.get("EXIT").cloned() {
        if action != "-" {
            shell.run_in_error_context(
                crate::shell::ErrorContext::exit_trap(),
                &action,
            );
        }
    }
    shell.exit_requested = saved_exit;
    save_history(&hist_path, &editor.history);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_form_for_control_bytes() {
        assert_eq!(caret_letter('\u{1}'), 'A'); // ^A
        assert_eq!(caret_letter('\u{1a}'), 'Z'); // ^Z
        assert_eq!(caret_letter('\u{1b}'), '['); // ^[
        assert_eq!(caret_letter('\u{1f}'), '_'); // ^_
        assert_eq!(caret_letter('\u{7f}'), '?'); // ^?
    }

    #[test]
    fn ctrl_classification() {
        assert!(is_ctrl('\u{1a}')); // Ctrl+Z
        assert!(is_ctrl('\u{3}')); // Ctrl+C
        assert!(is_ctrl('\u{7f}')); // DEL
        assert!(!is_ctrl('a'));
        assert!(!is_ctrl('≤'));
    }

    #[test]
    fn disp_width_control_is_two() {
        assert_eq!(disp_width('\u{1a}'), 2); // 控制字符以 ^X 两格呈现
        assert_eq!(disp_width('a'), 1);
        assert_eq!(disp_width('中'), 2); // 宽字符
    }

    #[test]
    fn visible_width_strips_ansi_and_counts_control() {
        assert_eq!(visible_width("wash$ "), 6);
        // ANSI 颜色序列对可见宽度无贡献。
        assert_eq!(visible_width("\x1b[32mwash\x1b[0m"), 4);
        // 裸控制字节按两格计。
        assert_eq!(visible_width("\u{1a}"), 2);
    }

    #[test]
    fn common_prefix_is_char_safe() {
        // 中文文件名共享多字节首字节边界，绝不应当 panic（P0-2）。
        let zh = vec!["你foo".to_string(), "你bar".to_string()];
        assert_eq!(common_prefix(&zh), "你");
        let ascii = vec!["abc".to_string(), "abd".to_string(), "ab".to_string()];
        assert_eq!(common_prefix(&ascii), "ab");
        assert_eq!(common_prefix(&[]), "");
        assert_eq!(common_prefix(&["x".to_string()]), "x");
    }

    #[test]
    fn cursor_pos_multibyte_multiline() {
        // buffer = "中文\nab", cursor right after 'a' (char index 4).
        // 中/文 are 3 bytes each, so a byte-index bug would report line 0.
        let mut ed = Editor::new(Vec::new());
        ed.buffer = "中文\nab".to_string();
        ed.cursor = 4;
        let lines: Vec<&str> = ed.buffer.split('\n').collect();
        assert_eq!(ed.cursor_pos(&lines), (1, 1));

        // caret at end of the CJK first line (after 文, char index 2):
        // display width = 2 + 2 = 4, still on line 0.
        ed.cursor = 2;
        assert_eq!(ed.cursor_pos(&lines), (0, 4));
    }

    #[test]
    fn render_prompt_expands_bash_escapes() {
        // 用一个最小 Shell 验证 $PS1 转义：\u \h \w \$ 与 $VAR 展开。
        let mut sh = crate::shell::Shell::new();
        sh.env.insert("MYVAR".to_string(), "hi".to_string());
        sh.cwd = "/home/wbw".to_string();
        let p = render_prompt("\\u@\\h \\w \\$ $MYVAR", &sh);
        assert!(p.contains("@"));
        assert!(p.contains("/home/wbw"));
        assert!(p.contains("hi"));
        assert!(p.ends_with("hi") || p.contains('$'));
        // \a 必须产出 BEL 而非被丢弃，否则 OSC 标题序列无法终止。
        assert!(render_prompt("\\a", &sh).contains('\u{7}'));
    }
}
