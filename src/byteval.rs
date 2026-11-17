//! Shell 变量值的无损字节表示。
//!
//! bash 在 `LC_ALL=C` 下的变量是裸字节序列（典型来源：`read -n` 在多字节
//! 字符中间截断）。Rust 的 `String` 必须是合法 UTF-8，直接塞入裸字节
//! （`String::from_utf8_unchecked`）是 UB。这里用一个**可逆**的合法 UTF-8
//! 编码：ASCII 原样，高位字节 `0x80..=0xFF` 映射到转义序列
//! `U+E100` + `U+E000..=U+E07F`。
//!
//! 为什么要转义哨兵 `U+E100`：如果只用一个 PUA 字符表示一个字节，源码里
//! 的字面 PUA 字符（例如 `$'\ue000'`）会在 egress 被误当成编码字节。加哨兵
//! 后，`decode_raw` 只在「哨兵 + PUA」成对出现时还原字节，落单的 PUA 字符
//! 一律按自身 UTF-8 字节输出，字面 PUA 不再失真。
//!
//! **纪律**：任何按字节解释变量值的操作（长度、切片、模式替换、审计输出）
//! 必须先 `decode_raw`；任何把字节流入变量的路径必须先 `encode_raw`。

const PUA_BASE: u32 = 0xE000;
const ESC: u32 = 0xE100;

fn is_encoded_pua(cp: u32) -> bool {
    (PUA_BASE..PUA_BASE + 0x80).contains(&cp)
}

/// 把裸字节编码成可安全存入 `String` 的合法 UTF-8 文本。
pub fn encode_raw(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            out.push(b as char);
        } else {
            out.push(char::from_u32(ESC).unwrap());
            out.push(char::from_u32(PUA_BASE + (b - 0x80) as u32).unwrap());
        }
    }
    out
}

/// 把编码文本还原成原始字节；落单的 PUA / 其他字符按其 UTF-8 编码写出。
pub fn decode_raw(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(ch) = it.next() {
        if ch as u32 == ESC {
            if let Some(&next) = it.peek() {
                if is_encoded_pua(next as u32) {
                    it.next();
                    out.push((next as u32 - PUA_BASE) as u8 + 0x80);
                    continue;
                }
            }
        }
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
    out
}

/// 解码后的字节长度（不分配）。等价于 `decode_raw(s).len()`。
pub fn decoded_len(s: &str) -> usize {
    let mut n = 0;
    let mut it = s.chars().peekable();
    while let Some(ch) = it.next() {
        if ch as u32 == ESC {
            if let Some(&next) = it.peek() {
                if is_encoded_pua(next as u32) {
                    it.next();
                    n += 1;
                    continue;
                }
            }
        }
        n += ch.len_utf8();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_bytes() {
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode_raw(&encode_raw(&all)), all);
        assert_eq!(decoded_len(&encode_raw(&all)), all.len());
    }

    #[test]
    fn valid_utf8_passthrough() {
        // 未编码的合法 UTF-8（含落单 PUA）在 decode 侧原样输出。
        for s in ["中文abc", "\u{e000}", "é"] {
            assert_eq!(decode_raw(s), s.as_bytes());
            assert_eq!(decoded_len(s), s.len());
        }
    }

    #[test]
    fn truncated_utf8_roundtrip() {
        let bytes = [0xe4, 0xb8, 0xad, 0xe6];
        assert_eq!(decode_raw(&encode_raw(&bytes)), bytes);
        assert_eq!(decoded_len(&encode_raw(&bytes)), bytes.len());
    }

    #[test]
    fn concatenated_encoded_values() {
        // 拼接：ESC+PUA 对跨边界仍成立。
        let a = encode_raw(&[0xff]);
        let b = encode_raw(b"x\x80");
        let joined = format!("{}{}", a, b);
        assert_eq!(decode_raw(&joined), vec![0xff, b'x', 0x80]);
    }
}
