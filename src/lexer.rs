// Phase 1 lexer: tokenizes a command line into words made of structured
// `WordPart`s so the expander can later perform bash-style expansion.
//
// A word is `Vec<WordPart>`. Variable-like parts carry a `quoted` flag
// (true when they originated inside double quotes) so the expander knows
// whether to suppress word-splitting and globbing for that part.
//
// Detected (not yet evaluated) expansions:
//   $VAR  ${VAR}  $(cmd)  `cmd`  $((expr))  ~  {a,b}  {a..b}
//
// `2>` vs `2 >` adjacency for fd redirects is preserved (see Phase 0 notes).

use crate::error::{Result, WashError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordPart {
    Text(String, bool), // literal text, quoted flag
    Var(String, bool),  // $name (name may be a special char: ?, $, @, *, #, 0-9, !, -)
    Braced(String, bool), // ${ ... } body (without braces)
    CmdSub(String, bool), // $( ... ) body
    Backtick(String, bool), // ` ... ` body
    Arith(String, bool), // $(( ... )) body
    Tilde,              // ~ at word start (unquoted)
    Brace(Vec<String>), // {a,b,c} alternatives (no nesting in Phase 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirKind {
    In,
    Out,
    Append,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(Vec<WordPart>),
    Pipe,
    Semicolon,
    Background,
    Redirect { kind: RedirKind, fd: Option<i32> },
}

fn take_digit_prefix(word: &mut String) -> Option<i32> {
    if !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()) {
        let fd = word.parse::<i32>().ok();
        word.clear();
        fd
    } else {
        None
    }
}

fn read_paren(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut depth = 1;
    let mut s = String::new();
    let mut j = i;
    while j < chars.len() {
        match chars[j] {
            '(' => {
                depth += 1;
                s.push('(');
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((s, j + 1));
                }
                s.push(')');
            }
            '\\' => {
                if j + 1 < chars.len() {
                    s.push('\\');
                    s.push(chars[j + 1]);
                    j += 2;
                    continue;
                }
                s.push('\\');
            }
            c => s.push(c),
        }
        j += 1;
    }
    None
}

fn read_brace(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut depth = 1;
    let mut s = String::new();
    let mut j = i;
    while j < chars.len() {
        match chars[j] {
            '{' => {
                depth += 1;
                s.push('{');
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((s, j + 1));
                }
                s.push('}');
            }
            '\\' => {
                if j + 1 < chars.len() {
                    s.push('\\');
                    s.push(chars[j + 1]);
                    j += 2;
                    continue;
                }
                s.push('\\');
            }
            c => s.push(c),
        }
        j += 1;
    }
    None
}

fn brace_alternatives(inner: &str) -> Option<Vec<String>> {
    if let Some(range) = parse_range(inner) {
        return Some(range);
    }
    if inner.contains(',') {
        let mut alts = Vec::new();
        let mut cur = String::new();
        let mut depth = 0;
        for c in inner.chars() {
            match c {
                ',' if depth == 0 => {
                    alts.push(std::mem::take(&mut cur));
                }
                '{' => {
                    depth += 1;
                    cur.push(c);
                }
                '}' => {
                    depth -= 1;
                    cur.push(c);
                }
                _ => cur.push(c),
            }
        }
        alts.push(cur);
        if alts.len() >= 2 {
            return Some(alts);
        }
    }
    None
}

fn parse_range(inner: &str) -> Option<Vec<String>> {
    let parts: Vec<&str> = inner.splitn(3, "..").collect();
    if parts.len() < 2 {
        return None;
    }
    let start = parts[0];
    let end = parts[1];
    let inc: i64 = if parts.len() == 3 {
        parts[2].parse().ok()?
    } else {
        1
    };
    if let (Ok(a), Ok(b)) = (start.parse::<i64>(), end.parse::<i64>()) {
        if inc == 0 {
            return None;
        }
        let mut v = Vec::new();
        let mut x = a;
        if inc > 0 {
            while x <= b {
                v.push(x.to_string());
                x += inc;
            }
        } else {
            while x >= b {
                v.push(x.to_string());
                x += inc;
            }
        }
        return Some(v);
    }
    let sc: Vec<char> = start.chars().collect();
    let ec: Vec<char> = end.chars().collect();
    if sc.len() == 1 && ec.len() == 1 {
        let a = sc[0] as i32;
        let b = ec[0] as i32;
        if a <= b {
            let mut v = Vec::new();
            let mut x = a;
            while x <= b {
                v.push(((x as u8) as char).to_string());
                x += 1;
            }
            return Some(v);
        }
    }
    None
}

