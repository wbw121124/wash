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
    /// `N>&M`, `N<&M`, `>&N`, `<&N`, `>&-`: duplicate/close a descriptor.
    /// The target is pushed as the following `Token::Word`.
    Dup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(Vec<WordPart>),
    /// A reserved word in command position (`if`, `then`, `elif`, `else`,
    /// `fi`).  Bash only recognises a reserved word where a command may
    /// start, so `echo fi` still passes "fi" as an ordinary argument.
    Reserved(String),
    /// A compound command (`if ... fi`) that has already been parsed.
    Compound(crate::parser::Compound),
    /// A shell function definition (`NAME() { ... }` or `function NAME { ... }`).
    /// Carries the function name; the body lives in `CommandList::func_defs`.
    FunctionDef(String),
    Pipe,
    /// The `&&` operator: run the next pipeline only when the previous one
    /// succeeded.
    AndAnd,
    /// The `||` operator: run the next pipeline only when the previous one
    /// failed.
    OrOr,
    Semicolon,
    Background,
    Redirect { kind: RedirKind, fd: Option<i32> },
}

/// The reserved words wash recognises.  A reserved word is only special
/// where a command may start; everywhere else it is an ordinary word.
///
/// The compound-command keywords (`if`/`then`/`elif`/`else`/`fi` already
/// present) are joined here by the loop / case / function keywords so that
/// `for`, `while`, `until`, `case`, `do`, `done`, `esac` and `function`
/// are recognised in command position and handed to the parser as reserved
/// tokens. `in` is intentionally omitted: it only ever appears inside a
/// `for`/`case` body (never at a command start), so the parser recognises
/// it positionally by its text.
pub const RESERVED_WORDS: &[&str] = &[
    "elif", "else", "fi", "if", "then", "for", "while", "until", "do", "done", "case", "esac",
    "function",
];

/// Push a word token.  A word that starts a command and spells a reserved
/// word becomes a `Reserved` token; every other word stays a `Word`.
///
/// `at_command_start` tells whether a command may start at this position
/// and `redirect_target` tells whether the word is the target of a
/// redirection (such a word is neither a command word nor a reserved word).
fn push_word(
    tokens: &mut Vec<Token>,
    parts: Vec<WordPart>,
    at_command_start: &mut bool,
    redirect_target: &mut bool,
) {
    if *redirect_target {
        // The word is the target of a redirection: it is not a command word
        // and it does not change the command position.
        *redirect_target = false;
        tokens.push(Token::Word(parts));
        return;
    }
    let reserved = match parts.as_slice() {
        [WordPart::Text(text, false)] => RESERVED_WORDS.contains(&text.as_str()),
        _ => false,
    };
    if reserved && *at_command_start {
        let name = match &parts[0] {
            WordPart::Text(text, _) => text.clone(),
            _ => String::new(),
        };
        tokens.push(Token::Reserved(name));
        // A reserved word is followed by a command again.
        *at_command_start = true;
        return;
    }
    tokens.push(Token::Word(parts));
    *at_command_start = false;
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

/// Read the target of a descriptor-duplication redirection (`>&N`, `<&N`,
/// `>&-`, `>&$fd`).  Returns the target as a word (so a variable target still
/// goes through the normal expander) and the index just past it.
fn read_dup_target(chars: &[char], i: usize) -> (Vec<WordPart>, usize) {
    let mut j = i;
    if j < chars.len() && chars[j] == '-' {
        return (vec![WordPart::Text("-".to_string(), false)], j + 1);
    }
    if j < chars.len() && chars[j] == '$' {
        j += 1;
        let start = j;
        while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
            j += 1;
        }
        let name: String = chars[start..j].iter().collect();
        return (vec![WordPart::Var(name, false)], j);
    }
    let start = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    let text: String = chars[start..j].iter().collect();
    (vec![WordPart::Text(text, false)], j)
}

