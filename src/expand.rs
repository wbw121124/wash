// Phase 1 expander: evaluate `WordPart` lists into the concrete words that
// the executor will run. Implements the bash expansion order (simplified):
// brace -> tilde -> parameter/command/arithmetic -> word-splitting -> glob.
//
// Quoted parts (originating inside double quotes) are NOT word-split or
// globbed. Arithmetic results are never split/globbed.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::lexer::WordPart;
use crate::parser;
use crate::shell::VarAttrs;

thread_local! {
    /// Set when a `$(( ))` / `$[ ]` expansion fails.  Bash treats a failed
    /// arithmetic expansion as a fatal error for the whole command line: the
    /// current command is not run, the remaining commands are skipped and the
    /// status is 1 -- while the interactive shell itself survives.  The
    /// expander only has a read-only view of the shell, so the flag travels
    /// through thread-local state (command substitutions run in forked
    /// children and therefore cannot clobber the parent's flag).
    static ARITH_FAILED: Cell<bool> = Cell::new(false);
    /// A fatal parameter expansion (`set -u` unbound variable, `${var?word}`).
    /// Kept separate from ARITH_FAILED because it carries exit status 127
    /// instead of 1. A non-interactive shell exits with 127; an interactive
    /// REPL only aborts the current line so the session survives.
    static EXPANSION_ABORT: Cell<bool> = Cell::new(false);
}

/// Whether the current command line hit a fatal arithmetic expansion error.
pub fn arith_failed() -> bool {
    ARITH_FAILED.with(|c| c.get())
}

/// Reset the fatal-arithmetic flag at the start of each top-level command line.
pub fn clear_arith_failed() {
    ARITH_FAILED.with(|c| c.set(false))
}

/// Report a fatal expansion error once and mark the command line as aborted.
fn flag_arith_failure(message: &str) {
    if !arith_failed() {
        eprintln!("wash: {}", message);
        ARITH_FAILED.with(|c| c.set(true));
    }
}

/// Whether the current command line hit `set -u` / `${var?word}`.
pub fn expansion_aborted() -> bool {
    EXPANSION_ABORT.with(|c| c.get())
}

/// Reset the fatal-parameter-expansion flag for each top-level command line.
pub fn clear_expansion_abort() {
    EXPANSION_ABORT.with(|c| c.set(false))
}

/// Print bash's diagnostic once and mark this command line as aborted. The
/// caller (`Shell::run_str`) decides whether the process exits or the REPL
/// survives, so the expander needs no knowledge of interactivity.
fn flag_expansion_abort(message: &str) {
    if !expansion_aborted() {
        eprintln!("wash: {}", message);
        EXPANSION_ABORT.with(|c| c.set(true));
    }
}

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
    /// The current value of `$-`, snapshotted from the shell's option state
    /// and startup flags when this expansion context is built.  It is kept as
    /// an owned String (not injected into `env`) so it stays in sync with
    /// `set` across separate commands.
    pub shell_flags: String,
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

