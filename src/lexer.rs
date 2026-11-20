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

use std::cell::Cell;

use crate::error::{DiagnosticFamily, Result, WashError};

thread_local! {
    /// Depth of nested tokenize() calls (command substitutions, alias
    /// bodies).  Parse-time here-document warnings belong to the OUTER
    /// command line: a nested parse must not emit a second copy.
    static NESTED_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Tokenize a nested command string without emitting parse-time heredoc
/// warnings (the outer `tokenize()` already reported them).
pub fn tokenize_nested(input: &str) -> Result<Vec<Token>> {
    NESTED_DEPTH.with(|d| d.set(d.get() + 1));
    let result = tokenize(input);
    NESTED_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    result
}

/// Emit bash's "here-document delimited by end-of-file" warning.
/// The raw bash line carries a `bash: line N: ` prefix which the diff
/// suite's normalize_stderr keeps (only the shell name is stripped), so the
/// warning must carry the same `line N:` where N is the total number of
/// input lines (the EOF position).  Warnings are queued here and flushed at
/// the end of `tokenize`, once the total line count is known and before any
/// deferred syntax error -- preserving bash's diagnostic order
/// (warning first, then the syntax error; qa9 A13/A14/A15).
fn emit_heredoc_eof_warning(line: usize, delim: &str) {
    if NESTED_DEPTH.with(|d| d.get()) != 0 {
        return;
    }
    HD_EOF_WARNINGS.with(|w| {
        w.borrow_mut().push((line, delim.to_string()));
    });
}

thread_local! {
    /// Queued here-document end-of-file warnings: `(operator line, delim)`.
    static HD_EOF_WARNINGS: std::cell::RefCell<Vec<(usize, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Flush the queued here-document EOF warnings.  `total_lines` is the
/// 1-based line the input ends on (= number of physical lines), which bash
/// uses as the warning's own `line N:` position.
fn flush_heredoc_eof_warnings(total_lines: usize) {
    HD_EOF_WARNINGS.with(|w| {
        for (op_line, delim) in w.borrow_mut().drain(..) {
            eprintln!(
                "wash: line {}: warning: here-document at line {} delimited by end-of-file (wanted `{}')",
                total_lines, op_line, delim
            );
        }
    });
}

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
    /// `<<DELIM`: here-document.  The body is collected by the lexer at
    /// tokenize time and carried in `Token::Redirect::heredoc`.
    HereDoc,
    /// `<<-DELIM`: here-document that strips leading tabs from body lines
    /// (and from the terminator comparison).
    HereDocDash,
    /// `<<<WORD`: here-string (one line: WORD expanded, plus a newline).
    HereString,
}

/// Where a command-separating `;` came from.  bash's yacc layer names the
/// offending token differently for a typed `;`, a `;;` case terminator and a
/// newline acting as a separator, so the origin must survive tokenization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemicolonKind {
    /// A literal `;` typed by the user.
    Literal,
    /// The first `;` of a `;;` pair (the second one is tokenized next).
    DoubleLead,
    /// A newline standing in for `;`.
    Newline,
}

/// A separator's kind together with its 1-based physical source line, so a
/// parse error can report and echo the exact offending line like bash does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemicolonOrigin {
    pub kind: SemicolonKind,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Word(Vec<WordPart>),
    /// A word inside `[[ ... ]]`.  bash's conditional parser and yacc layer
    /// render the offending token from its raw source text (`yytext`), which
    /// keeps the original quotes and escapes (`[[ a "=" a ]]` prints
    /// ``unexpected token `"="'``, not ``unexpected token `='``).  `WordPart`
    /// only records *whether* a part was quoted, so the exact source text is
    /// carried alongside the parts.  Semantically this is identical to
    /// `Word`; the raw text is used only by cond diagnostics.
    CondWord(Vec<WordPart>, String),
    /// A reserved word in command position (`if`, `then`, `elif`, `else`,
    /// `fi`).  Bash only recognises a reserved word where a command may
    /// start, so `echo fi` still passes "fi" as an ordinary argument.
    /// The usize is the 1-based physical line the reserved word starts on
    /// (heredoc body lines are counted, matching bash): an unterminated
    /// compound command must report `... from \`if' command on line N` with
    /// the line of the *innermost* opener (bash oracle qa9 A12/A13/A14).
    Reserved(String, usize),
    /// A compound command (`if ... fi`) that has already been parsed.
    Compound(crate::parser::Compound),
    /// `[[` — opens a conditional expression.  bash's COND_START: it is
    /// recognised only where a command may start, exactly like a reserved
    /// word, so `echo [[` prints `[[`.
    CondStart(usize),
    /// `(( EXPR ))` — an arithmetic command.  Recognised only in command
    /// position, like `[[`.  The body is the raw arithmetic source between
    /// the opening `((` and the matching `))`; its exit status is 0 when the
    /// expression is non-zero, 1 when it is zero.
    ArithCmd(String, usize),
    /// `]]` — closes a conditional expression (bash's COND_END).  It must
    /// be a word of its own: bash lexes the *word* `]]` as the reserved
    /// word COND_END, so `abc]]` stays an ordinary word and the expression
    /// is left unterminated.
    CondEnd,
    /// `<` or `>` inside `[[ ... ]]` — the string comparison operators.
    /// bash returns them as the character tokens `<` / `>`; inside a
    /// conditional expression they are never redirections.
    CondOp(String),
    /// A word of only digits immediately followed by `<`/`>` inside
    /// `[[ ... ]]`.  bash lexes such a word as a NUMBER token, which the
    /// conditional parser rejects ("unexpected token `2' in conditional
    /// command").
    CondNumber(String),
    /// A newline inside `[[ ... ]]`.  bash's lexer returns the character
    /// '\n'; the conditional parser skips it only at the start of a term.
    CondNewline,
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
    Semicolon(SemicolonOrigin),
    Background,
    Redirect {
        kind: RedirKind,
        fd: Option<i32>,
        /// `<<`/`<<-` only: the body collected at tokenize time (see
        /// `HereDocBody`).  `None` for every other redirection kind, and
        /// until `collect_heredocs` has filled it in.
        heredoc: Option<HereDocBody>,
    },
}

