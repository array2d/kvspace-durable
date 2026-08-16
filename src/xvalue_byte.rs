// xvalue_byte.rs — 对齐 xvalue_byte.go（char/utf8、char/ascii、char/utf32）

use crate::r#const::*;
use crate::xvalue::{tlv_encode, XValue};

pub fn new_char_byte(v: &[u8]) -> XValue {
    XValue::CharByte(v.to_vec())
}
pub fn new_char_ascii(v: &[u8]) -> XValue {
    XValue::CharAscii(v.to_vec())
}
pub fn new_char32(v: &[u32]) -> XValue {
    XValue::Char32(v.to_vec())
}

/// NewChar 根据 kind 从字符串构造字符值（char/utf32 默认，char/utf8/ascii 为字节）。
pub fn new_char(kind: &str, s: &str) -> XValue {
    match kind {
        KIND_CHAR_UTF8 => XValue::CharByte(s.as_bytes().to_vec()),
        KIND_CHAR_ASCII => XValue::CharAscii(s.as_bytes().to_vec()),
        _ => XValue::Char32(s.chars().map(|c| c as u32).collect()),
    }
}

pub fn decode_char_byte(body: &[u8]) -> Vec<u8> {
    body.to_vec()
}
pub fn decode_char_ascii(body: &[u8]) -> Vec<u8> {
    body.to_vec()
}
pub fn decode_char32(body: &[u8]) -> Vec<u32> {
    body.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn encode_char_byte(data: &[u8]) -> Vec<u8> {
    tlv_encode(KIND_CHAR_UTF8, data, data.len() as i32)
}
pub fn encode_char_ascii(data: &[u8]) -> Vec<u8> {
    tlv_encode(KIND_CHAR_ASCII, data, data.len() as i32)
}
pub fn encode_char32(data: &[u32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    tlv_encode(KIND_CHAR, &raw, data.len() as i32)
}

/// IsCharKind 判断 kind 是否为字符家族（前缀 char/）。
pub fn is_char_kind(kind: &str) -> bool {
    kind.starts_with("char/")
}
