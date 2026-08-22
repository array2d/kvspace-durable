// xvalue_index.rs — 对齐 xvalue_index.go（Index、ObjIndex、ExtIndex）

use crate::r#const::*;
use crate::xvalue::{ExtIndex, XValue};

pub fn new_index(children: &[String]) -> XValue {
    XValue::Index(children.to_vec())
}
pub fn new_obj_index(children: &[String]) -> XValue {
    XValue::Obj(children.to_vec())
}
pub fn new_map_index(children: &[String]) -> XValue {
    XValue::Map(children.to_vec())
}
pub fn new_ext_index(children: &[String], ext_path: &str) -> XValue {
    XValue::ExtIndex(ExtIndex {
        childs: children.to_vec(),
        ext_path: ext_path.to_string(),
    })
}

/// index/objindex/strkeymapindex 三类 index body 一律前缀 [4B count LE]（成员数），
/// 后接成员名列表（INDEX_VALUE_SEP 连接）。count 使成员数 O(1) 可取，不再靠 split 现数。
pub fn encode_index_raw(children: &[String]) -> Vec<u8> {
    let mut buf = (children.len() as u32).to_le_bytes().to_vec();
    buf.extend(children.join(INDEX_VALUE_SEP).into_bytes());
    buf
}

/// 跳过 body 前 [4B count LE] 前缀，返回成员名段。
fn body_names(body: &[u8]) -> &[u8] {
    if body.len() < 4 {
        return &[];
    }
    &body[4..]
}

pub fn decode_index(body: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(body_names(body)).into_owned();
    if s.is_empty() {
        return Vec::new();
    }
    s.split('\n').map(|x| x.to_string()).collect()
}
pub fn decode_obj_index(body: &[u8]) -> Vec<String> {
    decode_index(body)
}
pub fn decode_ext_index(body: &[u8]) -> ExtIndex {
    let (ext_path, childs) = decode_ext_index_raw(body);
    ExtIndex { childs, ext_path }
}

/// encodeExtIndexRaw：[4B count LE][…extpath\nname1\nname2...]，count = children.len()（extpath 不计入）。
pub fn encode_ext_index_raw(ext_path: &str, children: &[String]) -> Vec<u8> {
    let mut buf = (children.len() as u32).to_le_bytes().to_vec();
    let mut parts = vec![format!("{}{}", EXT_INDEX_HEAD, ext_path)];
    parts.extend(children.iter().cloned());
    buf.extend(parts.join(INDEX_VALUE_SEP).into_bytes());
    buf
}

/// decodeExtIndexRaw：跳过 [4B count LE]，首段（去 ExtIndexHead 前缀）= extpath，余段按 IndexValueSep 拆 children。
pub fn decode_ext_index_raw(body: &[u8]) -> (String, Vec<String>) {
    let s = String::from_utf8_lossy(body_names(body)).into_owned();
    if s.is_empty() {
        return (String::new(), Vec::new());
    }
    let mut it = s.splitn(2, '\n');
    let first = it.next().unwrap_or("");
    let ext_path = first.trim_start_matches(EXT_INDEX_HEAD).to_string();
    let children = match it.next() {
        Some(rest) => rest.split('\n').map(|x| x.to_string()).collect(),
        None => Vec::new(),
    };
    (ext_path, children)
}
