// xvalue_obj.rs — 对齐 xvalue_obj.go（Obj{}，空 obj）+ map 构造

use crate::xvalue::XValue;

pub fn new_obj() -> XValue {
    XValue::Obj(Vec::new())
}

/// 同构 map：key 恒 char 字符串，value 固定 kind；空 map 无子键。
pub fn new_map() -> XValue {
    XValue::Map(Vec::new())
}
