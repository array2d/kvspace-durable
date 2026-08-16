// xvalue_int.rs — 对齐 xvalue_int.go

use crate::r#const::*;
use crate::xvalue::{tlv_encode, XValue};

pub fn new_int8(v: &[i8]) -> XValue {
    XValue::Int8(v.to_vec())
}
pub fn new_int16(v: &[i16]) -> XValue {
    XValue::Int16(v.to_vec())
}
pub fn new_int32(v: &[i32]) -> XValue {
    XValue::Int32(v.to_vec())
}
pub fn new_int64(v: &[i64]) -> XValue {
    XValue::Int64(v.to_vec())
}

pub fn decode_int8(body: &[u8]) -> Vec<i8> {
    body.iter().map(|&b| b as i8).collect()
}
pub fn decode_int16(body: &[u8]) -> Vec<i16> {
    body.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}
pub fn decode_int32(body: &[u8]) -> Vec<i32> {
    body.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}
pub fn decode_int64(body: &[u8]) -> Vec<i64> {
    body.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect()
}

pub fn encode_int8(data: &[i8]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().map(|&v| v as u8).collect();
    tlv_encode(KIND_INT8, &raw, data.len() as i32)
}
pub fn encode_int16(data: &[i16]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| (v as u16).to_le_bytes()).collect();
    tlv_encode(KIND_INT16, &raw, data.len() as i32)
}
pub fn encode_int32(data: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| (v as u32).to_le_bytes()).collect();
    tlv_encode(KIND_INT32, &raw, data.len() as i32)
}
pub fn encode_int64(data: &[i64]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| (v as u64).to_le_bytes()).collect();
    tlv_encode(KIND_INT64, &raw, data.len() as i32)
}
