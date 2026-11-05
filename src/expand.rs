// Phase 1 expander: evaluate `WordPart` lists into the concrete words that
// the executor will run. Implements the bash expansion order (simplified):
// brace -> tilde -> parameter/command/arithmetic -> word-splitting -> glob.
//
// Quoted parts (originating inside double quotes) are NOT word-split or
// globbed. Arithmetic results are never split/globbed.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::lexer::WordPart;
use crate::parser;
use crate::shell::VarAttrs;

pub struct ExpandCtx<'a> {
    pub env: &'a HashMap<String, String>,
    pub positional: &'a [String],
    pub last_status: i32,
    pub self_pid: i32,
    pub noglob: bool,
    pub nounset: bool,
    /// The pid of the most recently started background job (`$!`).
    pub bg_pid: i32,
    /// The value of `$0` (the shell name, or the script path while a script
    /// is being sourced).
    pub dollar_zero: &'a str,
    /// The variable attributes a command substitution inherits.
    pub attrs: &'a VarAttrs,
    /// The function table a command substitution inherits.  Like bash, a
    /// `$(...)` subshell can call functions defined in the parent shell, which
    /// is what makes recursive functions (e.g. `fact`) work through
    /// command substitution.
    pub functions: &'a HashMap<String, parser::CommandList>,
    /// Array variables (`name=(a b c)`).  Indexed by the (scalar) array name.
    pub arrays: &'a HashMap<String, Vec<String>>,
    /// The assignments requested by the `${var=word}` / `${var:=word}`
    /// expansions.
    ///
    /// The expander only has a read-only view of the shell environment, so
    /// the assignments are collected here and written back to the shell by
    /// the executor once the expansion is done.
    pub pending_assign: Option<&'a RefCell<Vec<(String, String)>>>,
}

