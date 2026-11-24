// Bash-compatible arithmetic evaluation.
//
// Used by the `let` builtin, by `declare -i` (integer attributes) and by
// `(( expression ))`. The grammar and the error messages follow bash 5.3
// (`builtins/let.def` -> `expand_arith_string`), so that expressions such as
// `x=1+2`, `x++`, `a ? b : c`, `16#ff` or `1<<3` behave identically.
//
// Numbers are evaluated as 64-bit signed integers (bash uses `intmax_t`).

use std::collections::HashMap;

/// Errors produced while evaluating an arithmetic expression.
#[derive(Debug, Clone)]
pub struct ArithError {
    /// The base message bash prints (without the error token).
    pub message: String,
    /// The token bash appends as `(error token is "...")`; empty when none.
    pub token: String,
    /// bash prefixes a value-recursion error with the variable's raw value
    /// instead of the top-level expression (`x=3.5; $((x))` reports
    /// `3.5: ...`).  `None` uses the caller's expression text.
    pub expr_prefix: Option<String>,
}

impl ArithError {
    fn new(msg: &str) -> Self {
        ArithError {
            message: msg.to_string(),
            token: String::new(),
            expr_prefix: None,
        }
    }
}

/// Render a token the way bash's error reporting does (`"0"`, `"x"`, ...`).
fn token_text(t: &Option<Tok>) -> String {
    match t {
        Some(Tok::Num(v)) => v.to_string(),
        Some(Tok::Ident(s)) => s.clone(),
        Some(Tok::Op(o)) => o.to_string(),
        Some(Tok::LParen) => "(".to_string(),
        Some(Tok::RParen) => ")".to_string(),
        Some(Tok::Comma) => ",".to_string(),
        None => String::new(),
    }
}

/// Variable access used while evaluating an expression.
pub trait ArithEnv {
    /// Value of `name`, or `None` when the variable is unset.
    ///
    /// `may_unbound` is true when this read may legitimately raise an
    /// "unbound variable" error under `set -u` (a real read, or a
    /// compound-assignment/increment LHS that must be read first). It is
    /// false only for the LHS of a plain `=` assignment, which bash does
    /// not treat as a read.
    fn get(&self, name: &str, may_unbound: bool) -> Option<i64>;
    /// Assign `value` to `name`.
    fn set(&mut self, name: &str, value: i64);
    /// Raw string value of a variable, so the evaluator can recursively
    /// expand it the way bash does (`x="1+2"; $((x))` is 3).  Envs that
    /// cannot provide raw values return None and the plain numeric lookup
    /// is used.
    fn get_str(&self, _name: &str) -> Option<String> {
        None
    }
}

/// An environment backed by a plain map (used for `let` on the shell's
/// variable table and for tests).
pub struct MapEnv<'a> {
    pub vars: &'a mut HashMap<String, String>,
}

impl<'a> ArithEnv for MapEnv<'a> {
    fn get(&self, name: &str, _may_unbound: bool) -> Option<i64> {
        self.vars.get(name).and_then(|v| parse_number(v).ok())
    }
    fn set(&mut self, name: &str, value: i64) {
        self.vars.insert(name.to_string(), value.to_string());
    }
    fn get_str(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }
}

/// A read-only environment: assignments are silently discarded.
pub struct ReadOnlyEnv<'a> {
    pub vars: &'a HashMap<String, String>,
}

impl<'a> ArithEnv for ReadOnlyEnv<'a> {
    fn get(&self, name: &str, _may_unbound: bool) -> Option<i64> {
        self.vars.get(name).and_then(|v| parse_number(v).ok())
    }
    fn set(&mut self, _name: &str, _value: i64) {}
    fn get_str(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }
}

/// Parse a bare integer the way bash's arithmetic evaluator does: decimal,
/// `0x`/`0X` hexadecimal, leading `0` octal, and `base#digits` form.
pub fn parse_number(s: &str) -> Result<i64, ArithError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(ArithError::new("invalid number"));
    }
    let (neg, t) = match t.as_bytes()[0] {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    if t.is_empty() {
        return Err(ArithError::new("invalid number"));
    }
    let value = parse_unsigned(t)?;
    let value = if neg { -value } else { value };
    Ok(value)
}