/// Report an unbound variable the way bash does under `set -u`: print the
/// diagnostic and abort the current command line. `Shell::run_str` exits a
/// non-interactive shell with 127 but keeps an interactive REPL alive.
fn unbound_variable(name: &str) -> String {
    flag_expansion_abort(&format!("{}: unbound variable", name));
    String::new()
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
            // `$*` is NOT an error under `set -u` (unlike `$1`): bash treats
            // the positional parameters as set even when there are none, so it
            // expands to the empty string with no diagnostic.
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
        "-" => Expansion::Scalar(ctx.shell_flags.clone()),
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

    // `${@:off:len}` / `${*:off:len}` slice the positional parameters.
    // The unsliced forms are handled by `eval_var`.
    if matches!(rawname, "@" | "*") && op == ":" {
        // bash counts positional slices from `$0`: `${@:1:2}` yields `$1 $2`.
        // The leading `$0` is shell-identity bound, so tests avoid offset 0.
        let mut params = Vec::with_capacity(ctx.positional.len() + 1);
        params.push(ctx.dollar_zero.to_string());
        params.extend(ctx.positional.iter().cloned());
        let fields = slice_fields(params, arg, ctx);
        return if rawname == "@" {
            Expansion::At(fields)
        } else {
            Expansion::Star(fields)
        };
    }

    // Array subscript handling: `name[subscript]`.  `${a[@]}` / `${a[*]}`
    // yield one field per element / a single IFS-joined field, and
    // `${a[i]}` yields element `i` (0-based) as a scalar.  A `:off:len`
    // suffix slices the element list before it is turned into fields.
    if let Some((name, subscript)) = split_subscript(rawname) {
        let elems = ctx.arrays.get(name).cloned().unwrap_or_default();
        match subscript {
            "@" | "*" => {
                let fields = if op == ":" {
                    slice_fields(elems, arg, ctx)
                } else {
                    elems
                };
                return if subscript == "@" {
                    Expansion::At(fields)
                } else {
                    Expansion::Star(fields)
                };
            }
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

/// Evaluate an offset/length expression of a `${...:off:len}` expansion.
/// Bash evaluates these as arithmetic expressions, so `${a[@]:1+1:1}` is
/// legal; empty text means zero (no offset / no length limit).
fn eval_offset(expr: &str, ctx: &ExpandCtx) -> Option<i64> {
    let s = expr.trim();
    if s.is_empty() {
        return Some(0);
    }
    if let Ok(v) = s.parse::<i64>() {
        return Some(v);
    }
    let mut env = ArithCtx {
        ctx,
        assigned: RefCell::new(HashMap::new()),
    };
    crate::arith::eval(s, &mut env).ok()
}

/// Slice a list of fields for `${a[@]:off:len}` / `${@:off:len}`.
/// A negative offset counts back from the end.  A negative *length* is a
/// fatal error for a list slice in bash (`substring expression < 0`), which
/// aborts the whole command line just like a failed arithmetic expansion.
fn slice_fields(fields: Vec<String>, arg: &str, ctx: &ExpandCtx) -> Vec<String> {
    let n = fields.len() as i64;
    let (off_s, len_s) = match arg.split_once(':') {
        Some((a, b)) => (a, b),
        None => (arg, ""),
    };
    let mut off = eval_offset(off_s, ctx).unwrap_or(0);
    if off < 0 {
        off += n;
    }
    off = off.clamp(0, n);
    let start = off as usize;
    if len_s.is_empty() {
        return fields[start..].to_vec();
    }
    let len = eval_offset(len_s, ctx).unwrap_or(0);
    if len < 0 {
        flag_arith_failure(&format!("{}: substring expression < 0", len_s.trim()));
        return Vec::new();
    }
    fields[start..].iter().take(len as usize).cloned().collect()
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
            // Bash's special parameters (`? $ # * @ ! - 0`) are always
            // considered set under `set -u`, so they must be forwarded to
            // `eval_var` *before* the nounset check.  (`${!}` is intercepted
            // earlier by the indirect-expansion branch and `$!` is handled in
            // `eval_var`; both intentionally remain nounset-fatal separately.)
            if is_special_param(name) {
                return expansion_to_scalar(eval_var(name, ctx), ctx);
            }
            if !is_set && ctx.nounset {
                unbound_variable(name);
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

/// Report a failed `${var?word}` expansion and abort the current command
/// line; `Shell::run_str` decides whether the process exits or the REPL lives.
fn expansion_error(name: &str, word: &str) -> String {
    flag_expansion_abort(&format!("{}: {}", name, word));
    String::new()
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
    // A space is required before a negative offset (`${v: -3}`), otherwise
    // `:-` would be parsed as the "use default" operator.  Trim it here.
    let mut off = off_s.trim().parse::<i64>().unwrap_or(0);
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
        let len = len_s.trim().parse::<i64>().unwrap_or(0);
        if len < 0 {
            // A negative length means "up to |len| characters before the
            // end of the string": `${v:0:-1}` drops the last character.
            // When the resulting end lies before the start, bash aborts the
            // command line with "substring expression < 0".
            let end = n + len;
            if end < off {
                flag_arith_failure(&format!("{}: substring expression < 0", len_s.trim()));
                return String::new();
            }
            (end - off) as usize
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

/// Split the `pattern/replacement` argument of `${v/pat/rep}`.  A `/` escaped
/// with a backslash belongs to the pattern (`${v/\//-}`), so only an
/// unescaped slash separates the two halves.  With no slash at all bash
/// deletes the matches.
fn split_replacement(arg: &str) -> (&str, &str) {
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 2;
            continue;
        }
        if b[i] == b'/' {
            return (&arg[..i], &arg[i + 1..]);
        }
        i += 1;
    }
    (arg, "")
}

/// Pattern substitution works on **bytes**, not Unicode scalars.  The oracle
/// runs with `LC_ALL=C`, where bash treats a multibyte character as several
/// independent bytes (`${v//[!中]/X}` leaves the three bytes of `中` intact
/// and replaces every other byte).  Mapping each byte to a `char` in 0..=255
/// lets `glob_match` (which compares and ranges over `char`) implement exactly
/// that byte semantics.
fn bytes_as_chars(s: &str) -> Vec<char> {
    s.as_bytes().iter().map(|&b| b as char).collect()
}

fn chars_as_bytes(cs: &[char]) -> Vec<u8> {
    cs.iter().map(|&c| c as u8).collect()
}

/// Turn backslash escapes in a `${v/pat/rep}` pattern into forms the glob
/// matcher understands.  `\*`, `\?`, `\[`, `\]`, `\\` become one-character
/// bracket expressions so they compare literally; all other `\x` become `x`.
fn unescape_pattern(pat: &str) -> Vec<char> {
    let src = bytes_as_chars(pat);
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        if src[i] == '\\' && i + 1 < src.len() {
            let c = src[i + 1];
            if matches!(c, '*' | '?' | '[' | ']' | '\\') {
                out.push('[');
                out.push(c);
                out.push(']');
            } else {
                out.push(c);
            }
            i += 2;
        } else {
            out.push(src[i]);
            i += 1;
        }
    }
    out
}

/// Expand the replacement text of `${v/pat/rep}`: an unescaped `&` stands for
/// the matched bytes, and `\&` is a literal ampersand.
fn expand_replacement(repl: &str, matched: &[char]) -> Vec<char> {
    let src = bytes_as_chars(repl);
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        match src[i] {
            '\\' if i + 1 < src.len() && src[i + 1] == '&' => {
                out.push('&');
                i += 2;
            }
            '&' => {
                out.extend_from_slice(matched);
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn replace_pattern(value: &str, arg: &str, all: bool) -> String {
    let (raw_pat, repl) = split_replacement(arg);
    if raw_pat.is_empty() {
        return value.to_string();
    }
    // `#` anchors the pattern at the start of the value, `%` at the end.
    let (pat_src, anchor) = if let Some(rest) = raw_pat.strip_prefix('#') {
        (rest, 1i8)
    } else if let Some(rest) = raw_pat.strip_prefix('%') {
        (rest, -1i8)
    } else {
        (raw_pat, 0i8)
    };
    let pat = unescape_pattern(pat_src);
    let chars = bytes_as_chars(value);
    let n = chars.len();
    let mut out: Vec<char> = Vec::new();
    let mut i = 0usize;
    let mut copied = 0usize; // bytes of `value` already emitted
    let mut did_replace = false;

    // At `i == n` only a first, not-yet-replaced match may be tried: bash
    // does not append a second empty match after a `*` consumed the string.
    while i < n || (i == n && !did_replace) {
        if !all && did_replace {
            break;
        }
        // Find the longest match starting at `i` (bash prefers the longest
        // match at the earliest position).  `#` anchors at the start, `%`
        // requires the match to reach the end of the value.
        let mut best: Option<usize> = None;
        let mut j = n;
        loop {
            let allowed = match anchor {
                1 => i == 0,
                -1 => j == n,
                _ => true,
            };
            if allowed && glob_match(&chars[i..j], &pat) {
                best = Some(j);
                break;
            }
            if j == i {
                break;
            }
            j -= 1;
        }
        if let Some(end) = best {
            out.extend_from_slice(&chars[copied..i]);
            out.extend(expand_replacement(repl, &chars[i..end]));
            copied = end;
            did_replace = true;
            i = if end > i { end } else { i + 1 };
            if i > n {
                break;
            }
        } else {
            i += 1;
        }
    }
    out.extend_from_slice(&chars[copied..]);
    // Reconstruct the original bytes: a replacement may legally leave an
    // incomplete multibyte sequence, so use lossy decoding (bash keeps bytes).
    String::from_utf8_lossy(&chars_as_bytes(&out)).into_owned()
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

/// bash reports the *raw remainder* of the expression from the offending
/// token onwards: `1+*2` reports `*2`, and `$(( 1 / 0 ))` reports `"0 "`.
fn bash_error_token(body: &str, token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    match body.rfind(token) {
        Some(pos) => body[pos..].to_string(),
        None => token.to_string(),
    }
}

pub fn eval_arith(body: &str, ctx: &ExpandCtx) -> String {
    // Once a command line has failed bash evaluates no further arithmetic
    // expansions on it, and prints only the first diagnostic.  The early
    // return also avoids repeating assignments performed inside expressions.
    if arith_failed() {
        return String::new();
    }
    let mut env = ArithCtx {
        ctx,
        assigned: RefCell::new(HashMap::new()),
    };
    match crate::arith::eval(body, &mut env) {
        Ok(v) => v.to_string(),
        Err(e) => {
            // bash prefixes the expression itself, and labels parser-level
            // failures as "arithmetic syntax error".
            let msg = if e.message == "operand expected" {
                format!("arithmetic syntax error: {}", e.message)
            } else {
                e.message.clone()
            };
            flag_arith_failure(&format!(
                "{}: {} (error token is \"{}\")",
                body.trim_start(),
                msg,
                bash_error_token(body, &e.token)
            ));
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
