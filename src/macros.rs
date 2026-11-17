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

/// 按裸字节写 stderr（写失败忽略，绝不 panic）。
///
/// bash 的部分内建（`source` / `type` 等）把参数按 `%s` 原样写诊断，
/// 可能包含任意字节；byteval 编码串必须先 `decode_raw` 再交给本宏，
/// 不能用 `safe_eprintln!`（它只能写合法 UTF-8 的 `String`）。
#[macro_export]
macro_rules! safe_eprint_bytes {
    ($($part:expr),+ $(,)?) => {{
        use std::io::Write as _;
        let mut _buf: Vec<u8> = Vec::new();
        $( _buf.extend_from_slice(AsRef::<[u8]>::as_ref(&$part)); )+
        let _ = std::io::stderr().write_all(&_buf);
    }};
}
