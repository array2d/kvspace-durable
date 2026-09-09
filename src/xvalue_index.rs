// xvalue_index.rs — memindex 定宽排序矩阵（index / object / stringkeymap / extindex 共用）

use crate::coord::cmp_coord;
use crate::r#const::ERR_MAP_NDIM;
use crate::xvalue::{ExtIndex, XValue};

pub fn new_index(children: &[String]) -> XValue {
    XValue::Index(children.to_vec())
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

// 成员名单编码为定宽排序矩阵（Go-slice cap/len 语义），几何 [len,cap,M] 落 XValueHead.dims：
//   len = 有效成员数（listlen，O(1)）；cap ≥ len = 预留行数；M = 成员 UTF-8 字节最大长向上 8 对齐（行宽）。
//   body = cap×M：前 len 行 = 成员名 UTF-8 + NUL 补齐到 M（cmp_coord 有序），后 cap−len 行全 NUL。
//   容量内增删 body 长度恒 cap×M → 就地覆写不重分配（减少扩容）；满则 cap 翻倍。
//   全表规范排序 → 三后端 blob 逐字节一致，listat/listlen O(1)、成员二分 O(log len)。

/// 行宽向上 8 字节对齐。
pub fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// cap 增长：容量足够则不变；空则取 need；否则从旧容量翻倍直到覆盖 need（默认 2×）。
pub fn grow_cap(old_cap: usize, need: usize) -> usize {
    if need <= old_cap {
        old_cap
    } else if old_cap == 0 {
        need
    } else {
        let mut c = old_cap;
        while c < need {
            c *= 2;
        }
        c
    }
}

/// 成员名单 → (dims=[len,cap,M], body=cap×M)。cap_hint/m_hint 为下限（只增不减，用于保留预留容量
/// 与既有行宽）；encode 侧规范排序，调用方无需预排。
pub fn encode_index_grow(
    children: &[String],
    cap_hint: usize,
    m_hint: usize,
) -> (Vec<i32>, Vec<u8>) {
    let mut c: Vec<&str> = children.iter().map(|s| s.as_str()).collect();
    c.sort_by(|a, b| cmp_coord(a, b));
    let len = c.len();
    let m = m_hint.max(align8(c.iter().map(|s| s.len()).max().unwrap_or(0)));
    let cap = cap_hint.max(len);
    let mut body = vec![0u8; cap * m];
    for (i, s) in c.iter().enumerate() {
        body[i * m..i * m + s.len()].copy_from_slice(s.as_bytes());
    }
    (vec![len as i32, cap as i32, m as i32], body)
}

/// 成员名单 → 紧凑编码（cap=len）。用于直接 set/新建。
pub fn encode_index(children: &[String]) -> (Vec<i32>, Vec<u8>) {
    encode_index_grow(children, 0, 0)
}

/// 定宽矩阵解码：len=dims[0]、M=dims[2]，读前 len 行去尾 NUL。
pub fn decode_index(body: &[u8], dims: &[i32]) -> Vec<String> {
    let m = matrix_width(dims);
    (0..matrix_count(dims))
        .filter_map(|i| matrix_at(body, m, i))
        .collect()
}

pub fn decode_ext_index(body: &[u8], dims: &[i32]) -> ExtIndex {
    let cap = matrix_cap(dims);
    let m = matrix_width(dims);
    let off = body.len().saturating_sub(cap * m);
    let ext_path = String::from_utf8_lossy(&body[..off]).into_owned();
    let mat = &body[off..];
    let childs = (0..matrix_count(dims))
        .filter_map(|i| matrix_at(mat, m, i))
        .collect();
    ExtIndex { childs, ext_path }
}

/// extindex → (dims=[len,cap,M], body)。body = 头部变长 ext_path + 尾部 cap×M 矩阵（childs，规范排序）。
/// ext_path 置头部：帧生命周期内一次写定、childs 才 churn，矩阵起点 off=body_len−cap*M 稳定。
pub fn encode_ext_index(ext_path: &str, children: &[String]) -> (Vec<i32>, Vec<u8>) {
    encode_ext_index_grow(ext_path, children, 0, 0)
}

/// extindex 带容量下限（cap/m 只增不减，对齐 encode_index_grow）。
pub fn encode_ext_index_grow(
    ext_path: &str,
    children: &[String],
    cap_hint: usize,
    m_hint: usize,
) -> (Vec<i32>, Vec<u8>) {
    let (dims, mat) = encode_index_grow(children, cap_hint, m_hint);
    let mut body = ext_path.as_bytes().to_vec();
    body.extend(mat);
    (dims, body)
}

/// 有效成员数 len = head dims[0]（O(1)，不碰 body）。
pub fn matrix_count(dims: &[i32]) -> usize {
    dims.first().map(|&n| n.max(0) as usize).unwrap_or(0)
}

/// 预留容量 cap = head dims[1]。
pub fn matrix_cap(dims: &[i32]) -> usize {
    dims.get(1).map(|&n| n.max(0) as usize).unwrap_or(0)
}

/// 行宽 M = head dims[2]。
pub fn matrix_width(dims: &[i32]) -> usize {
    dims.get(2).map(|&m| m.max(0) as usize).unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_roundtrip_align8() {
        let (dims, body) = encode_index(&["yy".into(), "x".into(), "zzz".into()]);
        assert_eq!(dims, vec![3, 3, 8]); // len=3, cap=3, M=align8(3)=8
        assert_eq!(body.len(), 24);
        assert_eq!(decode_index(&body, &dims), vec!["x", "yy", "zzz"]);
    }

    #[test]
    fn index_empty() {
        let (dims, body) = encode_index(&[]);
        assert_eq!(dims, vec![0, 0, 0]);
        assert!(body.is_empty());
    }

    #[test]
    fn grow_reserves_cap() {
        // 预留 cap=4：body 恒 4×8=32，前 1 行有效。
        let (dims, body) = encode_index_grow(&["x".into()], 4, 0);
        assert_eq!(dims, vec![1, 4, 8]);
        assert_eq!(body.len(), 32);
        assert_eq!(decode_index(&body, &dims), vec!["x"]);
    }

    #[test]
    fn grow_cap_doubling() {
        assert_eq!(grow_cap(0, 1), 1);
        assert_eq!(grow_cap(4, 4), 4);
        assert_eq!(grow_cap(4, 5), 8);
        assert_eq!(grow_cap(4, 20), 32);
    }

    #[test]
    fn ext_index_roundtrip() {
        let (dims, body) = encode_ext_index("/p", &["a".into()]);
        assert_eq!(dims, vec![1, 1, 8]); // len=1, cap=1, M=8
        assert_eq!(body.len(), 2 + 8); // "/p" + 1×8 矩阵
        let e = decode_ext_index(&body, &dims);
        assert_eq!(e.ext_path, "/p");
        assert_eq!(e.childs, vec!["a"]);
    }
}