/// The result of evaluating a parameter expansion.
///
/// A scalar expansion is a single string that, when unquoted, still undergoes
/// IFS word-splitting.  The `@` and `*` forms of an array expansion, however,
/// are already a *list* of fields: `@` yields one field per element (never
/// re-split) and `*` yields a single field with the elements joined by the
/// first character of `$IFS`.  Keeping these as a pre-split list lets the
/// expander produce the correct number of words for `${arr[@]}` / `${arr[*]}`.
enum Expansion {
    Scalar(String),
    At(Vec<String>),
    Star(Vec<String>),
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
            WordPart::Var(name, _) => out.push_str(&expansion_to_scalar(eval_var(name, ctx), ctx)),
            WordPart::Braced(body, _) => {
                out.push_str(&expansion_to_scalar(eval_braced(body, ctx), ctx))
            }
            WordPart::CmdSub(body, _) | WordPart::Backtick(body, _) => {
                out.push_str(&crate::executor::capture_command(
                    body,
                    ctx.env,
                    ctx.attrs,
                    ctx.functions,
                    ctx.arrays,
                ));
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

/// A word under construction.
///
/// Bash expands a word left-to-right.  When an unquoted expansion splits into
/// several fields, only the **last** field is still part of the word being
/// built; all earlier fields are complete and must not receive the word's
/// suffix.  `open` marks that still-growing field:
///
/// ```text
/// pre$@suf   ->  [ "pre" + first ] [ middle ... ] [ last + "suf" ]
/// ```
#[derive(Clone, Debug)]
struct Candidate {
    text: String,
    open: bool,
}

/// Append literal text to every still-open candidate.  Closed fields are
/// already complete words and stay untouched.
fn append_literal(candidates: Vec<Candidate>, text: &str) -> Vec<Candidate> {
    candidates
        .into_iter()
        .map(|c| {
            if c.open {
                Candidate { text: format!("{}{}", c.text, text), open: true }
            } else {
                c
            }
        })
        .collect()
}

/// Expand a single word into 0+ resulting words (splitting + globbing for
/// unquoted expansions).
pub fn expand_word(parts: &[WordPart], ctx: &ExpandCtx) -> Vec<String> {
    let mut candidates: Vec<Candidate> = vec![Candidate { text: String::new(), open: true }];
    for part in parts {
        match part {
            WordPart::Text(s, quoted) => {
                let mut next = append_literal(candidates, s);
                if !*quoted && !ctx.noglob {
                    // Unquoted literal text is still subject to globbing.  A
                    // glob that yields several matches turns all but the last
                    // into complete fields.
                    let mut globbed = Vec::new();
                    for c in next.drain(..) {
                        if !c.open {
                            globbed.push(c);
                            continue;
                        }
                        let matches = glob(&c.text);
                        let n = matches.len();
                        for (k, m) in matches.into_iter().enumerate() {
                            globbed.push(Candidate { text: m, open: k + 1 == n });
                        }
                    }
                    next = globbed;
                }
                candidates = next;
            }
            WordPart::Tilde => {
                let home = ctx.env.get("HOME").cloned().unwrap_or_else(|| "/".to_string());
                candidates = candidates
                    .into_iter()
                    .map(|c| {
                        if c.open {
                            Candidate {
                                text: if c.text.is_empty() { home.clone() } else { format!("{}~", c.text) },
                                open: true,
                            }
                        } else {
                            c
                        }
                    })
                    .collect();
            }
            WordPart::Var(name, quoted) => {
                let exp = eval_var(name, ctx);
                candidates = apply_value(candidates, exp, *quoted, ctx);
            }
            WordPart::Braced(body, quoted) => {
                let exp = eval_braced(body, ctx);
                candidates = apply_value(candidates, exp, *quoted, ctx);
            }
            WordPart::CmdSub(body, quoted) | WordPart::Backtick(body, quoted) => {
                let val = crate::executor::capture_command(
                    body,
                    ctx.env,
                    ctx.attrs,
                    ctx.functions,
                    ctx.arrays,
                );
                candidates = apply_value(candidates, Expansion::Scalar(val), *quoted, ctx);
            }
            WordPart::Arith(body, _) => {
                let val = eval_arith(body, ctx);
                candidates = append_literal(candidates, &val);
            }
            WordPart::Brace(alts) => {
                // Brace expansion forks the word into several *open* words.
                let mut next = Vec::with_capacity(candidates.len() * alts.len());
                for c in candidates {
                    if !c.open {
                        next.push(c);
                        continue;
                    }
                    for a in alts {
                        next.push(Candidate { text: format!("{}{}", c.text, a), open: true });
                    }
                }
                candidates = next;
            }
        }
    }
    candidates.into_iter().map(|c| c.text).collect()
}

/// The value of `$IFS` for word splitting.  An *unset* IFS behaves like the
/// default `" \t\n"`; an explicitly empty IFS disables splitting entirely.
fn ifs_value(ctx: &ExpandCtx) -> String {
    ctx.env
        .get("IFS")
        .cloned()
        .unwrap_or_else(|| " \t\n".to_string())
}

/// The separator bash uses to join `$*` / `${arr[*]}` / `${arr[@]}` in a
/// scalar context: the first character of IFS, or `""` when IFS is empty.
fn ifs_join_sep(ctx: &ExpandCtx) -> String {
    match ctx.env.get("IFS") {
        Some(s) => s.chars().next().map(|c| c.to_string()).unwrap_or_default(),
        None => " ".to_string(),
    }
}

/// Split `val` on the characters of `ifs` following bash's rules:
///
/// * IFS whitespace (space/tab/newline present in IFS) is ignored at the
///   start/end; a run of it delimits one field.
/// * An IFS character that is *not* whitespace, together with any adjacent
///   IFS whitespace, delimits one field.  Consecutive non-whitespace IFS
///   characters therefore produce empty fields (`:a` -> `["", "a"]`).
/// * A trailing delimiter does not produce an extra empty field.
fn split_ifs(val: &str, ifs: &str) -> Vec<String> {
    if ifs.is_empty() {
        return if val.is_empty() { Vec::new() } else { vec![val.to_string()] };
    }
    let chars: Vec<char> = val.chars().collect();
    let is_ws = |c: char| matches!(c, ' ' | '\t' | '\n') && ifs.contains(c);
    let is_nonws = |c: char| !matches!(c, ' ' | '\t' | '\n') && ifs.contains(c);
    let n = chars.len();
    let mut i = 0usize;
    while i < n && is_ws(chars[i]) {
        i += 1;
    }
    let mut fields: Vec<String> = Vec::new();
    let mut cur = String::new();
    while i < n {
        let c = chars[i];
        if is_ws(c) {
            while i < n && is_ws(chars[i]) {
                i += 1;
            }
            // Absorb one adjacent non-whitespace IFS char into this delimiter.
            if i < n && is_nonws(chars[i]) {
                i += 1;
                while i < n && is_ws(chars[i]) {
                    i += 1;
                }
            }
            fields.push(std::mem::take(&mut cur));
        } else if is_nonws(c) {
            i += 1;
            while i < n && is_ws(chars[i]) {
                i += 1;
            }
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
            i += 1;
        }
    }
    // Only a non-empty pending field survives the trailing delimiter.
    if !cur.is_empty() {
        fields.push(cur);
    }
    fields
}

/// The final field list produced by one expansion (before the word's prefix
/// and suffix are attached).
fn expansion_fields(exp: Expansion, quoted: bool, ctx: &ExpandCtx) -> Vec<String> {
    if quoted {
        return match exp {
            Expansion::Scalar(s) => vec![s],
            Expansion::At(fs) => fs,
            Expansion::Star(fs) => vec![fs.join(&ifs_join_sep(ctx))],
        };
    }
    let ifs = ifs_value(ctx);
    let mut fields: Vec<String> = Vec::new();
    match exp {
        Expansion::Scalar(s) => fields.extend(split_ifs(&s, &ifs)),
        Expansion::At(fs) => {
            // Unquoted `$@`: each positional parameter / element is split
            // independently, then the resulting lists are concatenated.
            for f in &fs {
                fields.extend(split_ifs(f, &ifs));
            }
        }
        Expansion::Star(fs) => {
            let joined = fs.join(&ifs_join_sep(ctx));
            fields.extend(split_ifs(&joined, &ifs));
        }
    }
    let mut out = Vec::new();
    for f in &fields {
        out.extend(glob_fields("", f, ctx.noglob));
    }
    out
}

/// Append `exp` to each open candidate.  When it yields several fields, only
/// the first inherits the word's accumulated prefix and only the last stays
/// open to receive the suffix — matching bash's `pre$@suf` behaviour.
fn apply_value(candidates: Vec<Candidate>, exp: Expansion, quoted: bool, ctx: &ExpandCtx) -> Vec<Candidate> {
    let fields = expansion_fields(exp, quoted, ctx);
    let mut next = Vec::new();
    for c in candidates {
        if !c.open {
            next.push(c);
            continue;
        }
        if fields.is_empty() {
            next.push(c);
            continue;
        }
        let n = fields.len();
        for (k, f) in fields.iter().enumerate() {
            if k == 0 {
                next.push(Candidate {
                    text: format!("{}{}", c.text, f),
                    open: n == 1,
                });
            } else {
                next.push(Candidate {
                    text: f.clone(),
                    open: k + 1 == n,
                });
            }
        }
    }
    next
}

/// Reduce an `Expansion` to a single scalar string (used in assignment-RHS
/// contexts, where array `@`/`*` are joined by the first character of `$IFS`).
fn expansion_to_scalar(exp: Expansion, ctx: &ExpandCtx) -> String {
    match exp {
        Expansion::Scalar(s) => s,
        Expansion::At(fs) | Expansion::Star(fs) => fs.join(&ifs_join_sep(ctx)),
    }
}

// ---------------------------------------------------------------------------
// Parameter evaluation
// ---------------------------------------------------------------------------

fn eval_var(name: &str, ctx: &ExpandCtx) -> Expansion {
    match name {
        "" => Expansion::Scalar(String::new()),
        "?" => Expansion::Scalar(ctx.last_status.to_string()),
        "$" => Expansion::Scalar(ctx.self_pid.to_string()),
        "#" => Expansion::Scalar(ctx.positional.len().to_string()),
        "@" => Expansion::At(ctx.positional.to_vec()),
        "*" => {
            // `$*` is an error under `set -u` when there are no positional
            // parameters (`$@` never is).
            if ctx.nounset && ctx.positional.is_empty() {
                unbound_variable("$*");
            }
            Expansion::Star(ctx.positional.to_vec())
        }
        "0" => Expansion::Scalar(ctx.dollar_zero.to_string()),
        "!" => {
            // `$!` is the pid of the most recent background job; it is empty
            // when no background job has been started.
            if ctx.bg_pid == 0 {
                Expansion::Scalar(String::new())
            } else {
                Expansion::Scalar(ctx.bg_pid.to_string())
            }
        }
        "-" => Expansion::Scalar(String::new()), // shell options (Phase 4+)
        _ => {
            if let Some(idx) = name.parse::<usize>().ok() {
                if idx >= 1 && idx <= ctx.positional.len() {
                    return Expansion::Scalar(ctx.positional[idx - 1].clone());
                }
                if ctx.nounset {
                    unbound_variable(&format!("${}", idx));
                }
                return Expansion::Scalar(String::new());
            }
            // An array name with no subscript expands to element 0 (just like
            // `$arr` / `${arr}` does in bash).
            if let Some(elems) = ctx.arrays.get(name) {
                if elems.is_empty() {
                    if ctx.nounset {
                        unbound_variable(name);
                    }
                    return Expansion::Scalar(String::new());
                }
                return Expansion::Scalar(elems[0].clone());
            }
            match ctx.env.get(name) {
                Some(value) => Expansion::Scalar(value.clone()),
                None => {
                    if ctx.nounset {
                        unbound_variable(name);
                    }
                    Expansion::Scalar(String::new())
                }
            }
        }
    }
}

fn eval_braced(body: &str, ctx: &ExpandCtx) -> Expansion {
    // forms: name, name:-w, name:=w, name:+w, name:?w, #name, name:off,
    // name:off:len, name#pat, name##pat, name%pat, name%%pat,
    // name/pat/repl, name//pat/repl, name^, name^^, name,, name,,
    // name[subscript], !name (indirect)
    //
    // The "bare" operators (`-`, `=`, `+`, `?`) differ from their colon
    // variants: they only look at whether the variable is *set*, not at
    // whether it is empty.
    if body.starts_with('#') {
        let name = &body[1..];
        if name.is_empty() {
            return Expansion::Scalar(ctx.positional.len().to_string());
        }
        // `${#arr[@]}` / `${#arr[*]}`: the number of elements.
        // `${#arr[i]}`: the length of element `i`.
        if let Some((arr, subscript)) = split_subscript(name) {
            let elems = ctx.arrays.get(arr);
            return match subscript {
                "@" | "*" => {
                    Expansion::Scalar(elems.map(|e| e.len()).unwrap_or(0).to_string())
                }
                _ => {
                    let len = match (elems, subscript.parse::<i64>()) {
                        (Some(e), Ok(i)) if i >= 0 && (i as usize) < e.len() => {
                            e[i as usize].chars().count()
                        }
                        _ => 0,
                    };
                    Expansion::Scalar(len.to_string())
                }
            };
        }
        if !ctx.env.contains_key(name) {
            if ctx.nounset {
                unbound_variable(name);
            }
            return Expansion::Scalar("0".to_string());
        }
        return Expansion::Scalar(
            ctx.env.get(name).map(|v| v.len().to_string()).unwrap_or_else(|| "0".to_string()),
        );
    }
    if body.starts_with('!') && !body.contains(':') && !body.contains('/') && !body.contains('%')
        && !body.contains('#')
    {
        // indirect expansion ${!VAR}
        let name = &body[1..];
        let target = ctx.env.get(name).cloned().unwrap_or_default();
        return Expansion::Scalar(ctx.env.get(&target).cloned().unwrap_or_default());
    }

    let (rawname, op, arg) = split_braced(body);

    // Numeric names are positional parameters (`${1}`, `${10}`, `${0}`), not
    // environment variables.  Route them through the same resolver used by
    // bare `$n` so `${n:-word}`, `${n:+word}` etc. behave like bash.
    if !rawname.is_empty() && rawname.bytes().all(|b| b.is_ascii_digit()) {
        let val = resolve_param(rawname, ctx);
        let is_set = !val.is_empty();
        return Expansion::Scalar(apply_scalar_op(
            &val,
            rawname,
            op,
            arg,
            is_set,
            !is_set,
            ctx,
        ));
    }

    // Array subscript handling: `name[subscript]`.  `${a[@]}` / `${a[*]}`
    // yield one field per element / a single IFS-joined field, and
    // `${a[i]}` yields element `i` (0-based) as a scalar.
    if let Some((name, subscript)) = split_subscript(rawname) {
        let elems = ctx.arrays.get(name).cloned().unwrap_or_default();
        match subscript {
            "@" => return Expansion::At(elems),
            "*" => return Expansion::Star(elems),
            _ => {
                let val = if let Ok(i) = subscript.parse::<i64>() {
                    if i >= 0 && (i as usize) < elems.len() {
                        elems[i as usize].clone()
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };
                return Expansion::Scalar(apply_scalar_op(
                    &val,
                    name,
                    op,
                    arg,
                    !val.is_empty(),
                    val.is_empty(),
                    ctx,
                ));
            }
        }
    }

    // A name that names an array (with no subscript) expands to element 0,
    // just like `$arr` / `${arr}` does in bash.
    let arr_elems = ctx.arrays.get(rawname);
    let is_set = ctx.env.contains_key(rawname) || arr_elems.is_some();
    let value = match arr_elems {
        Some(e) if !e.is_empty() => e[0].clone(),
        _ => ctx.env.get(rawname).cloned().unwrap_or_default(),
    };
    let unset_or_empty = !is_set || value.is_empty();

    let result = apply_scalar_op(&value, rawname, op, arg, is_set, unset_or_empty, ctx);
    Expansion::Scalar(result)
}

/// Split `raw` into `(name, subscript)` when it has the form `name[subscript]`.
/// Returns `None` for a plain (non-subscripted) name.
fn split_subscript(raw: &str) -> Option<(&str, &str)> {
    let open = raw.find('[')?;
    let rest = &raw[open + 1..];
    let close = rest.find(']')?;
    let name = &raw[..open];
    if name.is_empty() {
        return None;
    }
    Some((name, &rest[..close]))
}

/// Apply a (possibly empty) braced-operator to a scalar `val`, returning the
/// resulting string.  `is_set` / `unset_or_empty` carry the variable's status
/// so the bare vs colon operators behave correctly.  The `""` (no-operator)
/// arm also forwards special parameters to `eval_var`.
fn apply_scalar_op(
    val: &str,
    name: &str,
    op: &str,
    arg: &str,
    is_set: bool,
    unset_or_empty: bool,
    ctx: &ExpandCtx,
) -> String {
    match op {
        "-" => {
            if is_set {
                val.to_string()
            } else {
                arg.to_string()
            }
        }
        ":-" => {
            if unset_or_empty {
                arg.to_string()
            } else {
                val.to_string()
            }
        }
        "+" => {
            if is_set {
                arg.to_string()
            } else {
                String::new()
            }
        }
        ":+" => {
            if unset_or_empty {
                String::new()
            } else {
                arg.to_string()
            }
        }
        "=" => {
            if is_set {
                val.to_string()
            } else {
                assign_default(ctx, name, arg);
                arg.to_string()
            }
        }
        ":=" => {
            if unset_or_empty {
                assign_default(ctx, name, arg);
                arg.to_string()
            } else {
                val.to_string()
            }
        }
        "?" => {
            if is_set {
                val.to_string()
            } else {
                expansion_error(name, arg)
            }
        }
        ":?" => {
            if unset_or_empty {
                expansion_error(name, arg);
            }
            val.to_string()
        }
        ":" => substring(val, arg),
        "#" => strip_prefix(val, arg, false),
        "##" => strip_prefix(val, arg, true),
        "%" => strip_suffix(val, arg, false),
        "%%" => strip_suffix(val, arg, true),
        "/" | "//" => replace_pattern(val, arg, op == "//"),
        "^" => {
            let mut c = val.chars();
            match c.next() {
                Some(f) => format!("{}{}", f.to_uppercase().collect::<String>(), c.collect::<String>()),
                None => val.to_string(),
            }
        }
        "^^" => val.to_uppercase(),
        "," => {
            let mut c = val.chars();
            match c.next() {
                Some(f) => format!("{}{}", f.to_lowercase().collect::<String>(), c.collect::<String>()),
                None => val.to_string(),
            }
        }
        ",," => val.to_lowercase(),
        "" => {
            // A plain `${NAME}` reference.  Under `set -u` an unset variable
            // is an error; the special parameters are forwarded to `eval_var`.
            if !is_set && ctx.nounset {
                unbound_variable(name);
            }
            if is_special_param(name) {
                return expansion_to_scalar(eval_var(name, ctx), ctx);
            }
            val.to_string()
        }
        _ => val.to_string(),
    }
}

/// True for the parameters that are not stored in the variable table
/// (`?`, `$`, `#`, `*`, `@`, `!`, `-`, `0`).
fn is_special_param(name: &str) -> bool {
    matches!(name, "?" | "$" | "#" | "*" | "@" | "!" | "-" | "0")
}

/// Record the assignment requested by `${var=word}` / `${var:=word}`.
fn assign_default(ctx: &ExpandCtx, name: &str, value: &str) {
    if let Some(cell) = &ctx.pending_assign {
        cell.borrow_mut().push((name.to_string(), value.to_string()));
    }
}

/// Report a failed parameter expansion (`${var?word}`) and abort the shell
/// the way a non-interactive bash does.
fn expansion_error(name: &str, word: &str) -> ! {
    eprintln!("wash: {}: {}", name, word);
    std::process::exit(127);
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
            // The "bare" operators only count as an operator when a variable
            // name precedes them: `${?}` is the exit status, not an operator.
            '-' | '=' | '+' | '?' if i > 0 => {
                return (&body[..i], &body[i..i + 1], &body[i + 1..]);
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

/// Expand one unquoted field into **one word per glob match**.  Joining all
/// matches with a space (the old `glob_one`) collapsed them into a single
/// argument, breaking `x=dir/*.txt; printf "[%s]" $x`, `for f in $x`, etc.
fn glob_fields(prefix: &str, field: &str, noglob: bool) -> Vec<String> {
    if noglob {
        return vec![format!("{}{}", prefix, field)];
    }
    let matches = glob(field);
    if matches.is_empty() {
        vec![format!("{}{}", prefix, field)]
    } else {
        matches
            .iter()
            .map(|m| format!("{}{}", prefix, m))
            .collect()
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
    let (mut dir, base) = split_dir(pattern);
    // A pattern without a directory component refers to the current directory;
    // `read_dir("")` fails, so normalise the empty directory to ".".
    if dir.is_empty() {
        dir = ".".to_string();
    }
    // Resolve the directory: msys drive paths must become native Windows paths
    // for `std::fs::read_dir`, but we keep the msys form for the output.
    let (dir_for_read, dir_for_out) = match translate_msys_path(&dir) {
        Some(win) => (win, dir.clone()),
        None => (dir.clone(), dir.clone()),
    };
    let out_prefix = if dir_for_out.is_empty() || dir_for_out == "." {
        String::new()
    } else {
        format!("{}/", dir_for_out)
    };
    let patc: Vec<char> = base.chars().collect();
    // A leading dot in the pattern opts into matching hidden files.
    let hidden_ok = base.starts_with('.');
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir_for_read) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // Skip dotfiles unless the pattern explicitly asks for them.
            if name.starts_with('.') && !hidden_ok {
                continue;
            }
            let nc: Vec<char> = name.chars().collect();
            if glob_match(&nc, &patc) {
                out.push(if out_prefix.is_empty() {
                    name
                } else {
                    format!("{}{}", out_prefix, name)
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

/// Glob matcher supporting `*`, `?` and `[...]` character classes (with `!`/`^`
/// negation and `a-z` ranges). Recursive backtracking keeps the `*` handling
/// correct without the off-by-one risks of the previous iterative version.
pub fn glob_match(s: &[char], pat: &[char]) -> bool {
    fn rec(s: &[char], si: usize, pat: &[char], pi: usize) -> bool {
        let mut si = si;
        let mut pi = pi;
        while pi < pat.len() {
            match pat[pi] {
                '*' => {
                    // Try every alignment of the rest of the pattern.
                    let mut k = si;
                    while k <= s.len() {
                        if rec(s, k, pat, pi + 1) {
                            return true;
                        }
                        k += 1;
                    }
                    return false;
                }
                '?' => {
                    if si >= s.len() {
                        return false;
                    }
                    si += 1;
                    pi += 1;
                }
                '[' => {
                    if si >= s.len() {
                        return false;
                    }
                    let mut j = pi + 1;
                    let neg = if j < pat.len() && (pat[j] == '!' || pat[j] == '^') {
                        j += 1;
                        true
                    } else {
                        false
                    };
                    let mut matched = false;
                    // A `]` immediately after `[` or `[!` is a literal `]`.
                    if j < pat.len() && pat[j] == ']' {
                        if s[si] == ']' {
                            matched = true;
                        }
                        j += 1;
                    }
                    let mut k = j;
                    while k < pat.len() && pat[k] != ']' {
                        if k + 2 < pat.len() && pat[k + 1] == '-' && pat[k + 2] != ']' {
                            let lo = pat[k];
                            let hi = pat[k + 2];
                            if s[si] >= lo && s[si] <= hi {
                                matched = true;
                            }
                            k += 3;
                        } else {
                            if s[si] == pat[k] {
                                matched = true;
                            }
                            k += 1;
                        }
                    }
                    if neg {
                        matched = !matched;
                    }
                    if !matched {
                        return false;
                    }
                    si += 1;
                    pi = k + 1; // consume the closing `]`
                }
                c => {
                    if si >= s.len() || s[si] != c {
                        return false;
                    }
                    si += 1;
                    pi += 1;
                }
            }
        }
        si == s.len()
    }
    rec(s, 0, pat, 0)
}

// ---------------------------------------------------------------------------
// Arithmetic $(( ... ))
// ---------------------------------------------------------------------------

/// Adapter exposing the shell's expansion context to `arith::eval`, so that
/// `$(( ... ))` uses the same panic-free engine as `let` / `(( ))` (proper
/// `**`, wrapping shifts, negative-exponent errors, etc.).
struct ArithCtx<'a> {
    ctx: &'a ExpandCtx<'a>,
    /// Values assigned inside the expression. Later reads in the SAME
    /// expression must see them (`$(((x=5)+x))` is 10, not 5).
    assigned: RefCell<HashMap<String, i64>>,
}

impl<'a> crate::arith::ArithEnv for ArithCtx<'a> {
    fn get(&self, name: &str) -> Option<i64> {
        if let Some(v) = self.assigned.borrow().get(name) {
            return Some(*v);
        }
        if let Ok(n) = name.parse::<usize>() {
            if n == 0 {
                return self.ctx.dollar_zero.parse::<i64>().ok();
            }
            return self.ctx.positional.get(n - 1).and_then(|s| s.parse::<i64>().ok());
        }
        match name {
            "@" | "*" => return None,
            "#" => return Some(self.ctx.positional.len() as i64),
            "?" => return Some(self.ctx.last_status as i64),
            "0" => return self.ctx.dollar_zero.parse::<i64>().ok(),
            _ => {}
        }
        self.ctx.env.get(name).and_then(|v| v.parse::<i64>().ok())
    }
    fn set(&mut self, name: &str, value: i64) {
        self.assigned.borrow_mut().insert(name.to_string(), value);
        // Persist the assignment through the same channel used by
        // `${var=word}`; the executor drains it into `sh.env` after expansion,
        // so `x=0; echo $((x=5)); echo $x` prints 5 / 5 like bash.
        if let Some(cell) = &self.ctx.pending_assign {
            cell.borrow_mut().push((name.to_string(), value.to_string()));
        }
    }
}

pub fn eval_arith(body: &str, ctx: &ExpandCtx) -> String {
    let mut env = ArithCtx {
        ctx,
        assigned: RefCell::new(HashMap::new()),
    };
    match crate::arith::eval(body, &mut env) {
        Ok(v) => v.to_string(),
        Err(e) => {
            eprintln!("wash: {} (error token is \"{}\")", e.message, e.token);
            String::new()
        }
    }
}

/// Resolve a parameter name to its string value, mirroring bash's expansion
/// order used inside arithmetic: a numeric name (`1`, `2`, ...) refers to a
/// positional parameter, `$0`/`$@`/`$*`/`$#`/`$?` to the usual special
/// parameters, and any other name to an environment variable.
fn resolve_param(name: &str, ctx: &ExpandCtx) -> String {
    if let Ok(n) = name.parse::<usize>() {
        if n == 0 {
            return ctx.dollar_zero.to_string();
        }
        return ctx.positional.get(n - 1).cloned().unwrap_or_default();
    }
    match name {
        "@" | "*" => ctx.positional.join(" "),
        "#" => ctx.positional.len().to_string(),
        "?" => ctx.last_status.to_string(),
        "0" => ctx.dollar_zero.to_string(),
        _ => ctx.env.get(name).cloned().unwrap_or_default(),
    }
}
