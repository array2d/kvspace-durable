// xvalue_byte.rs — 对齐 xvalue_byte.go（char/utf8、char/ascii、char/utf32）

use crate::r#const::*;
use crate::xvalue::{encode_head, Arr, XValue};

/// char/* 恒一维序列（含空串/单字符），dims = [len]。
fn char_dims(n: usize) -> Vec<i32> {
    vec![n as i32]
}

pub fn new_char_byte(v: &[u8]) -> XValue {
    XValue::CharByte(Arr { data: v.to_vec(), dims: char_dims(v.len()) })
}
pub fn new_char_ascii(v: &[u8]) -> XValue {
    XValue::CharAscii(Arr { data: v.to_vec(), dims: char_dims(v.len()) })
}
pub fn new_char32(v: &[u32]) -> XValue {
    XValue::Char32(Arr { data: v.to_vec(), dims: char_dims(v.len()) })
}

/// NewChar 根据 kind 从字符串构造字符值（char/utf32 默认，char/utf8/ascii 为字节）。
pub fn new_char(kind: &str, s: &str) -> XValue {
    match kind {
        KIND_CHAR_UTF8 => XValue::CharByte(Arr { data: s.as_bytes().to_vec(), dims: char_dims(s.len()) }),
        KIND_CHAR_ASCII => XValue::CharAscii(Arr { data: s.as_bytes().to_vec(), dims: char_dims(s.len()) }),
        _ => {
            let v: Vec<u32> = s.chars().map(|c| c as u32).collect();
            XValue::Char32(Arr { data: v.clone(), dims: char_dims(v.len()) })
        }
    }
}

pub fn decode_char_byte(body: &[u8], dims: &[i32]) -> Arr<u8> {
    Arr { data: body.to_vec(), dims: dims.to_vec() }
}
pub fn decode_char_ascii(body: &[u8], dims: &[i32]) -> Arr<u8> {
    Arr { data: body.to_vec(), dims: dims.to_vec() }
}
pub fn decode_char32(body: &[u8], dims: &[i32]) -> Arr<u32> {
    Arr { data: body.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(), dims: dims.to_vec() }
}

pub fn encode_char_byte(data: &[u8], dims: &[i32]) -> Vec<u8> {
    encode_head(KIND_CHAR_UTF8, 0, dims, data)
}
pub fn encode_char_ascii(data: &[u8], dims: &[i32]) -> Vec<u8> {
    encode_head(KIND_CHAR_ASCII, 0, dims, data)
}
pub fn encode_char32(data: &[u32], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    encode_head(KIND_CHAR, 0, dims, &raw)
}

/// IsCharKind 判断 kind 是否为字符家族（前缀 char/）。
pub fn is_char_kind(kind: &str) -> bool {
    kind.starts_with("char/")
}
