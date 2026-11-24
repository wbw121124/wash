// Phase 1 expander: evaluate `WordPart` lists into the concrete words that
// the executor will run. Implements the bash expansion order (simplified):
// brace -> tilde -> parameter/command/arithmetic -> word-splitting -> glob.
//
// Quoted parts (originating inside double quotes) are NOT word-split or
// globbed. Arithmetic results are never split/globbed.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use crate::lexer::WordPart;
use crate::parser;
use crate::shell::AttrRefs;

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
    /// A non-fatal parameter expansion error that still fails the command with
    /// status 1 (NOT the 127 reserved for `set -u`).  Currently only used for
    /// `${!VAR}` "invalid indirect expansion" / "invalid variable name", which
    /// bash reports with rc 1 and an empty stdout (the command is aborted).
    static EXPANSION_ABORT_RC1: Cell<bool> = Cell::new(false);
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

/// Reset the fatal-parameter-expansion flags for each top-level command line.
pub fn clear_expansion_abort() {
    EXPANSION_ABORT.with(|c| c.set(false));
    EXPANSION_ABORT_RC1.with(|c| c.set(false));
}

/// Whether the current command line hit the rc-1 `invalid indirect expansion`
/// error (distinct from the rc-127 `set -u` path).
pub fn expansion_aborted_rc1() -> bool {
    EXPANSION_ABORT_RC1.with(|c| c.get())
}