/// The body of a `<<`/`<<-` here-document, collected by the lexer.
///
/// bash reads the body as soon as the line holding the operator ends: the
/// following lines are consumed as raw text until one equals the delimiter
/// (quote-removed, exact match; `<<-` compares and stores with leading tabs
/// stripped).  If the input ends first, bash warns and uses what it read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HereDocBody {
    /// Raw body text: one entry per source line, each terminated by `\n`.
    /// Leading tabs are already stripped for `<<-`.
    pub text: String,
    /// The delimiter after quote removal — what bash's end-of-file warning
    /// displays (and what the terminator line must equal).
    pub delim: String,
    /// False when the delimiter word was quoted (`'EOF'`, `"EOF"`): the body
    /// must not be expanded at execution time.
    pub expand: bool,
    /// 1-based line of the `<<` operator (the end-of-file warning's
    /// "here-document at line N").
    pub line: usize,
    /// True when the input ended before the terminator was found.
    pub eof: bool,
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
/// `recognised == false` 的项（`in`/`time`/`{`/`}`/`!`/`[[`/`]]`/`coproc`
/// 等）在 bash 里同样是关键字，但 wash 不在命令位置把它们当保留字：
/// `in` 只在 for/select/case 体内由 parser 按文本位置识别，其余尚未实现
/// 对应语法（`select` 已实现，故其 `recognised` 为 `true`）。
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
    ("select", true),
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
/// `line` is the 1-based physical line the word starts on (heredoc body
/// lines included, matching bash) and is carried by `Reserved` tokens so
/// unterminated compound diagnostics can name the opener's line.
fn push_word(
    tokens: &mut Vec<Token>,
    parts: Vec<WordPart>,
    line: usize,
    at_command_start: &mut bool,
    redirect_target: &mut bool,
    cond_depth: &mut usize,
    cond_regex_rhs: &mut bool,
    // The raw source characters of this word, from its first character up
    // to (but excluding) the delimiter that ended it.  Only cond words keep
    // it: bash renders malformed `[[ ]]` tokens from their raw `yytext`.
    word_chars: &[char],
) {
    if *redirect_target {
        // The word is the target of a redirection: it is not a command word
        // and it does not change the command position.
        *redirect_target = false;
        tokens.push(Token::Word(parts));
        return;
    }
    // `[[` and `]]` delimit a conditional expression.  `[[` opens it and is
    // only recognised where a command may start; `]]` closes it and must be
    // a word of its own (bash lexes the *word* `]]` as the reserved word
    // COND_END, so `abc]]` remains an ordinary word).
    let plain = match parts.as_slice() {
        [WordPart::Text(text, false)] => Some(text.as_str()),
        _ => None,
    };
    if *cond_depth > 0 {
        if plain == Some("]]") {
            *cond_regex_rhs = false;
            tokens.push(Token::CondEnd);
            *cond_depth = 0;
            *at_command_start = true;
            return;
        }
        // bash's COND_REGEXP state: on the word following an unquoted `=~`,
        // `|`, `(` and `)` are ordinary pattern characters rather than
        // operators, so `[[ x =~ ^(a|b)$ ]]` keeps them in one word.
        if *cond_regex_rhs {
            *cond_regex_rhs = false;
        } else if plain == Some("=~") {
            *cond_regex_rhs = true;
        }
        // Inside a conditional expression no word is a reserved word:
        // `[[ if == if ]]` compares the two words.  Keep the raw source
        // text so a malformed expression can render the token exactly as
        // bash does (quotes and escapes included).
        let raw: String = word_chars.iter().collect();
        tokens.push(Token::CondWord(parts, raw));
        *at_command_start = false;
        return;
    }
    if plain == Some("[[") && *at_command_start {
        tokens.push(Token::CondStart(line));
        *cond_depth = 1;
        // The word after `[[` is an operand, not a command word.
        *at_command_start = false;
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
        tokens.push(Token::Reserved(name, line));
        // A reserved word is followed by a command again.
        *at_command_start = true;
        return;
    }
    tokens.push(Token::Word(parts));
    *at_command_start = false;
}

/// Lex a `<` or `>` inside a conditional expression: the bracket is the
/// string comparison operator, not a redirection.
///
/// bash lexes a word of only digits that is *immediately* followed by the
/// bracket as a NUMBER token (`2>1` is `NUMBER 2`, `>` , `1`); the
/// conditional parser rejects a NUMBER with "unexpected token `2' in
/// conditional command".  A quoted or non-numeric word stays an ordinary
/// word.
fn cond_angle(
    tokens: &mut Vec<Token>,
    parts: &mut Vec<WordPart>,
    pending: &mut String,
    pending_quoted: bool,
    ch: char,
) {
    let all_digits = parts.is_empty()
        && !pending.is_empty()
        && pending.bytes().all(|b| b.is_ascii_digit());
    if all_digits {
        tokens.push(Token::CondNumber(std::mem::take(pending)));
    } else {
        if !pending.is_empty() {
            parts.push(WordPart::Text(std::mem::take(pending), pending_quoted));
        }
        if !parts.is_empty() {
            tokens.push(Token::Word(std::mem::take(parts)));
        }
    }
    tokens.push(Token::CondOp(ch.to_string()));
}

