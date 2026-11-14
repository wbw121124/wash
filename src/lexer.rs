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
    /// `>|`: force truncation even when `set -C` (noclobber) is active.
    Clobber,
    Append,
    Both,
    /// `&>>`: append both stdout and stderr (`&>` is `Both`).
    BothAppend,
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

/// GNU bash 5.3 `compgen -k` 列出的全部 shell 关键字，按 bash 的输出顺序。
///
/// 这是 wash 关于「哪些词是 shell 关键字」的唯一权威定义，三处共用这一份表，
/// 避免各自维护常量导致两处漂移：
///   * lexer 只把 `recognised == true` 的项在命令起始位置当作 `Token::Reserved`
///     （见 `push_word`），parser 随之只收到这些保留字；
///   * `compgen -k`（`builtins.rs`）按本表顺序原样输出全部 22 项；
///   * `type` 的关键字判定（`builtins.rs::describe_command`）也查本表。
///
/// `recognised == false` 的项（`in`/`select`/`time`/`{`/`}`/`!`/`[[`/`]]`/
/// `coproc` 等）在 bash 里同样是关键字，但 wash 不在命令位置把它们当保留字：
/// `in` 只在 for/case 体内由 parser 按文本位置识别，其余尚未实现对应语法。
///
/// 来源：MSYS2 `LC_ALL=C bash -c 'compgen -k'`，GNU bash
/// 5.3.20(1)-release (x86_64-pc-cygwin)，共 22 项。
pub const SHELL_KEYWORDS: &[(&str, bool)] = &[
    ("if", true),
    ("then", true),
    ("else", true),
    ("elif", true),
    ("fi", true),
    ("case", true),
    ("esac", true),
    ("for", true),
    ("select", false),
    ("while", true),
    ("until", true),
    ("do", true),
    ("done", true),
    ("in", false),
    ("function", true),
    ("time", false),
    ("{", false),
    ("}", false),
    ("!", false),
    ("[[", false),
    ("]]", false),
    ("coproc", false),
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
        [WordPart::Text(text, false)] => SHELL_KEYWORDS
            .iter()
            .any(|(word, recognised)| *recognised && *word == text.as_str()),
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

/// Whether the word being built is an assignment word (`NAME=...` /
/// `NAME+=...`).  Only the part before the first `=` must be a literal name;
/// the RHS may contain expansions (`x=$v{a,b}` is still an assignment).
fn word_is_assignment(parts: &[WordPart], pending: &str) -> bool {
    let mut name = String::new();
    let mut found_eq = false;
    for p in parts {
        match p {
            WordPart::Text(s, _) => {
                if let Some(pos) = s.find('=') {
                    name.push_str(&s[..pos]);
                    found_eq = true;
                    break;
                }
                name.push_str(s);
            }
            _ => return false,
        }
    }
    if !found_eq {
        match pending.find('=') {
            Some(pos) => name.push_str(&pending[..pos]),
            None => return false,
        }
    }
    let base = name.strip_suffix('+').unwrap_or(&name);
    let b = base.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

/// Have all words emitted since the last command separator been assignment
/// words?  bash disables brace expansion for the whole leading assignment
/// run (`x={a,b} y={c,d}`), but `export x={a,b}` still expands because
/// `export` is an ordinary command word.
fn in_assignment_prefix(tokens: &[Token]) -> bool {
    for t in tokens.iter().rev() {
        match t {
            Token::Semicolon | Token::Background | Token::Pipe
            | Token::AndAnd | Token::OrOr => return true,
            Token::Word(parts) => {
                if !word_is_assignment(parts, "") {
                    return false;
                }
            }
            Token::Redirect { .. } => continue,
            _ => return false,
        }
    }
    true
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

/// Read the body of the deprecated `$[ ... ]` arithmetic expansion.
/// `i` points just past the opening `[`.  Nested brackets (array subscripts
/// such as `$[a[1]+2]`) are tracked so the inner `]` does not end the body.
/// Returns the body and the index just past the matching `]`.
fn read_bracket(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut depth = 1i32;
    let mut s = String::new();
    let mut j = i;
    while j < chars.len() {
        match chars[j] {
            '[' => {
                depth += 1;
                s.push('[');
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((s, j + 1));
                }
                s.push(']');
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

// --- brace expansion -------------------------------------------------------
//
// bash expands braces textually and **innermost-first**: it scans for the
// leftmost `{` whose matching `}` encloses a top-level comma list or a
// `a..b` sequence, replaces that span with each member, and then re-runs the
// whole thing on every result.  Brackets that do not qualify are stepped over
// and the *next* `{` is tried, which is why `{{a,b}}` becomes `{a}`/`{b}` and
// `{a{b,c}d}` becomes `{abd}`/`{acd}`.
//
// Quoting participates in two ways (both oracle-verified against bash 5.3.20):
//   * a comma inside `'...'` / `"..."` does not split, and a `{` inside quotes
//     is not a brace candidate -- `{a,"{b,c}"}` is `a` + `{b,c}` as two words;
//   * after the last expansion the resulting word goes through **quote
//     removal**, which is what makes `{a,"b,c"}` -> `b,c`, `{a,b\ c}` -> `b c`
//     and `{A..z}`'s 0x5C element an empty word.

/// bash's quote removal for one *unquoted-source* word fragment: drop the
/// quotes, unescape `\x` (a trailing lone `\` disappears), and inside double
/// quotes only unescape `\"`, `\\`, `\$` and ``\` ``.
fn quote_remove(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                if let Some(n) = it.next() {
                    out.push(n);
                }
            }
            '\'' => {
                for c in it.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    out.push(c);
                }
            }
            '"' => {
                while let Some(c) = it.next() {
                    if c == '"' {
                        break;
                    }
                    if c == '\\' {
                        match it.peek() {
                            Some(&n) if n == '"' || n == '\\' || n == '$' || n == '`' => {
                                out.push(n);
                                it.next();
                            }
                            _ => out.push('\\'),
                        }
                    } else {
                        out.push(c);
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Index of the `}` that closes the `{` at `open`, or `None` when the brace is
/// never closed. Quotes and backslash escapes are honoured, so `{a,"b}c"}` and
/// `{a,\}}` close where bash closes them.
fn find_brace_span(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut i = open;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 2; // escaped char: never structural (overshoot just ends the loop)
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split the inside of one brace span on its top-level commas.
///
/// `\,` and commas inside quotes do not separate members; everything (escapes
/// and quotes included) is kept verbatim so the recursive step can still see
/// it. Returns `None` when the span holds no top-level comma, i.e. it is not a
/// comma list (`{abc}`, `{1..a}`, `{a\,b}`).
fn split_brace_members(inner: &str) -> Option<Vec<String>> {
    let mut members = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in inner.chars() {
        if escaped {
            // Keep the pair intact: quote removal happens at the very end.
            cur.push('\\');
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            _ if quote == Some(c) => {
                quote = None;
                cur.push(c);
            }
            '\'' | '"' if quote.is_none() => {
                quote = Some(c);
                cur.push(c);
            }
            '{' if quote.is_none() => {
                depth += 1;
                cur.push(c);
            }
            '}' if quote.is_none() => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            ',' if quote.is_none() && depth == 0 => members.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if escaped {
        cur.push('\\');
    }
    members.push(cur);
    if members.len() >= 2 {
        Some(members)
    } else {
        None
    }
}

/// One textual brace-expansion step: the verbatim prefix, the members of the
/// leftmost expandable `{...}` span, and the verbatim suffix.
struct BraceStep {
    pre: String,
    members: Vec<String>,
    post: String,
}

/// Find the leftmost `{...}` in `raw` that bash would expand, honouring the
/// same escape / quote skipping as the old scanner. `None` means `raw` is a
/// leaf: no further brace expansion is possible.
fn first_brace_expansion(raw: &str) -> Option<BraceStep> {
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => {
                i += 2;
                continue;
            }
            '\'' | '"' => {
                // Skip the quoted run: no brace inside it may expand.
                let q = chars[i];
                i += 1;
                while i < chars.len() && chars[i] != q {
                    if q == '"' && chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            '{' => {
                if let Some(close) = find_brace_span(&chars, i) {
                    let inner: String = chars[i + 1..close].iter().collect();
                    let members = match parse_range(&inner) {
                        Some(words) => Some(words),
                        None => split_brace_members(&inner),
                    };
                    if let Some(members) = members {
                        return Some(BraceStep {
                            pre: chars[..i].iter().collect(),
                            members,
                            post: chars[close + 1..].iter().collect(),
                        });
                    }
                }
                // Not an expression: try the next `{` (bash does the same, which
                // is why `{a{b,c}d}` expands the inner braces only).
                i += 1;
                continue;
            }
            _ => i += 1,
        }
    }
    None
}

/// Expand one raw word fragment into its final words: innermost brace
/// expression first, then quote removal on whatever text remains.
///
/// This is bash's `brace_expand()` in miniature; `pre`/`post` are carried
/// along verbatim so `x{a,{b,c}}y` yields `xay xby xcy`.
///
/// The expansion is a **DFS pre-order** walk, but it uses an explicit worklist
/// instead of Rust recursion: nesting depth equals input length, so a
/// recursive implementation overflows the 1 MB Windows main-thread stack at
/// ~1700 nested braces while bash happily expands 3000+ levels. Pushing the
/// members in reverse onto a LIFO `Vec<String>` reproduces the old call order
/// exactly -- every member's own subtree is fully expanded before the next
/// member is popped. Only leaves (words with no expandable brace left) go
/// through `quote_remove`, once, as before.
fn expand_brace_text(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending: Vec<String> = vec![raw.to_string()];
    while let Some(current) = pending.pop() {
        match first_brace_expansion(&current) {
            Some(step) => {
                let BraceStep { pre, members, post } = step;
                for m in members.into_iter().rev() {
                    let mut word = String::with_capacity(pre.len() + m.len() + post.len());
                    word.push_str(&pre);
                    word.push_str(&m);
                    word.push_str(&post);
                    pending.push(word);
                }
            }
            None => out.push(quote_remove(&current)),
        }
    }
    out
}

/// The words one brace span expands to, or `None` when the span is not a brace
/// expression (the caller then keeps the whole word literal).
fn brace_alternatives(inner: &str) -> Option<Vec<String>> {
    let members = match parse_range(inner) {
        Some(words) => words,
        None => split_brace_members(inner)?,
    };
    let mut alts = Vec::new();
    for m in members {
        alts.extend(expand_brace_text(&m));
    }
    Some(alts)
}

/// One endpoint of a `{a..b}` sequence, after bash's classification.
enum SeqEnd {
    Int(i64),
    Char(i32),
}

/// Classify a sequence endpoint the way bash's `expand_seqterm()` does:
/// a base-10 integer (optional `+`/`-` sign) or a *single ASCII letter*.
/// Everything else (`{1..a}`'s `a` is fine on its own, `{1.5..3.5}`'s `1.5`,
/// `{..3}`'s empty string, `{!..~}`'s punctuation) is not a sequence
/// endpoint, which makes the whole brace word a literal.
fn parse_seq_end(text: &str) -> Option<SeqEnd> {
    if let Ok(v) = text.parse::<i64>() {
        return Some(SeqEnd::Int(v));
    }
    let mut it = text.chars();
    match (it.next(), it.next()) {
        // bash requires ISALPHA() && length 1; in the C locale that is
        // exactly the ASCII letters, so `{é..ê}` stays a literal word.
        (Some(c), None) if c.is_ascii_alphabetic() => Some(SeqEnd::Char(c as i32)),
        _ => None,
    }
}

/// Whether an endpoint literal asks bash to zero-pad the whole sequence.
///
/// bash's test is textual, not numeric: a number whose first digit is `0` and
/// that has at least one more digit (`{01..05}`, `{010..1}`, `{0..00}`), or a
/// negative number of that shape (`{-05..5}`). `{0..0}` and `{-0..5}` do NOT
/// pad, while `{+05..9}` does not either -- bash only looks at a leading `0`
/// or `-0`, never past a `+`.
fn seq_needs_pad(lit: &str) -> bool {
    let b = lit.as_bytes();
    (b.first() == Some(&b'0') && b.len() > 1)
        || (b.len() > 2 && b[0] == b'-' && b[1] == b'0')
}

/// Format one integer element of a zero-padded sequence.
///
/// bash formats with `printf("%0*jd", width, v)`, i.e. the width counts the
/// sign too: `{-05..5}` (width 3) prints `-05 … 000 … 005`, and `{-5..05}`
/// (width 2) prints `-5 … 00 … 05`. Rust's `{:0w$}` has the same semantics
/// (zeros go after the sign) and, like printf, never truncates a value that
/// is wider than the requested field.
fn pad_int(v: i64, width: usize) -> String {
    if width == 0 {
        v.to_string()
    } else {
        format!("{:0w$}", v, w = width)
    }
}

/// Expand `start..end` / `start..end..step` exactly as bash does.
///
/// bash's rules (all oracle-verified, see the brace corpus in
/// `tests/bash_diff_builtins.sh` GROUP A `brace-*`):
///
/// * the direction comes from the endpoints alone (`start <= end` ascends),
///   and the step is used as its absolute value -- `{3..1..1}` counts down,
///   `{1..3..-1}` counts up, `{1..-3..2}` yields `1 -1 -3`;
/// * a step of `0` (also `-0`) means 1, it is not an error;
/// * a numeric end makes the other end numeric too, letters make a code-point
///   sequence that also honours the step (`{A..z}` is the 58 code points
///   `A`..`z`, not 26 letters);
/// * zero padding only happens when an endpoint literal starts with `0`
///   (or `-0`) and keeps at least one digit after it; the field width is
///   `max(len(start), len(end))` of the *raw* literals, sign included, and it
///   is applied with C `%0*` semantics (zeros after the sign);
/// * mixed or unparsable endpoints are not a sequence at all.
///
/// Acceptance rule worth stating explicitly: an illegal form KEEPS THE WHOLE
/// WORD -- it never disappears and never expands to zero words. bash 5.3 was
/// measured on the seven malformed forms `{1..}`, `{..3}`, `{1..3..}`,
/// `{1.5..3}`, `{1..3..x}`, `{1..a}`, `{a..1}`: every one is exactly one
/// literal word.
///
/// Returns `None` when this is not a sequence: the caller then keeps the word
/// as a literal. It must NEVER return `Some(vec![])` -- bash never expands a
/// brace word into zero words, and an empty `Some` would silently delete the
/// word (that was the `{10..1}` bug: the word vanished from the command line).
fn parse_range(inner: &str) -> Option<Vec<String>> {
    // `splitn(3, "..")` leaves a 4-component form's tail in `parts[2]`
    // ("2..4"), which then fails the step parse below -- exactly bash's
    // "not a sequence" answer for `{1..3..2..4}`.
    let parts: Vec<&str> = inner.splitn(3, "..").collect();
    if parts.len() < 2 {
        return None;
    }
    let start = parts[0];
    let end = parts[1];
    if start.is_empty() || end.is_empty() {
        return None; // `{1..}`, `{..3}`
    }

    // Step: absent means 1; 0 (and -0) also means 1, per bash.
    let incr: i64 = match parts.get(2) {
        Some(text) => match text.parse::<i64>() {
            Ok(v) => v,
            Err(_) => return None, // `{1..3..x}`, `{1..3..}`, `{1..3..2..4}`
        },
        None => 1,
    };
    // bash negates a negative step to get its magnitude and cannot negate
    // INTMAX_MIN, so it gives up and leaves the word alone.
    if incr == i64::MIN {
        return None;
    }
    let step: i64 = if incr == 0 { 1 } else { incr.abs() };

    match (parse_seq_end(start), parse_seq_end(end)) {
        (Some(SeqEnd::Int(a)), Some(SeqEnd::Int(b))) => {
            // Width is taken from the two *literals* (sign included), and only
            // when one of them is zero-padded: `{1..05}` pads to width 2,
            // `{05..+900}` to width 4, `{1..200}` not at all.
            let width = if seq_needs_pad(start) || seq_needs_pad(end) {
                start.len().max(end.len())
            } else {
                0
            };
            let delta: i64 = if a <= b { step } else { -step };
            let mut out = Vec::new();
            let mut x = a;
            // Walk value by value. bash stops the sequence as soon as the next
            // step would overflow its integer type instead of wrapping:
            // `{9223372036854775807..9223372036854775807}` yields one element
            // and `{1..10..9223372036854775807}` only `1`. checked_add (never
            // wrapping_add) keeps that property; do not "optimise" this into
            // count-then-enumerate, which rounds differently.
            loop {
                let within = if delta > 0 { x <= b } else { x >= b };
                if !within {
                    break;
                }
                out.push(pad_int(x, width));
                match x.checked_add(delta) {
                    Some(next) => x = next,
                    None => break,
                }
            }
            Some(out)
        }
        (Some(SeqEnd::Char(a)), Some(SeqEnd::Char(b))) => {
            let delta: i64 = if a as i64 <= b as i64 { step } else { -step };
            let mut out = Vec::new();
            let mut x = a as i64;
            loop {
                let within = if delta > 0 { x <= b as i64 } else { x >= b as i64 };
                if !within {
                    break;
                }
                // Endpoints are single ASCII letters, so `x` stays in range.
                match char::from_u32(x as u32) {
                    Some(c) => out.push(c.to_string()),
                    None => return None,
                }
                match x.checked_add(delta) {
                    Some(next) => x = next,
                    None => break,
                }
            }
            Some(out)
        }
        // Mixed (`{1..a}`, `{a..1}`) or unparsable (`{1.5..3.5}`) endpoints are
        // a literal word, not an empty expansion.
        _ => None,
    }
}

/// Whether `c` ends a word, i.e. whether a `{` / `}` followed by it is a token
/// of its own.
///
/// bash only recognises `{` and `}` as reserved words when they are not glued
/// to another word character: `{ echo hi; }` (separators on both sides) is a
/// command group, while `{echo hi;}` is the word `{echo` followed by a stray
/// `}` and therefore a syntax error. The separator set is bash's own word
/// delimiter set: blanks, newline, and the metacharacters `; & | ( ) < >`.
fn is_word_separator(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
    )
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
                    } else if i < n && chars[i] == '[' {
                        // `$[ expr ]` is the obsolete spelling of `$(( expr ))`.
                        if let Some((body, ni)) = read_bracket(&chars, i + 1) {
                            parts.push(WordPart::Arith(body, true));
                            i = ni;
                            continue;
                        }
                        pending.push('$');
                        continue;
                    } else if i < n && is_expansion_start(chars[i]) {
                        let name = read_var_name(&chars, i);
                        i += name.chars().count();
                        parts.push(WordPart::Var(name, true));
                        continue;
                    } else {
                        // `$` is literal here; leave the following character
                        // for the next iteration (bash keeps `"a$ b"` as-is).
                        pending.push('$');
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
                    let is_clobber = i + 1 < n && chars[i + 1] == '|';
                    let kind = if is_append {
                        RedirKind::Append
                    } else if is_clobber {
                        RedirKind::Clobber
                    } else {
                        RedirKind::Out
                    };
                    tokens.push(Token::Redirect { kind, fd });
                    redirect_target = true;
                    i += if is_append || is_clobber { 2 } else { 1 };
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
                    let is_append = i + 2 < n && chars[i + 2] == '>';
                    tokens.push(Token::Redirect {
                        kind: if is_append {
                            RedirKind::BothAppend
                        } else {
                            RedirKind::Both
                        },
                        fd: None,
                    });
                    redirect_target = true;
                    i += if is_append { 3 } else { 2 };
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
                } else if i < n && chars[i] == '[' {
                    // `$[ expr ]` is the obsolete spelling of `$(( expr ))`.
                    if let Some((body, ni)) = read_bracket(&chars, i + 1) {
                        parts.push(WordPart::Arith(body, false));
                        i = ni;
                        continue;
                    }
                    pending.push('$');
                    continue;
                } else if i < n && is_expansion_start(chars[i]) {
                    let name = read_var_name(&chars, i);
                    i += name.chars().count();
                    parts.push(WordPart::Var(name, false));
                    continue;
                } else {
                    // Literal `$` (trailing, or followed by whitespace or
                    // punctuation).  The following character is NOT consumed.
                    pending.push('$');
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
                // bash does NOT brace-expand assignment words: `x={a,b}`
                // keeps the literal `{a,b}`, while `echo x={a,b}` and
                // `export x={a,b}` do expand.  Only the leading run of
                // assignment words in a simple command is affected.
                let assign_word = (at_command_start || in_assignment_prefix(&tokens))
                    && word_is_assignment(&parts, &pending);
                if !assign_word {
                    // Try brace expansion first (works mid-word too, e.g. `a{b,c}`).
                    // The span is located with the quote-aware scanner, so a `}`
                    // inside quotes (`{a,"b}c"}`) does not close it early.
                    if let Some(close) = find_brace_span(&chars, i) {
                        let inner: String = chars[i + 1..close].iter().collect();
                        if let Some(alts) = brace_alternatives(&inner) {
                            flush_text(&mut pending, &mut parts, pending_quoted);
                            parts.push(WordPart::Brace(alts));
                            i = close + 1;
                            continue;
                        }
                    }
                }
                // Not a brace expansion: `{` is a reserved word (command-group /
                // function-body opener) ONLY when it forms a token of its own,
                // which in bash means "the next character ends a word".  Glued
                // to anything else it is an ordinary literal character, so
                // `{abc}`, `{}`, `{1..a}`, `{echo` each stay ONE word.  That is
                // what makes `{echo hi;}` a syntax error (the `}` has no
                // partner) while `{ echo hi; }` runs a group.
                let complete_token = match chars.get(i + 1) {
                    None => true,
                    Some(&next) => is_word_separator(next),
                };
                if complete_token && pending.is_empty() && parts.is_empty() {
                    tokens.push(Token::Word(vec![WordPart::Text("{".to_string(), false)]));
                    at_command_start = true;
                    i += 1;
                } else {
                    // Literal `{`: keep it in the current word (`a{` -> `a{`,
                    // `{abc}` -> `{abc}`).
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
                // `}` mirrors `{`: reserved (function-body / group closer) only
                // as a token of its own, otherwise a literal character that
                // stays glued to the word.  So `}}x` is one word, `a}` is
                // `a}`, and `}` after `;` still closes a group.
                let complete_token = match chars.get(i + 1) {
                    None => true,
                    Some(&next) => is_word_separator(next),
                };
                if complete_token && pending.is_empty() && parts.is_empty() {
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

/// True when the character right after `$` starts a parameter expansion
/// (a name, a positional parameter, or one of the special parameters).
/// Anything else — whitespace, punctuation, EOF — leaves the `$` literal:
/// bash prints `echo "a$ b"` as `a$ b` and `echo a$` as `a$`.
fn is_expansion_start(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '_'
        || matches!(c, '*' | '@' | '#' | '?' | '-' | '$' | '!')
}