/// Return the abort status code if parameter expansion failed on this command
/// line, distinguishing the rc-127 `set -u` / `${var?word}` path from the rc-1
/// `invalid indirect expansion` path.  Returns `None` when expansion succeeded.
pub fn expansion_abort_code() -> Option<i32> {
    if EXPANSION_ABORT_RC1.with(|c| c.get()) {
        Some(1)
    } else if EXPANSION_ABORT.with(|c| c.get()) {
        Some(127)
    } else {
        None
    }
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

/// Report an `invalid indirect expansion` / `invalid variable name` error the
/// way bash does: print the diagnostic and abort the command line with status
/// 1 (NOT the 127 reserved for `set -u`).  The executor reads the abort via
/// `expansion_aborted_rc1` / `expansion_abort_code`.
fn flag_invalid_indirect(message: &str) {
    if !expansion_aborted() && !expansion_aborted_rc1() {
        eprintln!("wash: {}", message);
        EXPANSION_ABORT_RC1.with(|c| c.set(true));
    }
}

pub struct ExpandCtx<'a> {
    pub env: &'a HashMap<String, String>,
    pub positional: &'a [String],
    pub last_status: i32,
    pub self_pid: i32,
    pub noglob: bool,
    /// `shopt -s nullglob`: an unquoted glob pattern matching nothing expands
    /// to zero fields instead of staying literal.
    pub nullglob: bool,
    pub nounset: bool,
    /// The pid of the most recently started background job (`$!`).
    pub bg_pid: i32,
    /// The value of `$0` (the shell name, or the script path while a script
    /// is being sourced).
    pub dollar_zero: &'a str,
    /// The variable attributes a command substitution inherits (borrowed —
    /// see `AttrRefs`; expansion only reads, never mutates, these sets).
    pub attrs: &'a AttrRefs<'a>,
    /// The function table a command substitution inherits.  Like bash, a
    /// `$(...)` subshell can call functions defined in the parent shell, which
    /// is what makes recursive functions (e.g. `fact`) work through
    /// command substitution.
    pub functions: &'a HashMap<String, parser::CommandList>,
    /// Array variables (`name=(a b c)`).  Indexed by the (scalar) array name.
    pub arrays: &'a HashMap<String, Vec<String>>,
    /// Assigned indices of each indexed array (sparse arrays).
    pub array_set_indices: &'a HashMap<String, HashSet<i64>>,
    /// Associative arrays (`declare -A`).  Read-only during expansion, same
    /// borrow discipline as `arrays`.
    pub assoc_arrays: &'a HashMap<String, Vec<(String, String)>>,
    /// The assignments requested by the `${var=word}` / `${var:=word}`
    /// expansions.
    ///
    /// The expander only has a read-only view of the shell environment, so
    /// the assignments are collected here and written back to the shell by
    /// the executor once the expansion is done.
    pub pending_assign: Option<&'a RefCell<Vec<(String, String)>>>,
    /// Variables assigned by arithmetic expansions earlier on the same
    /// command line.  bash applies `$(( x++ ))` immediately, so a later
    /// `$x` in the *same* command sees the incremented value even though
    /// the executor only writes `pending_assign` into `sh.env` after the
    /// whole command has been expanded.  The overlay is shared by every
    /// `ExpandCtx` built for one command (argv, redirections, assignment
    /// RHSes) but discarded at the end of that command.
    pub arith_overlay: Option<&'a RefCell<HashMap<String, String>>>,
    /// The current value of `$-`, snapshotted from the shell's option state
    /// and startup flags when this expansion context is built.  It is kept as
    /// an owned String (not injected into `env`) so it stays in sync with
    /// `set` across separate commands.
    pub shell_flags: String,
    /// The shell's `set -o` options, borrowed: `[[ -o name ]]` has to know
    /// whether a named option is on.
    pub set_options: &'a crate::shell::SetOptions,
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
                    ctx.nounset,
                    ctx.nullglob,
                    ctx.bg_pid,
                    ctx.positional,
                    ctx.dollar_zero,
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

/// Expand a here-document body the way bash does for an unquoted delimiter:
/// parameter, command and arithmetic expansion with backslash escapes —
/// exactly like a double-quoted string.  No word splitting, no globbing, no
/// tilde/brace expansion, and quote characters are literal.
///
/// The body is first scanned into `WordPart`s (all marked quoted, so the
/// scalar expander below never splits or globs) and then evaluated with
/// `expand_scalar`, which shares every expansion code path with normal
/// words.
pub fn expand_heredoc_body(body: &str, ctx: &ExpandCtx) -> String {
    let chars: Vec<char> = body.chars().collect();
    let n = chars.len();
    let mut parts: Vec<WordPart> = Vec::new();
    let mut pending = String::new();
    let mut i = 0usize;
    // Flush the literal buffer before pushing a structured part.
    macro_rules! flush {
        () => {
            if !pending.is_empty() {
                parts.push(WordPart::Text(std::mem::take(&mut pending), true));
            }
        };
    }
    while i < n {
        let c = chars[i];
        match c {
            // Inside a here-document a backslash only escapes `` $ ` \ ``
            // (and newline, a line continuation); every other occurrence
            // keeps the backslash.
            '\\' => {
                if i + 1 < n {
                    let nxt = chars[i + 1];
                    if matches!(nxt, '$' | '`' | '\\') {
                        pending.push(nxt);
                    } else if nxt == '\n' {
                        // line continuation: both characters dropped
                    } else {
                        pending.push('\\');
                        pending.push(nxt);
                    }
                    i += 2;
                } else {
                    pending.push('\\');
                    i += 1;
                }
            }
            '$' => {
                flush!();
                i += 1;
                if i < n && chars[i] == '(' {
                    if i + 1 < n && chars[i + 1] == '(' {
                        if let Some((expr, ni)) = crate::lexer::read_paren(&chars, i + 2) {
                            if ni < n && chars[ni] == ')' {
                                parts.push(WordPart::Arith(expr, true));
                                i = ni + 1;
                                continue;
                            }
                        }
                        parts.push(WordPart::Text("$(".to_string(), true));
                        i += 1;
                        continue;
                    }
                    if let Some((sub, ni)) = crate::lexer::read_paren(&chars, i + 1) {
                        parts.push(WordPart::CmdSub(sub, true));
                        i = ni;
                        continue;
                    }
                    parts.push(WordPart::Text("$(".to_string(), true));
                    i += 1;
                    continue;
                } else if i < n && chars[i] == '{' {
                    if let Some((inner, ni)) = crate::lexer::read_brace(&chars, i + 1) {
                        parts.push(WordPart::Braced(inner, true));
                        i = ni;
                        continue;
                    }
                    parts.push(WordPart::Text("${".to_string(), true));
                    i += 1;
                    continue;
                } else if i < n && crate::lexer::is_expansion_start(chars[i]) {
                    let name = crate::lexer::read_var_name(&chars, i);
                    i += name.chars().count();
                    parts.push(WordPart::Var(name, true));
                    continue;
                } else {
                    // `$` is literal here; the next character is handled by
                    // the following iteration.
                    pending.push('$');
                    continue;
                }
            }
            '`' => {
                flush!();
                let mut sub = String::new();
                i += 1;
                while i < n && chars[i] != '`' {
                    sub.push(chars[i]);
                    i += 1;
                }
                i += 1;
                parts.push(WordPart::Backtick(sub, true));
                continue;
            }
            _ => {
                pending.push(c);
                i += 1;
            }
        }
    }
    flush!();
    expand_scalar(&parts, ctx)
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
    /// 该候选仅由「未加引号的空展开」构成、当前没有任何字面内容，因而当文本
    /// 最终为空时应被整体丢弃（bash: `$empty`、空的 `"$@"` → 0 个词）。
    /// 显式空串 `""`、带引号的 `$var`、或已拼接字面量的字段永不 droppable。
    droppable: bool,
    /// 该候选含有来自「未加引号」部分的通配符，因而在整词展开结束后要参与
    /// glob（bash 先展开整词、再对每个字段 glob）。带引号的 `*` 是字面量，
    /// 不会置位此标志。
    globbable: bool,
}

/// Append literal text to every still-open candidate.  Closed fields are
/// already complete words and stay untouched.  Quoted text (even an empty
/// `""`) makes the field explicit and therefore non-droppable.
fn append_literal(candidates: Vec<Candidate>, text: &str, quoted: bool) -> Vec<Candidate> {
    let magic = !quoted && has_glob(text);
    candidates
        .into_iter()
        .map(|c| {
            if c.open {
                Candidate {
                    text: format!("{}{}", c.text, text),
                    open: true,
                    droppable: if quoted { false } else { c.droppable },
                    globbable: c.globbable || magic,
                }
            } else {
                c
            }
        })
        .collect()
}

/// Expand a single word into 0+ resulting words (splitting + globbing for
/// unquoted expansions).
pub fn expand_word(parts: &[WordPart], ctx: &ExpandCtx) -> Vec<String> {
    let mut candidates: Vec<Candidate> = vec![Candidate {
        text: String::new(),
        open: true,
        droppable: false,
        globbable: false,
    }];
    for part in parts {
        match part {
            WordPart::Text(s, quoted) => {
                // bash globs only the FULLY expanded field, so no globbing
                // happens here; the word-end pass below handles it.
                candidates = append_literal(candidates, s, *quoted);
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
                                droppable: false,
                                globbable: c.globbable,
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
                // A subshell spawned by an earlier word on this command line
                // inherits arithmetic assignments already applied by bash.
                // Merge the overlay into the env snapshot handed to the child.
                let merged_env;
                let child_env: &HashMap<String, String> = match &ctx.arith_overlay {
                    Some(overlay) if !overlay.borrow().is_empty() => {
                        merged_env = {
                            let mut m = ctx.env.clone();
                            for (name, value) in overlay.borrow().iter() {
                                m.insert(name.clone(), value.clone());
                            }
                            m
                        };
                        &merged_env
                    }
                    _ => ctx.env,
                };
                let val = crate::executor::capture_command(
                    body,
                    child_env,
                    ctx.attrs,
                    ctx.functions,
                    ctx.arrays,
                    ctx.nounset,
                    ctx.nullglob,
                    ctx.bg_pid,
                    ctx.positional,
                    ctx.dollar_zero,
                );
                candidates = apply_value(candidates, Expansion::Scalar(val), *quoted, ctx);
            }
            WordPart::Arith(body, _) => {
                let val = eval_arith(body, ctx);
                candidates = append_literal(candidates, &val, false);
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
                        next.push(Candidate {
                            text: format!("{}{}", c.text, a),
                            open: true,
                            droppable: false,
                            globbable: c.globbable,
                        });
                    }
                }
                candidates = next;
            }
        }
    }
    // Globbing runs once, on the fully expanded fields (bash expands the
    // whole word first, then globs each field).  Only fields that received a
    // wildcard from an UNQUOTED part participate; a quoted `*` is literal.
    let mut results: Vec<String> = Vec::new();
    for c in candidates {
        // bash: 未加引号的空展开 / 空 "$@" 产生 0 个词；显式 ""、带引号的
        // $var、已拼接字面量的字段（droppable=false）必须保留为 1 个词。
        if c.droppable && c.text.is_empty() {
            continue;
        }
        if !ctx.noglob && c.globbable && has_glob(&c.text) {
            let matches = glob(&c.text, ctx.nullglob);
            if matches.is_empty() {
                // nullglob: a pattern matching nothing contributes zero words;
                // otherwise the pattern stays literal.
                if !ctx.nullglob {
                    results.push(c.text);
                }
            } else {
                results.extend(matches);
            }
        } else {
            results.push(c.text);
        }
    }
    results
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
/// 返回 (字段列表, 空结果是否可丢弃)。
///
/// bash 规则：带引号的 `"$var"` / `"$*"` 空时仍产生 1 个空词；但带引号的
/// `"$@"` / `"${arr[@]}"` 在元素数为 0 时产生 **0 个词**；未加引号的任何
/// 展开空时都产生 0 个词。第二项让 apply_value 区分这几种情况。
fn expansion_fields(exp: Expansion, quoted: bool, ctx: &ExpandCtx) -> (Vec<String>, bool) {
    if quoted {
        return match exp {
            Expansion::Scalar(s) => (vec![s], false),
            Expansion::At(fs) => (fs, true),
            Expansion::Star(fs) => (vec![fs.join(&ifs_join_sep(ctx))], false),
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
    // Fields are returned un-globbed: globbing happens once per fully
    // expanded field at the end of `expand_word` (bash's expansion order).
    (fields, true)
}

/// Append `exp` to each open candidate.  When it yields several fields, only
/// the first inherits the word's accumulated prefix and only the last stays
/// open to receive the suffix — matching bash's `pre$@suf` behaviour.
fn apply_value(candidates: Vec<Candidate>, exp: Expansion, quoted: bool, ctx: &ExpandCtx) -> Vec<Candidate> {
    let (fields, empty_droppable) = expansion_fields(exp, quoted, ctx);
    let mut next = Vec::new();
    for c in candidates {
        if !c.open {
            next.push(c);
            continue;
        }
        if fields.is_empty() {
            // 展开没有产生任何字段：仅当来源是「可丢弃的空展开」且该候选
            // 尚无字面内容时，标记为 droppable（末尾统一删除）。带引号的
            // ""、已有前缀的 `a$empty` 等必须原样保留。
            if empty_droppable && c.text.is_empty() {
                next.push(Candidate {
                    text: String::new(),
                    open: true,
                    droppable: true,
                    globbable: c.globbable,
                });
            } else {
                next.push(c);
            }
            continue;
        }
        let n = fields.len();
        for (k, f) in fields.iter().enumerate() {
            // 未加引号的展开结果同样参与末尾的 glob（bash: `$x` 值含通配符
            // 时按 bash 语义展开）。
            let g = !quoted && has_glob(f);
            if k == 0 {
                next.push(Candidate {
                    text: format!("{}{}", c.text, f),
                    open: n == 1,
                    droppable: false,
                    globbable: c.globbable || g,
                });
            } else {
                next.push(Candidate {
                    text: f.clone(),
                    open: k + 1 == n,
                    droppable: false,
                    globbable: g,
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

/// The expansion logic shared by `$!` and `${!}` (the empty braced form).
///
/// Both denote the pid of the most recent background job.  When no job has
/// been started `bg_pid` is 0 and bash treats the parameter as unbound under
/// `set -u` (rc 127); otherwise it expands to that pid.  The caller supplies
/// the diagnostic `label` — `$!` for the bare form, `!` for the braced form —
/// so each matches bash's own message (`$!: unbound variable` vs
/// `!: unbound variable`).
///
/// Keeping this in one place is what stops the two spellings from drifting
/// apart (the original 0211a63 bug was exactly such a divergence).
fn eval_bang(ctx: &ExpandCtx, label: &str) -> Expansion {
    if ctx.bg_pid == 0 {
        if ctx.nounset {
            unbound_variable(label);
        }
        Expansion::Scalar(String::new())
    } else {
        Expansion::Scalar(ctx.bg_pid.to_string())
    }
}

/// True when `s` is a valid shell variable name (the rule bash uses for
/// indirect-expansion targets).
fn is_valid_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `${!prefix@}` / `${!prefix*}`: list, in sorted order, the names of every
/// variable (scalar or array) whose name starts with `prefix`.  `@` joins with a
/// single space; `*` joins with the first character of `$IFS` (bash semantics).
fn list_prefix_vars(ctx: &ExpandCtx, prefix: &str, sep: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    names.extend(ctx.env.keys().filter(|k| k.starts_with(prefix)).cloned());
    names.extend(ctx.arrays.keys().filter(|k| k.starts_with(prefix)).cloned());
    names.sort();
    names.dedup();
    names.join(sep)
}

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
            // `$!` is the pid of the most recent background job.  Unlike the
            // other special parameters, it is NOT always considered set: when
            // no background job has been started `bg_pid` is 0, and bash treats
            // `$!` as unbound under `set -u` (rc 127) — exactly like an
            // ordinary unset variable.  When a job exists it expands to that
            // pid (which is never "unset"), so no diagnostic is emitted.
            // `${!}` (empty braced form) shares this exact logic via
            // `eval_bang`.
            eval_bang(ctx, "$!")
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
            match expand_env_get(ctx, name) {
                Some(value) => Expansion::Scalar(value),
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
            if let Some(entries) = ctx.assoc_arrays.get(arr) {
                return match subscript {
                    "@" | "*" => Expansion::Scalar(entries.len().to_string()),
                    key => {
                        let len = entries
                            .iter()
                            .find(|(k, _)| k.as_str() == key)
                            .map(|(_, v)| crate::byteval::decoded_len(v))
                            .unwrap_or(0);
                        Expansion::Scalar(len.to_string())
                    }
                };
            }
            let elems = ctx.arrays.get(arr);
            return match subscript {
            "@" | "*" => {
                let n = match ctx.array_set_indices.get(arr) {
                    Some(indices) => indices.len(),
                    None => match elems {
                        Some(e) => e.len(),
                        // A scalar set via `a=1` is a one-element array for
                        // bash (`${#a[@]}` is 1).
                        None if expand_env_contains(ctx, arr) => 1,
                        None => 0,
                    },
                };
                Expansion::Scalar(n.to_string())
            }
                _ => {
                    let idx = subscript.parse::<i64>().ok().or_else(|| {
                        crate::arith::eval(
                            subscript,
                            &mut crate::arith::ReadOnlyEnv { vars: ctx.env },
                        )
                        .ok()
                    });
                    let len = match (elems, idx) {
                        (Some(e), Some(i)) if i >= 0 && (i as usize) < e.len() => {
                            crate::byteval::decoded_len(&e[i as usize])
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
            ctx.env
                .get(name)
                .map(|v| crate::byteval::decoded_len(v).to_string())
                .unwrap_or_else(|| "0".to_string()),
        );
    }
    if body.starts_with('!') && !body.contains(':') && !body.contains('/') && !body.contains('%')
        && (!body.contains('#') || body == "!#")
    {
        // indirect expansion ${!VAR}
        let name = &body[1..];
        // `${!}` (empty name) is the braced synonym of `$!`; `${!*}` and
        // `${!@}` are neither valid indirect expansions nor a synonym of `$!`.
        // Each is handled separately below.
        if name.is_empty() {
            // `${!}` is the braced form of `$!` (the pid of the most recent
            // background job).  It follows exactly the same rules as `$!`:
            // when no background job has been started `bg_pid` is 0 and bash
            // treats `${!}` as unbound under `set -u` (rc 127, label
            // `!: unbound variable`); when a job exists it expands to that pid.
            // The diagnostic label is `!` (the body minus the leading `$`),
            // matching bash's `!: unbound variable`.  `eval_bang` is the single
            // source of truth shared with `eval_var`'s `"!"` arm.
            return eval_bang(ctx, "!");
        } else if name == "*" || name == "@" {
            // `${!*}` / `${!@}`: NOT a synonym of `$!`.  Under `set -u` bash
            // reports them as unbound variables (`!*: unbound variable` /
            // `!@: unbound variable`); without `set -u` they expand to the
            // empty string.  These are NOT background-job aware.
            if ctx.nounset {
                unbound_variable(body);
            }
            return Expansion::Scalar(String::new());
        } else if name == "#" {
            // `${!#}` is bash's spelling of `$0` (the shell / script name).
            return Expansion::Scalar(ctx.dollar_zero.to_string());
        }
        // `${!name[@]}` / `${!name[*]}`: the index list of array `name` --
        // NOT a prefix-name list.  bash's rules (all rc 0, and NOT subject to
        // `set -u`: an unset name expands to the empty string even with
        // `set -u` on):
        //   * name is an array with N elements -> "0 1 ... N-1" (ascending),
        //     joined by a space (`[@]`) or the first character of `$IFS` (`[*]`)
        //   * name is a scalar  -> "0" (a scalar is a one-element array)
        //   * name is unset     -> empty
        if let Some(arr_name) = name.strip_suffix("[@]").or_else(|| name.strip_suffix("[*]")) {
            if is_valid_name(arr_name) {
                let sep = if name.ends_with("[@]") {
                    " ".to_string()
                } else {
                    ifs_join_sep(ctx)
                };
                let keys: Vec<String> = if let Some(entries) = ctx.assoc_arrays.get(arr_name) {
                    assoc_iter_keys(entries)
                } else {
                    match ctx.array_set_indices.get(arr_name) {
                        Some(indices) => {
                            let mut sorted: Vec<i64> = indices.iter().copied().collect();
                            sorted.sort_unstable();
                            sorted.into_iter().map(|i| i.to_string()).collect()
                        }
                        None => match ctx.arrays.get(arr_name) {
                            Some(elems) => (0..elems.len()).map(|i| i.to_string()).collect(),
                            None if ctx.env.contains_key(arr_name) => vec!["0".to_string()],
                            None => Vec::new(),
                        },
                    }
                };
                return Expansion::Scalar(keys.join(&sep));
            }
        }
        // `${!prefix@}` / `${!prefix*}`: list the names of every variable
        // (scalar or array) whose name starts with `prefix`, sorted, joined by a
        // space (`@`) or the first character of `$IFS` (`*`).  This is the
        // variable-name form of indirect expansion, NOT a synonym of `$!`.
        if let Some(prefix) = name.strip_suffix('@') {
            return Expansion::Scalar(list_prefix_vars(ctx, prefix, " "));
        } else if let Some(prefix) = name.strip_suffix('*') {
            return Expansion::Scalar(list_prefix_vars(ctx, prefix, &ifs_join_sep(ctx)));
        }
        // `${!VAR}`: indirect expansion.  The value of `VAR` names the variable
        // to read.  bash's rules, all reported with **rc 1** (a non-fatal
        // command failure, empty stdout — the command is aborted):
        //   * VAR unset            -> "VAR: invalid indirect expansion"
        //   * VAR empty           -> ": invalid variable name"
        //   * VAR = valid name    -> the named variable's value (empty if unset)
        //   * VAR = invalid name  -> "VAR: invalid variable name"
        return match expand_env_get(ctx, name) {
            None => {
                flag_invalid_indirect(&format!("{}: invalid indirect expansion", name));
                Expansion::Scalar(String::new())
            }
            Some(target) => {
                if target.is_empty() {
                    flag_invalid_indirect(": invalid variable name");
                    Expansion::Scalar(String::new())
                } else if is_valid_name(&target) {
                    Expansion::Scalar(expand_env_get(ctx, &target).unwrap_or_default())
                } else {
                    flag_invalid_indirect(&format!("{}: invalid variable name", target));
                    Expansion::Scalar(String::new())
                }
            }
        };
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
        if let Some(entries) = ctx.assoc_arrays.get(name) {
            let fields: Vec<String> = match subscript {
                "@" | "*" => assoc_iter_values(entries),
                key => entries
                    .iter()
                    .find(|(k, _)| k.as_str() == key)
                    .map(|(_, v)| vec![v.clone()])
                    .unwrap_or_default(),
            };
            return match subscript {
                "@" | "*" => {
                    if let Some(exp) = array_word_op(op, &fields, subscript, arg, ctx) {
                        return exp;
                    }
                    let fields = if op == ":" {
                        slice_fields(fields, arg, ctx)
                    } else {
                        fields
                    };
                    if subscript == "@" {
                        Expansion::At(fields)
                    } else {
                        Expansion::Star(fields)
                    }
                }
                _ => {
                    let val = fields.into_iter().next().unwrap_or_default();
                    Expansion::Scalar(apply_scalar_op(
                        &val, name, op, arg, !val.is_empty(), val.is_empty(), ctx,
                    ))
                }
            };
        }
        // A scalar set via `a=1` is a one-element array for `${a[@]}` /
        // `${a[*]}` (bash returns the scalar's value, not the empty list).
        let mut elems = ctx.arrays.get(name).cloned().unwrap_or_default();
        if elems.is_empty() && !ctx.array_set_indices.contains_key(name) {
            if let Some(v) = expand_env_get(ctx, name) {
                elems = vec![v];
            }
        }
        match subscript {
            "@" | "*" => {
                let elems = if let Some(indices) = ctx.array_set_indices.get(name) {
                    let mut sorted: Vec<i64> = indices.iter().copied().collect();
                    sorted.sort_unstable();
                    sorted
                        .into_iter()
                        .filter_map(|i| {
                            if i >= 0 && (i as usize) < elems.len() {
                                Some(elems[i as usize].clone())
                            } else {
                                None
                            }
                        })
                        .collect()
                } else {
                    elems
                };
                if let Some(exp) = array_word_op(op, &elems, subscript, arg, ctx) {
                    return exp;
                }
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
                // An indexed subscript is an arithmetic expression, so a
                // symbolic name (`a[key]`) evaluates the same way bash does
                // (an unbound variable evaluates to 0).
                let idx = subscript.parse::<i64>().ok().or_else(|| {
                    crate::arith::eval(
                        subscript,
                        &mut crate::arith::ReadOnlyEnv { vars: ctx.env },
                    )
                    .ok()
                });
                let val = match idx {
                    Some(i) if i >= 0 && (i as usize) < elems.len() => {
                        elems[i as usize].clone()
                    }
                    _ => String::new(),
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
    let is_set = expand_env_contains(ctx, rawname) || arr_elems.is_some();
    let value = match arr_elems {
        Some(e) if !e.is_empty() => e[0].clone(),
        _ => expand_env_get(ctx, rawname).unwrap_or_default(),
    };
    let unset_or_empty = !is_set || value.is_empty();

    let result = apply_scalar_op(&value, rawname, op, arg, is_set, unset_or_empty, ctx);
    Expansion::Scalar(result)
}

/// Keys of an associative array in the order bash's `${!m[@]}` iterates.
///
/// bash 5.3 stores associative arrays in a chained hash table created with
/// `hash_create (HASH_BUCKETS)`, where `HASH_BUCKETS` is 1024.  `hash_string`
/// is FNV-1 (`i *= 16777619; i ^= *s`, starting from 2166136261), the bucket
/// index is `hash & 1023`, new entries are *head*-inserted into their bucket,
/// and `${!m[@]}` walks buckets 0..1023 emitting each chain from head to
/// tail.  When `nentries >= 2 * nbuckets` bash grows the table by 4x and
/// rehashes every entry (walking the old chains head-to-tail and head-
/// inserting into the new table); that is reproduced here too, so even
/// arrays past the 2048-entry boundary stay byte-identical to the oracle.
fn assoc_hash(key: &str) -> usize {
    const FNV_OFFSET: u32 = 2166136261;
    const FNV_PRIME: u32 = 16777619;
    let mut hash = FNV_OFFSET;
    for byte in key.bytes() {
        hash = hash.wrapping_mul(FNV_PRIME) ^ u32::from(byte);
    }
    hash as usize
}

pub(crate) fn assoc_iter_keys(entries: &[(String, String)]) -> Vec<String> {
    const INITIAL_BUCKETS: usize = 1024;
    let mut nbuckets = INITIAL_BUCKETS;
    let mut buckets: Vec<Vec<&str>> = vec![Vec::new(); nbuckets];
    let mut nentries = 0usize;
    for (key, _) in entries {
        // `HASH_SHOULDGROW` is tested before the new entry is inserted.
        if nentries >= nbuckets * 2 {
            let old = std::mem::replace(&mut buckets, vec![Vec::new(); nbuckets * 4]);
            nbuckets *= 4;
            for chain in old {
                for k in chain {
                    buckets[assoc_hash(k) & (nbuckets - 1)].insert(0, k);
                }
            }
        }
        buckets[assoc_hash(key) & (nbuckets - 1)].insert(0, key.as_str());
        nentries += 1;
    }
    buckets.into_iter().flatten().map(str::to_string).collect()
}

/// Values of an associative array in the same order `${!m[@]}` lists keys.
fn assoc_iter_values(entries: &[(String, String)]) -> Vec<String> {
    let keys = assoc_iter_keys(entries);
    keys.iter()
        .filter_map(|k| entries.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.clone()))
        .collect()
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

/// Whole-array default/alternate operators for `${arr[@]...}` / `${arr[*]...}`.
///
/// bash 5.2+ tests these operators against the array as a whole, not per
/// element.  The bare forms (`-`, `+`) only look at whether the array holds
/// any elements; the colon forms (`:-`, `:+`) look at the array's joined
/// value.  `@` joins with a literal space, `*` with the first character of
/// `$IFS`.  When the operator fires, its word replaces the *whole* array
/// expansion as a single field (it is word-split later if unquoted); when it
/// does not fire, the caller falls back to the ordinary `@` / `*` expansion.
fn array_word_op(
    op: &str,
    elems: &[String],
    subscript: &str,
    arg: &str,
    ctx: &ExpandCtx,
) -> Option<Expansion> {
    let joined = if subscript == "@" {
        elems.join(" ")
    } else {
        elems.join(&ifs_join_sep(ctx))
    };
    let fires = match op {
        "-" => elems.is_empty(),
        ":-" => joined.is_empty(),
        "+" => !elems.is_empty(),
        ":+" => !joined.is_empty(),
        _ => false,
    };
    if fires {
        Some(Expansion::Scalar(arg.to_string()))
    } else {
        None
    }
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

/// Read a scalar variable during expansion, honouring assignments made by
/// earlier `$(( ... ))` expansions on this command line.
fn expand_env_get(ctx: &ExpandCtx, name: &str) -> Option<String> {
    if let Some(overlay) = &ctx.arith_overlay {
        if let Some(value) = overlay.borrow().get(name) {
            return Some(value.clone());
        }
    }
    ctx.env.get(name).cloned()
}

/// Whether a scalar variable is set during expansion, including values
/// introduced by an earlier arithmetic assignment on this command line.
fn expand_env_contains(ctx: &ExpandCtx, name: &str) -> bool {
    if let Some(overlay) = &ctx.arith_overlay {
        if overlay.borrow().contains_key(name) {
            return true;
        }
    }
    ctx.env.contains_key(name)
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
    // LC_ALL=C 下 bash 按裸字节切片：先解码 byteval 编码得到原始字节，
    // 每个字节算一个单位；结果再编码回 byteval，绝不让裸字节直接进 String。
    let raw = crate::byteval::decode_raw(value);
    let n = raw.len() as i64;
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
        raw.len() - start
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
    let selected: Vec<u8> = raw[start..].iter().take(taken).copied().collect();
    crate::byteval::encode_raw(&selected)
}

fn strip_prefix(value: &str, pat: &str, longest: bool) -> String {
    if pat.is_empty() {
        return value.to_string();
    }
    // LC_ALL=C 下 bash 按裸字节匹配：`?` / 字符类可以命中多字节字符的中间
    // 字节。值与模式都先解码成裸字节再映射为 char 跑 glob_match，结果编码
    // 回 byteval，保证字节级语义与 bash 一致。
    let chars = bytes_as_chars(value);
    let patc = bytes_as_chars(pat);
    if longest {
        // remove the LONGEST matching prefix: largest k where chars[..k] matches
        for k in (0..=chars.len()).rev() {
            if glob_match(&chars[..k], &patc) {
                return crate::byteval::encode_raw(&chars_as_bytes(&chars[k..]));
            }
        }
    } else {
        // remove the SHORTEST matching prefix: smallest k (first match scanning up)
        for k in 0..=chars.len() {
            if glob_match(&chars[..k], &patc) {
                return crate::byteval::encode_raw(&chars_as_bytes(&chars[k..]));
            }
        }
    }
    crate::byteval::encode_raw(&chars_as_bytes(&chars))
}

fn strip_suffix(value: &str, pat: &str, longest: bool) -> String {
    if pat.is_empty() {
        return value.to_string();
    }
    // 同 strip_prefix：字节级语义（见上）。
    let chars = bytes_as_chars(value);
    let patc = bytes_as_chars(pat);
    if longest {
        // remove the LONGEST matching suffix: smallest k where chars[k..] matches
        for k in 0..=chars.len() {
            if glob_match(&chars[k..], &patc) {
                return crate::byteval::encode_raw(&chars_as_bytes(&chars[..k]));
            }
        }
    } else {
        // remove the SHORTEST matching suffix: largest k (first match scanning down)
        for k in (0..=chars.len()).rev() {
            if glob_match(&chars[k..], &patc) {
                return crate::byteval::encode_raw(&chars_as_bytes(&chars[..k]));
            }
        }
    }
    crate::byteval::encode_raw(&chars_as_bytes(&chars))
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
    crate::byteval::decode_raw(s).iter().map(|&b| b as char).collect()
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
    // 结果字节流编码回 byteval；裸字节绝不直接进 String（否则又是 UB）。
    crate::byteval::encode_raw(&chars_as_bytes(&out))
}

// ---------------------------------------------------------------------------
// Globbing
// ---------------------------------------------------------------------------

fn has_glob(p: &str) -> bool {
    p.contains('*') || p.contains('?') || (p.contains('[') && p.contains(']'))
}

/// Join one pattern component onto an accumulated msys-spelling prefix.
fn join_component(pfx: &str, comp: &str, absolute: bool, first: bool) -> String {
    if first && absolute {
        format!("/{}", comp)
    } else if pfx.is_empty() {
        comp.to_string()
    } else {
        format!("{}/{}", pfx, comp)
    }
}

/// Native path for `read_dir` at a walk prefix: msys drive paths (`/d`,
/// `/d/tmp`) must be translated to Windows form; a relative empty prefix
/// means the current directory.
fn native_dir(pfx: &str) -> String {
    if pfx.is_empty() {
        return ".".to_string();
    }
    if let Some(win) = translate_msys_path(pfx) {
        return win;
    }
    // A bare drive root reached while walking `/d/*/...`.
    let b = pfx.as_bytes();
    if b.len() == 2 && b[0] == b'/' && (b[1] as char).is_ascii_alphabetic() {
        return format!("{}:/", (b[1] as char).to_ascii_uppercase());
    }
    pfx.to_string()
}

/// Expand a filename pattern to matching paths.  With `nullglob` off (bash's
/// default) a pattern matching nothing stays literal; with it on the pattern
/// expands to zero fields.
///
/// bash walks a pattern one path component at a time: every component is
/// matched against the corresponding directory level, so `src/*/main.rs`
/// globs the middle component too.  A literal final component must still
/// exist for the whole path to match (`src/*/nope` stays literal even when
/// `src/*/` matches directories).  A trailing `/` restricts matches to
/// directories and keeps the slash in the output (`src/*/`).
pub fn glob(pattern: &str, nullglob: bool) -> Vec<String> {
    if !has_glob(pattern) {
        return vec![pattern.to_string()];
    }
    let absolute = pattern.starts_with('/');
    let dir_only = pattern.ends_with('/') && pattern.contains('/');
    let comps: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
    if comps.is_empty() {
        return vec![pattern.to_string()];
    }
    // Each state is the msys-spelling prefix accumulated so far; the native
    // form is derived on demand for `read_dir`.
    let mut states: Vec<String> = vec![String::new()];
    let last = comps.len() - 1;
    for (ci, comp) in comps.iter().enumerate() {
        let is_last = ci == last;
        let magic = has_glob(comp);
        let hidden_ok = comp.starts_with('.');
        let patc: Vec<char> = comp.chars().collect();
        let mut next: Vec<String> = Vec::new();
        for pfx in &states {
            if !magic {
                next.push(join_component(pfx, comp, absolute, ci == 0));
                continue;
            }
            // The msys root has no native equivalent, so a magic first
            // component of an absolute pattern cannot enumerate anything
            // (same as before: the pattern stays literal).
            if absolute && pfx.is_empty() {
                continue;
            }
            let dir_native = native_dir(pfx);
            if let Ok(entries) = std::fs::read_dir(&dir_native) {
                for e in entries.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    // Skip dotfiles unless the component explicitly asks.
                    if name.starts_with('.') && !hidden_ok {
                        continue;
                    }
                    if dir_only && is_last {
                        // A trailing `/` matches directories only (through
                        // symlinks, like bash's stat-based check).
                        match e.metadata() {
                            Ok(m) if m.is_dir() => {}
                            _ => continue,
                        }
                    }
                    let nc: Vec<char> = name.chars().collect();
                    if glob_match(&nc, &patc) {
                        let out_name = if dir_only && is_last {
                            format!("{}/", name)
                        } else {
                            name
                        };
                        next.push(join_component(pfx, &out_name, absolute, ci == 0));
                    }
                }
            }
        }
        states = next;
        if states.is_empty() {
            break;
        }
    }
    // A literal final component must exist (bash's glob verifies the whole
    // path: `src/*/nope` matches nothing even though `src/*/` matches).
    if !has_glob(comps[last]) {
        states.retain(|p| std::fs::symlink_metadata(&native_dir(p)).is_ok());
    }
    if states.is_empty() {
        if nullglob {
            return Vec::new();
        }
        // bash default (nullglob off): a pattern with glob metacharacters
        // that matches nothing yields the literal pattern, not an empty
        // result.
        return vec![pattern.to_string()];
    }
    states.sort();
    states
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
    fn get(&self, name: &str, may_unbound: bool) -> Option<i64> {
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
        let val = expand_env_get(self.ctx, name).and_then(|v| v.parse::<i64>().ok());
        if may_unbound && val.is_none() && self.ctx.nounset && !is_special_param(name) {
            unbound_variable(name);
            return None;
        }
        val
    }
    /// Raw string value, so `arith::eval` can recursively expand a
    /// variable's value the way bash does (`x="1+2"; $((x))` is 3).
    fn get_str(&self, name: &str) -> Option<String> {
        if let Some(v) = self.assigned.borrow().get(name) {
            return Some(v.to_string());
        }
        if let Ok(n) = name.parse::<usize>() {
            if n == 0 {
                return Some(self.ctx.dollar_zero.to_string());
            }
            return self.ctx.positional.get(n - 1).cloned();
        }
        match name {
            "@" | "*" => return None,
            "#" => return Some(self.ctx.positional.len().to_string()),
            "?" => return Some(self.ctx.last_status.to_string()),
            "0" => return Some(self.ctx.dollar_zero.to_string()),
            _ => {}
        }
        expand_env_get(self.ctx, name)
    }
    fn set(&mut self, name: &str, value: i64) {
        self.assigned.borrow_mut().insert(name.to_string(), value);
        // bash applies arithmetic assignments immediately: a later `$x` on
        // the same command line must read the new value.  Publish it through
        // the command-wide overlay as well as through `pending_assign`, which
        // the executor drains into `sh.env` for subsequent commands.
        if let Some(overlay) = &self.ctx.arith_overlay {
            overlay
                .borrow_mut()
                .insert(name.to_string(), value.to_string());
        }
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
            let msg = if e.message == "operand expected"
                || e.message == "invalid arithmetic operator"
            {
                format!("arithmetic syntax error: {}", e.message)
            } else {
                e.message.clone()
            };
            // A value-recursion error is reported against the variable's raw
            // value, not the top-level expression (`x=3.5; $((x))` reports
            // `3.5: arithmetic syntax error: ...`).
            let prefix_src = e
                .expr_prefix
                .clone()
                .unwrap_or_else(|| body.to_string());
            flag_arith_failure(&format!(
                "{}: {} (error token is \"{}\")",
                prefix_src.trim_start(),
                msg,
                bash_error_token(&prefix_src, &e.token)
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
        _ => expand_env_get(ctx, name).unwrap_or_default(),
    }
}
