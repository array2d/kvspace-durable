// kvspace_common.rs — 对齐 kvspace_common.go：路径/索引通用助手。

use std::time::Duration;

use crate::kvspace::{KVPair, KVSpace};
use crate::r#const::*;
use crate::xvalue::{body_bytes, is_none, plain, XValue};
use crate::xvalue_index::new_index;

/// JoinPath 拼接父子路径。
pub fn join_path(parent: &str, child: &str) -> String {
    if parent == PATH_SEP {
        return format!("{}{}", PATH_SEP, child);
    }
    if parent.ends_with(PATH_SEP) || parent.ends_with(OBJ_SEP) {
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
                return (
                    format!("{}{}", parent, &last[..i + 1]),
                    m.to_string(),
                    SepKind::SepDict,
                );
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
                    let _ = kv.set(&[KVPair {
                        key: dir.to_string(),
                        val: new_index(&[]),
                        raw: None,
                    }]);
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
pub fn validate_ptr(
    kv: &mut dyn KVSpace,
    target: &str,
    ptr_kind: &str,
    ptr_array_len: i32,
) -> Result<(), String> {
    let v = get_one(kv, target);
    if is_none(&v) {
        return Ok(());
    }
    if !ptr_kind.is_empty() && v.kind() != ptr_kind {
        return Err(format!(
            "{}: ptr kind mismatch: target {} is {}, ptr expects {}",
            ERR_LINK_TYPE_MISMATCH,
            target,
            v.kind(),
            ptr_kind
        ));
    }
    if ptr_array_len > 0 && v.array_len() != ptr_array_len {
        return Err(format!(
            "{}: ptr arraylen mismatch: target {} has {}, ptr expects {}",
            ERR_INVALID_VALUE,
            target,
            v.array_len(),
            ptr_array_len
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
pub fn watch_value(
    kv: &mut dyn KVSpace,
    key: &str,
    target_value: &XValue,
    tick_duration: Duration,
) -> XValue {
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

// ── 展示（对齐 FprintList / FprintTree / GetAt / ReadPrefixExt / StripExtChildren） ──

pub fn get_at(kv: &mut dyn KVSpace, dir: &str, name: &str) -> XValue {
    kv.get(dir, &[name.to_string()], true).remove(0)
}

pub fn read_prefix_ext(kv: &mut dyn KVSpace, prefix: &str) -> String {
    if let XValue::ExtIndex(e) = get_one(kv, prefix) {
        return e.ext_path;
    }
    String::new()
}

pub fn strip_ext_children(
    kv: &mut dyn KVSpace,
    prefix: &str,
    children: Vec<String>,
) -> Vec<String> {
    let ext_target = read_prefix_ext(kv, prefix);
    if ext_target.is_empty() {
        return children;
    }
    let ext_children = kv.list(&ext_target, false, true);
    let n = children.len().saturating_sub(ext_children.len());
    children[..n].to_vec()
}

pub fn fprint_list(kv: &mut dyn KVSpace, prefix: &str, show_ext: bool, show_kind: bool) {
    let mut children = kv.list(prefix, true, true);
    if !show_ext {
        children = strip_ext_children(kv, prefix, children);
    }
    for c in children {
        let mut v = get_at(kv, prefix, &c);
        let child_dir = format!("{}{}", join_path(prefix, &c), DIR_INDEX_SUF);
        let mut has_dir = !kv.list(&child_dir, false, true).is_empty();
        if !has_dir {
            has_dir = !is_none(&get_at(kv, prefix, &format!("{}{}", c, DIR_INDEX_SUF)));
        }
        let mut key = c.clone();
        if has_dir {
            key.push_str(DIR_INDEX_SUF);
            v = XValue::None;
        }
        if is_none(&v) {
            println!("{}", key);
        } else if show_kind {
            println!("{}\t{}\t{}", key, v.kind(), plain(&v));
        } else {
            println!("{}\t{}", key, plain(&v));
        }
    }
    if !show_ext {
        let ext = read_prefix_ext(kv, prefix);
        if !ext.is_empty() {
            println!("{}{}", EXT_INDEX_HEAD, ext);
            for c in kv.list(&ext, false, true) {
                println!("  {}", c);
            }
        }
    }
}

pub fn fprint_tree(
    kv: &mut dyn KVSpace,
    prefix: &str,
    indent: &str,
    show_ext: bool,
    show_kind: bool,
) {
    let mut children = kv.list(prefix, true, true);
    if !show_ext {
        children = strip_ext_children(kv, prefix, children);
    }
    children.sort_by(|a, b| {
        let (a_, b_) = (a.trim_end_matches('/'), b.trim_end_matches('/'));
        if a_ == b_ {
            a.ends_with('/').cmp(&b.ends_with('/'))
        } else {
            a_.cmp(b_)
        }
    });

    let n = children.len();
    for (i, c) in children.iter().enumerate() {
        let v = get_at(kv, prefix, c);
        let base = c.trim_end_matches('/');
        let child_dir = format!("{}{}", join_path(prefix, base), DIR_INDEX_SUF);
        let mut has_child = !kv.list(&child_dir, false, true).is_empty();
        if !has_child {
            has_child = !is_none(&get_at(kv, prefix, &format!("{}{}", base, DIR_INDEX_SUF)));
        }

        let last = i == n - 1;
        let branch = if last { "└── " } else { "├── " };
        let next_indent = format!("{}{}", indent, if last { "    " } else { "│   " });

        if has_child && c.ends_with(DIR_INDEX_SUF) {
            println!("{}{}{}", indent, branch, c);
            fprint_tree(kv, &child_dir, &next_indent, show_ext, show_kind);
        } else if is_none(&v) {
            println!("{}{}{}", indent, branch, c);
        } else if show_kind {
            println!("{}{}{}\t{}\t{}", indent, branch, c, v.kind(), plain(&v));
        } else {
            println!("{}{}{}\t{}", indent, branch, c, plain(&v));
        }
    }

    if !show_ext {
        let ext = read_prefix_ext(kv, prefix);
        if !ext.is_empty() {
            println!("{}└── {}{}", indent, EXT_INDEX_HEAD, ext);
        }
    }
}