fn parse_unsigned(t: &str) -> Result<i64, ArithError> {
    // base#digits
    if let Some(pos) = t.find('#') {
        let (base_s, digits) = (&t[..pos], &t[pos + 1..]);
        let base: u32 = base_s
            .parse()
            .map_err(|_| ArithError::new("invalid arithmetic base"))?;
        if base < 2 || base > 64 {
            return Err(ArithError::new("invalid arithmetic base"));
        }
        if digits.is_empty() {
            return Err(ArithError::new("value too great for base"));
        }
        let mut v: i64 = 0;
        for c in digits.chars() {
            let d = match c {
                '0'..='9' => c as u32 - '0' as u32,
                'a'..='z' => c as u32 - 'a' as u32 + 10,
                'A'..='Z' => c as u32 - 'A' as u32 + 10,
                '@' => 62,
                '_' => 63,
                _ => return Err(ArithError::new("invalid arithmetic base")),
            };
            if d >= base {
                return Err(ArithError::new("value too great for base"));
            }
            v = v
                .wrapping_mul(base as i64)
                .wrapping_add(d as i64);
        }
        return Ok(v);
    }
    let (radix, digits) = if let Some(rest) = t.strip_prefix("0x").or(t.strip_prefix("0X")) {
        (16u32, rest)
    } else if t.len() > 1 && t.starts_with('0') {
        (8u32, &t[1..])
    } else {
        (10u32, t)
    };
    if digits.is_empty() {
        return Ok(0);
    }
    let mut v: i64 = 0;
    for c in digits.chars() {
        let d = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 10,
            'A'..='Z' => c as u32 - 'A' as u32 + 10,
            _ => return Err(ArithError::new("value too great for base")),
        };
        if d >= radix {
            return Err(ArithError::new("value too great for base"));
        }
        v = v.wrapping_mul(radix as i64).wrapping_add(d as i64);
    }
    Ok(v)
}

use std::cell::{Cell, RefCell};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
}

/// A cursor over a shared, CACHED token stream.
///
/// Hot shell loops re-evaluate the same tiny expressions (`i+1`, `i<2000`)
/// thousands of times, so the tokenization is memoized by the expression
/// string (see `tokenize_cached`): an evaluation then only walks the cached
/// token vector instead of re-scanning characters and re-allocating
/// `Tok::Ident` strings.  The old per-expression `last_text` is derivable from
/// the final token, so it is gone.
struct Lexer {
    toks: Arc<Vec<Tok>>,
    pos: usize,
}

const OPS: &[&str] = &[
    "<<=", ">>=", "...", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=", "-=", "*=", "/=",
    "%=", "&=", "^=", "|=", "++", "--", "+", "-", "**", "*", "/", "%", "<", ">", "!", "~", "&", "^", "|",
    "=", "?", ":", "(", ")", ",",
];

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl Lexer {
    fn new(toks: Arc<Vec<Tok>>) -> Lexer {
        Lexer { toks, pos: 0 }
    }
}

/// Memoized tokenization: hot loops re-tokenize the same tiny expression
/// strings thousands of times.  The cache is capped; when full it is simply
/// cleared (the working set of a loop is a handful of expressions, so a full
/// clear cannot evict it mid-loop).
const ARITH_TOK_CACHE_MAX: usize = 512;

