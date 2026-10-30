// xvalue_float.rs — 对齐 xvalue_float.go

use crate::r#const::*;
use crate::xvalue::{dims_from_len, to_wire_head, Arr, XValue};

pub fn new_float32(v: &[f32]) -> XValue {
    XValue::Float32(Arr {
        data: v.to_vec(),
        dims: dims_from_len(v.len()),
    })
}
pub fn new_float64(v: &[f64]) -> XValue {
    XValue::Float64(Arr {
        data: v.to_vec(),
        dims: dims_from_len(v.len()),
    })
}

pub fn float32_from_wire(body: &[u8], dims: &[i32]) -> Arr<f32> {
    Arr {
        data: body
            .chunks_exact(4)
            .map(|c| f32::from_bits(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
            .collect(),
        dims: dims.to_vec(),
    }
}
pub fn float64_from_wire(body: &[u8], dims: &[i32]) -> Arr<f64> {
    Arr {
        data: body
            .chunks_exact(8)
            .map(|c| f64::from_bits(u64::from_le_bytes(c.try_into().unwrap())))
            .collect(),
        dims: dims.to_vec(),
    }
}

pub fn float32_to_wire(data: &[f32], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data
        .iter()
        .flat_map(|&v| v.to_bits().to_le_bytes())
        .collect();
    to_wire_head(KIND_FLOAT32, 0, dims, &raw)
}
pub fn float64_to_wire(data: &[f64], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data
        .iter()
        .flat_map(|&v| v.to_bits().to_le_bytes())
        .collect();
    to_wire_head(KIND_FLOAT64, 0, dims, &raw)
}
