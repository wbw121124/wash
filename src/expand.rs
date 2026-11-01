// Phase 1 expander: evaluate `WordPart` lists into the concrete words that
// the executor will run. Implements the bash expansion order (simplified):
// brace -> tilde -> parameter/command/arithmetic -> word-splitting -> glob.
//
// Quoted parts (originating inside double quotes) are NOT word-split or
// globbed. Arithmetic results are never split/globbed.

use std::collections::HashMap;

use crate::lexer::WordPart;
use crate::shell::VarAttrs;

pub struct ExpandCtx<'a> {
    pub env: &'a HashMap<String, String>,
    pub positional: &'a [String],
    pub last_status: i32,
    pub self_pid: i32,
    pub noglob: bool,
    pub nounset: bool,
    /// The value of `$0` (the shell name, or the script path while a script
    /// is being sourced).
    pub dollar_zero: &'a str,
    /// The variable attributes a command substitution inherits.
    pub attrs: &'a VarAttrs,
}

/// Report an unbound variable the way bash does under `set -u` and abort the
/// shell: a non-interactive bash prints the diagnostic and exits with 127.
fn unbound_variable(name: &str) -> ! {
    eprintln!("wash: {}: unbound variable", name);
    std::process::exit(127);
}

/// Expand a command's raw argv (list of words) into the final argv strings.
pub fn expand_argv(raw: &[Vec<WordPart>], ctx: &ExpandCtx) -> Vec<String> {
    let mut out = Vec::new();
    for word in raw {
        out.extend(expand_word(word, ctx));
    }
    out
}

/// Expand a word as a scalar assignment RHS: concatenate part values WITHOUT
/// word-splitting or globbing (matching bash assignment semantics).
pub fn expand_scalar(parts: &[WordPart], ctx: &ExpandCtx) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            WordPart::Text(s, _) => out.push_str(s),
            WordPart::Var(name, _) => out.push_str(&eval_var(name, ctx)),
            WordPart::Braced(body, _) => out.push_str(&eval_braced(body, ctx)),
            WordPart::CmdSub(body, _) | WordPart::Backtick(body, _) => {
                out.push_str(&crate::executor::capture_command(body, ctx.env, ctx.attrs));
            }
            WordPart::Arith(body, _) => out.push_str(&eval_arith(body, ctx)),
            WordPart::Tilde => {
                out.push_str(&ctx.env.get("HOME").cloned().unwrap_or_default())
            }
            WordPart::Brace(alts) => out.push_str(&alts.concat()),
        }
    }
    out
}

/// Expand a single word into 0+ resulting words (splitting + globbing for
/// unquoted expansions).
pub fn expand_word(parts: &[WordPart], ctx: &ExpandCtx) -> Vec<String> {
    let mut candidates: Vec<String> = vec![String::new()];
    for part in parts {
        match part {
            WordPart::Text(s, quoted) => {
                let mut next: Vec<String> =
                    candidates.iter().map(|c| format!("{}{}", c, s)).collect();
                if !*quoted {
                    if ctx.noglob {
                        // `set -f` / `set -o noglob`: literal, no globbing.
                    } else {
                        // Unquoted literal text is still subject to globbing.
                        let mut globbed = Vec::new();
                        for w in next.drain(..) {
                            globbed.extend(glob(&w));
                        }
                        next = globbed;
                    }
                }
                candidates = next;
            }
            WordPart::Tilde => {
                let home = ctx.env.get("HOME").cloned().unwrap_or_else(|| "/".to_string());
                candidates = candidates
                    .iter()
                    .map(|c| if c.is_empty() { home.clone() } else { format!("{}~", c) })
                    .collect();
            }
            WordPart::Var(name, quoted) => {
                let val = eval_var(name, ctx);
                candidates = apply_value(candidates, &val, *quoted, ctx);
            }
            WordPart::Braced(body, quoted) => {
                let val = eval_braced(body, ctx);
                candidates = apply_value(candidates, &val, *quoted, ctx);
            }
            WordPart::CmdSub(body, quoted) | WordPart::Backtick(body, quoted) => {
                let val = crate::executor::capture_command(body, ctx.env, ctx.attrs);
                candidates = apply_value(candidates, &val, *quoted, ctx);
            }
            WordPart::Arith(body, _) => {
                let val = eval_arith(body, ctx);
                candidates = candidates.iter().map(|c| format!("{}{}", c, val)).collect();
            }
            WordPart::Brace(alts) => {
                let mut next = Vec::with_capacity(candidates.len() * alts.len());
                for c in &candidates {
                    for a in alts {
                        next.push(format!("{}{}", c, a));
                    }
                }
                candidates = next;
            }
        }
    }
    candidates
}

