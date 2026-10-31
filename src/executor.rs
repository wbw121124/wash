// Phase 1 executor: expand words, then run the AST via fork/execve. Also
// provides `capture_command` for command/process substitution (runs the inner
// command in a forked subshell and returns its stdout).
//
// Pipeline plumbing, redirections, and exit-status decoding go through the
// msys POSIX shim in `posix.rs`.

use std::collections::HashMap;
use std::ffi::{c_char, CString};
use std::io::Write;

use crate::expand::{expand_argv, expand_scalar, expand_word, ExpandCtx};
use crate::lexer::WordPart;
use crate::parser::{CommandList, Pipeline, Redir};
use crate::posix;
use crate::shell::Shell;

/// Holds CStrings + a null-terminated pointer array for an argv/envp vector.
struct ArgVec {
    _cstrings: Vec<CString>,
    ptrs: Vec<*const c_char>,
}

impl ArgVec {
    fn from(args: &[String]) -> Self {
        let mut cstrings = Vec::with_capacity(args.len());
        let mut ptrs = Vec::with_capacity(args.len() + 1);
        for a in args {
            let c = CString::new(a.as_str()).unwrap_or_default();
            ptrs.push(c.as_ptr());
            cstrings.push(c);
        }
        ptrs.push(std::ptr::null());
        ArgVec {
            _cstrings: cstrings,
            ptrs,
        }
    }
}

fn build_env_ptrs(env: &HashMap<String, String>) -> (Vec<CString>, Vec<*const c_char>) {
    let cstrings: Vec<CString> = env
        .iter()
        .map(|(k, v)| CString::new(format!("{}={}", k, v)).unwrap_or_default())
        .collect();
    let mut ptrs: Vec<*const c_char> = cstrings.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    (cstrings, ptrs)
}

fn find_executable(name: &str, path: &str) -> Option<CString> {
    if name.contains('/') || name.contains('\\') {
        if posix::access(name, posix::X_OK) == 0 {
            return Some(posix::to_cstr(name));
        }
        let exe = format!("{}.exe", name);
        if posix::access(&exe, posix::X_OK) == 0 {
            return Some(posix::to_cstr(&exe));
        }
        return None;
    }
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = format!("{}/{}", dir, name);
        if posix::access(&cand, posix::X_OK) == 0 {
            return Some(posix::to_cstr(&cand));
        }
        let cand_exe = format!("{}.exe", cand);
        if posix::access(&cand_exe, posix::X_OK) == 0 {
            return Some(posix::to_cstr(&cand_exe));
        }
    }
    None
}

