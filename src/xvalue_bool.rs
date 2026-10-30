// xvalue_bool.rs — 对齐 xvalue_bool.go

use crate::r#const::*;
use crate::xvalue::{dims_from_len, to_wire_head, Arr, XValue};

pub fn new_bool(v: &[bool]) -> XValue {
    XValue::Bool(Arr {
        data: v.to_vec(),
        dims: dims_from_len(v.len()),
    })
}

pub fn bool_from_wire(body: &[u8], dims: &[i32]) -> Arr<bool> {
    Arr {
        data: body.iter().map(|&b| b != 0).collect(),
        dims: dims.to_vec(),
    }
}

pub fn bool_to_wire(data: &[bool], dims: &[i32]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().map(|&b| if b { 1 } else { 0 }).collect();
    to_wire_head(KIND_BOOL, 0, dims, &raw)
}