/// Read the body of a `$( ... )` / `$(( ... ))` up to its matching `)`.
/// Parentheses inside single or double quotes are literal, so `$(echo ")")`
/// must not terminate at the quoted `)`.
fn read_paren(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut depth = 1;
    let mut s = String::new();
    let mut j = i;
    let mut in_single = false;
    let mut in_double = false;
    while j < chars.len() {
        let c = chars[j];
        // Inside quotes every character is copied verbatim; only the matching
        // quote (and backslash escapes in double quotes) is special.
        if in_single {
            s.push(c);
            if c == '\'' {
                in_single = false;
            }
            j += 1;
            continue;
        }
        if in_double {
            if c == '\\' && j + 1 < chars.len() {
                s.push(c);
                s.push(chars[j + 1]);
                j += 2;
                continue;
            }
            s.push(c);
            if c == '"' {
                in_double = false;
            }
            j += 1;
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                s.push(c);
            }
            '"' => {
                in_double = true;
                s.push(c);
            }
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
            _ => s.push(c),
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

    // Whether a command may start at the current position.  Reserved words
    // are only recognised there.
    let mut at_command_start = true;
    // Set right after a redirection: the next word is the redirection's
    // target, not a command word.
    let mut redirect_target = false;

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
                // An empty quoted string is still a word (`""`).
                if parts.is_empty() {
                    parts.push(WordPart::Text(String::new(), true));
                }
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
                    // An empty quoted string is still a word (`""`).
                    if parts.is_empty() {
                        parts.push(WordPart::Text(String::new(), true));
                    }
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
                    // Inside double quotes bash only strips the backslash when it
                    // precedes one of `"` `` ` `` `$` `\`; a backslash-newline is a
                    // line continuation (both dropped); every other char keeps the
                    // backslash. The outer `i += 1` consumes the backslash itself,
                    // so here we consume only the escaped character.
                    if i + 1 < n {
                        let nxt = chars[i + 1];
                        if matches!(nxt, '"' | '`' | '$' | '\\') {
                            pending.push(nxt);
                        } else if nxt == '\n' {
                            // line continuation: drop both
                        } else {
                            pending.push('\\');
                            pending.push(nxt);
                        }
                        i += 1;
                    }
                    // Else: trailing backslash → the quote is unterminated and the
                    // caller reports the EOF error.
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
                    // A backslash-newline is a line continuation: both
                    // characters are dropped.
                    if chars[i + 1] != '\n' {
                        pending.push(chars[i + 1]);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            ' ' | '\t' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                i += 1;
            }
            '\n' => {
                // A newline terminates the current command, exactly like `;`.
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                tokens.push(Token::Semicolon);
                at_command_start = true;
                i += 1;
            }
            '|' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                // `||` is the OR operator; a single `|` still starts a
                // pipeline.
                if i + 1 < n && chars[i + 1] == '|' {
                    tokens.push(Token::OrOr);
                    i += 2;
                } else {
                    tokens.push(Token::Pipe);
                    i += 1;
                }
                at_command_start = true;
            }
            ';' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                tokens.push(Token::Semicolon);
                at_command_start = true;
                i += 1;
            }
            '<' => {
                // A digit run immediately before the operator is the redirected
                // descriptor (`0<file`, `2>err`).  It MUST be consumed before
                // flushing `pending` into a word, otherwise it becomes an extra
                // argument and the fd is lost.
                let fd = take_digit_prefix(&mut pending);
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                if i + 1 < n && chars[i + 1] == '&' {
                    // `<&N` / `<&-`: duplicate or close an input descriptor.
                    let (target, ni) = read_dup_target(&chars, i + 2);
                    tokens.push(Token::Redirect {
                        kind: RedirKind::Dup,
                        fd: Some(fd.unwrap_or(0)),
                    });
                    tokens.push(Token::Word(target));
                    redirect_target = false;
                    i = ni;
                } else {
                    tokens.push(Token::Redirect {
                        kind: RedirKind::In,
                        fd,
                    });
                    redirect_target = true;
                    i += 1;
                }
            }
            '>' => {
                let fd = take_digit_prefix(&mut pending);
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                if i + 1 < n && chars[i + 1] == '&' {
                    // `>&N` / `>&-`: duplicate or close an output descriptor.
                    let (target, ni) = read_dup_target(&chars, i + 2);
                    tokens.push(Token::Redirect {
                        kind: RedirKind::Dup,
                        fd: Some(fd.unwrap_or(1)),
                    });
                    tokens.push(Token::Word(target));
                    redirect_target = false;
                    i = ni;
                } else {
                    let is_append = i + 1 < n && chars[i + 1] == '>';
                    let kind = if is_append {
                        RedirKind::Append
                    } else {
                        RedirKind::Out
                    };
                    tokens.push(Token::Redirect { kind, fd });
                    redirect_target = true;
                    i += if is_append { 2 } else { 1 };
                }
            }
            '&' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                if i + 1 < n && chars[i + 1] == '&' {
                    // `&&` is the AND operator.
                    tokens.push(Token::AndAnd);
                    i += 2;
                } else if i + 1 < n && chars[i + 1] == '>' {
                    tokens.push(Token::Redirect {
                        kind: RedirKind::Both,
                        fd: None,
                    });
                    redirect_target = true;
                    i += 2;
                } else {
                    tokens.push(Token::Background);
                    i += 1;
                }
                at_command_start = true;
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
                } else if i < n && chars[i] == '\'' {
                    // ANSI-C quoted string: $'...'
                    if let Some((text, ni)) = read_ansi_c(&chars, i - 1) {
                        flush_text(&mut pending, &mut parts, pending_quoted);
                        parts.push(WordPart::Text(text, true));
                        i = ni;
                        continue;
                    }
                    parts.push(WordPart::Text("$".to_string(), false));
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
                // Try brace expansion first (works mid-word too, e.g. `a{b,c}`).
                if let Some((inner, ni)) = read_brace(&chars, i + 1) {
                    if let Some(alts) = brace_alternatives(&inner) {
                        flush_text(&mut pending, &mut parts, pending_quoted);
                        parts.push(WordPart::Brace(alts));
                        i = ni;
                        continue;
                    }
                }
                // Not a brace expansion: `{` is a command-group / function-body
                // opener and a word delimiter.  It only starts a word at a
                // boundary, so when preceded by other text we keep it literal
                // (e.g. `a{` -> `a{`, matching bash).
                if pending.is_empty() && parts.is_empty() {
                    tokens.push(Token::Word(vec![WordPart::Text("{".to_string(), false)]));
                    at_command_start = true;
                    i += 1;
                } else {
                    pending.push('{');
                    i += 1;
                }
            }
            '(' | ')' => {
                // `(` and `)` are word delimiters in bash: `f() { ... }` must
                // tokenize as the separate words `f`, `(`, `)`, `{`.  Flush any
                // pending text first, then emit the delimiter as its own word.
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        &mut at_command_start,
                        &mut redirect_target,
                    );
                }
                tokens.push(Token::Word(vec![WordPart::Text(c.to_string(), false)]));
                at_command_start = true;
                i += 1;
            }
            '}' => {
                // `}` is a word delimiter (function-body / command-group closer)
                // when it stands at a word boundary; mid-word it stays literal
                // (e.g. `a}` -> `a}`).
                if pending.is_empty() && parts.is_empty() {
                    tokens.push(Token::Word(vec![WordPart::Text("}".to_string(), false)]));
                    at_command_start = true;
                    i += 1;
                } else {
                    pending.push('}');
                    i += 1;
                }
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
        push_word(
            &mut tokens,
            std::mem::take(&mut parts),
            &mut at_command_start,
            &mut redirect_target,
        );
    }

    if in_single || in_double {
        return Err(WashError::syntax("unexpected EOF while looking for matching quote"));
    }

    Ok(tokens)
}

