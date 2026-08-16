// xvalue_uint.rs — 对齐 xvalue_uint.go

use crate::r#const::*;
use crate::xvalue::{tlv_encode, XValue};

pub fn new_uint8(v: &[u8]) -> XValue {
    XValue::Uint8(v.to_vec())
}
pub fn new_uint16(v: &[u16]) -> XValue {
    XValue::Uint16(v.to_vec())
}
pub fn new_uint32(v: &[u32]) -> XValue {
    XValue::Uint32(v.to_vec())
}
pub fn new_uint64(v: &[u64]) -> XValue {
    XValue::Uint64(v.to_vec())
}

pub fn decode_uint8(body: &[u8]) -> Vec<u8> {
    body.to_vec()
}
pub fn decode_uint16(body: &[u8]) -> Vec<u16> {
    body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}
pub fn decode_uint32(body: &[u8]) -> Vec<u32> {
    body.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}
pub fn decode_uint64(body: &[u8]) -> Vec<u64> {
    body.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
}

pub fn encode_uint8(data: &[u8]) -> Vec<u8> {
    tlv_encode(KIND_UINT8, data, data.len() as i32)
}
pub fn encode_uint16(data: &[u16]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    tlv_encode(KIND_UINT16, &raw, data.len() as i32)
}
pub fn encode_uint32(data: &[u32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    tlv_encode(KIND_UINT32, &raw, data.len() as i32)
}
pub fn encode_uint64(data: &[u64]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    tlv_encode(KIND_UINT64, &raw, data.len() as i32)
}
