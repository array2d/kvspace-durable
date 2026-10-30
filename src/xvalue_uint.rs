// xvalue_uint.rs — 对齐 xvalue_uint.go

use crate::r#const::*;
use crate::xvalue::{to_wire_head, Arr};

pub fn uint8_from_wire(body: &[u8], dims: &[i32]) -> Arr<u8> {
    Arr {
        data: body.to_vec(),
        dims: dims.to_vec(),
    }
}
pub fn uint16_from_wire(body: &[u8], dims: &[i32]) -> Arr<u16> {
    Arr {
        data: body
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect(),
        dims: dims.to_vec(),
    }
}
pub fn uint32_from_wire(body: &[u8], dims: &[i32]) -> Arr<u32> {
    Arr {
        data: body
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        dims: dims.to_vec(),
    }
}
pub fn uint64_from_wire(body: &[u8], dims: &[i32]) -> Arr<u64> {
    Arr {
        data: body
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect(),
        dims: dims.to_vec(),
    }
}

pub fn uint8_to_wire(data: &[u8], dims: &[i32]) -> Vec<u8> {
    to_wire_head(KIND_UINT8, 0, dims, data)
}
pub fn uint16_to_wire(data: &[u16], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    to_wire_head(KIND_UINT16, 0, dims, &raw)
}
pub fn uint32_to_wire(data: &[u32], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    to_wire_head(KIND_UINT32, 0, dims, &raw)
}
pub fn uint64_to_wire(data: &[u64], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| v.to_le_bytes()).collect();
    to_wire_head(KIND_UINT64, 0, dims, &raw)
}
