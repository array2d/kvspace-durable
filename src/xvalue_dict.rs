// xvalue_dict.rs — 对齐 xvalue_dict.go（Dict{}，空 dict）

use crate::xvalue::XValue;

pub fn new_dict() -> XValue {
    XValue::Dict(Vec::new())
}