/// Whether the word being built is an assignment word (`NAME=...` /
/// `NAME+=...`).  Only the part before the first `=` must be a literal name;
/// the RHS may contain expansions (`x=$v{a,b}` is still an assignment).
fn word_is_assignment(parts: &[WordPart], pending: &str) -> bool {
    let mut name = String::new();
    let mut found_eq = false;
    // A subscript may contain expansions (`m[$k]=v`), so non-text parts are
    // tolerated while we are inside `[...]`; they are opaque here and are
    // expanded later by the executor.
    let mut in_subscript = false;
    for p in parts {
        match p {
            WordPart::Text(s, _) => {
                if let Some(pos) = s.find('=') {
                    name.push_str(&s[..pos]);
                    found_eq = true;
                    break;
                }
                name.push_str(s);
                for ch in s.chars() {
                    if ch == '[' {
                        in_subscript = true;
                    } else if ch == ']' {
                        in_subscript = false;
                    }
                }
            }
            _ if in_subscript => {}
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
    let ident = match base.find('[') {
        Some(open) => {
            if !base.ends_with(']') || open + 1 >= base.len() - 1 {
                return false;
            }
            &base[..open]
        }
        None => base,
    };
    let b = ident.as_bytes();
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
            Token::Semicolon(_) | Token::Background | Token::Pipe
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
pub(crate) fn read_paren(chars: &[char], i: usize) -> Option<(String, usize)> {
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

pub(crate) fn read_brace(chars: &[char], i: usize) -> Option<(String, usize)> {
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
/// Kind of unclosed delimiter detected during tokenization, together with the
/// 1-based line where it started.  Used to build a structured diagnostic so the
/// shell computes the right line number without re-parsing the message text.
enum Unclosed {
    /// `$(` unclosed in command position (unquoted): the `CmdsubEof` rule
    /// (line number = n_lines + 1).
    CmdSub(()),
    /// `$(` unclosed inside a double-quoted context: bash blames the enclosing
    /// double quote, so this is an `OtherDelimiterEof` at the double quote's
    /// open line.
    CmdSubInDouble(usize),
    /// Unclosed backtick.
    Backtick(usize),
    /// Unclosed `${`.
    Brace(usize),
    /// Unclosed `$((`.
    Arith(usize),
}

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
    // Character index where the current word began.  bash renders malformed
    // `[[ ]]` tokens from their raw source text, so cond words keep the
    // `chars[word_start..i]` slice.  Reset whenever a new word starts.
    let mut word_start: usize = 0;
    let mut in_single = false;
    let mut in_double = false;
    // 1-based source line number (bash counts every physical line, including
    // newlines inside quoted strings).
    let mut line_no: usize = 1;
    // `<<`/`<<-` operators seen on the current line, waiting for the line to
    // end so their bodies can be read (see `collect_heredocs`).
    let mut pending_heredocs: Vec<PendingHeredoc> = Vec::new();
    // An unclosed delimiter (`$(`, backtick, `${`, `$((`, or a quote opened
    // inside another quoting context).  bash reports a syntax error instead of
    // degrading the text to a literal; the error is deferred until after
    // `collect_heredocs()` so here-document EOF warnings are still emitted
    // first (see hd-073 / task #33).  Each variant carries the 1-based line
    // where the delimiter started so the shell picks the right line number.
    let mut unclosed: Option<Unclosed> = None;
    // A redirection operator left without a target word right before a
    // literal `;` (qa9 A04/A06): always the FIRST syntax error in the stream
    // (any earlier unterminated delimiter would have swallowed the `;` into
    // its quoting context), so it outranks every deferred EOF-family error.
    let mut redirect_missing_target = false;
    // 1-based line where the current double/single quote opened; used as the
    // `open_line` for an unterminated quote (or a delimiter started inside it).
    let mut open_quote_line: usize = 0;

    // Whether a command may start at the current position.  Reserved words
    // are only recognised there.
    let mut at_command_start = true;
    // Set right after a redirection: the next word is the redirection's
    // target, not a command word.
    let mut redirect_target = false;
    // Depth of nested `[[ ... ]]` conditional expressions.  While it is
    // non-zero the lexer produces the conditional grammar's tokens (`]]`,
    // the `<`/`>` comparison operators, newlines) instead of redirections
    // and reserved words.
    let mut cond_depth: usize = 0;
    // True while lexing the word after an unquoted `=~` inside `[[ ]]`: bash's
    // COND_REGEXP state keeps `|`, `(` and `)` inside that word.
    let mut cond_regex_rhs = false;

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
        // A word starts at the first character after the previous word or
        // operator was flushed.  Once characters are being accumulated the
        // start stays put until the word is pushed.  A quote opened at the
        // very start of a word is part of that word, so do not reset inside
        // a quoting context (that would drop the opening `"` / `'`).
        if !in_single && !in_double && parts.is_empty() && pending.is_empty() {
            word_start = i;
        }

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
                if c == '\n' {
                    line_no += 1;
                }
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
                            unclosed = Some(Unclosed::Arith(line_no));
                            parts.push(WordPart::Text("$(".to_string(), true));
                            i += 1;
                            continue;
                        }
                        if let Some((body, ni)) = read_paren(&chars, i + 1) {
                            register_shadow_heredocs(&body, &chars, i + 1, line_no, &mut pending_heredocs);
                            parts.push(WordPart::CmdSub(body, true));
                            i = ni;
                            continue;
                        }
                        unclosed = Some(Unclosed::CmdSubInDouble(open_quote_line));
                        parts.push(WordPart::Text("$(".to_string(), true));
                        i += 1;
                        continue;
                    } else if i < n && chars[i] == '{' {
                        if let Some((body, ni)) = read_brace(&chars, i + 1) {
                            parts.push(WordPart::Braced(body, true));
                            i = ni;
                            continue;
                        }
                        unclosed = Some(Unclosed::Brace(line_no));
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
                    let mut tick_closed = false;
                    while i < n && chars[i] != '`' {
                        body.push(chars[i]);
                        i += 1;
                    }
                    if i < n && chars[i] == '`' {
                        tick_closed = true;
                    }
                    i += 1;
                    if tick_closed {
                        parts.push(WordPart::Backtick(body, true));
                    } else {
                        unclosed = Some(Unclosed::Backtick(line_no));
                    }
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
                _ => {
                    if c == '\n' {
                        line_no += 1;
                    }
                    pending.push(c);
                }
            }
            i += 1;
            continue;
        }

        // unquoted context
        match c {
            '\'' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                pending_quoted = true;
                open_quote_line = line_no;
                in_single = true;
                i += 1;
            }
            '"' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                pending_quoted = true;
                open_quote_line = line_no;
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
            '#' if parts.is_empty() && pending.is_empty() && cond_depth == 0 => {
                // bash starts a comment only at the start of a word, outside
                // quotes and `[[ ]]`.  A `#` in the middle of a word (`a#b`),
                // inside quotes, or in a conditional expression is literal.
                // Skip to end of line WITHOUT consuming the newline: the next
                // iteration turns it into the usual command separator, which
                // is also what makes `> #f` report a missing redirect target.
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            ' ' | '\t' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
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
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
                    );
                }
                // A here-document operator seen on this line starts reading
                // its body here: bash consumes the following lines as raw
                // text until the terminator and tokenization resumes after.
                let (next_i, body_lines) =
                    collect_heredocs(&mut tokens, &mut pending_heredocs, &chars, i + 1);
                let sep_line = line_no;
                line_no += 1 + body_lines;
                if cond_depth > 0 {
                    // Inside `[[ ]]` a newline is a token of its own: the
                    // conditional parser skips it only at the start of a term.
                    tokens.push(Token::CondNewline);
                } else {
                    tokens.push(Token::Semicolon(SemicolonOrigin {
                        kind: SemicolonKind::Newline,
                        line: sep_line,
                    }));
                    at_command_start = true;
                }
                i = next_i;
            }
            '|' if cond_depth > 0 && cond_regex_rhs => {
                // In bash's COND_REGEXP state a `|` (even a doubled one) is a
                // regex alternation character, not the `||` cond operator.
                pending.push('|');
                if i + 1 < n && chars[i + 1] == '|' {
                    pending.push('|');
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '|' => {
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
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
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
                    );
                }
                // A redirection left without a target word immediately before
                // a literal `;`: bash streams the parse and reports the `;`
                // itself (`echo hi >; echo after` -> `near unexpected token
                // \`;'`, qa9 A04/A06).  Any unterminated delimiter opened
                // earlier would have swallowed this `;` into its quoting
                // context, so this error always predates every EOF-family
                // diagnostic and is reported first at the end of tokenize.
                // `echo hi >` at a real end of line never reaches this arm
                // (no `;` there) and still reports `newline` (qa9 A05).
                if redirect_target {
                    redirect_missing_target = true;
                    redirect_target = false;
                }
                // A `;;` pair is tokenized as two semicolons (the case parser
                // still sees both), but the first one is marked so an empty
                // loop body can report bash's `;;` instead of `;`.
                let kind = if i + 1 < n && chars[i + 1] == ';' {
                    SemicolonKind::DoubleLead
                } else {
                    SemicolonKind::Literal
                };
                tokens.push(Token::Semicolon(SemicolonOrigin { kind, line: line_no }));
                at_command_start = true;
                i += 1;
            }
            '<' => {
                if cond_depth > 0 {
                    // Inside `[[ ]]` the bracket is the string comparison
                    // operator, not a redirection.
                    cond_angle(&mut tokens, &mut parts, &mut pending, pending_quoted, '<');
                    i += 1;
                    continue;
                }
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
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
                    );
                }
                if i + 1 < n && chars[i + 1] == '&' {
                    // `<&N` / `<&-`: duplicate or close an input descriptor.
                    let (target, ni) = read_dup_target(&chars, i + 2);
                    tokens.push(Token::Redirect {
                        kind: RedirKind::Dup,
                        fd: Some(fd.unwrap_or(0)),
                        heredoc: None,
                    });
                    tokens.push(Token::Word(target));
                    redirect_target = false;
                    i = ni;
                } else if i + 2 < n && chars[i + 1] == '<' && chars[i + 2] == '<' {
                    // `<<<word`: here-string.  The word is the string; it is
                    // expanded at execution time (tilde/param/cmd/arith) but
                    // never globbed or word-split, and one newline is
                    // appended.
                    tokens.push(Token::Redirect {
                        kind: RedirKind::HereString,
                        fd,
                        heredoc: None,
                    });
                    redirect_target = true;
                    i += 3;
                } else if i + 1 < n && chars[i + 1] == '<' {
                    // `<<[-]DELIM`: here-document.  The delimiter is the next
                    // word (quote-removed, never expanded); the body is read
                    // from the following lines as soon as the current line
                    // ends (see `collect_heredocs`).
                    let strip_tabs = i + 2 < n && chars[i + 2] == '-';
                    tokens.push(Token::Redirect {
                        kind: if strip_tabs {
                            RedirKind::HereDocDash
                        } else {
                            RedirKind::HereDoc
                        },
                        fd,
                        heredoc: None,
                    });
                    pending_heredocs.push(PendingHeredoc {
                        token_index: tokens.len() - 1,
                        strip_tabs,
                        line: line_no,
                        shadow: false,
                        start: None,
                        delim: String::new(),
                    });
                    redirect_target = true;
                    i += if strip_tabs { 3 } else { 2 };
                } else {
                    tokens.push(Token::Redirect {
                        kind: RedirKind::In,
                        fd,
                        heredoc: None,
                    });
                    redirect_target = true;
                    i += 1;
                }
            }
            '>' => {
                if cond_depth > 0 {
                    // Inside `[[ ]]` the bracket is the string comparison
                    // operator, not a redirection.
                    cond_angle(&mut tokens, &mut parts, &mut pending, pending_quoted, '>');
                    i += 1;
                    continue;
                }
                let fd = take_digit_prefix(&mut pending);
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
                    );
                }
                if i + 1 < n && chars[i + 1] == '&' {
                    // `>&N` / `>&-`: duplicate or close an output descriptor.
                    let (target, ni) = read_dup_target(&chars, i + 2);
                    tokens.push(Token::Redirect {
                        kind: RedirKind::Dup,
                        fd: Some(fd.unwrap_or(1)),
                        heredoc: None,
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
                    tokens.push(Token::Redirect { kind, fd, heredoc: None });
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
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
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
                        heredoc: None,
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
                        unclosed = Some(Unclosed::Arith(line_no));
                        parts.push(WordPart::Text("$(".to_string(), false));
                        i += 1;
                        continue;
                    }
                    if let Some((body, ni)) = read_paren(&chars, i + 1) {
                        register_shadow_heredocs(&body, &chars, i + 1, line_no, &mut pending_heredocs);
                        parts.push(WordPart::CmdSub(body, false));
                        i = ni;
                        continue;
                    }
                    unclosed = Some(Unclosed::CmdSub(()));
                    parts.push(WordPart::Text("$(".to_string(), false));
                    i += 1;
                    continue;
                } else if i < n && chars[i] == '{' {
                    if let Some((body, ni)) = read_brace(&chars, i + 1) {
                        parts.push(WordPart::Braced(body, false));
                        i = ni;
                        continue;
                    }
                    unclosed = Some(Unclosed::Brace(line_no));
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
                let mut tick_closed = false;
                while i < n && chars[i] != '`' {
                    body.push(chars[i]);
                    i += 1;
                }
                if i < n && chars[i] == '`' {
                    tick_closed = true;
                }
                i += 1;
                if tick_closed {
                    parts.push(WordPart::Backtick(body, false));
                } else {
                    unclosed = Some(Unclosed::Backtick(line_no));
                }
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
            '(' if cond_depth > 0 && cond_regex_rhs => {
                // bash's `PST_REGEXP` state consumes a whole matched pair via
                // `parse_matched_pair`: `a(b)c` stays ONE regex word, while an
                // unmatched `(` is a lexer EOF error (not a regex error).
                let mut depth = 1usize;
                pending.push('(');
                i += 1;
                while i < n && depth > 0 {
                    let ch = chars[i];
                    if ch == '\\' && i + 1 < n {
                        pending.push(ch);
                        pending.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                    }
                    pending.push(ch);
                    i += 1;
                }
                if depth > 0 {
                    return Err(WashError::syntax_unclosed(
                        DiagnosticFamily::CondRegexEof,
                        line_no,
                        "unexpected EOF while looking for matching `)'",
                    ));
                }
            }
            // bash only special-cases `(` and `|` in PST_REGEXP: a lone `)`
            // stays a delimiter, so it falls through to the normal branch and
            // the conditional parser rejects it.
            '(' if at_command_start && chars.get(i + 1) == Some(&'(') => {
                // `(( EXPR ))` arithmetic command.  Only recognised where a
                // command may start, like `[[`.  Reuse the `$((` bracket
                // reader: it returns the index just after the matching `)`;
                // a second `)` must follow to close the command.
                flush_text(&mut pending, &mut parts, pending_quoted);
                if !parts.is_empty() {
                    push_word(
                        &mut tokens,
                        std::mem::take(&mut parts),
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
                    );
                }
                if let Some((body, ni)) = read_paren(&chars, i + 2) {
                    if chars.get(ni) == Some(&')') {
                        tokens.push(Token::ArithCmd(body, line_no));
                        i = ni + 1;
                        at_command_start = false;
                        continue;
                    }
                    // A single closing parenthesis: bash's `parse_arith_cmd`
                    // pushes the `(` back and treats the construct as a
                    // nested subshell, so let the generic path handle it.
                } else {
                    // End of input before the matching `)': bash's lexer
                    // reports the unclosed delimiter and produces no token.
                    return Err(WashError::syntax_unclosed(
                        DiagnosticFamily::OtherDelimiterEof,
                        line_no,
                        "unexpected EOF while looking for matching `)'",
                    ));
                }
                // A `((` that is not a double-paren construct: let the generic
                // path emit two `(` words so the parser produces bash's syntax
                // error rather than silently accepting a partial command.
                tokens.push(Token::Word(vec![WordPart::Text("(".to_string(), false)]));
                i += 1;
                at_command_start = false;
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
                        line_no,
                        &mut at_command_start,
                        &mut redirect_target,
                        &mut cond_depth,
                        &mut cond_regex_rhs,
                        &chars[word_start..i],
                    );
                }
                tokens.push(Token::Word(vec![WordPart::Text(c.to_string(), false)]));
                // Inside a conditional expression `(` / `)` group operands;
                // they never introduce a subshell.
                at_command_start = cond_depth == 0;
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
            line_no,
            &mut at_command_start,
            &mut redirect_target,
            &mut cond_depth,
            &mut cond_regex_rhs,
            &chars[word_start..i],
        );
    }

    // Here-documents on the final line (no trailing newline, or no terminator
    // at all): collect their bodies from whatever is left of the input.
    collect_heredocs(&mut tokens, &mut pending_heredocs, &chars, i);

    // All here-document EOF warnings are queued during scanning; flush them
    // now, before any deferred syntax error (bash's diagnostic order: warning
    // first, then the error -- qa9 A13/A14).  bash names the *last physical
    // line of the input* as the warning position (`line N:` where N is the
    // EOF line), which is exactly `input.lines().count()` -- NOT the lexer's
    // running `line_no`, which ends one past the last line (qa9 A13/A15).
    flush_heredoc_eof_warnings(input.lines().count());

    // A redirection without a target right before a literal `;` is the first
    // syntax error in the stream and outranks every EOF-family error (qa9
    // A04/A06; any earlier unterminated delimiter would have swallowed the
    // `;` into its quoting context, so the two can never coexist).  The
    // near-token message goes through the shell's legacy heuristic, which
    // computes the line number and echoes the offending line like bash.
    if redirect_missing_target {
        return Err(WashError::syntax(
            "syntax error near unexpected token `;'",
        ));
    }

    // An unterminated quote wins over any nested unclosed delimiter: bash
    // reports the outermost quoting context (e.g. `echo "$(echo hi"` is a
    // double-quote error, not a command-substitution error).
    if in_single || in_double {
        let msg = if in_double {
            "unexpected EOF while looking for matching `\"'"
        } else {
            "unexpected EOF while looking for matching `'"
        };
        return Err(WashError::syntax_unclosed(
            DiagnosticFamily::OtherDelimiterEof,
            open_quote_line,
            msg,
        ));
    }
    // Deferred unclosed-delimiter errors. `collect_heredocs()` above has already
    // emitted any here-document EOF warning, so bash's two-line diagnostic
    // order (warning, then syntax error) is preserved.  The structured family
    // and `open_line` let the shell pick the exact line number bash uses.
    if let Some(u) = unclosed {
        let (family, line, msg) = match u {
            Unclosed::CmdSubInDouble(l) => (
                DiagnosticFamily::OtherDelimiterEof,
                l,
                "unexpected EOF while looking for matching `\"'",
            ),
            Unclosed::CmdSub(_) => (
                DiagnosticFamily::CmdsubEof,
                0,
                "unexpected EOF while looking for matching `)'",
            ),
            Unclosed::Backtick(l) => (
                DiagnosticFamily::OtherDelimiterEof,
                l,
                "unexpected EOF while looking for matching ``'",
            ),
            Unclosed::Brace(l) => (
                DiagnosticFamily::OtherDelimiterEof,
                l,
                "unexpected EOF while looking for matching `}'",
            ),
            Unclosed::Arith(l) => (
                DiagnosticFamily::OtherDelimiterEof,
                l,
                "unexpected EOF while looking for matching `)'",
            ),
        };
        return Err(WashError::syntax_unclosed(family, line, msg));
    }

    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Here-documents (`<<` / `<<-`)
