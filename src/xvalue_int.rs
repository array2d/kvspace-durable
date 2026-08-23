// xvalue_int.rs — 对齐 xvalue_int.go

use crate::r#const::*;
use crate::xvalue::{dims_from_len, encode_head, Arr, XValue};

pub fn new_int8(v: &[i8]) -> XValue {
    XValue::Int8(Arr {
        data: v.to_vec(),
        dims: dims_from_len(v.len()),
    })
}
pub fn new_int16(v: &[i16]) -> XValue {
    XValue::Int16(Arr {
        data: v.to_vec(),
        dims: dims_from_len(v.len()),
    })
}
pub fn new_int32(v: &[i32]) -> XValue {
    XValue::Int32(Arr {
        data: v.to_vec(),
        dims: dims_from_len(v.len()),
    })
}
pub fn new_int64(v: &[i64]) -> XValue {
    XValue::Int64(Arr {
        data: v.to_vec(),
        dims: dims_from_len(v.len()),
    })
}

pub fn decode_int8(body: &[u8], dims: &[i32]) -> Arr<i8> {
    Arr {
        data: body.iter().map(|&b| b as i8).collect(),
        dims: dims.to_vec(),
    }
}
pub fn decode_int16(body: &[u8], dims: &[i32]) -> Arr<i16> {
    Arr {
        data: body
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect(),
        dims: dims.to_vec(),
    }
}
pub fn decode_int32(body: &[u8], dims: &[i32]) -> Arr<i32> {
    Arr {
        data: body
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        dims: dims.to_vec(),
    }
}
pub fn decode_int64(body: &[u8], dims: &[i32]) -> Arr<i64> {
    Arr {
        data: body
            .chunks_exact(8)
            .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
            .collect(),
        dims: dims.to_vec(),
    }
}

pub fn encode_int8(data: &[i8], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().map(|&v| v as u8).collect();
    encode_head(KIND_INT8, 0, dims, &raw)
}
pub fn encode_int16(data: &[i16], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data
        .iter()
        .flat_map(|&v| (v as u16).to_le_bytes())
        .collect();
    encode_head(KIND_INT16, 0, dims, &raw)
}
pub fn encode_int32(data: &[i32], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data
        .iter()
        .flat_map(|&v| (v as u32).to_le_bytes())
        .collect();
    encode_head(KIND_INT32, 0, dims, &raw)
}
pub fn encode_int64(data: &[i64], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data
        .iter()
        .flat_map(|&v| (v as u64).to_le_bytes())
        .collect();
    encode_head(KIND_INT64, 0, dims, &raw)
}
