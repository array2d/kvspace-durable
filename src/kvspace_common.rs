// kvspace_common.rs — 对齐 kvspace_common.go：路径/索引通用助手。

use std::time::Duration;

use crate::kvspace::KVSpace;
use crate::r#const::*;
use crate::xvalue::{body_bytes, is_none, XValue};

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

/// 去掉尾部分隔符（/ 或 ·，按字节长切片，多字节 · 安全）。
pub fn strip_dir_suf(path: &str) -> &str {
    if path.ends_with(DIR_INDEX_SUF) {
        &path[..path.len() - DIR_INDEX_SUF.len()]
    } else if path.ends_with(OBJ_SEP) {
        &path[..path.len() - OBJ_SEP.len()]
    } else {
        path
    }
}

pub fn is_descendant(base: &str, path: &str) -> bool {
    if base == PATH_SEP {
        return path != PATH_SEP && path.starts_with(PATH_SEP);
    }
    path.strip_prefix(base)
        .is_some_and(|rest| rest.starts_with(PATH_SEP) || rest.starts_with(OBJ_SEP))
}

pub fn is_frame_operand_key(key: &str) -> bool {
    if !key.starts_with("/vthread/") {
        return false;
    }
    let slot = key.rsplit('/').next().unwrap_or("");
    let Some(inner) = slot.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
        return false;
    };
    let Some((row, col)) = inner.split_once(',') else {
        return false;
    };
    row.parse::<u32>().is_ok_and(|n| n > 0) && col.parse::<i32>().is_ok_and(|n| n != 0)
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

/// SepKind 统一 KV 路径中 index 目录分隔符的种类。
/// / → SepDir（层级目录）；· → SepDict（成员目录，含命名成员与坐标段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SepKind {
    SepDir,
    SepDict,
}

/// SplitIndex 解析绝对路径末段。`m·[0,1]` 切成父目录 `m·` + 成员名 `[0,1]`：
/// 坐标段整体是一个成员名，不是多级路径。成员分隔符 · 不在小数/坐标内出现，
/// 故直接 rfind(OBJ_SEP) 即可，无需 `.` 时代的 `.[` 特判。
pub fn split_index(path: &str) -> (String, String, SepKind) {
    let (mut parent, last) = sep_path(path);
    if parent != PATH_SEP {
        parent.push_str(DIR_INDEX_SUF);
    }
    let dot = last.rfind(OBJ_SEP);
    if let Some(i) = dot {
        if i > 0 {
            let m = &last[i + OBJ_SEP.len()..];
            if !m.is_empty() {
                return (
                    format!("{}{}", parent, &last[..i + OBJ_SEP.len()]),
                    m.to_string(),
                    SepKind::SepDict,
                );
            }
        }
    }
    (parent, last.to_string(), SepKind::SepDir)
}

/// langtype 归一：**只**剥「数组形状前缀」（`[5]`、`[]`、`[?]`——方括号内是维度数字/空）。
/// `[int64]·int64`、`[float64,float64]·int32` 里的方括号是**元组/标量键类型**，不是形状
/// （见 [[map容器]]：`[float64,float64]` ≠ `[2]float64`），一律原样保留——剥掉会让 map 的
/// 键类型整个丢失，Ptr 类型检查随即误判。
fn norm_langtype(s: &str) -> String {
    if let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            let inner = &rest[..end];
            let is_shape = inner.is_empty()
                || inner.split(',').all(|d| {
                    d.trim().is_empty() || d.trim() == "?" || d.trim().parse::<i32>().is_ok()
                });
            if is_shape {
                return rest[end + 1..].to_string();
            }
        }
    }
    s.to_string()
}

/// ValidatePtr：指针只解引用一跳，目标的完整 kindexpr 必须与指针声明的 target_kindexpr 相等。
/// 因此 *int 指向一个本身是 *int 的目标会被拒绝（声明 int ≠ 实际 *int）——不可连续解引用。
pub fn validate_ptr(
    kv: &mut dyn KVSpace,
    target: &str,
    target_kindexpr: &str,
) -> Result<(), String> {
    // 空指针（target 空，如 `next:*Node=""` 的零值）：无目标可校验，直接放行。
    // 注意须在 get_one 之前判——get_one("") 会落到根目录，解析出非 None 且 kind 不符，
    // 误报 kind mismatch 而拒绝空指针写入（曾致 struct 空 Ptr 字段默认值整个丢失）。
    if target.is_empty() || target_kindexpr.is_empty() {
        return Ok(());
    }
    // 空 kindexp（`name:` / `*`）= 不约束，已在上面放行；`any` 不再是类型名，落到
    // 精确比较并报 kind mismatch（不得当通配放行）。
    let v = get_one(kv, target);
    if is_none(&v) {
        return Ok(());
    }
    // 比 **kind 层**（剥 `[dims]`），不比整串：`[5]char/utf32` 与声明 `[]char/utf32`
    // 是同一个 kind 的不同形状，语义兼容。整串相等曾把「定长实参传给变长形参」
    // 这类合法场景误判为类型不符而拒绝写入。
    let actual = norm_langtype(
        &crate::xvalue::from_wire_head(&v.to_wire()).langtype,
    );
    let expect = norm_langtype(target_kindexpr);
    if actual != expect {
        return Err(format!(
            "{}: ptr kind mismatch: target {} is {}, ptr expects {}",
            ERR_LINK_TYPE_MISMATCH, target, actual, expect
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
