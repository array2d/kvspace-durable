// xvalue_byte.rs — 对齐 xvalue_byte.go（char/utf8、char/ascii、char/utf32）

use crate::r#const::*;
use crate::xvalue::{to_wire_head, Arr, XValue};

/// char/* 恒一维序列（含空串/单字符），dims = [len]。
fn char_dims(n: usize) -> Vec<i32> {
    vec![n as i32]
}

pub fn new_char_byte(v: &[u8]) -> XValue {
    XValue::CharByte(Arr {
        data: v.to_vec(),
        dims: char_dims(v.len()),
    })
}
pub fn char_byte_from_wire(body: &[u8], dims: &[i32]) -> Arr<u8> {
    Arr {
        data: body.to_vec(),
        dims: dims.to_vec(),
    }
}
pub fn char_ascii_from_wire(body: &[u8], dims: &[i32]) -> Arr<u8> {
    Arr {
        data: body.to_vec(),
        dims: dims.to_vec(),
    }
}
pub fn char32_from_wire(body: &[u8], dims: &[i32]) -> Arr<u32> {
    Arr {
        data: body
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        dims: dims.to_vec(),
    }
}

pub fn char_byte_to_wire(data: &[u8], dims: &[i32]) -> Vec<u8> {
    to_wire_head(KIND_CHAR_UTF8, 0, dims, data)
}
pub fn char_ascii_to_wire(data: &[u8], dims: &[i32]) -> Vec<u8> {
    to_wire_head(KIND_CHAR_ASCII, 0, dims, data)
}
pub fn char32_to_wire(data: &[u32], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    to_wire_head(KIND_CHAR, 0, dims, &raw)
}