thread_local! {
    static TOK_CACHE: RefCell<HashMap<String, Arc<Vec<Tok>>>> =
        RefCell::new(HashMap::new());
    // Guard against stack exhaustion from deeply nested arithmetic.  MSYS2
    // bash 5.3 does NOT SIGSEGV here: it accepts ~1022 levels and then
    // deterministically fails with "expression recursion level exceeded"
    // at 1023.  wash keeps a much smaller cap so adversarial input returns
    // a deterministic error instead of overflowing its native stack.
    static ARITH_NEST_DEPTH: Cell<usize> = const { Cell::new(0) };
    // Depth of recursive variable-value evaluation (`x="1+2"; $((x))`).
    // Kept separate from ARITH_NEST_DEPTH and much smaller: every value
    // level costs ~20 native parse frames, and wash's 1 MB main-thread
    // stack allows roughly 50 levels.  bash tolerates far more (~700 on
    // MSYS2 bash 5.3) — this threshold is a documented deviation, not a
    // bash-compatible number; do not claim parity at this boundary.
    static VAR_EVAL_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Nesting cap for the recursive arithmetic productions (parenthesised
/// sub-expressions, `**` right-hand sides and conditional branches).  Well
/// below any native-stack limit, so the parser returns an error instead of
/// overflowing the stack on adversarial input.
const MAX_ARITH_NEST: usize = 200;

/// RAII token for one level of arithmetic recursion.  Dropped on every exit
/// path (including `?` early returns), so normal nesting is never miscounted.
struct NestGuard;

impl NestGuard {
    fn enter() -> Result<NestGuard, ArithError> {
        ARITH_NEST_DEPTH.with(|depth| {
            let n = depth.get();
            if n >= MAX_ARITH_NEST {
                Err(ArithError::new("expression recursion level exceeded"))
            } else {
                depth.set(n + 1);
                Ok(NestGuard)
            }
        })
    }
}

impl Drop for NestGuard {
    fn drop(&mut self) {
        ARITH_NEST_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Nesting cap for recursive variable-value evaluation.
const MAX_VAR_EVAL_DEPTH: usize = 50;

/// RAII token for one level of variable-value recursion.
struct VarEvalGuard;

impl VarEvalGuard {
    fn enter() -> Option<VarEvalGuard> {
        VAR_EVAL_DEPTH.with(|depth| {
            if depth.get() >= MAX_VAR_EVAL_DEPTH {
                None
            } else {
                depth.set(depth.get() + 1);
                Some(VarEvalGuard)
            }
        })
    }
}

impl Drop for VarEvalGuard {
    fn drop(&mut self) {
        VAR_EVAL_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn tokenize_cached(src: &str) -> Result<Arc<Vec<Tok>>, ArithError> {
    if let Some(hit) = TOK_CACHE.with(|c| c.borrow().get(src).cloned()) {
        return Ok(hit);
    }
    let toks = Arc::new(tokenize(src)?);
    TOK_CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if map.len() >= ARITH_TOK_CACHE_MAX {
            map.clear();
        }
        map.insert(src.to_string(), Arc::clone(&toks));
    });
    Ok(toks)
}

/// Scan an arithmetic expression into its token vector (uncached cold path).
fn tokenize(src: &str) -> Result<Vec<Tok>, ArithError> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            // Collect a numeric literal, possibly with base#digits.
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '#') {
                if chars[i] == '#' {
                    // base#digits: only digits and letters allowed after
                    i += 1;
                    while i < chars.len()
                        && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '@')
                    {
                        i += 1;
                    }
                    break;
                }
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            let v = parse_unsigned(&text).map_err(|e| ArithError {
                message: e.message,
                // bash names the whole literal (`10#ff` -> error token
                // "10#ff").
                token: text.clone(),
                expr_prefix: None,
            })?;
            toks.push(Tok::Num(v));
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            // array subscript a[1] is not supported; treat as identifier
            let text: String = chars[start..i].iter().collect();
            toks.push(Tok::Ident(text));
            continue;
        }
        if c == '(' {
            toks.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            toks.push(Tok::RParen);
            i += 1;
            continue;
        }
        if c == ',' {
            // Emit an operator token so `parse_comma`'s eat_op(",")
            // recognises it. `Tok::Comma` was never matchable by eat_op,
            // which made every comma expression a syntax error.
            toks.push(Tok::Op(","));
            i += 1;
            continue;
        }
        if c == '?' {
            // Emit an operator token so parse_conditional's eat_op("?")
            // recognises it (same fix as Tok::Comma above).
            toks.push(Tok::Op("?"));
            i += 1;
            continue;
        }
        if c == ':' {
            toks.push(Tok::Op(":"));
            i += 1;
            continue;
        }
        // operators
        let mut matched = false;
        for op in OPS {
            let n = op.chars().count();
            if i + n <= chars.len() {
                let slice: String = chars[i..i + n].iter().collect();
                if slice == *op {
                    toks.push(Tok::Op(op));
                    i += n;
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            // bash's arithmetic lexer reports an unknown character as an
            // "invalid arithmetic operator" and points at the rest of the
            // input from that character (`1.5` -> error token ".5").
            let rest: String = chars[i..].iter().collect();
            return Err(ArithError {
                message: "invalid arithmetic operator".to_string(),
                token: rest,
                expr_prefix: None,
            });
        }
    }
    Ok(toks)
}

impl Lexer {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.peek_op(op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn error_token(&self) -> String {
        match self.peek() {
            Some(Tok::Num(v)) => v.to_string(),
            Some(Tok::Ident(s)) => s.clone(),
            Some(Tok::Op(o)) => o.to_string(),
            Some(Tok::LParen) => "(".to_string(),
            Some(Tok::RParen) => ")".to_string(),
            Some(Tok::Comma) => ",".to_string(),
            // Position exhausted: the old lexer reported the LAST SCANNED
            // token's text, which is exactly the final token of the stream.
            None => self
                .toks
                .last()
                .map(|t| token_text(&Some(t.clone())))
                .unwrap_or_default(),
        }
    }
}

fn operand_expected(lex: &Lexer) -> ArithError {
    ArithError {
        message: "operand expected".to_string(),
        token: lex.error_token(),
        expr_prefix: None,
    }
}

/// Recursively evaluate a variable's value the way bash does: the value is
/// itself an arithmetic expression (`x="1+2"` -> 3, `x=" 1 + 2 "` -> 3,
/// `x="0x10"` -> 16, `x="y=5"` assigns y).  Errors carry the raw value as
/// the expression prefix so the diagnostic matches bash
/// (`3.5: arithmetic syntax error: invalid arithmetic operator ...`).
fn eval_var_string(s: &str, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    // Fast path: a plain number (also trims whitespace and accepts 0x/base#).
    if let Ok(v) = parse_number(s) {
        return Ok(v);
    }
    // Recursion cap: bash reports `x: expression recursion level exceeded
    // (error token is "x")` for self-referential values.
    let _guard = match VarEvalGuard::enter() {
        Some(g) => g,
        None => {
            return Err(ArithError {
                message: "expression recursion level exceeded".to_string(),
                token: s.trim().to_string(),
                expr_prefix: Some(s.trim_start().to_string()),
            });
        }
    };
    let toks = match tokenize(s) {
        Ok(t) => std::sync::Arc::new(t),
        Err(mut e) => {
            e.expr_prefix = Some(s.trim_start().to_string());
            return Err(e);
        }
    };
    let mut lex = Lexer::new(toks);
    if lex.toks.is_empty() {
        // An empty value evaluates to 0 without an error (bash: `x=`).
        return Ok(0);
    }
    let v = match parse_comma(&mut lex, env) {
        Ok(v) => v,
        Err(mut e) => {
            e.expr_prefix = Some(s.trim_start().to_string());
            return Err(e);
        }
    };
    if lex.pos < lex.toks.len() {
        return Err(ArithError {
            message: "arithmetic syntax error in expression".to_string(),
            token: lex.error_token(),
            expr_prefix: Some(s.trim_start().to_string()),
        });
    }
    Ok(v)
}

/// Read a variable through `env`, recursively evaluating its raw value the
/// way bash does (`x="1+2"; $((x))` is 3).  `may_unbound` passes through to
/// the plain numeric lookup for unset names.
fn env_read(env: &mut dyn ArithEnv, name: &str, may_unbound: bool) -> Result<i64, ArithError> {
    match env.get_str(name) {
        Some(raw) => {
            if raw.trim().is_empty() {
                return Ok(env.get(name, may_unbound).unwrap_or(0));
            }
            eval_var_string(&raw, env)
        }
        None => Ok(env.get(name, may_unbound).unwrap_or(0)),
    }
}

/// Evaluate an arithmetic expression.
pub fn eval(src: &str, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let toks = tokenize_cached(src)?;
    let mut lex = Lexer::new(toks);
    if lex.toks.is_empty() {
        return Err(ArithError::new("expression expected"));
    }
    let v = parse_comma(&mut lex, env)?;
    if lex.pos < lex.toks.len() {
        return Err(ArithError {
            message: "arithmetic syntax error in expression".to_string(),
            token: lex.error_token(),
                expr_prefix: None,
        });
    }
    Ok(v)
}

fn parse_comma(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_assign(lex, env)?;
    while lex.eat_op(",") {
        v = parse_assign(lex, env)?;
    }
    Ok(v)
}

fn parse_assign(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    // Detect `name op= ...` but not `==`, `!=`, `<=`, `>=`.  The (cached)
    // tokens are only cloned on the real assignment path; plain expressions
    // like `i+1` fall through with zero clones.
    let mut assign: Option<(String, &'static str)> = None;
    if let Some(Tok::Ident(n)) = lex.peek() {
        if let Some(Tok::Op(op)) = lex.toks.get(lex.pos + 1) {
            let aop = match *op {
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|=" => {
                    Some(*op)
                }
                _ => None,
            };
            if let Some(aop) = aop {
                assign = Some((n.clone(), aop));
            }
        }
    }
    if let Some((name, aop)) = assign {
        lex.pos += 2;
        let divisor_tok = lex.peek().cloned();
        let rhs = parse_assign(lex, env)?;
        let cur = env_read(env, &name, aop != "=")?;
        let newv = match aop {
            "=" => rhs,
            "+=" => cur.wrapping_add(rhs),
            "-=" => cur.wrapping_sub(rhs),
            "*=" => cur.wrapping_mul(rhs),
            "/=" => {
                if rhs == 0 {
                    return Err(ArithError {
                        message: "division by 0".to_string(),
                        token: token_text(&divisor_tok),
                            expr_prefix: None,
                    });
                }
                cur.wrapping_div(rhs)
            }
            "%=" => {
                if rhs == 0 {
                    return Err(ArithError {
                        message: "division by 0".to_string(),
                        token: token_text(&divisor_tok),
                            expr_prefix: None,
                    });
                }
                cur.wrapping_rem(rhs)
            }
            "<<=" => cur.wrapping_shl(rhs as u32),
            ">>=" => cur.wrapping_shr(rhs as u32),
            "&=" => cur & rhs,
            "^=" => cur ^ rhs,
            "|=" => cur | rhs,
            _ => unreachable!(),
        };
        env.set(&name, newv);
        return Ok(newv);
    }
    parse_conditional(lex, env)
}

/// A missing operand directly after `?` or `:` is reported by bash as
/// "expression expected" rather than the generic "operand expected".
fn expression_expected(mut e: ArithError) -> ArithError {
    if e.message == "operand expected" {
        e.message = "expression expected".to_string();
    }
    e
}

fn parse_conditional(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let cond = parse_logical_or(lex, env)?;
    if lex.eat_op("?") {
        // Conditional branches recurse through parse_assign; cap the depth so
        // a long `a?b:c?d:...` chain cannot exhaust the native stack.
        let _guard = NestGuard::enter()?;
        let then = parse_assign(lex, env).map_err(expression_expected)?;
        if !lex.eat_op(":") {
            return Err(ArithError {
                message: "`:' expected for conditional expression".to_string(),
                token: lex.error_token(),
                    expr_prefix: None,
            });
        }
        let other = parse_assign(lex, env).map_err(expression_expected)?;
        return Ok(if cond != 0 { then } else { other });
    }
    Ok(cond)
}

fn parse_logical_or(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_logical_and(lex, env)?;
    while lex.eat_op("||") {
        let rhs = parse_logical_and(lex, env)?;
        v = if v != 0 || rhs != 0 { 1 } else { 0 };
    }
    Ok(v)
}

fn parse_logical_and(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_bit_or(lex, env)?;
    while lex.eat_op("&&") {
        let rhs = parse_bit_or(lex, env)?;
        v = if v != 0 && rhs != 0 { 1 } else { 0 };
    }
    Ok(v)
}

fn parse_bit_or(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_bit_xor(lex, env)?;
    while lex.peek_op("|") {
        lex.pos += 1;
        let rhs = parse_bit_xor(lex, env)?;
        v |= rhs;
    }
    Ok(v)
}

fn parse_bit_xor(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_bit_and(lex, env)?;
    while lex.peek_op("^") {
        lex.pos += 1;
        let rhs = parse_bit_and(lex, env)?;
        v ^= rhs;
    }
    Ok(v)
}

fn parse_bit_and(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_equality(lex, env)?;
    while lex.peek_op("&") {
        lex.pos += 1;
        let rhs = parse_equality(lex, env)?;
        v &= rhs;
    }
    Ok(v)
}

fn parse_equality(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_relational(lex, env)?;
    loop {
        if lex.eat_op("==") {
            let rhs = parse_relational(lex, env)?;
            v = if v == rhs { 1 } else { 0 };
        } else if lex.eat_op("!=") {
            let rhs = parse_relational(lex, env)?;
            v = if v != rhs { 1 } else { 0 };
        } else {
            break;
        }
    }
    Ok(v)
}

fn parse_relational(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_shift(lex, env)?;
    loop {
        if lex.eat_op("<=") {
            let rhs = parse_shift(lex, env)?;
            v = if v <= rhs { 1 } else { 0 };
        } else if lex.eat_op(">=") {
            let rhs = parse_shift(lex, env)?;
            v = if v >= rhs { 1 } else { 0 };
        } else if lex.peek_op("<") {
            lex.pos += 1;
            let rhs = parse_shift(lex, env)?;
            v = if v < rhs { 1 } else { 0 };
        } else if lex.peek_op(">") {
            lex.pos += 1;
            let rhs = parse_shift(lex, env)?;
            v = if v > rhs { 1 } else { 0 };
        } else {
            break;
        }
    }
    Ok(v)
}

fn parse_shift(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_additive(lex, env)?;
    loop {
        if lex.eat_op("<<") {
            let rhs = parse_additive(lex, env)?;
            v = v.wrapping_shl(rhs as u32);
        } else if lex.eat_op(">>") {
            let rhs = parse_additive(lex, env)?;
            v = v.wrapping_shr(rhs as u32);
        } else {
            break;
        }
    }
    Ok(v)
}

fn parse_additive(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_multiplicative(lex, env)?;
    loop {
        if lex.peek_op("+") {
            lex.pos += 1;
            let rhs = parse_multiplicative(lex, env)?;
            v = v.wrapping_add(rhs);
        } else if lex.peek_op("-") {
            lex.pos += 1;
            let rhs = parse_multiplicative(lex, env)?;
            v = v.wrapping_sub(rhs);
        } else {
            break;
        }
    }
    Ok(v)
}

fn parse_multiplicative(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut v = parse_unary(lex, env)?;
    loop {
        if lex.peek_op("*") {
            lex.pos += 1;
            let rhs = parse_unary(lex, env)?;
            v = v.wrapping_mul(rhs);
        } else if lex.peek_op("/") {
            lex.pos += 1;
            let divisor_tok = lex.peek().cloned();
            let rhs = parse_unary(lex, env)?;
            if rhs == 0 {
                return Err(ArithError {
                    message: "division by 0".to_string(),
                    token: token_text(&divisor_tok),
                        expr_prefix: None,
                });
            }
            v = v.wrapping_div(rhs);
        } else if lex.peek_op("%") {
            lex.pos += 1;
            let divisor_tok = lex.peek().cloned();
            let rhs = parse_unary(lex, env)?;
            if rhs == 0 {
                return Err(ArithError {
                    message: "division by 0".to_string(),
                    token: token_text(&divisor_tok),
                        expr_prefix: None,
                });
            }
            v = v.wrapping_rem(rhs);
        } else {
            break;
        }
    }
    Ok(v)
}

fn parse_unary(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    // Fold the leading unary operators iteratively instead of recursing once
    // per operator: tens of thousands of `!` used to overflow the native
    // stack.  Prefix operators associate right-to-left, so the runs are kept
    // in source order and applied in reverse.  Adjacent identical operators
    // cancel in pairs (`~~x == x`, `--x == x`); a run of `!` is applied once
    // when odd and twice when even (`!!x` boolises, matching bash), so `!`
    // keeps its exact semantics while the work stays linear.
    let mut runs: Vec<(char, usize)> = Vec::new();
    loop {
        let op = if lex.peek_op("!") {
            '!'
        } else if lex.peek_op("~") {
            '~'
        } else if lex.peek_op("-") {
            '-'
        } else if lex.peek_op("+") {
            '+'
        } else {
            break;
        };
        lex.pos += 1;
        if op == '+' {
            continue; // unary plus is a no-op
        }
        match runs.last_mut() {
            Some((last, n)) if *last == op => *n += 1,
            _ => runs.push((op, 1)),
        }
    }

    // The operand: prefix ++/-- mutates a variable once; otherwise descend.
    let mut v = if lex.peek_op("++") || lex.peek_op("--") {
        let op = if lex.peek_op("++") { "++" } else { "--" };
        lex.pos += 1;
        let name = match lex.peek().cloned() {
            Some(Tok::Ident(n)) => n,
            _ => return Err(operand_expected(lex)),
        };
        lex.pos += 1;
        let cur = env_read(env, &name, true)?;
        let newv = if op == "++" {
            cur.wrapping_add(1)
        } else {
            cur.wrapping_sub(1)
        };
        env.set(&name, newv);
        newv
    } else {
        parse_power(lex, env)?
    };

    for &(op, n) in runs.iter().rev() {
        v = match op {
            '!' => {
                if n % 2 == 1 {
                    if v == 0 { 1 } else { 0 } // logical not
                } else {
                    if v == 0 { 0 } else { 1 } // !! boolises
                }
            }
            '~' => {
                if n % 2 == 1 { !v } else { v }
            }
            '-' => {
                if n % 2 == 1 { v.wrapping_neg() } else { v }
            }
            _ => v,
        };
    }
    Ok(v)
}

fn parse_power(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let base = parse_postfix(lex, env)?;
    if lex.peek_op("**") {
        lex.pos += 1;
        // right associative: each exponent recursion takes a depth slot
        let _guard = NestGuard::enter()?;
        let exp = parse_unary(lex, env)?;
        if exp < 0 {
            // bash points at the exponent operand itself, e.g. `2**-1`
            // reports (error token is "1").  The whole right-hand side has
            // been consumed by now, so `error_token()` falls back to the
            // text of the last token.
            return Err(ArithError {
                message: "exponent less than 0".to_string(),
                token: lex.error_token(),
                    expr_prefix: None,
            });
        }
        return Ok(base.wrapping_pow(exp as u32));
    }
    Ok(base)
}

fn parse_postfix(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let v = parse_primary(lex, env)?;
    if lex.peek_op("++") || lex.peek_op("--") {
        let op = if lex.peek_op("++") { "++" } else { "--" };
        // Postfix applies to a variable; we need the name, so re-read it.
        if let Some(Tok::Ident(name)) = lex.toks.get(lex.pos - 1).cloned() {
            lex.pos += 1;
            let newv = if op == "++" {
                v.wrapping_add(1)
            } else {
                v.wrapping_sub(1)
            };
            env.set(&name, newv);
            return Ok(v);
        }
    }
    Ok(v)
}

fn parse_primary(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    // Zero-clone hot path: the identifier is borrowed straight out of the
    // cached token vector.  Disjoint field borrows (`lex.toks` immutable,
    // `lex.pos` mutated) are fine with direct field access.
    let is_ident = matches!(lex.peek(), Some(Tok::Ident(_)));
    if is_ident {
        let name: &str = match &lex.toks[lex.pos] {
            Tok::Ident(n) => n,
            _ => unreachable!(),
        };
        lex.pos += 1;
        // bash evaluates a variable's *value* recursively as an expression
        // (`x="1+2"; $((x))` is 3); env_read implements exactly that.
        return env_read(env, name, true);
    }
    match lex.peek().cloned() {
        Some(Tok::Num(v)) => {
            lex.pos += 1;
            Ok(v)
        }
        Some(Tok::LParen) => {
            lex.pos += 1;
            // Each nested `(` recurses through parse_comma; cap the depth so
            // thousands of parentheses cannot overflow the native stack.
            let _guard = NestGuard::enter()?;
            let v = parse_comma(lex, env)?;
            if !matches!(lex.peek(), Some(Tok::RParen)) {
                return Err(operand_expected(lex));
            }
            lex.pos += 1;
            Ok(v)
        }
        _ => Err(operand_expected(lex)),
    }
}