// ---------------------------------------------------------------------------

/// A `<<`/`<<-` operator waiting for its body.  Recorded by `tokenize` when
/// the operator is lexed; fulfilled by `collect_heredocs` at the end of the
/// line (bash reads the body as soon as the line holding the operator ends).
///
/// `shadow` entries model heredocs written INSIDE `$(...)`: bash registers
/// them with the OUTER command line too, whose body is read from the outer
/// input lines.  They have no `Token::Redirect` here, never consume input
/// and only decide whether an end-of-file warning is due.
struct PendingHeredoc {
    /// Index of the `Token::Redirect` carrying the operator (`usize::MAX`
    /// for a shadow entry, which has no outer redirect token).
    token_index: usize,
    /// `<<-`: strip leading tabs from body lines and the terminator.
    strip_tabs: bool,
    /// 1-based line of the operator (the end-of-file warning's line number).
    line: usize,
    /// Warning-only entry for a heredoc inside a command substitution.
    shadow: bool,
    /// Shadow entries start scanning outer lines here (just after the
    /// newline ending the line that holds the `<<`).
    start: Option<usize>,
    /// Shadow entries carry their quote-removed delimiter here.
    delim: String,
}

/// The delimiter of the heredoc whose `Redirect` token sits at `idx`: the
/// word right after it, flattened to its literal text.  bash does NOT expand
/// the delimiter word — `cat <<$D` waits for a line reading `$D` — so
/// variable parts contribute their literal spelling.
fn heredoc_delim(tokens: &[Token], idx: usize) -> String {
    match tokens.get(idx + 1) {
        Some(Token::Word(parts)) => parts.iter().map(delimiter_part_text).collect(),
        _ => String::new(),
    }
}

