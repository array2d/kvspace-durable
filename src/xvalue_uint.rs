// xvalue_uint.rs — 对齐 xvalue_uint.go

use crate::r#const::*;
use crate::xvalue::{dims_from_len, encode_head, Arr, XValue};

pub fn new_uint8(v: &[u8]) -> XValue {
    XValue::Uint8(Arr { data: v.to_vec(), dims: dims_from_len(v.len()) })
}
pub fn new_uint16(v: &[u16]) -> XValue {
    XValue::Uint16(Arr { data: v.to_vec(), dims: dims_from_len(v.len()) })
}
pub fn new_uint32(v: &[u32]) -> XValue {
    XValue::Uint32(Arr { data: v.to_vec(), dims: dims_from_len(v.len()) })
}
pub fn new_uint64(v: &[u64]) -> XValue {
    XValue::Uint64(Arr { data: v.to_vec(), dims: dims_from_len(v.len()) })
}

pub fn decode_uint8(body: &[u8], dims: &[i32]) -> Arr<u8> {
    Arr { data: body.to_vec(), dims: dims.to_vec() }
}
pub fn decode_uint16(body: &[u8], dims: &[i32]) -> Arr<u16> {
    Arr { data: body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect(), dims: dims.to_vec() }
}
pub fn decode_uint32(body: &[u8], dims: &[i32]) -> Arr<u32> {
    Arr { data: body.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(), dims: dims.to_vec() }
}
pub fn decode_uint64(body: &[u8], dims: &[i32]) -> Arr<u64> {
    Arr { data: body.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect(), dims: dims.to_vec() }
}

pub fn encode_uint8(data: &[u8], dims: &[i32]) -> Vec<u8> {
    encode_head(KIND_UINT8, 0, dims, data)
}
pub fn encode_uint16(data: &[u16], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    encode_head(KIND_UINT16, 0, dims, &raw)
}
pub fn encode_uint32(data: &[u32], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    encode_head(KIND_UINT32, 0, dims, &raw)
}
pub fn encode_uint64(data: &[u64], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    encode_head(KIND_UINT64, 0, dims, &raw)
}
