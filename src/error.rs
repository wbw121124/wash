// Unified error type for wash.
//
// Phase 0 keeps it simple: a human-readable message plus an exit code. As we
// approach bash compatibility, the message format will be aligned with bash
// ("bash: line N: ...") so our oracle tests stay comparable.

use std::fmt;

/// Structured classification of a parse/lex diagnostic, used by the shell to
/// pick the correct line-number rule without re-parsing the message text.
///
/// bash reports several distinct diagnostics whose *message strings* can be
/// byte-identical (e.g. an unclosed `$(` and an unclosed `$((` both say
/// "unexpected EOF while looking for matching `)'"), yet they require
/// different line numbers.  The family disambiguates them so the shell no
/// longer has to guess the rule from the message prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticFamily {
    /// Default: the shell falls back to the message-prefix heuristic, keeping
    /// every existing parser/executor error behaving exactly as before.
    Unspecified,
    /// Unclosed `$(` in command position: line number is `n_lines + 1`
    /// (the virtual line just past end of input).
    CmdsubEof,
    /// Unclosed other delimiter (backtick / `${` / `$((` / a quote opened in
    /// a quoting context): line number is the 1-based line where the
    /// delimiter *started* (`open_line`).
    OtherDelimiterEof,
    /// `syntax error near unexpected token ...`: line number is the real
    /// 1-based line of the offending command plus any here-document body
    /// lines, and that line is echoed back.
    ///
    /// Round 6 defines and handles this family but no producer constructs it
    /// yet: near-token diagnostics still flow through `Unspecified`, whose
    /// message-prefix heuristic is byte-identical to bash. Round 7 will make
    /// the parser tag near-token errors with this family (compound-command
    /// diagnostics). Allowed dead code so the scaffold stays warning-free.
    #[allow(dead_code)]
    NearToken,
}

#[derive(Debug, Clone)]
pub struct WashError {
    pub message: String,
    #[allow(dead_code)]
    pub exit_code: i32,
    /// Structured diagnostic family; drives the shell's line-number rule.
    pub family: DiagnosticFamily,
    /// For `OtherDelimiterEof`: the 1-based line where the unclosed delimiter
    /// started.  Ignored by other families.
    pub open_line: usize,
}

pub type Result<T> = std::result::Result<T, WashError>;

impl WashError {
    pub fn new(message: impl Into<String>, exit_code: i32) -> Self {
        WashError {
            message: message.into(),
            exit_code,
            family: DiagnosticFamily::Unspecified,
            open_line: 0,
        }
    }

    pub fn syntax(message: impl Into<String>) -> Self {
        WashError::new(message, 2)
    }

    /// Build a syntax error that carries a structured `family` and the
    /// 1-based `open_line` of the unclosed delimiter, so the shell can compute
    /// the correct line number without guessing from the message text.
    pub fn syntax_unclosed(
        family: DiagnosticFamily,
        open_line: usize,
        message: impl Into<String>,
    ) -> Self {
        WashError {
            message: message.into(),
            exit_code: 2,
            family,
            open_line,
        }
    }
}

impl fmt::Display for WashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WashError {}
