// xvalue_index.rs — memindex 定宽排序矩阵（index / object / stringkeymap / extindex 共用）

use crate::coord::cmp_coord;
use crate::r#const::ERR_MAP_NDIM;
use crate::xvalue::{ExtIndex, XValue};

pub fn new_index(children: &[String]) -> XValue {
    XValue::Index(children.to_vec())
}
pub fn new_obj_index() -> XValue {
    XValue::Obj
}
/// stringkeymap 恒 ndim≥1；dims 为空即非法（无维度的字符串键容器是 object）。
pub fn new_map_index(dims: &[i32]) -> XValue {
    if dims.is_empty() {
        panic!("{}", ERR_MAP_NDIM);
    }
    XValue::Map(dims.to_vec())
}
pub fn new_ext_index(children: &[String], ext_path: &str) -> XValue {
    XValue::ExtIndex(ExtIndex {
        childs: children.to_vec(),
        ext_path: ext_path.to_string(),
    })
}

// 成员名单统一编码为定宽排序矩阵：几何 [N,M] 落 XValueHead 的 dims，body 纯 N×M。
//   N = 成员数；M = 成员 UTF-8 字节最大长度（行宽）。
//   每行 = 成员名 UTF-8 + NUL 补齐到 M（UTF-8 永不含 0x00，NUL 补齐/终止安全）。
//   全表按 cmp_coord 规范排序 → 三后端 blob 逐字节一致，listat/listlen O(1)、成员二分 O(log N)。

/// 成员名单 → (dims=[N,M], body)。encode 侧规范排序，调用方无需预排。
pub fn encode_index(children: &[String]) -> (Vec<i32>, Vec<u8>) {
    let mut c: Vec<&str> = children.iter().map(|s| s.as_str()).collect();
    c.sort_by(|a, b| cmp_coord(a, b));
    let n = c.len();
    let m = c.iter().map(|s| s.len()).max().unwrap_or(0);
    let mut body = vec![0u8; n * m];
    for (i, s) in c.iter().enumerate() {
        body[i * m..i * m + s.len()].copy_from_slice(s.as_bytes());
    }
    (vec![n as i32, m as i32], body)
}

/// 定宽矩阵解码：N=dims[0]、M=dims[1]，每行去尾 NUL。
pub fn decode_index(body: &[u8], dims: &[i32]) -> Vec<String> {
    let m = matrix_width(dims);
    (0..matrix_count(dims))
        .filter_map(|i| matrix_at(body, m, i))
        .collect()
}

pub fn decode_ext_index(body: &[u8], dims: &[i32]) -> ExtIndex {
    let n = matrix_count(dims);
    let m = matrix_width(dims);
    let off = body.len().saturating_sub(n * m);
    let ext_path = String::from_utf8_lossy(&body[..off]).into_owned();
    let mat = &body[off..];
    let childs = (0..n).filter_map(|i| matrix_at(mat, m, i)).collect();
    ExtIndex { childs, ext_path }
}

/// extindex → (dims=[N,M], body)。body = 头部变长 ext_path + 尾部 N×M 矩阵（childs，规范排序）。
/// ext_path 置头部：帧生命周期内一次写定、childs 才 churn，矩阵起点 off=body_len−N*M 稳定。
pub fn encode_ext_index(ext_path: &str, children: &[String]) -> (Vec<i32>, Vec<u8>) {
    let (dims, mat) = encode_index(children);
    let mut body = ext_path.as_bytes().to_vec();
    body.extend(mat);
    (dims, body)
}

/// 成员数 = head dims[0]（O(1)，不碰 body）。
pub fn matrix_count(dims: &[i32]) -> usize {
    dims.first().map(|&n| n.max(0) as usize).unwrap_or(0)
}

/// 行宽 M = head dims[1]。
fn matrix_width(dims: &[i32]) -> usize {
    dims.get(1).map(|&m| m.max(0) as usize).unwrap_or(0)
}

/// 第 idx 行（O(1)），去尾 NUL。越界返回 None。
pub fn matrix_at(body: &[u8], m: usize, idx: usize) -> Option<String> {
    if m == 0 {
        return Some(String::new());
    }
    let s = idx * m;
    let e = s + m;
    if e > body.len() {
        return None;
    }
    let row = &body[s..e];
    let end = row.iter().position(|&b| b == 0).unwrap_or(m);
    Some(String::from_utf8_lossy(&row[..end]).into_owned())
}