/// Append `val` to each candidate, applying word-splitting + globbing unless
/// `quoted`.
fn apply_value(candidates: Vec<String>, val: &str, quoted: bool, ctx: &ExpandCtx) -> Vec<String> {
    if quoted {
        return candidates.iter().map(|c| format!("{}{}", c, val)).collect();
    }
    let fields = split_ifs(val);
    let mut next = Vec::new();
    for c in &candidates {
        if fields.is_empty() {
            next.push(c.clone());
        } else {
            for f in &fields {
                if ctx.noglob {
                    next.push(format!("{}{}", c, f));
                } else {
                    next.push(format!("{}{}", c, glob_one(f)));
                }
            }
        }
    }
    next
}

fn split_ifs(val: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    for ch in val.chars() {
        if ch == ' ' || ch == '\t' || ch == '\n' {
            if !cur.is_empty() {
                fields.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        fields.push(cur);
    }
    fields
}

// ---------------------------------------------------------------------------
// Parameter evaluation
// ---------------------------------------------------------------------------

fn eval_var(name: &str, ctx: &ExpandCtx) -> String {
    match name {
        "" => String::new(),
        "?" => ctx.last_status.to_string(),
        "$" => ctx.self_pid.to_string(),
        "#" => ctx.positional.len().to_string(),
        "@" => ctx.positional.join(" "),
        "*" => {
            // `$*` is an error under `set -u` when there are no positional
            // parameters (`$@` never is).
            if ctx.nounset && ctx.positional.is_empty() {
                unbound_variable("$*");
            }
            ctx.positional.join(" ")
        }
        "0" => ctx.dollar_zero.to_string(),
        "!" => String::new(), // last background pid (Phase 2)
        "-" => String::new(), // shell options (Phase 4+)
        _ => {
            if let Some(idx) = name.parse::<usize>().ok() {
                if idx >= 1 && idx <= ctx.positional.len() {
                    return ctx.positional[idx - 1].clone();
                }
                if ctx.nounset {
                    unbound_variable(&format!("${}", idx));
                }
                return String::new();
            }
            match ctx.env.get(name) {
                Some(value) => value.clone(),
                None => {
                    if ctx.nounset {
                        unbound_variable(name);
                    }
                    String::new()
                }
            }
        }
    }
}

fn eval_braced(body: &str, ctx: &ExpandCtx) -> String {
    // forms: name, name:-w, name:=w, name:+w, name:?w, #name, name:off,
    // name:off:len, name#pat, name##pat, name%pat, name%%pat,
    // name/pat/repl, name//pat/repl, name^, name^^, name,, name,,
    // !name (indirect)
    if body.starts_with('#') {
        let name = &body[1..];
        if name.is_empty() {
            return ctx.positional.len().to_string();
        }
        if !ctx.env.contains_key(name) {
            if ctx.nounset {
                unbound_variable(name);
            }
            return "0".to_string();
        }
        return ctx.env.get(name).map(|v| v.len().to_string()).unwrap_or_else(|| "0".to_string());
    }
    if body.starts_with('!') && !body.contains(':') && !body.contains('/') && !body.contains('%')
        && !body.contains('#')
    {
        // indirect expansion ${!VAR}
        let name = &body[1..];
        let target = ctx.env.get(name).cloned().unwrap_or_default();
        return ctx.env.get(&target).cloned().unwrap_or_default();
    }

    let (name, op, arg) = split_braced(body);

    let value = ctx.env.get(name).cloned().unwrap_or_default();
    let unset_or_empty = value.is_empty();

    match op {
        ":-" => {
            if unset_or_empty {
                arg.to_string()
            } else {
                value
            }
        }
        ":=" => {
            if unset_or_empty {
                ctx.env.get(name); // (assignment handled by caller mutably; Phase 1 returns default)
                arg.to_string()
            } else {
                value
            }
        }
        ":+" => {
            if unset_or_empty {
                String::new()
            } else {
                arg.to_string()
            }
        }
        ":?" => {
            if unset_or_empty {
                eprintln!("wash: {}: {}", name, arg);
                std::process::exit(1);
            }
            value
        }
        ":" => {
            // substring ${name:offset} or ${name:offset:length}
            substring(&value, arg)
        }
        "#" => strip_prefix(&value, arg, false),
        "##" => strip_prefix(&value, arg, true),
        "%" => strip_suffix(&value, arg, false),
        "%%" => strip_suffix(&value, arg, true),
        "/" | "//" => replace_pattern(&value, arg, op == "//"),
        "^" => {
            let mut c = value.chars();
            match c.next() {
                Some(f) => format!("{}{}", f.to_uppercase().collect::<String>(), c.collect::<String>()),
                None => value.to_string(),
            }
        }
        "^^" => value.to_uppercase(),
        "," => {
            let mut c = value.chars();
            match c.next() {
                Some(f) => format!("{}{}", f.to_lowercase().collect::<String>(), c.collect::<String>()),
                None => value.to_string(),
            }
        }
        ",," => value.to_lowercase(),
        "" => {
            // A plain `${NAME}` reference: `set -u` makes an unset variable
            // an error.
            if !ctx.env.contains_key(name) && ctx.nounset {
                unbound_variable(name);
            }
            value
        }
        _ => value,
    }
}

fn split_braced(body: &str) -> (&str, &str, &str) {
    // Find the first operator at top level. Two-char operators take priority.
    let b = body.as_bytes();
    for (i, &ch) in b.iter().enumerate() {
        match ch as char {
            ':' => {
                let nxt = b.get(i + 1).copied().map(|c| c as char);
                if matches!(nxt, Some('-' | '=' | '+' | '?')) {
                    return (&body[..i], &body[i..i + 2], &body[i + 2..]);
                }
                return (&body[..i], ":", &body[i + 1..]);
            }
            '#' => {
                if b.get(i + 1).copied() == Some(b'#') {
                    return (&body[..i], "##", &body[i + 2..]);
                }
                return (&body[..i], "#", &body[i + 1..]);
            }
            '%' => {
                if b.get(i + 1).copied() == Some(b'%') {
                    return (&body[..i], "%%", &body[i + 2..]);
                }
                return (&body[..i], "%", &body[i + 1..]);
            }
            '/' => {
                if b.get(i + 1).copied() == Some(b'/') {
                    return (&body[..i], "//", &body[i + 2..]);
                }
                return (&body[..i], "/", &body[i + 1..]);
            }
            '^' => {
                if b.get(i + 1).copied() == Some(b'^') {
                    return (&body[..i], "^^", &body[i + 2..]);
                }
                return (&body[..i], "^", &body[i + 1..]);
            }
            ',' => {
                if b.get(i + 1).copied() == Some(b',') {
                    return (&body[..i], ",,", &body[i + 2..]);
                }
                return (&body[..i], ",", &body[i + 1..]);
            }
            _ => {}
        }
    }
    (body, "", "")
}

fn substring(value: &str, arg: &str) -> String {
    let mut parts = arg.splitn(2, ':');
    let off_s = parts.next().unwrap_or("");
    let len_s = parts.next().unwrap_or("");
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len() as i64;
    let mut off = off_s.parse::<i64>().unwrap_or(0);
    if off < 0 {
        off = n + off;
    }
    if off < 0 {
        off = 0;
    }
    if off > n {
        off = n;
    }
    let start = off as usize;
    let taken = if len_s.is_empty() {
        chars.len() - start
    } else {
        let len = len_s.parse::<i64>().unwrap_or(0);
        if len < 0 {
            0
        } else {
            len as usize
        }
    };
    chars[start..].iter().take(taken).collect()
}

fn strip_prefix(value: &str, pat: &str, longest: bool) -> String {
    if pat.is_empty() {
        return value.to_string();
    }
    let chars: Vec<char> = value.chars().collect();
    let patc: Vec<char> = pat.chars().collect();
    if longest {
        // remove the LONGEST matching prefix: largest k where chars[..k] matches
        for k in (0..=chars.len()).rev() {
            if glob_match(&chars[..k], &patc) {
                return chars[k..].iter().collect();
            }
        }
    } else {
        // remove the SHORTEST matching prefix: smallest k (first match scanning up)
        for k in 0..=chars.len() {
            if glob_match(&chars[..k], &patc) {
                return chars[k..].iter().collect();
            }
        }
    }
    value.to_string()
}

fn strip_suffix(value: &str, pat: &str, longest: bool) -> String {
    if pat.is_empty() {
        return value.to_string();
    }
    let chars: Vec<char> = value.chars().collect();
    let patc: Vec<char> = pat.chars().collect();
    if longest {
        // remove the LONGEST matching suffix: smallest k where chars[k..] matches
        for k in 0..=chars.len() {
            if glob_match(&chars[k..], &patc) {
                return chars[..k].iter().collect();
            }
        }
    } else {
        // remove the SHORTEST matching suffix: largest k (first match scanning down)
        for k in (0..=chars.len()).rev() {
            if glob_match(&chars[k..], &patc) {
                return chars[..k].iter().collect();
            }
        }
    }
    value.to_string()
}

fn replace_pattern(value: &str, arg: &str, all: bool) -> String {
    // arg is "pattern/replacement"
    if let Some(slash) = arg.find('/') {
        let pat = &arg[..slash];
        let repl = &arg[slash + 1..];
        if pat.is_empty() {
            return value.to_string();
        }
        if all {
            value.replacen(pat, repl, usize::MAX)
        } else {
            value.replacen(pat, repl, 1)
        }
    } else {
        value.replacen(arg, "", usize::MAX)
    }
}

// ---------------------------------------------------------------------------
// Globbing
// ---------------------------------------------------------------------------

fn has_glob(p: &str) -> bool {
    p.contains('*') || p.contains('?') || (p.contains('[') && p.contains(']'))
}

fn glob_one(pattern: &str) -> String {
    let matches = glob(pattern);
    if matches.is_empty() {
        pattern.to_string()
    } else {
        matches.join(" ")
    }
}

/// Translate an msys-style drive path (`/d/foo/bar`) to a native Windows path
/// (`D:\foo\bar`) so `std::fs::read_dir` can enumerate it. Returns `None` for
/// paths that are not msys drive roots (e.g. `/usr/bin`).
fn translate_msys_path(p: &str) -> Option<String> {
    let chars: Vec<char> = p.chars().collect();
    if chars.len() >= 3 && chars[0] == '/' && chars[1].is_ascii_alphabetic() && chars[2] == '/' {
        let drive = chars[1].to_ascii_uppercase();
        let rest: String = chars[3..].iter().collect();
        return Some(format!("{}:\\{}", drive, rest.replace('/', "\\")));
    }
    None
}

/// Expand a filename pattern to matching paths. If no matches, returns the
/// pattern itself (bash default; nullglob is off in Phase 1).
pub fn glob(pattern: &str) -> Vec<String> {
    if !has_glob(pattern) {
        return vec![pattern.to_string()];
    }
    let (dir, base) = split_dir(pattern);
    // Resolve the directory: msys drive paths must become native Windows paths
    // for `std::fs::read_dir`, but we keep the msys form for the output.
    let (dir_for_read, dir_for_out) = match translate_msys_path(&dir) {
        Some(win) => (win, dir.clone()),
        None => (dir.clone(), dir.clone()),
    };
    let patc: Vec<char> = base.chars().collect();
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir_for_read) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let nc: Vec<char> = name.chars().collect();
            if glob_match(&nc, &patc) {
                out.push(if dir_for_out.is_empty() {
                    name
                } else {
                    format!("{}/{}", dir_for_out, name)
                });
            }
        }
    }
    out.sort();
    // bash default (nullglob off): a pattern with glob metacharacters that
    // matches nothing yields the literal pattern, not an empty result.
    if out.is_empty() {
        return vec![pattern.to_string()];
    }
    out
}

