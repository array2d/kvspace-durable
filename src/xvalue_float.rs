// xvalue_float.rs — 对齐 xvalue_float.go

use crate::r#const::*;
use crate::xvalue::{tlv_encode, XValue};

pub fn new_float32(v: &[f32]) -> XValue {
    XValue::Float32(v.to_vec())
}
pub fn new_float64(v: &[f64]) -> XValue {
    XValue::Float64(v.to_vec())
}

pub fn decode_float32(body: &[u8]) -> Vec<f32> {
    body.chunks_exact(4).map(|c| f32::from_bits(u32::from_le_bytes([c[0], c[1], c[2], c[3]]))).collect()
}
pub fn decode_float64(body: &[u8]) -> Vec<f64> {
    body.chunks_exact(8).map(|c| f64::from_bits(u64::from_le_bytes(c.try_into().unwrap()))).collect()
}

pub fn encode_float32(data: &[f32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_bits().to_le_bytes()).collect();
    tlv_encode(KIND_FLOAT32, &raw, data.len() as i32)
}
pub fn encode_float64(data: &[f64]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_bits().to_le_bytes()).collect();
    tlv_encode(KIND_FLOAT64, &raw, data.len() as i32)
}
