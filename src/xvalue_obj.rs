// xvalue_obj.rs — 对齐 xvalue_obj.go（Obj{}，空 obj）+ map 构造

use crate::xvalue::XValue;

/// 空的散 key ndarray：给定 dims（恒 ndim≥1），无成员。
pub fn new_map(dims: &[i32]) -> XValue {
    crate::xvalue_index::new_map_index(dims)
}