fn split_dir(pattern: &str) -> (String, String) {
    match pattern.rfind('/') {
        Some(i) => (pattern[..i].to_string(), pattern[i + 1..].to_string()),
        None => ("".to_string(), pattern.to_string()),
    }
}

/// Simple glob matcher supporting *, ?, and [...] (no extglob yet).
pub fn glob_match(s: &[char], pat: &[char]) -> bool {
    // iterative backtracking
    let (mut si, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None, (0usize, 0usize));
    while si < s.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == s[si]) {
            si += 1;
            pi += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            mark = (si, pi);
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            si = mark.0 + 1;
            mark.0 = si;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}

// ---------------------------------------------------------------------------
// Arithmetic $(( ... ))
// ---------------------------------------------------------------------------

pub fn eval_arith(body: &str, ctx: &ExpandCtx) -> String {
    let mut p = ArithParser {
        chars: body.chars().collect(),
        pos: 0,
        ctx,
    };
    match p.parse_expr() {
        Ok(v) => v.to_string(),
        Err(_) => "0".to_string(),
    }
}

struct ArithParser<'a> {
    chars: Vec<char>,
    pos: usize,
    ctx: &'a ExpandCtx<'a>,
}

impl<'a> ArithParser<'a> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_expr(&mut self) -> Result<i64, ()> {
        self.parse_bor()
    }
    fn parse_bor(&mut self) -> Result<i64, ()> {
        let mut v = self.parse_bxor()?;
        loop {
            self.skip_ws();
            if self.peek() == Some('|') && self.chars.get(self.pos + 1).copied() != Some('|') {
                self.pos += 1;
                v |= self.parse_bxor()?;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_bxor(&mut self) -> Result<i64, ()> {
        let mut v = self.parse_band()?;
        loop {
            self.skip_ws();
            if self.peek() == Some('^') {
                self.pos += 1;
                v ^= self.parse_band()?;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_band(&mut self) -> Result<i64, ()> {
        let mut v = self.parse_eq()?;
        loop {
            self.skip_ws();
            if self.peek() == Some('&') && self.chars.get(self.pos + 1).copied() != Some('&') {
                self.pos += 1;
                v &= self.parse_eq()?;
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_eq(&mut self) -> Result<i64, ()> {
        let mut v = self.parse_rel()?;
        loop {
            self.skip_ws();
            let p = self.peek();
            if p == Some('=') && self.chars.get(self.pos + 1).copied() == Some('=') {
                self.pos += 2;
                let r = self.parse_rel()?;
                v = if v == r { 1 } else { 0 };
            } else if p == Some('!') && self.chars.get(self.pos + 1).copied() == Some('=') {
                self.pos += 2;
                let r = self.parse_rel()?;
                v = if v != r { 1 } else { 0 };
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_rel(&mut self) -> Result<i64, ()> {
        let mut v = self.parse_shift()?;
        loop {
            self.skip_ws();
            let p = self.peek();
            let two = self.chars.get(self.pos + 1).copied();
            let op = match (p, two) {
                (Some('<'), Some('<')) => Some(0),
                (Some('>'), Some('>')) => Some(1),
                (Some('<'), _) => Some(2),
                (Some('>'), _) => Some(3),
                _ => None,
            };
            if let Some(kind) = op {
                if kind < 2 {
                    self.pos += 2;
                } else {
                    self.pos += 1;
                }
                let r = self.parse_shift()?;
                v = match kind {
                    0 => v << r,
                    1 => v >> r,
                    2 => {
                        if v < r {
                            1
                        } else {
                            0
                        }
                    }
                    _ => {
                        if v > r {
                            1
                        } else {
                            0
                        }
                    }
                };
            } else {
                break;
            }
        }
        Ok(v)
    }
    fn parse_shift(&mut self) -> Result<i64, ()> {
        self.parse_add()
    }
    fn parse_add(&mut self) -> Result<i64, ()> {
        let mut v = self.parse_mul()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.pos += 1;
                    v += self.parse_mul()?;
                }
                Some('-') => {
                    self.pos += 1;
                    v -= self.parse_mul()?;
                }
                _ => break,
            }
        }
        Ok(v)
    }
    fn parse_mul(&mut self) -> Result<i64, ()> {
        let mut v = self.parse_unary()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    v *= self.parse_unary()?;
                }
                Some('/') => {
                    self.pos += 1;
                    let r = self.parse_unary()?;
                    v = if r == 0 { 0 } else { v / r };
                }
                Some('%') => {
                    self.pos += 1;
                    let r = self.parse_unary()?;
                    v = if r == 0 { 0 } else { v % r };
                }
                _ => break,
            }
        }
        Ok(v)
    }
    fn parse_unary(&mut self) -> Result<i64, ()> {
        self.skip_ws();
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Ok(-self.parse_unary()?)
            }
            Some('+') => {
                self.pos += 1;
                self.parse_unary()
            }
            Some('~') => {
                self.pos += 1;
                Ok(!self.parse_unary()?)
            }
            Some('!') => {
                self.pos += 1;
                Ok(if self.parse_unary()? == 0 { 1 } else { 0 })
            }
            _ => self.parse_pow(),
        }
    }
    fn parse_pow(&mut self) -> Result<i64, ()> {
        let base = self.parse_primary()?;
        self.skip_ws();
        if self.peek() == Some('*') && self.chars.get(self.pos + 1).copied() == Some('*') {
            self.pos += 2;
            let exp = self.parse_unary()?;
            Ok(base.pow(exp as u32))
        } else {
            Ok(base)
        }
    }
    fn parse_primary(&mut self) -> Result<i64, ()> {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let v = self.parse_expr()?;
                self.skip_ws();
                if self.peek() == Some(')') {
                    self.pos += 1;
                }
                Ok(v)
            }
            Some('$') => {
                self.pos += 1;
                // $name or ${name}
                let name = if self.peek() == Some('{') {
                    self.pos += 1;
                    let mut n = String::new();
                    while let Some(c) = self.peek() {
                        if c == '}' {
                            self.pos += 1;
                            break;
                        }
                        n.push(c);
                        self.pos += 1;
                    }
                    n
                } else {
                    let mut n = String::new();
                    while let Some(c) = self.peek() {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            n.push(c);
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    n
                };
                Ok(self.ctx.env.get(&name).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0))
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                // bare variable name (arithmetic allows X*3)
                let mut n = String::new();
                while let Some(ch) = self.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        n.push(ch);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Ok(self.ctx.env.get(&n).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0))
            }
            Some(c) if c.is_ascii_digit() => {
                let mut s = String::new();
                while let Some(d) = self.peek() {
                    if d.is_ascii_digit() {
                        s.push(d);
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Ok(s.parse::<i64>().unwrap_or(0))
            }
            _ => Err(()),
        }
    }
}
