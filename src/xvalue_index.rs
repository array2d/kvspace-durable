// xvalue_index.rs — 对齐 xvalue_index.go（Index、DictIndex、ExtIndex）

use crate::r#const::*;
use crate::xvalue::{ExtIndex, XValue};

pub fn new_index(children: &[String]) -> XValue {
    XValue::Index(children.to_vec())
}
pub fn new_dict_index(children: &[String]) -> XValue {
    XValue::Dict(children.to_vec())
}
pub fn new_ext_index(children: &[String], ext_path: &str) -> XValue {
    XValue::ExtIndex(ExtIndex {
        childs: children.to_vec(),
        ext_path: ext_path.to_string(),
    })
}

pub fn decode_index(body: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(body).into_owned();
    if s.is_empty() {
        return Vec::new();
    }
    s.split('\n').map(|x| x.to_string()).collect()
}
pub fn decode_dict_index(body: &[u8]) -> Vec<String> {
    decode_index(body)
}
pub fn decode_ext_index(body: &[u8]) -> ExtIndex {
    let (ext_path, childs) = decode_ext_index_raw(body);
    ExtIndex { childs, ext_path }
}

/// encodeExtIndexRaw：parts = [ExtIndexHead + extpath] + children，用 IndexValueSep 连接。
pub fn encode_ext_index_raw(ext_path: &str, children: &[String]) -> Vec<u8> {
    let mut parts = vec![format!("{}{}", EXT_INDEX_HEAD, ext_path)];
    parts.extend(children.iter().cloned());
    parts.join(INDEX_VALUE_SEP).into_bytes()
}

/// decodeExtIndexRaw：首段（去掉 ExtIndexHead 前缀）= extpath，余段按 IndexValueSep 拆分为 children。
pub fn decode_ext_index_raw(body: &[u8]) -> (String, Vec<String>) {
    let s = String::from_utf8_lossy(body).into_owned();
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
