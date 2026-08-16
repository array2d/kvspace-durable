// kvspace_common.rs — 对齐 kvspace_common.go：路径/索引通用助手。

use std::time::Duration;

use crate::r#const::*;
use crate::kvspace::{KVSpace, KVPair};
use crate::xvalue::{body_bytes, is_none, XValue};
use crate::xvalue_index::new_index;

/// JoinPath 拼接父子路径。
pub fn join_path(parent: &str, child: &str) -> String {
    if parent == PATH_SEP {
        return format!("{}{}", PATH_SEP, child);
    }
    if parent.ends_with(PATH_SEP) || parent.ends_with(DICT_SEP) {
        return format!("{}{}", parent, child);
    }
    format!("{}{}{}", parent, PATH_SEP, child)
}

/// SepPath 拆分路径为 (prefix, last)。
pub fn sep_path(path: &str) -> (String, String) {
    if path == PATH_SEP {
        return (PATH_SEP.to_string(), String::new());
    }
    match path.rfind('/') {
        None => (String::new(), path.to_string()),
        Some(0) => (PATH_SEP.to_string(), path[1..].to_string()),
        Some(i) => (path[..i].to_string(), path[i + 1..].to_string()),
    }
}

/// SepKind 统一 KV 路径中 4 种 index 目录分隔符的种类。
/// / → SepDir（层级目录）；. → SepDict（成员目录）；[ → SepArray（数组坐标）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SepKind {
    SepDir,
    SepDict,
    SepArray,
}

/// SplitIndex 统一解析绝对路径末段，识别 4 种 index 分隔符：/ . [ ,
pub fn split_index(path: &str) -> (String, String, SepKind) {
    let (mut parent, last) = sep_path(path);
    if parent != PATH_SEP {
        parent.push_str(DIR_INDEX_SUF);
    }
    if let Some(i) = last.rfind('.') {
        if i > 0 {
            let m = &last[i + 1..];
            if !m.is_empty() {
                return (format!("{}{}", parent, &last[..i + 1]), m.to_string(), SepKind::SepDict);
            }
        }
    }
    if let Some(i) = last.rfind('[') {
        if i > 0 && last.ends_with(']') {
            let idx = &last[i + 1..last.len() - 1];
            if !idx.is_empty() && !idx.contains('[') && !idx.contains(']') {
                return (parent, last.to_string(), SepKind::SepArray);
            }
        }
    }
    (parent, last.to_string(), SepKind::SepDir)
}

/// MkIndexRecursive 递归创建目录，已存在的目录跳过。
pub fn mk_index_recursive(kv: &mut dyn KVSpace, path: &str) {
    if !path.ends_with(DIR_INDEX_SUF) {
        panic!("MkIndex: path must end with {}", DIR_INDEX_SUF);
    }
    let mut i = 1;
    while i < path.len() {
        match path[i..].find('/') {
            None => break,
            Some(j) => {
                i += j + 1;
                let dir = &path[..i];
                let (mut p, n) = sep_path(&dir[..dir.len() - 1]);
                if p != PATH_SEP {
                    p.push_str(DIR_INDEX_SUF);
                }
                if !dir_exists(kv, &p, &n) {
                    let _ = kv.set(&[KVPair { key: dir.to_string(), val: new_index(&[]) }]);
                }
            }
        }
    }
}

pub fn dir_exists(kv: &mut dyn KVSpace, parent_dir: &str, name: &str) -> bool {
    for m in kv.list(parent_dir, false, true) {
        if m == name || m == format!("{}{}", name, DIR_INDEX_SUF) {
            return true;
        }
    }
    false
}

/// ValidatePtr 检查 Ptr 的 kind/arraylen 与目标值的匹配。
pub fn validate_ptr(kv: &mut dyn KVSpace, target: &str, ptr_kind: &str, ptr_array_len: i32) -> Result<(), String> {
    let v = get_one(kv, target);
    if is_none(&v) {
        return Ok(());
    }
    if !ptr_kind.is_empty() && v.kind() != ptr_kind {
        return Err(format!(
            "{}: ptr kind mismatch: target {} is {}, ptr expects {}",
            ERR_LINK_TYPE_MISMATCH, target, v.kind(), ptr_kind
        ));
    }
    if ptr_array_len > 0 && v.array_len() != ptr_array_len {
        return Err(format!(
            "{}: ptr arraylen mismatch: target {} has {}, ptr expects {}",
            ERR_INVALID_VALUE, target, v.array_len(), ptr_array_len
        ));
    }
    Ok(())
}

/// GetOne 读取单个 key 的便捷方法。
pub fn get_one(kv: &mut dyn KVSpace, key: &str) -> XValue {
    let (mut p, l) = sep_path(key);
    if p != PATH_SEP {
        p.push_str(DIR_INDEX_SUF);
    }
    kv.get(&p, &[l], true).remove(0)
}

/// WatchValue 通用指数回退等待：轮询 GetOne(key) 直到 == targetValue。
pub fn watch_value(kv: &mut dyn KVSpace, key: &str, target_value: &XValue, tick_duration: Duration) -> XValue {
    const SPIN_COUNT: i64 = 100;
    let mut cur = Duration::ZERO;
    let mut i: i64 = 0;
    loop {
        let v = get_one(kv, key);
        if equal_xvalue(&v, target_value) {
            return v;
        }
        if i < SPIN_COUNT {
            i += 1;
            continue;
        }
        if cur.is_zero() {
            cur = Duration::from_micros(1);
        } else if cur < tick_duration {
            cur = (cur * 2).min(tick_duration);
        }
        std::thread::sleep(cur);
    }
}

pub fn equal_xvalue(a: &XValue, b: &XValue) -> bool {
    if a.kind() != b.kind() {
        return false;
    }
    body_bytes(a) == body_bytes(b)
}
