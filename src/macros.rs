//! 非 panic 的诊断输出宏。
//!
//! Rust 标准库的 `eprintln!` 在 stderr 写失败时直接 panic（rc=101）。bash
//! 在 stderr 不可写（例如 `cmd > /dev/full 2>&1`）时只是静默放弃诊断，
//! 仍按命令自身的语义退出。为保持逐字节/rc 兼容，所有面向用户的诊断
//! 都走本宏：写失败一律忽略，绝不 panic。

#[macro_export]
macro_rules! safe_eprintln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}
