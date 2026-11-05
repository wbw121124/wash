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
}

impl ArithError {
    fn new(msg: &str) -> Self {
        ArithError {
            message: msg.to_string(),
            token: String::new(),
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
        Some(Tok::Question) => "?".to_string(),
        Some(Tok::Colon) => ":".to_string(),
        None => String::new(),
    }
}

/// Variable access used while evaluating an expression.
pub trait ArithEnv {
    /// Value of `name`, or `None` when the variable is unset.
    fn get(&self, name: &str) -> Option<i64>;
    /// Assign `value` to `name`.
    fn set(&mut self, name: &str, value: i64);
}

/// An environment backed by a plain map (used for `let` on the shell's
/// variable table and for tests).
pub struct MapEnv<'a> {
    pub vars: &'a mut HashMap<String, String>,
}

impl<'a> ArithEnv for MapEnv<'a> {
    fn get(&self, name: &str) -> Option<i64> {
        self.vars.get(name).and_then(|v| parse_number(v).ok())
    }
    fn set(&mut self, name: &str, value: i64) {
        self.vars.insert(name.to_string(), value.to_string());
    }
}

/// A read-only environment: assignments are silently discarded.
pub struct ReadOnlyEnv<'a> {
    pub vars: &'a HashMap<String, String>,
}

impl<'a> ArithEnv for ReadOnlyEnv<'a> {
    fn get(&self, name: &str) -> Option<i64> {
        self.vars.get(name).and_then(|v| parse_number(v).ok())
    }
    fn set(&mut self, _name: &str, _value: i64) {}
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

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(i64),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Question,
    Colon,
}

struct Lexer {
    toks: Vec<Tok>,
    pos: usize,
    /// The token text used for error messages ("error token is ...").
    last_text: String,
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
    fn new(src: &str) -> Result<Lexer, ArithError> {
        let chars: Vec<char> = src.chars().collect();
        let mut toks = Vec::new();
        let mut i = 0usize;
        let mut last_text = String::new();
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
                let v = parse_unsigned(&text)
                    .map_err(|e| ArithError::new(&e.message))?;
                last_text = text;
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
                last_text = text.clone();
                toks.push(Tok::Ident(text));
                continue;
            }
            if c == '(' {
                last_text = "(".to_string();
                toks.push(Tok::LParen);
                i += 1;
                continue;
            }
            if c == ')' {
                last_text = ")".to_string();
                toks.push(Tok::RParen);
                i += 1;
                continue;
            }
            if c == ',' {
                last_text = ",".to_string();
                toks.push(Tok::Comma);
                i += 1;
                continue;
            }
            if c == '?' {
                last_text = "?".to_string();
                toks.push(Tok::Question);
                i += 1;
                continue;
            }
            if c == ':' {
                last_text = ":".to_string();
                toks.push(Tok::Colon);
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
                        last_text = slice;
                        toks.push(Tok::Op(op));
                        i += n;
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                return Err(ArithError::new(&format!(
                    "syntax error in expression (error token is \"{}\")",
                    c
                )));
            }
        }
        Ok(Lexer {
            toks,
            pos: 0,
            last_text,
        })
    }

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
            Some(Tok::Question) => "?".to_string(),
            Some(Tok::Colon) => ":".to_string(),
            None => self.last_text.clone(),
        }
    }
}

fn operand_expected(lex: &Lexer) -> ArithError {
    ArithError {
        message: "operand expected".to_string(),
        token: lex.error_token(),
    }
}

/// Evaluate an arithmetic expression.
pub fn eval(src: &str, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let mut lex = Lexer::new(src)?;
    if lex.toks.is_empty() {
        return Err(ArithError::new("expression expected"));
    }
    let v = parse_comma(&mut lex, env)?;
    if lex.pos < lex.toks.len() {
        return Err(ArithError {
            message: "syntax error in expression".to_string(),
            token: lex.error_token(),
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
    let start = lex.pos;
    // Detect `name op= ...` but not `==`, `!=`, `<=`, `>=`.
    if let Some(Tok::Ident(name)) = lex.peek().cloned() {
        if let Some(Tok::Op(op)) = lex.toks.get(lex.pos + 1).cloned() {
            let assign_op = match op {
                "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|=" => Some(op),
                _ => None,
            };
            if let Some(aop) = assign_op {
                lex.pos += 2;
                let divisor_tok = lex.peek().cloned();
                let rhs = parse_assign(lex, env)?;
                let cur = env.get(&name).unwrap_or(0);
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
                            });
                        }
                        cur.wrapping_div(rhs)
                    }
                    "%=" => {
                        if rhs == 0 {
                            return Err(ArithError {
                                message: "division by 0".to_string(),
                                token: token_text(&divisor_tok),
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
        }
    }
    let _ = start;
    parse_conditional(lex, env)
}

fn parse_conditional(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let cond = parse_logical_or(lex, env)?;
    if lex.eat_op("?") {
        let then = parse_assign(lex, env)?;
        if !lex.eat_op(":") {
            return Err(operand_expected(lex));
        }
        let other = parse_assign(lex, env)?;
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
    if lex.eat_op("!") {
        let v = parse_unary(lex, env)?;
        return Ok(if v == 0 { 1 } else { 0 });
    }
    if lex.eat_op("~") {
        let v = parse_unary(lex, env)?;
        return Ok(!v);
    }
    if lex.eat_op("-") {
        let v = parse_unary(lex, env)?;
        return Ok(v.wrapping_neg());
    }
    if lex.eat_op("+") {
        return parse_unary(lex, env);
    }
    // prefix ++ / --
    if lex.peek_op("++") || lex.peek_op("--") {
        let op = if lex.peek_op("++") { "++" } else { "--" };
        lex.pos += 1;
        let name = match lex.peek().cloned() {
            Some(Tok::Ident(n)) => n,
            _ => return Err(operand_expected(lex)),
        };
        lex.pos += 1;
        let cur = env.get(&name).unwrap_or(0);
        let newv = if op == "++" {
            cur.wrapping_add(1)
        } else {
            cur.wrapping_sub(1)
        };
        env.set(&name, newv);
        return Ok(newv);
    }
    parse_power(lex, env)
}

fn parse_power(lex: &mut Lexer, env: &mut dyn ArithEnv) -> Result<i64, ArithError> {
    let base = parse_postfix(lex, env)?;
    if lex.peek_op("**") {
        lex.pos += 1;
        // right associative
        let exp = parse_unary(lex, env)?;
        if exp < 0 {
            return Err(ArithError::new("exponent less than 0"));
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
    match lex.peek().cloned() {
        Some(Tok::Num(v)) => {
            lex.pos += 1;
            Ok(v)
        }
        Some(Tok::Ident(name)) => {
            lex.pos += 1;
            Ok(env.get(&name).unwrap_or(0))
        }
        Some(Tok::LParen) => {
            lex.pos += 1;
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