/// Decode a bash ANSI-C quoted string (`$'...'`). `chars[i]` must be the `$`
/// and `chars[i + 1]` the opening quote. Returns the decoded text and the
/// index just after the closing quote.
fn read_ansi_c(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut j = i + 2;
    while j < chars.len() {
        let c = chars[j];
        if c == '\'' {
            return Some((out, j + 1));
        }
        if c != '\\' || j + 1 >= chars.len() {
            out.push(c);
            j += 1;
            continue;
        }
        let n = chars[j + 1];
        j += 2;
        match n {
            'a' => out.push('\u{07}'),
            'b' => out.push('\u{08}'),
            'e' | 'E' => out.push('\u{1b}'),
            'f' => out.push('\u{0c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{0b}'),
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            '?' => out.push('?'),
            '0'..='7' => {
                let mut val = n.to_digit(8).unwrap();
                let mut cnt = 1;
                while cnt < 3 && j < chars.len() && chars[j].is_digit(8) {
                    val = val * 8 + chars[j].to_digit(8).unwrap();
                    j += 1;
                    cnt += 1;
                }
                out.push((val & 0xff) as u8 as char);
            }
            'x' => {
                let mut val = 0u32;
                let mut cnt = 0;
                while cnt < 2 && j < chars.len() && chars[j].is_ascii_hexdigit() {
                    val = val * 16 + chars[j].to_digit(16).unwrap();
                    j += 1;
                    cnt += 1;
                }
                if cnt == 0 {
                    out.push_str("\\x");
                } else {
                    out.push((val & 0xff) as u8 as char);
                }
            }
            'u' | 'U' => {
                let max = if n == 'u' { 4 } else { 8 };
                let mut val = 0u32;
                let mut cnt = 0;
                while cnt < max && j < chars.len() && chars[j].is_ascii_hexdigit() {
                    val = val * 16 + chars[j].to_digit(16).unwrap();
                    j += 1;
                    cnt += 1;
                }
                if cnt == 0 {
                    out.push('\\');
                    out.push(n);
                } else {
                    out.push(char::from_u32(val).unwrap_or('\u{fffd}'));
                }
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    None
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