pub fn tokenize(input: &str) -> Result<Vec<Token>> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let n = chars.len();

    let mut parts: Vec<WordPart> = Vec::new();
    let mut pending = String::new();
    let mut pending_quoted = false;
    let mut in_single = false;
    let mut in_double = false;

    // Flush the current `pending` literal buffer into a `Text` part. `quoted`
    // must reflect whether the buffered characters came from a quoted context
    // (single or double quotes) — only unquoted text is subject to word
    // splitting and globbing downstream.
    let flush_text = |pending: &mut String, parts: &mut Vec<WordPart>, quoted: bool| {
        if !pending.is_empty() {
            parts.push(WordPart::Text(std::mem::take(pending), quoted));
        }
    };

    while i < n {
        let c = chars[i];

        if in_single {
            if c == '\'' {
                flush_text(&mut pending, &mut parts, pending_quoted);
                pending_quoted = false;
                in_single = false;
            } else {
                pending.push(c);
            }
            i += 1;
            continue;
        }
        if in_double {
            match c {
                '"' => {
                    flush_text(&mut pending, &mut parts, pending_quoted);
                    pending_quoted = false;
                    in_double = false;
                }
                '$' => {
                    flush_text(&mut pending, &mut parts, pending_quoted);
                    i += 1;
                    if i < n && chars[i] == '(' {
                        if i + 1 < n && chars[i + 1] == '(' {
                            if let Some((body, ni)) = read_paren(&chars, i + 2) {
                                if ni < n && chars[ni] == ')' {
                                    parts.push(WordPart::Arith(body, true));
                                    i = ni + 1;
                                    continue;
                                }
                            }
                            parts.push(WordPart::Text("$(".to_string(), true));
                            i += 1;
                            continue;
                        }
                        if let Some((body, ni)) = read_paren(&chars, i + 1) {
                            parts.push(WordPart::CmdSub(body, true));
                            i = ni;
                            continue;
                        }
                        parts.push(WordPart::Text("$(".to_string(), true));
                        i += 1;
                        continue;
                    } else if i < n && chars[i] == '{' {
                        if let Some((body, ni)) = read_brace(&chars, i + 1) {
                            parts.push(WordPart::Braced(body, true));
                            i = ni;
                            continue;
                        }
                        parts.push(WordPart::Text("${".to_string(), true));
                        i += 1;
                        continue;
                    } else {
                        let name = read_var_name(&chars, i);
                        i += name.chars().count();
                        parts.push(WordPart::Var(name, true));
                        continue;
                    }
                }
                '`' => {
                    flush_text(&mut pending, &mut parts, pending_quoted);
                    let mut body = String::new();
                    i += 1;
                    while i < n && chars[i] != '`' {
                        body.push(chars[i]);
                        i += 1;
                    }
                    i += 1;
                    parts.push(WordPart::Backtick(body, true));
                    continue;
                }
                '\\' => {
                    if i + 1 < n {
                        pending.push(chars[i + 1]);
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                _ => pending.push(c),
            }
            i += 1;
            continue;
        }

        // unquoted context
        match c {
            '\'' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                pending_quoted = true;
                in_single = true;
                i += 1;
            }
            '"' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                pending_quoted = true;
                in_double = true;
                i += 1;
            }
            '\\' => {
                if i + 1 < n {
                    pending.push(chars[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            ' ' | '\t' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut parts)));
                }
                i += 1;
            }
            '|' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut parts)));
                }
                tokens.push(Token::Pipe);
                i += 1;
            }
            ';' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut parts)));
                }
                tokens.push(Token::Semicolon);
                i += 1;
            }
            '<' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut parts)));
                }
                let fd = take_digit_prefix(&mut pending);
                tokens.push(Token::Redirect {
                    kind: RedirKind::In,
                    fd,
                });
                i += 1;
            }
            '>' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut parts)));
                }
                let is_append = i + 1 < n && chars[i + 1] == '>';
                let fd = take_digit_prefix(&mut pending);
                let kind = if is_append {
                    RedirKind::Append
                } else {
                    RedirKind::Out
                };
                tokens.push(Token::Redirect { kind, fd });
                i += if is_append { 2 } else { 1 };
            }
            '&' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut parts)));
                }
                if i + 1 < n && chars[i + 1] == '>' {
                    tokens.push(Token::Redirect {
                        kind: RedirKind::Both,
                        fd: None,
                    });
                    i += 2;
                } else {
                    tokens.push(Token::Background);
                    i += 1;
                }
            }
            '$' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                i += 1;
                if i < n && chars[i] == '(' {
                    if i + 1 < n && chars[i + 1] == '(' {
                        if let Some((body, ni)) = read_paren(&chars, i + 2) {
                            if ni < n && chars[ni] == ')' {
                                parts.push(WordPart::Arith(body, false));
                                i = ni + 1;
                                continue;
                            }
                        }
                        parts.push(WordPart::Text("$(".to_string(), false));
                        i += 1;
                        continue;
                    }
                    if let Some((body, ni)) = read_paren(&chars, i + 1) {
                        parts.push(WordPart::CmdSub(body, false));
                        i = ni;
                        continue;
                    }
                    parts.push(WordPart::Text("$(".to_string(), false));
                    i += 1;
                    continue;
                } else if i < n && chars[i] == '{' {
                    if let Some((body, ni)) = read_brace(&chars, i + 1) {
                        parts.push(WordPart::Braced(body, false));
                        i = ni;
                        continue;
                    }
                    parts.push(WordPart::Text("${".to_string(), false));
                    i += 1;
                    continue;
                } else {
                    let name = read_var_name(&chars, i);
                    i += name.chars().count();
                    parts.push(WordPart::Var(name, false));
                    continue;
                }
            }
            '`' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                let mut body = String::new();
                i += 1;
                while i < n && chars[i] != '`' {
                    body.push(chars[i]);
                    i += 1;
                }
                i += 1;
                parts.push(WordPart::Backtick(body, false));
            }
            '{' => {
                if let Some((inner, ni)) = read_brace(&chars, i + 1) {
                    if let Some(alts) = brace_alternatives(&inner) {
                        flush_text(&mut pending, &mut parts, pending_quoted);
                        parts.push(WordPart::Brace(alts));
                        i = ni;
                        continue;
                    }
                }
                pending.push('{');
                i += 1;
            }
            '~' => {
                if pending.is_empty() && parts.is_empty() {
                    parts.push(WordPart::Tilde);
                    i += 1;
                } else {
                    pending.push('~');
                    i += 1;
                }
            }
            _ => {
                pending.push(c);
                i += 1;
            }
        }
    }

    flush_text(&mut pending, &mut parts, pending_quoted);
    if !parts.is_empty() {
        tokens.push(Token::Word(parts));
    }

    if in_single || in_double {
        return Err(WashError::syntax("unexpected EOF while looking for matching quote"));
    }

    for t in &tokens {
        if let Token::Background = t {
            return Err(WashError::syntax(
                "background jobs (`&`) are not supported yet (Phase 2)",
            ));
        }
    }

    Ok(tokens)
}

fn read_var_name(chars: &[char], i: usize) -> String {
    if i >= chars.len() {
        return String::new();
    }
    let c = chars[i];
    if c.is_ascii_alphabetic() || c == '_' {
        let mut name = String::new();
        let mut j = i;
        while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
            name.push(chars[j]);
            j += 1;
        }
        name
    } else {
        c.to_string()
    }
}