fn try_builtin(argv: &[String], sh: Option<&mut Shell>) -> Option<i32> {
    let name = argv.first()?;
    match name.as_str() {
        "cd" => {
            let target = argv.get(1).map(|s| s.as_str()).unwrap_or("");
            let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
            let dest: &str = if target.is_empty() { &home } else { target };
            if posix::chdir(dest) != 0 {
                eprintln!("wash: cd: {}: No such file or directory", dest);
                return Some(1);
            }
            if let Some(sh) = sh {
                sh.cwd = dest.to_string();
                sh.env.insert("PWD".to_string(), dest.to_string());
            }
            Some(0)
        }
        "exit" => Some(0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Output builtins: echo / printf
//
// These run in the forked child AFTER redirections have been applied, so that
// `echo out > file` and `printf ... > file` actually write the file. Only the
// STATE builtins (`cd`, `exit`) and `printf -v` (which mutates shell state)
// run in the parent process.
// ---------------------------------------------------------------------------

/// Output builtins that must execute in the child (with redirections applied).
/// Returns `Some(exit_code)` if `argv[0]` is one of these builtins.
fn run_output_builtin(argv: &[String]) -> Option<i32> {
    match argv.first().map(|s| s.as_str()) {
        Some("echo") => Some(builtin_echo(argv)),
        Some("printf") => Some(builtin_printf(argv)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// echo
// ---------------------------------------------------------------------------

fn builtin_echo(argv: &[String]) -> i32 {
    // Options are recognised only in LEADING arguments (before the first
    // non-option argument). A leading arg is an option arg only if every
    // character after the initial '-' is one of n/e/E. `--` (or any arg whose
    // second char is not n/e/E) stops option parsing and becomes text.
    let mut nflag = false;
    let mut eflag = false;
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a.starts_with('-') && a.len() > 1 {
            let mut all_ok = true;
            for ch in a.chars().skip(1) {
                if !matches!(ch, 'n' | 'e' | 'E') {
                    all_ok = false;
                    break;
                }
            }
            if !all_ok {
                break;
            }
            for ch in a.chars().skip(1) {
                match ch {
                    'n' => nflag = true,
                    'e' => eflag = true,
                    'E' => eflag = false,
                    _ => {}
                }
            }
            i += 1;
        } else {
            break;
        }
    }

    let text_args = &argv[i..];
    let mut out: Vec<u8> = Vec::new();
    if eflag {
        let joined = text_args.join(" ");
        let (bytes, stopped) = interpret_escapes(joined.as_bytes(), true);
        out.extend_from_slice(&bytes);
        if !stopped && !nflag {
            out.push(b'\n');
        }
    } else {
        let s = text_args.join(" ");
        out.extend_from_slice(s.as_bytes());
        if !nflag {
            out.push(b'\n');
        }
    }

    let _ = std::io::stdout().write_all(&out);
    let _ = std::io::stdout().flush();
    0
}

/// True if `s` is a legal bash identifier (used to validate `printf -v var`).
fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// How `printf`'s leading options parsed.
enum PrintfOpt {
    /// `-v var`: assign the rendered output to shell variable `var`.
    Assign { var: String, format_idx: usize },
    /// Ordinary invocation; the format string starts at `format_idx`.
    Normal { format_idx: usize },
    /// A usage error (missing `-v` argument, invalid option, or a `-v` with no
    /// format string). Caller should emit a diagnostic and exit 2.
    Usage,
}

/// Parse the leading options of a `printf` command line, mirroring bash's
/// `getopt(argc, argv, "v:")` behaviour:
///   * `-v var` or `-vvar` (attached) selects the assignment variable;
///   * `--` ends option scanning (and is removed);
///   * any other `-X` argument is an invalid option;
///   * a `-` alone, or the first non-option argument, is the format string.
fn parse_printf_opt(argv: &[String]) -> PrintfOpt {
    let mut idx = 1usize;
    let mut var: Option<String> = None;

    loop {
        if idx >= argv.len() {
            // Ran out of arguments. A bare `printf` with `-v` but no format is
            // a usage error; a bare `printf` with no args yields an empty format
            // which the caller turns into a usage error too.
            return if var.is_some() {
                PrintfOpt::Usage
            } else {
                PrintfOpt::Normal { format_idx: argv.len() }
            };
        }
        let a = &argv[idx];
        if a == "-v" {
            if idx + 1 >= argv.len() {
                return PrintfOpt::Usage; // `-v` requires an argument
            }
            var = Some(argv[idx + 1].clone());
            idx += 2;
        } else if a.starts_with("-v") && a.len() > 2 {
            var = Some(a[2..].to_string());
            idx += 1;
        } else if a == "--" {
            idx += 1;
            break;
        } else if a.starts_with('-') && a.len() > 1 {
            // Any other `-X` is an invalid option.
            return PrintfOpt::Usage;
        } else {
            // First non-option argument is the format string.
            break;
        }
    }

    if var.is_some() && idx >= argv.len() {
        // `-v var` with no format string.
        return PrintfOpt::Usage;
    }

    match var {
        Some(v) => PrintfOpt::Assign {
            var: v,
            format_idx: idx,
        },
        None => PrintfOpt::Normal { format_idx: idx },
    }
}

// ---------------------------------------------------------------------------
// printf
// ---------------------------------------------------------------------------

fn builtin_printf(argv: &[String]) -> i32 {
    // `printf -v` runs in the PARENT (see `run_standalone_exp`) so its result
    // persists in the shell's environment. In a child (e.g. a pipeline
    // subshell) the variable cannot reach the parent, so we simply compute the
    // output, discard it, and emit nothing to stdout — matching bash.
    match parse_printf_opt(argv) {
        PrintfOpt::Assign { format_idx, .. } => {
            let format = argv.get(format_idx).cloned().unwrap_or_default();
            let args = argv.get(format_idx + 1..).unwrap_or(&[]);
            let (_bytes, conv_error) = printf_format(&format, args);
            return if conv_error { 1 } else { 0 };
        }
        PrintfOpt::Usage => {
            eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
            return 2;
        }
        PrintfOpt::Normal { format_idx } => {
            let format = argv.get(format_idx).cloned().unwrap_or_default();
            let args = argv.get(format_idx + 1..).unwrap_or(&[]);
            if format.is_empty() {
                eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
                return 2;
            }
            let (bytes, conv_error) = printf_format(&format, args);
            let _ = std::io::stdout().write_all(&bytes);
            let _ = std::io::stdout().flush();
            if conv_error {
                1
            } else {
                0
            }
        }
    }
}

/// Format `format` against `args` and return the rendered bytes plus a flag
/// indicating whether a numeric conversion failed (bash's "conv_error" → exit
/// code 1). This is the shared core used by both the child (stdout) and the
/// parent (`-v`, store to env).
fn printf_format(format: &str, args: &[String]) -> (Vec<u8>, bool) {
    let bytes = format.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut conv_error = false;
    let mut ai: usize = 0;

    let has_conv = {
        let mut f = false;
        let mut k = 0;
        while k < bytes.len() {
            if bytes[k] == b'%' && k + 1 < bytes.len() && bytes[k + 1] != b'%' {
                f = true;
                break;
            }
            k += 1;
        }
        f
    };

    loop {
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'%' {
                if i + 1 >= bytes.len() {
                    // Trailing '%' (no conversion): bash → no output, error.
                    conv_error = true;
                    i += 1;
                    continue;
                }
                let nxt = bytes[i + 1];
                if nxt == b'%' {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                let (ni, stop) = parse_printf_spec(bytes, i, &mut out, args, &mut ai, &mut conv_error);
                i = ni;
                if stop {
                    return (out, conv_error);
                }
            } else if c == b'\\' {
                let (esc, ni, stop) = escape_at(bytes, i, false);
                out.extend_from_slice(&esc);
                i = ni;
                if stop {
                    return (out, conv_error);
                }
            } else {
                out.push(c);
                i += 1;
            }
        }
        if !has_conv {
            break;
        }
        if ai >= args.len() {
            break;
        }
    }

    (out, conv_error)
}

fn parse_printf_spec(
    bytes: &[u8],
    start: usize,
    out: &mut Vec<u8>,
    args: &[String],
    ai: &mut usize,
    conv_error: &mut bool,
) -> (usize, bool) {
    let mut i = start + 1; // skip '%'

    // Flags.
    let mut flags = String::new();
    let mut minus = false;
    let mut zero = false;
    while i < bytes.len() && matches!(bytes[i], b'-' | b'+' | b' ' | b'#' | b'0') {
        let f = bytes[i] as char;
        flags.push(f);
        if f == '-' {
            minus = true;
        }
        if f == '0' {
            zero = true;
        }
        i += 1;
    }

    // Width.
    let mut width: i64 = 0;
    let mut have_width = false;
    if i < bytes.len() && bytes[i] == b'*' {
        width = get_star_arg(args, ai, conv_error);
        *ai += 1;
        have_width = true;
        i += 1;
    } else if i < bytes.len() && bytes[i].is_ascii_digit() {
        let (w, ni) = parse_int_literal(bytes, i);
        width = w;
        have_width = true;
        i = ni;
    }

    // Precision.
    let mut prec: i64 = 0;
    let mut have_prec = false;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        if i < bytes.len() && bytes[i] == b'*' {
            prec = get_star_arg(args, ai, conv_error);
            *ai += 1;
            have_prec = true;
            i += 1;
        } else if i < bytes.len() && bytes[i].is_ascii_digit() {
            let (p, ni) = parse_int_literal(bytes, i);
            prec = p;
            have_prec = true;
            i = ni;
        } else {
            prec = 0;
            have_prec = true;
        }
    }

    // Length modifiers (ignored — we always format with ll / L). NOTE: `q` is
    // intentionally NOT here, because in bash `%q` is the quote *conversion*,
    // not a length modifier.
    while i < bytes.len() && matches!(bytes[i], b'l' | b'h' | b'L' | b'j' | b'z' | b't') {
        i += 1;
    }

    if i >= bytes.len() {
        // Incomplete conversion (e.g. trailing "%5"): bash → error, no output.
        *conv_error = true;
        return (start + 1, false);
    }

    let conv = bytes[i];
    i += 1;

    // Resolve width/precision.
    let mut rwidth = width;
    let rprec = prec;
    if have_prec && rprec < 0 {
        have_prec = false; // negative precision → omitted
    }
    if have_width && rwidth < 0 {
        minus = true; // negative width → left-justify
        rwidth = -rwidth;
    }

    // The argument for this conversion.
    let arg: &str = if *ai < args.len() {
        args[*ai].as_str()
    } else {
        ""
    };
    let arg_missing = *ai >= args.len();

    match conv {
        b's' | b'b' => {
            let content: Vec<u8> = if conv == b'b' {
                let (bts, stop) = interpret_escapes(arg.as_bytes(), true);
                if stop {
                    *ai += 1;
                    return (i, true);
                }
                bts
            } else {
                arg.as_bytes().to_vec()
            };
            let content = truncate_prec(&content, have_prec, rprec);
            let padded =
                apply_field(&content, minus, zero, if have_width { rwidth } else { 0 }, true);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'q' => {
            let content = sh_quote(arg.as_bytes());
            let content = truncate_prec(&content, have_prec, rprec);
            // `%q` ignores the `0` flag (space padding only).
            let padded =
                apply_field(&content, minus, false, if have_width { rwidth } else { 0 }, false);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'c' => {
            let content: Vec<u8> = if arg.is_empty() {
                vec![0u8] // empty → NUL byte (bash behaviour)
            } else {
                vec![arg.as_bytes()[0]]
            };
            let padded = apply_field(&content, minus, zero, if have_width { rwidth } else { 0 }, true);
            out.extend_from_slice(&padded);
            *ai += 1;
        }
        b'd' | b'i' | b'o' | b'u' | b'x' | b'X' => {
            let (v_ll, v_ull, valid) = if arg_missing {
                (0i64, 0u64, true)
            } else if conv == b'u' {
                let (v, fully, overflow) = posix::parse_unsigned_long_long(arg);
                (0i64, v, fully && !overflow)
            } else {
                let (v, fully, overflow) = posix::parse_long_long(arg);
                (v, v as u64, fully && !overflow)
            };
            if !valid {
                *conv_error = true;
            }
            let mut cf = String::from("%");
            cf.push_str(&flags);
            if have_width {
                cf.push_str(&rwidth.to_string());
            }
            if have_prec {
                cf.push('.');
                cf.push_str(&rprec.to_string());
            }
            let rendered = match conv {
                b'd' | b'i' => {
                    cf.push_str("lld");
                    posix::snprintf_ll_d(&cf, v_ll)
                }
                b'o' => {
                    cf.push_str("llo");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
                b'x' => {
                    cf.push_str("llx");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
                b'X' => {
                    cf.push_str("llX");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
                _ => {
                    cf.push_str("llu");
                    posix::snprintf_ll_u(&cf, v_ull)
                }
            };
            out.extend_from_slice(&rendered);
            *ai += 1;
        }
        b'e' | b'E' | b'f' | b'g' | b'G' | b'a' | b'A' => {
            let mut cf = String::from("%");
            cf.push_str(&flags);
            if have_width {
                cf.push_str(&rwidth.to_string());
            }
            if have_prec {
                cf.push('.');
                cf.push_str(&rprec.to_string());
            }
            cf.push('L');
            cf.push(conv as char);
            let arg_for_c = if arg_missing { "0" } else { arg };
            let (rendered, valid) = posix::printf_long_double(&cf, arg_for_c);
            if !valid {
                *conv_error = true;
            }
            out.extend_from_slice(&rendered);
            *ai += 1;
        }
        _ => {
            // Unknown conversion → bash: error, no output, but the argument is
            // still consumed.
            *conv_error = true;
            *ai += 1;
        }
    }

    (i, false)
}

/// Read a `*` field-width / precision argument as a base-0 integer.
fn get_star_arg(args: &[String], ai: &mut usize, conv_error: &mut bool) -> i64 {
    if *ai >= args.len() {
        return 0;
    }
    let a = &args[*ai];
    let (v, fully, overflow) = posix::parse_long_long(a);
    if !fully || overflow {
        *conv_error = true;
    }
    v
}

/// Parse a run of decimal digits starting at `i` (for literal width/precision).
fn parse_int_literal(bytes: &[u8], i: usize) -> (i64, usize) {
    let mut j = i;
    let mut v: i64 = 0;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((bytes[j] - b'0') as i64);
        j += 1;
    }
    (v, j)
}

/// Interpret a single backslash escape at `bytes[i]`. Returns the bytes to
/// emit, the new index, and (for `allow_c`) whether output must stop (`\c`).
fn escape_at(bytes: &[u8], i: usize, allow_c: bool) -> (Vec<u8>, usize, bool) {
    // Caller guarantees bytes[i] == '\\'.
    if i + 1 >= bytes.len() {
        return (vec![b'\\'], i + 1, false);
    }
    let nxt = bytes[i + 1];
    match nxt {
        b'a' => (vec![0x07], i + 2, false),
        b'b' => (vec![0x08], i + 2, false),
        b'e' => (vec![0x1b], i + 2, false),
        b'f' => (vec![0x0c], i + 2, false),
        b'n' => (vec![0x0a], i + 2, false),
        b'r' => (vec![0x0d], i + 2, false),
        b't' => (vec![0x09], i + 2, false),
        b'v' => (vec![0x0b], i + 2, false),
        b'\\' => (vec![b'\\'], i + 2, false),
        b'c' => {
            if allow_c {
                (Vec::new(), i + 2, true)
            } else {
                (vec![b'\\', b'c'], i + 2, false)
            }
        }
        b'0' => {
            // Octal escape: bash requires the leading zero (`\0NNN`); up to three
            // additional octal digits are consumed, the value is taken mod 256.
            // A bare `\0` yields a NUL byte.
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2; // skip the backslash and the `0`
            while cnt < 3 && j < bytes.len() && (bytes[j] as char).is_digit(8) {
                val = val * 8 + (bytes[j] - b'0') as u32;
                j += 1;
                cnt += 1;
            }
            (vec![(val & 0xff) as u8], j, false)
        }
        b'x' | b'X' => {
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2;
            while cnt < 2 && j < bytes.len() && (bytes[j] as char).is_digit(16) {
                val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                j += 1;
                cnt += 1;
            }
            if cnt == 0 {
                (vec![b'\\', nxt], i + 2, false)
            } else {
                (vec![(val & 0xff) as u8], j, false)
            }
        }
        b'u' => {
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2;
            while cnt < 4 && j < bytes.len() && (bytes[j] as char).is_digit(16) {
                val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                j += 1;
                cnt += 1;
            }
            if cnt == 0 {
                (vec![b'\\', b'u'], i + 2, false)
            } else {
                (encode_unicode(val), j, false)
            }
        }
        b'U' => {
            let mut val: u32 = 0;
            let mut cnt = 0;
            let mut j = i + 2;
            while cnt < 8 && j < bytes.len() && (bytes[j] as char).is_digit(16) {
                val = val * 16 + (bytes[j] as char).to_digit(16).unwrap();
                j += 1;
                cnt += 1;
            }
            if cnt == 0 {
                (vec![b'\\', b'U'], i + 2, false)
            } else {
                (encode_unicode(val), j, false)
            }
        }
        _ => (vec![b'\\', nxt], i + 2, false),
    }
}

/// Interpret backslash escapes over a whole buffer (`allow_c` enables `\c` to
/// stop output). Used by `echo -e` and `%b`.
fn interpret_escapes(input: &[u8], allow_c: bool) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'\\' {
            let (esc, ni, stop) = escape_at(input, i, allow_c);
            out.extend_from_slice(&esc);
            i = ni;
            if stop {
                return (out, true);
            }
        } else {
            out.push(input[i]);
            i += 1;
        }
    }
    (out, false)
}

/// Apply field width / precision / flags to a converted string.
/// `zero_pad` enables zero padding (used by `%s %c %b`; `%q` never zero-pads).
fn apply_field(content: &[u8], minus: bool, zero: bool, width: i64, zero_pad: bool) -> Vec<u8> {
    if width <= 0 {
        return content.to_vec();
    }
    let w = width as usize;
    if content.len() >= w {
        return content.to_vec();
    }
    let pad = w - content.len();
    if minus {
        let mut r = content.to_vec();
        r.extend(std::iter::repeat(b' ').take(pad));
        r
    } else if zero && zero_pad {
        let mut r = vec![b'0'; pad];
        r.extend_from_slice(content);
        r
    } else {
        let mut r = vec![b' '; pad];
        r.extend_from_slice(content);
        r
    }
}

fn truncate_prec(content: &[u8], have_prec: bool, prec: i64) -> Vec<u8> {
    if have_prec && prec >= 0 {
        let p = prec as usize;
        if content.len() > p {
            return content[..p].to_vec();
        }
    }
    content.to_vec()
}

/// Encode a Unicode code point as UTF-8 (works for any u32, matching bash's
/// `printf`/`echo -e` raw byte output for `\u`/`\U`).
fn encode_unicode(cp: u32) -> Vec<u8> {
    if cp <= 0x7F {
        vec![(cp & 0xFF) as u8]
    } else if cp <= 0x7FF {
        vec![
            (0xC0 | (cp >> 6)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else if cp <= 0xFFFF {
        vec![
            (0xE0 | (cp >> 12)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else if cp <= 0x1FFFFF {
        vec![
            (0xF0 | (cp >> 18)) as u8,
            (0x80 | ((cp >> 12) & 0x3F)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else if cp <= 0x3FFFFFF {
        vec![
            (0xF8 | (cp >> 24)) as u8,
            (0x80 | ((cp >> 18) & 0x3F)) as u8,
            (0x80 | ((cp >> 12) & 0x3F)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    } else {
        vec![
            (0xFC | (cp >> 30)) as u8,
            (0x80 | ((cp >> 24) & 0x3F)) as u8,
            (0x80 | ((cp >> 18) & 0x3F)) as u8,
            (0x80 | ((cp >> 12) & 0x3F)) as u8,
            (0x80 | ((cp >> 6) & 0x3F)) as u8,
            (0x80 | (cp & 0x3F)) as u8,
        ]
    }
}

/// Bash `%q` quoting (mirrors `sh_quote`): quote a string so it can be safely
/// reused as shell input.
fn sh_quote(s: &[u8]) -> Vec<u8> {
    if s.is_empty() {
        return b"''".to_vec();
    }
    // bash drops newline bytes entirely.
    let filtered: Vec<u8> = s.iter().cloned().filter(|&b| b != b'\n').collect();
    if filtered.is_empty() {
        return b"''".to_vec();
    }

    // A control byte (other than newline) triggers `$'...'` ANSI-C quoting.
    let needs_ansi = filtered
        .iter()
        .any(|&b| (b >= 0x01 && b <= 0x1f) || b == 0x7f);

    if needs_ansi {
        let mut r: Vec<u8> = Vec::new();
        r.push(b'$');
        r.push(b'\'');
        for &b in &filtered {
            match b {
                0x07 => r.extend_from_slice(b"\\a"),
                0x08 => r.extend_from_slice(b"\\b"),
                0x09 => r.extend_from_slice(b"\\t"),
                0x0b => r.extend_from_slice(b"\\v"),
                0x0c => r.extend_from_slice(b"\\f"),
                0x0d => r.extend_from_slice(b"\\r"),
                0x1b => r.extend_from_slice(b"\\E"),
                0x01..=0x1f | 0x7f => {
                    r.push(b'\\');
                    let o = format!("{:03o}", b);
                    r.extend_from_slice(o.as_bytes());
                }
                b'\\' => r.extend_from_slice(b"\\\\"),
                b'\'' => r.extend_from_slice(b"\\'"),
                _ => r.push(b),
            }
        }
        r.push(b'\'');
        r
    } else {
        let mut r: Vec<u8> = Vec::new();
        for &b in &filtered {
            if is_special_q(b) {
                r.push(b'\\');
                r.push(b);
            } else {
                r.push(b);
            }
        }
        r
    }
}

/// Whether `b` is a "special" character that `%q` backslash-escapes (everything
/// printable/ASCII except alnum and `% + - . / : = @ _`; high bytes are raw).
fn is_special_q(b: u8) -> bool {
    if b >= 0x80 {
        return false; // raw UTF-8
    }
    if b.is_ascii_alphanumeric() {
        return false;
    }
    !matches!(b, b'%' | b'+' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'_')
}

// Redirections (post-expansion)
// ---------------------------------------------------------------------------

enum ExpRedir {
    In(i32, String),
    Out(i32, String, bool),
    Both(String),
}

fn expand_file(file: &[WordPart], ctx: &ExpandCtx) -> String {
    let words = expand_word(file, ctx);
    words.into_iter().next().unwrap_or_default()
}

fn expand_redirs(redirs: &[Redir], ctx: &ExpandCtx) -> Vec<ExpRedir> {
    redirs
        .iter()
        .map(|r| match r {
            Redir::In { fd, file } => ExpRedir::In(*fd, expand_file(file, ctx)),
            Redir::Out { fd, file, append } => ExpRedir::Out(*fd, expand_file(file, ctx), *append),
            Redir::Both { file } => ExpRedir::Both(expand_file(file, ctx)),
        })
        .collect()
}

fn apply_exp_redirs(redirs: &[ExpRedir]) {
    for r in redirs {
        match r {
            ExpRedir::In(fd, file) => {
                let f = posix::open_read(file);
                posix::dup2(f, *fd);
                posix::close(f);
            }
            ExpRedir::Out(fd, file, append) => {
                let f = posix::open_write(file, *append);
                posix::dup2(f, *fd);
                posix::close(f);
            }
            ExpRedir::Both(file) => {
                let f = posix::open_write(file, false);
                posix::dup2(f, 1);
                posix::dup2(f, 2);
                posix::close(f);
            }
        }
    }
}

/// Honour output redirections for the parent-side `printf -v` path. Even though
/// `-v` writes nothing to stdout, bash still creates/truncates `> file` / `>> file`
/// targets. We open (and immediately close) those files so the side effect
/// matches, without redirecting the parent shell's own file descriptors.
fn touch_output_redirs(redirs: &[ExpRedir]) {
    for r in redirs {
        match r {
            ExpRedir::Out(_fd, file, append) => {
                let f = posix::open_write(file, *append);
                if f >= 0 {
                    posix::close(f);
                }
            }
            ExpRedir::Both(file) => {
                let f = posix::open_write(file, false);
                if f >= 0 {
                    posix::close(f);
                }
            }
            ExpRedir::In(..) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Command substitution capture
// ---------------------------------------------------------------------------

/// Apply a command's variable assignments on top of `base` to build its
/// (non-persistent) environment.
fn build_cmd_env(
    base: &HashMap<String, String>,
    assigns: &[(String, Vec<WordPart>, bool)],
    ctx: &ExpandCtx,
) -> HashMap<String, String> {
    let mut env = base.clone();
    for (name, valparts, append) in assigns {
        let val = expand_scalar(valparts, ctx);
        if *append {
            let cur = env.get(name).cloned().unwrap_or_default();
            env.insert(name.clone(), cur + &val);
        } else {
            env.insert(name.clone(), val);
        }
    }
    env
}

/// Run `cmd` in a forked subshell, capturing its stdout (with one trailing
/// newline stripped, matching bash).
pub fn capture_command(cmd: &str, env: &HashMap<String, String>) -> String {
    let tokens = match crate::lexer::tokenize(cmd) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let list = match crate::parser::parse_command_list(&tokens) {
        Ok(l) => l,
        Err(_) => return String::new(),
    };
    let fds = posix::pipe();
    let r = fds[0];
    let w = fds[1];
    let pid = posix::fork();
    if pid == 0 {
        posix::dup2(w, 1);
        posix::close(w);
        posix::close(r);
        let mut sh = Shell::from_env(env.clone());
        crate::executor::execute(&list, &mut sh);
        posix::exit(0);
    }
    posix::close(w);
    let s = posix::read_all(r);
    posix::close(r);
    posix::wait_pid(pid);
    if let Some(stripped) = s.strip_suffix('\n') {
        stripped.to_string()
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

fn child_run_exp(argv: &[String], path: &str, env_ptrs: &[*const c_char]) -> ! {
    if argv.is_empty() {
        posix::exit(0);
    }
    // Output builtins (echo/printf) run here, in the child, AFTER the caller
    // has applied redirections. This keeps `echo x > file` / `printf ... > file`
    // working. (State builtins cd/exit are handled in the parent instead.)
    if let Some(code) = run_output_builtin(argv) {
        let _ = std::io::stdout().flush();
        posix::exit(code);
    }
    // Defensive: state builtins should normally be handled in the parent, but
    // honour them here too.
    if let Some(code) = try_builtin(argv, None) {
        posix::exit(code);
    }
    let argvec = ArgVec::from(argv);
    match find_executable(&argv[0], path) {
        Some(exe) => {
            posix::execve(&exe, &argvec.ptrs, env_ptrs);
        }
        None => {
            eprintln!("wash: {}: command not found", argv[0]);
        }
    }
    posix::exit(127);
}

fn run_standalone_exp(
    argv: &[String],
    redirs: &[ExpRedir],
    assigns: &[(String, Vec<WordPart>, bool)],
    env_ptrs: &[*const c_char],
    sh: &mut Shell,
    path: &str,
) -> i32 {
    if argv.is_empty() {
        // Standalone assignment(s): persist to the shell environment.
        if !assigns.is_empty() {
            let ctx = ExpandCtx {
                env: &sh.env,
                positional: &sh.positional,
                last_status: sh.last_status,
                self_pid: sh.self_pid,
            };
            // Expand all RHS values first (immutable borrow of sh.env), then
            // mutate sh.env once the borrow is released.
            let applied: Vec<(String, String, bool)> = assigns
                .iter()
                .map(|(n, vp, ap)| (n.clone(), expand_scalar(vp, &ctx), *ap))
                .collect();
            for (name, val, append) in applied {
                if append {
                    let cur = sh.env.get(&name).cloned().unwrap_or_default();
                    sh.env.insert(name, cur + &val);
                } else {
                    sh.env.insert(name, val);
                }
            }
        }
        apply_exp_redirs(redirs);
        return 0;
    }
    // `printf -v var ...` mutates shell state, so it must execute in the PARENT
    // (here) rather than in the forked child, otherwise the assignment would be
    // lost. We render the format and store the result in `sh.env`.
    if argv.first().map(|s| s.as_str()) == Some("printf") {
        match parse_printf_opt(argv) {
            PrintfOpt::Assign { var, format_idx } => {
                if !is_valid_ident(&var) {
                    eprintln!("wash: printf: `{}': not a valid identifier", var);
                    return 2;
                }
                let format = match argv.get(format_idx) {
                    Some(f) => f.clone(),
                    None => {
                        eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
                        return 2;
                    }
                };
                let args = argv.get(format_idx + 1..).unwrap_or(&[]);
                let (val, conv_error) = printf_format(&format, args);
                sh.env.insert(var, String::from_utf8_lossy(&val).into_owned());
                // Honour output redirections (e.g. `> file`) so the file is
                // created/truncated exactly as bash does.
                touch_output_redirs(redirs);
                return if conv_error { 1 } else { 0 };
            }
            PrintfOpt::Usage => {
                eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
                return 2;
            }
            PrintfOpt::Normal { .. } => {}
        }
    }
    if let Some(code) = try_builtin(argv, Some(sh)) {
        return code;
    }
    let pid = posix::fork();
    if pid == 0 {
        apply_exp_redirs(redirs);
        child_run_exp(argv, path, env_ptrs);
    } else {
        posix::wait_pid(pid)
    }
}

/// A fully prepared (expanded) command: argv, redirections, the assignments
/// it carried, and its private environment pointer array.
struct Prepared {
    argv: Vec<String>,
    redirs: Vec<ExpRedir>,
    assigns: Vec<(String, Vec<WordPart>, bool)>,
    env_ptrs: Vec<*const c_char>,
}

fn prepare(p: &Pipeline, sh: &Shell) -> Vec<Prepared> {
    let ctx = ExpandCtx {
        env: &sh.env,
        positional: &sh.positional,
        last_status: sh.last_status,
        self_pid: sh.self_pid,
    };
    p.commands
        .iter()
        .map(|c| {
            let argv = expand_argv(&c.argv, &ctx);
            let redirs = expand_redirs(&c.redirs, &ctx);
            let cmd_env = build_cmd_env(&sh.env, &c.assignments, &ctx);
            let (_ec, env_ptrs) = build_env_ptrs(&cmd_env);
            Prepared {
                argv,
                redirs,
                assigns: c.assignments.clone(),
                env_ptrs,
            }
        })
        .collect()
}

fn execute_pipeline(p: &Pipeline, sh: &mut Shell, path: &str) -> i32 {
    let prepared = prepare(p, sh);
    let n = prepared.len();
    if n == 1 {
        let pr = &prepared[0];
        return run_standalone_exp(
            &pr.argv,
            &pr.redirs,
            &pr.assigns,
            &pr.env_ptrs,
            sh,
            path,
        );
    }

    let mut prev_read: Option<i32> = None;
    let mut pids: Vec<i32> = Vec::new();

    for (idx, pr) in prepared.iter().enumerate() {
        let make_pipe = idx < n - 1;
        let (this_read, this_write) = if make_pipe {
            let fds = posix::pipe();
            (Some(fds[0]), Some(fds[1]))
        } else {
            (None, None)
        };

        let pid = posix::fork();
        if pid == 0 {
            if let Some(fd) = prev_read {
                posix::dup2(fd, 0);
                posix::close(fd);
            }
            if let Some(fd) = this_write {
                posix::dup2(fd, 1);
                posix::close(fd);
            }
            if let Some(fd) = this_read {
                posix::close(fd);
            }
            apply_exp_redirs(&pr.redirs);
            if pr.argv.is_empty() {
                posix::exit(0);
            }
            child_run_exp(&pr.argv, path, &pr.env_ptrs);
        } else {
            if let Some(fd) = prev_read {
                posix::close(fd);
            }
            if let Some(fd) = this_write {
                posix::close(fd);
            }
            if let Some(fd) = this_read {
                prev_read = Some(fd);
            }
            pids.push(pid);
        }
    }

    if let Some(fd) = prev_read {
        posix::close(fd);
    }

    let mut last = 0;
    for pid in pids {
        last = posix::wait_pid(pid);
    }
    last
}

pub fn execute(list: &CommandList, sh: &mut Shell) -> i32 {
    let path = sh.env.get("PATH").cloned().unwrap_or_default();

    let mut last = 0;
    for p in &list.pipelines {
        last = execute_pipeline(p, sh, &path);
        // Propagate exit status so `$?` reflects the previous command.
        sh.last_status = last;
    }
    last
}
