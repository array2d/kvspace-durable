// xvalue_bool.rs — 对齐 xvalue_bool.go

use crate::r#const::*;
use crate::xvalue::{tlv_encode, XValue};

pub fn new_bool(v: &[bool]) -> XValue {
    XValue::Bool(v.to_vec())
}

pub fn decode_bool(body: &[u8]) -> Vec<bool> {
    body.iter().map(|&b| b != 0).collect()
}

pub fn encode_bool(data: &[bool]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().map(|&b| if b { 1 } else { 0 }).collect();
    tlv_encode(KIND_BOOL, &raw, data.len() as i32)
}
