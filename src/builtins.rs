// Bash-compatible builtin commands.
//
// Every builtin implementation lives in this module. `run_builtin` is the
// single dispatch entry point used by the executor:
//
//   * `Some(code)` — the command was a builtin and has already run; `code` is
//     its exit status.
//   * `None`      — `argv[0]` is not a builtin, so the caller must look it up
//     in `$PATH` and run it as an external program.
//
// The behaviour of every builtin follows GNU bash 5.3 (the reference shell
// used by the project's byte-diff test suite): same output bytes, same exit
// statuses and the same side effects on the shell state.
//
// State-changing builtins (`cd`, `export`, `set`, `alias`, ...) must run in
// the *parent* shell, otherwise their side effects would be lost. The
// executor therefore calls `run_builtin` before forking; the forked child
// only uses it for the stateless output builtins.

use std::io::Write;

use crate::posix;
use crate::shell::Shell;

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Every builtin name understood by `run_builtin`.
const BUILTINS: &[&str] = &["echo", "printf"];

/// True when `name` is the name of a builtin command.
pub fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(&name)
}

/// Run a builtin command in the given shell.
///
/// See the module comment for the `Option` contract.
pub fn run_builtin(argv: &[String], sh: &mut Shell) -> Option<i32> {
    let name = argv.first()?.as_str();
    let code = match name {
        "echo" => builtin_echo(argv),
        "printf" => builtin_printf(argv, Some(sh)),
        _ => return None,
    };
    Some(code)
}

// ---------------------------------------------------------------------------
// echo
// ---------------------------------------------------------------------------

/// The `echo` builtin.
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

// ---------------------------------------------------------------------------
// printf
// ---------------------------------------------------------------------------

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

/// The `printf` builtin.
///
/// `-v var` assigns the rendered output to the shell variable `var`, which
/// requires a shell: in a subshell the assignment is discarded, exactly like
/// bash.
fn builtin_printf(argv: &[String], sh: Option<&mut Shell>) -> i32 {
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
            let (value, conv_error) = printf_format(&format, args);
            if let Some(sh) = sh {
                sh.env.insert(var, String::from_utf8_lossy(&value).into_owned());
            }
            if conv_error {
                1
            } else {
                0
            }
        }
        PrintfOpt::Usage => {
            eprintln!("wash: printf: usage: printf [-v var] format [arguments]");
            2
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

// ---------------------------------------------------------------------------
// Quoting helpers shared by `export -p`, `alias` and `set`
// ---------------------------------------------------------------------------

/// True when the byte needs no quoting at all (bash's "safe" character set).
fn is_safe_shell_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'%' | b'+' | b'-' | b'.' | b'/' | b':' | b'=' | b'@' | b'_')
}

/// True when the value contains characters that force bash's `$'...'` form.
fn needs_ansi_quote(value: &str) -> bool {
    value
        .bytes()
        .any(|b| b < 0x20 || b == 0x7f || b >= 0x80)
}

/// Quote a value the way bash's `export -p` does: `"..."` with the usual
/// backslash escapes, or `$'...'` when the value holds control characters.
fn quote_double(value: &str) -> String {
    if needs_ansi_quote(value) {
        return ansi_quote(value);
    }
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '$' => out.push_str("\\$"),
            '`' => out.push_str("\\`"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Quote a value the way bash's `alias` does: `'...'` with embedded quotes
/// escaped as `'\''`.
fn quote_single(value: &str) -> String {
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Quote a value the way bash's `set` (without arguments) does: raw when the
/// value only contains safe characters, `$'...'` for control characters and
/// `'...'` otherwise. An empty value is printed as nothing at all.
fn quote_for_set(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if needs_ansi_quote(value) {
        return ansi_quote(value);
    }
    if value.bytes().all(is_safe_shell_char) {
        return value.to_string();
    }
    quote_single(value)
}

/// Bash's ANSI-C quoting (`$'...'`).
fn ansi_quote(value: &str) -> String {
    let mut out = String::from("$'");
    for &b in value.as_bytes() {
        match b {
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            0x09 => out.push_str("\\t"),
            0x0a => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            0x0d => out.push_str("\\r"),
            0x1b => out.push_str("\\E"),
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\{:03o}", b)),
        }
    }
    out.push('\'');
    out
}

// ---------------------------------------------------------------------------
// Helpers shared with the executor
// ---------------------------------------------------------------------------

/// Quote a word the way bash's `set -x` trace does.
pub fn quote_for_trace(word: &str) -> String {
    if word.is_empty() {
        return "''".to_string();
    }
    if needs_ansi_quote(word) {
        return ansi_quote(word);
    }
    if word.bytes().all(is_safe_shell_char) {
        return word.to_string();
    }
    quote_single(word)
}