/// Whether any part of the delimiter word is quoted (`'EOF'`, `"EOF"`,
/// `E'O'F`).  A quoted delimiter suppresses body expansion at execution time.
/// bash: `<<\EOF` is equivalent to `<<'EOF'` — the body is NOT expanded.
/// wash previously only checked the `quoted` flag of `WordPart::Text`, which
/// is set incorrectly for escape‑sequence‑produced text (e.g. `\EOF` →
/// `WordPart::Text("EOF", false)`).  The fix below also recognises a leading
/// backslash as suppressing expansion, matching bash semantics.
fn heredoc_delim_quoted(tokens: &[Token], idx: usize) -> bool {
    match tokens.get(idx + 1) {
        Some(Token::Word(parts)) => {
            // 1. any explicit quoted flag
            if parts.iter().any(|p| matches!(p, WordPart::Text(_, true))) {
                return true;
            }
            // 2. leading backslash suppresses expansion (bash `<<\EOF` → body not expanded)
            if !parts.is_empty() {
                if let WordPart::Text(s, _) = &parts[0] {
                    if s.starts_with('\\') {
                        return true;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

/// Literal spelling of one delimiter-word part (see `heredoc_delim`).
fn delimiter_part_text(p: &WordPart) -> String {
    match p {
        WordPart::Text(s, _) => s.clone(),
        WordPart::Var(name, _) => format!("${}", name),
        WordPart::Braced(body, _) => format!("${{{}}}", body),
        WordPart::CmdSub(body, _) => format!("$({})", body),
        WordPart::Backtick(body, _) => format!("`{}`", body),
        WordPart::Arith(body, _) => format!("$(({}))", body),
        WordPart::Tilde => "~".to_string(),
        WordPart::Brace(alts) => alts.concat(),
    }
}

/// Register the heredocs written inside a `$(...)` body with the OUTER
/// command line.
///
/// bash parses the command substitution from its own text (which is what
/// wash's `WordPart::CmdSub` carries), but it ALSO registers the `<<`
/// operator with the outer line and reads an outer heredoc body from the
/// lines following that line.  That outer body is what produces the
/// "delimited by end-of-file" warning when the terminator is glued to the
/// closing `)` (e.g. `x=$(cat <<EOF\nhi\nEOF)`), while a standalone `EOF`
/// line terminates it and no warning is printed.
///
/// These entries are warning-only: they never consume input lines and never
/// attach a body to a token, so the command substitution still runs.
fn register_shadow_heredocs(
    body: &str,
    chars: &[char],
    from: usize,
    line_no: usize,
    pending: &mut Vec<PendingHeredoc>,
) {
    let b: Vec<char> = body.chars().collect();
    let n = b.len();
    let body_start = chars[from..]
        .iter()
        .position(|&c| c == '\n')
        .map(|p| from + p + 1)
        .unwrap_or(chars.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut j = 0usize;
    while j < n {
        let c = b[j];
        if in_single {
            if c == '\'' {
                in_single = false;
            }
            j += 1;
            continue;
        }
        if in_double {
            if c == '\\' && j + 1 < n {
                j += 2;
                continue;
            }
            if c == '"' {
                in_double = false;
            }
            j += 1;
            continue;
        }
        if c == '\'' {
            in_single = true;
            j += 1;
            continue;
        }
        if c == '"' {
            in_double = true;
            j += 1;
            continue;
        }
        if c == '<' && j + 1 < n && b[j + 1] == '<' {
            if j + 2 < n && b[j + 2] == '<' {
                // `<<<` is a here-string, not a here-document.
                j += 3;
                continue;
            }
            let strip_tabs = j + 2 < n && b[j + 2] == '-';
            let mut k = j + if strip_tabs { 3 } else { 2 };
            while k < n && (b[k] == ' ' || b[k] == '\t') {
                k += 1;
            }
            // Quote-removed delimiter: stop at any shell word delimiter.
            let mut delim = String::new();
            while k < n {
                let d = b[k];
                if matches!(d, '\n' | ' ' | '\t' | ';' | '&' | '|' | '(' | ')' | '<' | '>') {
                    break;
                }
                if d == '\'' || d == '"' {
                    // quote removal only; the quote character itself is not
                    // part of the delimiter bash compares against.
                    k += 1;
                    continue;
                }
                if d == '\\' && k + 1 < n {
                    k += 1;
                    delim.push(b[k]);
                    k += 1;
                    continue;
                }
                delim.push(d);
                k += 1;
            }
            if !delim.is_empty() {
                pending.push(PendingHeredoc {
                    token_index: usize::MAX,
                    strip_tabs,
                    line: line_no,
                    shadow: true,
                    start: Some(body_start),
                    delim,
                });
            }
            j = k;
            continue;
        }
        j += 1;
    }
}

/// Is there a line equal to `delim` (after `<<-` tab stripping) at/after
/// `start`?  Used only by shadow entries; normal heredocs consume their
/// lines while searching (see `collect_heredocs`).
fn scan_heredoc_terminator(chars: &[char], start: usize, delim: &str, strip_tabs: bool) -> bool {
    let n = chars.len();
    let mut pos = start;
    while pos < n {
        let le = chars[pos..n]
            .iter()
            .position(|&c| c == '\n')
            .map(|p| pos + p)
            .unwrap_or(n);
        let line: String = chars[pos..le].iter().collect();
        let candidate: &str = if strip_tabs {
            line.trim_start_matches('\t')
        } else {
            &line
        };
        if candidate == delim {
            return true;
        }
        if le >= n {
            break;
        }
        pos = le + 1;
    }
    false
}

/// Collect the bodies of all heredoc operators pending on the line that just
/// ended, in order (bash: `cat <<A <<B` gives A the first body and B the
/// second).  Body lines are consumed as raw text — tokenization resumes after
/// the last terminator line — and each body is stored in its `Token::Redirect`
/// as a `HereDocBody`.
///
/// Parse-time EOF warnings are emitted HERE, not after a successful parse:
/// bash warns while reading the heredoc, so a later syntax error (an
/// unclosed `(`, for example) must not swallow the warning.
///
/// Returns the input offset where tokenization resumes and the number of
/// body/terminator lines consumed (for source line numbering).
fn collect_heredocs(
    tokens: &mut [Token],
    pending: &mut Vec<PendingHeredoc>,
    chars: &[char],
    mut pos: usize,
) -> (usize, usize) {
    let n = chars.len();
    let mut consumed = 0usize;
    for op in pending.drain(..) {
        // Heredocs inside `$(...)`: the outer line either has a standalone
        // terminator line or warns.  No body, no input consumption.
        if op.shadow {
            let start = op.start.unwrap_or(pos);
            if !scan_heredoc_terminator(chars, start, &op.delim, op.strip_tabs) {
                emit_heredoc_eof_warning(op.line, &op.delim);
            }
            continue;
        }
        // A missing delimiter word is a syntax error at the operator: bash
        // never starts reading a body and therefore never warns.
        let has_delim_word = matches!(tokens.get(op.token_index + 1), Some(Token::Word(_)));
        if !has_delim_word {
            if let Some(Token::Redirect { heredoc, .. }) = tokens.get_mut(op.token_index) {
                *heredoc = Some(HereDocBody {
                    text: String::new(),
                    delim: String::new(),
                    expand: false,
                    line: op.line,
                    eof: false,
                });
            }
            continue;
        }
        let delim = heredoc_delim(tokens, op.token_index);
        let expand = !heredoc_delim_quoted(tokens, op.token_index);
        let mut body = String::new();
        let mut eof = true;
        while pos < n {
            // One physical line: [pos, le), where `le` is the `\n` (or the
            // end of input for a last line without trailing newline).
            let le = chars[pos..n]
                .iter()
                .position(|&c| c == '\n')
                .map(|p| pos + p)
                .unwrap_or(n);
            let last_line = le >= n;
            let line: String = chars[pos..le].iter().collect();
            // `<<-` compares (and stores) with leading tabs stripped.
            let candidate: &str = if op.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                &line
            };
            if candidate == delim {
                // Terminator found: the body ends here; tokenization resumes
                // on the following line.
                eof = false;
                pos = if last_line { n } else { le + 1 };
                consumed += 1;
                break;
            }
            body.push_str(candidate);
            body.push('\n');
            consumed += 1;
            pos = if last_line { n } else { le + 1 };
            if last_line {
                // Input ended mid-body: bash warns and uses what it read.
                break;
            }
        }
        if eof {
            emit_heredoc_eof_warning(op.line, &delim);
        }
        if let Some(Token::Redirect { heredoc, .. }) = tokens.get_mut(op.token_index) {
            *heredoc = Some(HereDocBody {
                text: body,
                delim,
                expand,
                line: op.line,
                eof,
            });
        }
    }
    (pos, consumed)
}

/// Decode a bash ANSI-C quoted string (`$'...'`). `chars[i]` must be the `$`
/// and `chars[i + 1]` the opening quote. Returns the decoded text and the
/// index just after the closing quote.
fn read_ansi_c(chars: &[char], i: usize) -> Option<(String, usize)> {
    let mut out = String::new();
    let mut j = i + 2;
    // bash 用 C 字符串保存 $'...'，任何转义产生 NUL 都会截断其后全部内容。
    let mut nul_seen = false;
    // $'\uHHHH' 的解释取决于字符分类 locale，而不是消息 locale。
    let utf8 = crate::i18n::is_utf8_ctype();

    while j < chars.len() {
        let c = chars[j];
        if c == '\'' {
            return Some((out, j + 1));
        }
        if c != '\\' || j + 1 >= chars.len() {
            if !nul_seen {
                out.push(c);
            }
            j += 1;
            continue;
        }
        let n = chars[j + 1];
        j += 2;
        match n {
            'a' => {
                if !nul_seen {
                    out.push('\u{07}')
                }
            }
            'b' => {
                if !nul_seen {
                    out.push('\u{08}')
                }
            }
            'e' | 'E' => {
                if !nul_seen {
                    out.push('\u{1b}')
                }
            }
            'f' => {
                if !nul_seen {
                    out.push('\u{0c}')
                }
            }
            'n' => {
                if !nul_seen {
                    out.push('\n')
                }
            }
            'r' => {
                if !nul_seen {
                    out.push('\r')
                }
            }
            't' => {
                if !nul_seen {
                    out.push('\t')
                }
            }
            'v' => {
                if !nul_seen {
                    out.push('\u{0b}')
                }
            }
            '\\' => {
                if !nul_seen {
                    out.push('\\')
                }
            }
            '\'' => {
                if !nul_seen {
                    out.push('\'')
                }
            }
            '"' => {
                if !nul_seen {
                    out.push('"')
                }
            }
            '?' => {
                if !nul_seen {
                    out.push('?')
                }
            }
            '0'..='7' => {
                let mut val = n.to_digit(8).unwrap();
                let mut cnt = 1;
                while cnt < 3 && j < chars.len() && chars[j].is_digit(8) {
                    val = val * 8 + chars[j].to_digit(8).unwrap();
                    j += 1;
                    cnt += 1;
                }
                if !nul_seen {
                    emit_ansi_byte(&mut out, &mut nul_seen, (val & 0xff) as u8);
                }
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
                    if !nul_seen {
                        out.push_str("\\x");
                    }
                } else {
                    emit_ansi_byte(&mut out, &mut nul_seen, (val & 0xff) as u8);
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
                if nul_seen {
                    continue;
                }
                if cnt == 0 {
                    out.push('\\');
                    out.push(n);
                } else if utf8 {
                    match char::from_u32(val) {
                        Some('\0') => nul_seen = true,
                        // 源码里的字面哨兵由入口 escape_literal 双写；这里是
                        // lexer 新生成的字符，必须同样双写，否则与后续 PUA
                        // 拼成 ESC+PUA 会在 egress 被误当成编码字节。
                        Some('\u{e100}') => {
                            out.push('\u{e100}');
                            out.push('\u{e100}');
                        }
                        Some(ch) => out.push(ch),
                        None => out.push('\u{fffd}'),
                    }
                } else if val <= 0xff {
                    // 单字节 locale：\u00e9 是一个裸字节 e9，不是 U+00E9
                    // （后者会被 UTF-8 编码成两个字节）。走 byteval 编码。
                    emit_ansi_byte(&mut out, &mut nul_seen, val as u8);
                } else if val <= 0xffff {
                    // bash 把不能单字节表示的码点回退成 \uXXXX（大写十六进制），
                    // 包括 \U0000E000 这类 8 位写法。
                    out.push_str(&format!("\\u{:04X}", val));
                } else {
                    out.push_str(&format!("\\U{:08X}", val));
                }
            }
            other => {
                if !nul_seen {
                    out.push('\\');
                    out.push(other);
                }
            }
        }
    }
    None
}

/// ANSI-C 转义产生的单个字节写入 `out`：NUL 触发截断，高位字节走 byteval
/// 编码（直接 `as char` 会得到 UTF-8 双字节，不是原始单字节）。
fn emit_ansi_byte(out: &mut String, nul_seen: &mut bool, b: u8) {
    if b == 0 {
        *nul_seen = true;
        return;
    }
    if b < 0x80 {
        out.push(b as char);
    } else {
        out.push_str(&crate::byteval::encode_raw(&[b]));
    }
}

pub(crate) fn read_var_name(chars: &[char], i: usize) -> String {
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
pub(crate) fn is_expansion_start(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '_'
        || matches!(c, '*' | '@' | '#' | '?' | '-' | '$' | '!')
}
