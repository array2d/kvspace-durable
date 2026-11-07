// fs/kvspace.rs — 结构感知的文件系统 KVSpace。
// 编码：kvspace 的 '·'（成员分隔）一律替换为 '·/'（父目录名带尾中点 + "/" 分隔成员），反向 '·/' → '·'。
// Directory members come from physical keys.

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::coord::cmp_coord;
use crate::kvspace::{KVPair, KVSpace};
use crate::kvspace_common::{
    is_descendant, is_frame_operand_key, join_path, sep_path, split_index, strip_dir_suf,
    subtree_base, watch_value,
};
use crate::r#const::*;
use crate::xvalue::*;
const SELF_MARKER: &str = "__self__";
const DIR_MARKER: &str = "__dir__";
/// Prefix for atomic-write temp files (`~~` cannot collide with `~` + hex escaped key names)
/// and is skipped during directory listing and copies.
const TMP_PREFIX: &str = "~~tmp.";
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Replaces a full value without exposing a missing or truncated file to concurrent readers:
/// overwrites the body in place when length, header, and attr match; otherwise writes a temp
/// file in the same directory and renames it over `path`.
fn write_atomic(path: &Path, val: &[u8]) -> std::io::Result<()> {
    if overwrite_body_in_place(path, val)? {
        return Ok(());
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{TMP_PREFIX}{}.{seq}", std::process::id()));
    fs::write(&tmp, val)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Overwrites only the body if `path` already has the same size, header, and attr bytes.
fn overwrite_body_in_place(path: &Path, val: &[u8]) -> std::io::Result<bool> {
    let Some(head) = crate::headlenpow::from_wire(val) else {
        return Ok(false);
    };
    let attr_off = crate::headlenpow::PREFIX + head.body.len();
    let f = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if f.metadata()?.len() != val.len() as u64 {
        return Ok(false);
    }
    let mut old = vec![0u8; crate::headlenpow::PREFIX + (val.len() - attr_off)];
    let (old_head, old_attr) = old.split_at_mut(crate::headlenpow::PREFIX);
    f.read_exact_at(old_head, 0)?;
    f.read_exact_at(old_attr, attr_off as u64)?;
    if old_head != &val[..crate::headlenpow::PREFIX] || old_attr != &val[attr_off..] {
        return Ok(false);
    }
    f.write_all_at(head.body, crate::headlenpow::PREFIX as u64)?;
    Ok(true)
}

fn is_tmp_name(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| n.starts_with(TMP_PREFIX))
}

pub struct FsKVSpace {
    root: PathBuf,
}

pub fn connect(root: &str) -> FsKVSpace {
    let root = if root.is_empty() {
        "/tmp/kvspace-fs"
    } else {
        root
    };
    FsKVSpace::new(root)
}

impl FsKVSpace {
    pub fn new(root: &str) -> Self {
        fs::create_dir_all(root)
            .unwrap_or_else(|e| panic!("kvspace-fs: create root {}: {}", root, e));
        FsKVSpace {
            root: PathBuf::from(root),
        }
    }

    /// kvspace key → fs 路径：'·'（成员分隔）→ '·/'；段首 '·' 是字面量不替换。
    fn fs_path(&self, key: &str) -> PathBuf {
        let sep = OBJ_SEP.chars().next().unwrap();
        let mut rel = String::with_capacity(key.len());
        let mut prev = '/';
        for c in key.chars() {
            if c == sep && prev != '/' {
                rel.push(sep);
                rel.push('/');
            } else {
                rel.push(c);
            }
            prev = c;
        }
        let path = rel
            .trim_start_matches('/')
            .split('/')
            .map(|part| {
                if part == SELF_MARKER || part == DIR_MARKER || part.starts_with('~') {
                    let mut escaped = String::from("~");
                    for byte in part.bytes() {
                        escaped.push_str(&format!("{byte:02x}"));
                    }
                    escaped
                } else {
                    part.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        self.root.join(path)
    }

    fn unescape_name(name: &str) -> String {
        let Some(hex) = name.strip_prefix('~') else {
            return name.to_string();
        };
        if hex.len() % 2 != 0 {
            return name.to_string();
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
            .collect::<Result<Vec<_>, _>>();
        bytes
            .ok()
            .and_then(|v| String::from_utf8(v).ok())
            .unwrap_or_else(|| name.to_string())
    }

    fn is_dir_key(key: &str) -> bool {
        key.ends_with(DIR_INDEX_SUF) || key.ends_with(OBJ_SEP)
    }

    fn leaf_marker(key: &str) -> &'static str {
        if key != PATH_SEP && key.ends_with(DIR_INDEX_SUF) {
            DIR_MARKER
        } else {
            SELF_MARKER
        }
    }

    fn read_leaf(&self, key: &str) -> Option<Vec<u8>> {
        if key.contains("//") {
            return None;
        }
        let p = self.fs_path(key);
        if p.is_dir() {
            fs::read(p.join(Self::leaf_marker(key))).ok()
        } else {
            fs::read(p).ok()
        }
    }
    fn leaf_file(&self, key: &str) -> Option<PathBuf> {
        if key.contains("//") {
            return None;
        }
        let p = self.fs_path(key);
        let f = if p.is_dir() {
            p.join(Self::leaf_marker(key))
        } else {
            p
        };
        if f.is_file() {
            Some(f)
        } else {
            None
        }
    }
    /// 解析逻辑 key 到叶文件（先 meta，再 ext 扩展存储）。
    fn physical_file(&mut self, key: &str) -> Option<PathBuf> {
        if crate::r#const::is_reserved(key) {
            return None;
        }
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if crate::r#const::is_reserved(&full) {
            return None;
        }
        if let Some(f) = self.leaf_file(&full) {
            return Some(f);
        }
        let ext_t = self.prefix_ext(&p);
        if !ext_t.is_empty() {
            if let Some(f) = self.leaf_file(&join_path(&ext_t, &l)) {
                return Some(f);
            }
        }
        None
    }

    fn write_leaf(&self, key: &str, val: &[u8]) {
        if key != PATH_SEP && key.ends_with(DIR_INDEX_SUF) {
            self.ensure_dir(key);
        }
        let p = self.fs_path(key);
        if p.is_dir() {
            write_atomic(&p.join(Self::leaf_marker(key)), val)
                .unwrap_or_else(|e| panic!("kvspace-fs: set {}: {}", key, e));
        } else {
            if let Some(parent) = p.parent() {
                let _ = fs::create_dir_all(parent);
            }
            write_atomic(&p, val).unwrap_or_else(|e| panic!("kvspace-fs: set {}: {}", key, e));
        }
    }

    fn remove_leaf(&self, key: &str) {
        let p = self.fs_path(key);
        if p.is_dir() {
            let _ = fs::remove_file(p.join(Self::leaf_marker(key)));
        } else {
            let _ = fs::remove_file(p);
        }
    }

    fn parent_name(path: &str) -> (String, String) {
        let mut path = path.to_string();
        if Self::is_dir_key(&path) && path != PATH_SEP {
            if path.ends_with(DIR_INDEX_SUF) {
                path.pop();
            } else if path.ends_with(OBJ_SEP) {
                path.pop();
            }
        }
        let (mut parent, last) = sep_path(&path);
        if parent != PATH_SEP {
            parent.push_str(DIR_INDEX_SUF);
        }
        (parent, last)
    }

    /// 去掉尾斜杠的节点路径（目录 key 的尾斜杠在 OS 层被折叠）。
    fn node_path(&self, key: &str) -> PathBuf {
        let s = self.fs_path(key).to_string_lossy().into_owned();
        PathBuf::from(s.trim_end_matches('/'))
    }

    /// 确保 key 对应节点是目录；若当前是文件，转为目录并把内容搬到 __self__。
    fn ensure_dir(&self, key: &str) {
        let node = self.node_path(key);
        if node.is_file() {
            let content = fs::read(&node).ok();
            let _ = fs::remove_file(&node);
            let _ = fs::create_dir_all(&node);
            if let Some(c) = content {
                let _ = fs::write(node.join(SELF_MARKER), c);
            }
        } else {
            let _ = fs::create_dir_all(&node);
        }
    }

    // ── link 解析（读叶值，同 backend.rs） ─────────────────────────────

    fn resolve_path(&self, path: &str) -> String {
        let mut path = path.to_string();
        for _ in 0..16 {
            let (resolved, changed) = self.resolve_one(&path);
            if !changed {
                return resolved;
            }
            path = resolved;
        }
        path
    }

    fn resolve_parent(&self, path: &str) -> String {
        let dir_suf = Self::is_dir_key(path) && path != PATH_SEP;
        let clean = if dir_suf { strip_dir_suf(path) } else { path };
        let (parent, last) = sep_path(clean);
        if parent == clean {
            return path.to_string();
        }
        let resolved = self.resolve_path(&parent);
        let mut result = join_path(&resolved, &last);
        if dir_suf {
            result.push_str(if path.ends_with(OBJ_SEP) {
                OBJ_SEP
            } else {
                DIR_INDEX_SUF
            });
        }
        result
    }

    fn resolve_one(&self, path: &str) -> (String, bool) {
        if path == PATH_SEP {
            return (path.to_string(), false);
        }
        let dir_suf = if path.ends_with(OBJ_SEP) {
            Some(OBJ_SEP)
        } else if path.ends_with(DIR_INDEX_SUF) {
            Some(DIR_INDEX_SUF)
        } else {
            None
        };
        let clean = if dir_suf.is_some() {
            strip_dir_suf(path)
        } else {
            path
        };
        let trimmed = clean.trim_matches('/');
        let parts: Vec<&str> = if trimmed.is_empty() {
            Vec::new()
        } else {
            trimmed.split('/').collect()
        };
        let mut cur = PATH_SEP.to_string();
        for (i, p) in parts.iter().enumerate() {
            cur = join_path(&cur, p);
            if let Some(data) = self.read_leaf(&cur) {
                let v = from_wire(&data);
                if is_ptr(&v) {
                    let target = ptr_target(&v);
                    let mut next = if i + 1 < parts.len() {
                        join_path(&target, &parts[i + 1..].join("/"))
                    } else {
                        target
                    };
                    if let Some(suf) = dir_suf {
                        if !Self::is_dir_key(&next) {
                            next.push_str(suf);
                        }
                    }
                    return (next, true);
                }
            }
        }
        (path.to_string(), false)
    }

    fn prefix_ext(&self, prefix: &str) -> String {
        if let Some(data) = self.read_leaf(prefix) {
            let head = from_wire_head(&data);
            if head.r#ref == crate::xvalue::REF_EXT {
                return String::from_utf8_lossy(head.body(&data)).into_owned();
            }
        }
        String::new()
    }

    // ── 目录 children 派生 ────────────────────────────────────────────

    fn dir_children(&self, dir_key: &str) -> Vec<String> {
        if dir_key.contains("//") {
            return Vec::new();
        }
        let p = self.fs_path(dir_key);
        let mut children = Vec::new();
        if let Ok(entries) = fs::read_dir(&p) {
            for e in entries.flatten() {
                if is_tmp_name(&e.file_name()) {
                    continue;
                }
                let fname = e.file_name().to_string_lossy().into_owned();
                if fname == SELF_MARKER
                    || fname == DIR_MARKER
                    || (dir_key == PATH_SEP && fname == META_ROOT_NAME)
                {
                    continue;
                }
                let name = Self::unescape_name(&fname);
                // Hide runtime system keys containing RUNTIME_MEMBER_SEP (‥) from List.
                if name.contains(RUNTIME_MEMBER_SEP) {
                    continue;
                }
                if name.ends_with(OBJ_SEP) {
                    children.push(name.trim_end_matches(OBJ_SEP).to_string());
                } else if e.path().is_dir() {
                    let dp = e.path();
                    if dp.join(SELF_MARKER).is_file() && !dp.join(DIR_MARKER).is_file() {
                        children.push(name);
                    } else {
                        children.push(format!("{}/", name));
                    }
                } else {
                    children.push(name);
                }
            }
        }
        // memindex 统一按 cmp_coord 规范排序（坐标 row-major 数值序、字符串键字典序），三后端一致。
        children.sort_by(|a, b| cmp_coord(a, b));
        children.dedup();
        children
    }

    /// 递归复制文件/目录（extindex/self/order/map marker 作普通文件一并复制）。
    fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
        if src.is_dir() {
            fs::create_dir_all(dst)?;
            for e in fs::read_dir(src)? {
                let e = e?;
                if is_tmp_name(&e.file_name()) {
                    continue;
                }
                Self::copy_recursive(&e.path(), &dst.join(e.file_name()))?;
            }
        } else {
            if let Some(p) = dst.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(src, dst)?;
        }
        Ok(())
    }
}

impl KVSpace for FsKVSpace {
    fn get(&mut self, prefix: &str, keys: &[String], resolve: bool) -> Vec<XValue> {
        if prefix != PATH_SEP && !Self::is_dir_key(prefix) {
            panic!("{}: {}", ERR_DIR_MUST_END_WITH_SLASH, prefix);
        }
        let prefix = if resolve {
            self.resolve_path(prefix)
        } else {
            prefix.to_string()
        };
        let ext_t = self.prefix_ext(&prefix);

        keys.iter()
            .map(|k| {
                let full = join_path(&prefix, k);
                if crate::r#const::is_reserved(&full) {
                    return XValue::None;
                }
                if Self::is_dir_key(&full) {
                    return self
                        .read_leaf(&full)
                        .map(|raw| from_wire(&raw))
                        .unwrap_or(XValue::None);
                }
                if let Some(data) = self.read_leaf(&full) {
                    return from_wire(&data);
                }
                if !ext_t.is_empty() {
                    let target = join_path(&ext_t, k);
                    if let Some(data) = self.read_leaf(&target) {
                        return from_wire(&data);
                    }
                }
                XValue::None
            })
            .collect()
    }

    fn get_raw(&mut self, key: &str) -> Vec<u8> {
        if crate::r#const::is_reserved(key) {
            return Vec::new();
        }
        if Self::is_dir_key(key) {
            let resolved = self.resolve_path(key);
            if Self::is_dir_key(&resolved) {
                return self.read_leaf(&resolved).unwrap_or_default();
            }
        }
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if crate::r#const::is_reserved(&full) {
            return Vec::new();
        }
        if let Some(data) = self.read_leaf(&full) {
            return data;
        }
        let ext_t = self.prefix_ext(&p);
        if !ext_t.is_empty() {
            let ext_full = join_path(&ext_t, &l);
            if let Some(data) = self.read_leaf(&ext_full) {
                return data;
            }
        }
        Vec::new()
    }

    fn get_part(&mut self, key: &str, off: u32, len: u32) -> Vec<u8> {
        if crate::r#const::is_reserved(key) {
            return Vec::new();
        }
        if Self::is_dir_key(key) {
            let resolved = self.resolve_path(key);
            if let Some(v) = self.read_leaf(&resolved) {
                let s = (off as usize).min(v.len());
                let e = (s + len as usize).min(v.len());
                return v[s..e].to_vec();
            }
        }
        let Some(pf) = self.physical_file(key) else {
            return Vec::new();
        };
        let Ok(f) = fs::File::open(&pf) else {
            return Vec::new();
        };
        let mut buf = vec![0u8; len as usize];
        match f.read_at(&mut buf, off as u64) {
            Ok(n) => {
                buf.truncate(n);
                buf
            }
            Err(_) => Vec::new(),
        }
    }

    fn set_part(&mut self, key: &str, off: u32, buf: &[u8]) -> Result<(), String> {
        if crate::r#const::is_reserved(key) {
            return Err(format!("reserved metadata key: {key}"));
        }
        let pf = self
            .physical_file(key)
            .ok_or_else(|| format!("set_part: missing key {}", key))?;
        /*
         * **`fs://` 不走 `Backend`**：`conn.rs:33` 直接构造 `FsKVSpace`，
         * 所以 `backend.rs::set_part` 的判界覆盖不到这里。判据必须在这一层也有一份，
         * 且与 `backend.rs` 用**同一个量**：`16 + effective_len`。
         *
         * pwrite 对越界会自己撑大文件，不判界就是「原地写变成扩容」。
         */
        {
            let current = fs::read(&pf)
                .map_err(|e| format!("kvspace-fs: set_part read {:?}: {}", pf, e))?;
            let effective = crate::headlenpow::effective_len(&current)
                .ok_or_else(|| format!("set_part: malformed wire at {key}"))?;
            let upper = crate::headlenpow::PREFIX as u64 + effective;
            if (off as u64).saturating_add(buf.len() as u64) > upper {
                return Err(format!(
                    "kvspace-fs: set_part out of range at {key} (off {off} + {} > {upper})",
                    buf.len()
                ));
            }
        }
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&pf)
            .map_err(|e| format!("kvspace-fs: set_part open {:?}: {}", pf, e))?;
        f.write_at(buf, off as u64)
            .map_err(|e| format!("kvspace-fs: set_part write {:?}: {}", pf, e))?;
        Ok(())
    }

    fn set(&mut self, pairs: &[KVPair]) -> Result<(), String> {
        for pair in pairs {
            if crate::r#const::is_reserved(&pair.key) {
                return Err(format!("reserved metadata key: {}", pair.key));
            }
            let key = self.resolve_parent(&pair.key);
            if key.contains("//") || crate::r#const::is_reserved(&key) {
                return Err(format!("invalid key: {key}"));
            }
            let raw = pair.raw.clone().unwrap_or_else(|| pair.val.to_wire());
            let head = crate::headlenpow::from_wire(&raw)
                .ok_or_else(|| format!("invalid XValue at {key}"))?;
            crate::kvspace_common::validate_status_wire(&key, &raw)?;
            /* attr 区长度从 head 读（无前缀）；空 attr 是合法的（=0）。 */
            let attr = crate::headlenpow::attr_len(&raw)
                .ok_or_else(|| format!("invalid XValue at {key}"))?;
            let total = (head.len as usize)
                .checked_add(crate::headlenpow::PREFIX)
                .and_then(|x| x.checked_add(attr as usize))
                .ok_or_else(|| format!("invalid XValue at {key}"))?;
            if total > raw.len() {
                return Err(format!("invalid XValue at {key}"));
            }
            let raw = &raw[..total];
            let (parent, _, _) = split_index(&key);
            if parent.ends_with(OBJ_SEP) {
                let base = strip_dir_suf(&parent);
                if !base.starts_with("/lib") && self.read_leaf(base).is_none() {
                    return Err(format!(
                        "{}: memhead {} does not exist — declare the container first",
                        ERR_MEMHEAD_MISSING, base
                    ));
                }
            }
            let ext_target = self.prefix_ext(&parent);
            if !ext_target.is_empty() && self.read_leaf(&key).is_none() {
                let suffix = key.strip_prefix(&parent).unwrap_or("");
                if self.read_leaf(&format!("{ext_target}{suffix}")).is_some()
                    && !(head.reference == crate::headlenpow::Reference::Pointer && is_frame_operand_key(&key))
                {
                    return Err(format!("{}: {}", ERR_EXT_WRITE, key));
                }
            }
            // 单 key 整体写（类型在值 attr 区）：天然原子，无 attr key、无写序。
            self.ensure_dir(&parent);
            if Self::is_dir_key(&key) {
                self.ensure_dir(&key);
            }
            self.write_leaf(&key, raw);
        }
        Ok(())
    }

    fn list(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> Vec<String> {
        if crate::r#const::is_reserved(prefix) {
            return Vec::new();
        }
        if prefix != PATH_SEP && !Self::is_dir_key(prefix) {
            panic!("{}: {}", ERR_DIR_MUST_END_WITH_SLASH, prefix);
        }
        let resolved = if resolve {
            self.resolve_path(prefix)
        } else {
            prefix.to_string()
        };
        if !Self::is_dir_key(&resolved) {
            return Vec::new();
        }
        if crate::r#const::is_reserved(&resolved) {
            return Vec::new();
        }
        let mut members = self.dir_children(&resolved);

        if expand_ext {
            let ext_t = self.prefix_ext(&resolved);
            if !ext_t.is_empty() {
                for m in self.dir_children(&ext_t) {
                    if !members.contains(&m) {
                        members.push(m);
                    }
                }
            }
        }
        members
    }

    fn del(&mut self, keys: &[String]) -> Result<(), String> {
        for key in keys {
            if crate::r#const::is_reserved(key) {
                return Err(format!("reserved metadata key: {key}"));
            }
            let resolved = self.resolve_parent(key);
            if crate::r#const::is_reserved(&resolved) {
                return Err(format!("reserved metadata key: {resolved}"));
            }
            self.remove_leaf(&resolved);
        }
        Ok(())
    }

    fn del_tree(&mut self, prefix: &str) -> Result<(), String> {
        if crate::r#const::is_reserved(prefix) {
            return Err(format!("reserved metadata key: {prefix}"));
        }
        // Resolve links only on parent components (POSIX rm semantics); if the final
        // component is itself a link, delete the link rather than its target subtree.
        let resolved = self.resolve_parent(prefix);
        if crate::r#const::is_reserved(&resolved) {
            return Err(format!("reserved metadata key: {resolved}"));
        }
        let base = subtree_base(&resolved).to_string();
        if !base.is_empty() {
            if let Some(data) = self.read_leaf(&base) {
                if from_wire_head(&data).is_ptr() {
                    return self.del(&[base]);
                }
            }
        }
        // Subtree covers base, base/*, base·*, and base‥* across the node path, its
        // sibling `base·` directory, and sibling `base‥*` entries in the parent directory.
        let mut doomed = Vec::new();
        if base.is_empty() {
            // Root: remove everything except reserved metadata, keeping the root dir itself.
            if let Ok(entries) = fs::read_dir(&self.root) {
                for e in entries.flatten() {
                    if e.file_name() != META_ROOT_NAME {
                        doomed.push(e.path());
                    }
                }
            }
        } else {
            let node = self.node_path(&base);
            doomed.push(self.node_path(&format!("{base}{OBJ_SEP}")));
            if let (Some(parent), Some(name)) = (node.parent(), node.file_name()) {
                let stem = format!(
                    "{}{}",
                    Self::unescape_name(&name.to_string_lossy()),
                    RUNTIME_MEMBER_SEP
                );
                if let Ok(entries) = fs::read_dir(parent) {
                    for e in entries.flatten() {
                        if !is_tmp_name(&e.file_name())
                            && Self::unescape_name(&e.file_name().to_string_lossy())
                                .starts_with(&stem)
                        {
                            doomed.push(e.path());
                        }
                    }
                }
            }
            doomed.push(node);
        }
        for node in doomed {
            let r = if node.is_dir() {
                fs::remove_dir_all(&node)
            } else if node.is_file() {
                fs::remove_file(&node)
            } else {
                continue;
            };
            match r {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(format!("DelTree {}: {e}", node.display()));
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn cp(&mut self, src: &str, dst: &str) -> Result<(), String> {
        if crate::r#const::is_reserved(src) || crate::r#const::is_reserved(dst) {
            return Err("reserved metadata key".into());
        }
        let raw = self.get_raw(src);
        if raw.is_empty() {
            return Err(format!("Cp: source not found: {}", src));
        }
        // Copy raw wire bytes verbatim (langtype lives in the value's attr trailer).
        let v = from_wire(&raw);
        self.set(&[KVPair {
            key: dst.to_string(),
            val: v,
            raw: Some(raw),
        }])
    }

    fn cp_tree(&mut self, src: &str, dst: &str) -> Result<(), String> {
        if crate::r#const::is_reserved(src) || crate::r#const::is_reserved(dst) {
            return Err("reserved metadata key".into());
        }
        let src_res = self.resolve_path(src);
        let dst_res = self.resolve_path(dst);
        if crate::r#const::is_reserved(&src_res) || crate::r#const::is_reserved(&dst_res) {
            return Err("reserved metadata key".into());
        }
        let de_suffix = |k: &str| {
            if Self::is_dir_key(k) && k != PATH_SEP {
                strip_dir_suf(k).to_string()
            } else {
                k.to_string()
            }
        };
        let src_base = de_suffix(&src_res);
        let dst_base = de_suffix(&dst_res);
        if src_base == dst_base {
            return Ok(());
        }
        if is_descendant(&src_base, &dst_base) {
            return Err("CpTree: destination is inside source".into());
        }
        // 一个节点跨两条 fs 实体：base（值/层级子树）与兄弟成员目录 base·。
        let sb = self.node_path(&src_base);
        let smem = self.node_path(&format!("{}{}", src_base, OBJ_SEP));
        if !sb.exists() && !smem.exists() {
            return Err(format!("CpTree: source not found: {}", src));
        }
        let _ = self.del_tree(&dst_base);
        let db = self.node_path(&dst_base);
        let dmem = self.node_path(&format!("{}{}", dst_base, OBJ_SEP));
        if sb.exists() {
            Self::copy_recursive(&sb, &db).map_err(|e| format!("CpTree {}→{}: {}", src, dst, e))?;
        }
        if smem.exists() {
            Self::copy_recursive(&smem, &dmem)
                .map_err(|e| format!("CpTree {}→{}: {}", src, dst, e))?;
        }
        // 确保 dst 父目录存在（成员名单结构派生，无需登记）。
        let (parent, _name) = Self::parent_name(&dst_base);
        self.ensure_dir(&parent);
        Ok(())
    }

    fn cp_list(&mut self, src: &str, dst: &str) -> Result<(), String> {
        if crate::r#const::is_reserved(src) || crate::r#const::is_reserved(dst) {
            return Err("reserved metadata key".into());
        }
        let src_res = self.resolve_path(src);
        let dst_res = self.resolve_path(dst);
        if crate::r#const::is_reserved(&src_res) || crate::r#const::is_reserved(&dst_res) {
            return Err("reserved metadata key".into());
        }
        let de_suffix = |k: &str| {
            if Self::is_dir_key(k) && k != PATH_SEP {
                strip_dir_suf(k).to_string()
            } else {
                k.to_string()
            }
        };
        let src_base = de_suffix(&src_res);
        let dst_base = de_suffix(&dst_res);
        if src_base == dst_base {
            return Ok(());
        }
        let sb = self.node_path(&src_base);
        let smem = self.node_path(&format!("{}{}", src_base, OBJ_SEP));
        if !sb.exists() && !smem.exists() {
            return Err(format!("CpList: source not found: {}", src));
        }
        let _ = self.del_tree(&dst_base);
        if let Some(data) = self.read_leaf(&src_base) {
            self.write_leaf(&dst_base, &data);
        }
        // 一层成员目录：只拷直接条目（marker + 直接成员值），子目录只取其 base 值 __self__，不递归。
        if smem.is_dir() {
            let dmem = self.node_path(&format!("{}{}", dst_base, OBJ_SEP));
            let _ = fs::create_dir_all(&dmem);
            if let Ok(rd) = fs::read_dir(&smem) {
                for e in rd.flatten() {
                    if is_tmp_name(&e.file_name()) {
                        continue;
                    }
                    let sp = e.path();
                    let dp = dmem.join(e.file_name());
                    if sp.is_file() {
                        let _ = fs::copy(&sp, &dp);
                    } else if sp.is_dir() {
                        for marker in [SELF_MARKER, DIR_MARKER] {
                            if let Ok(c) = fs::read(sp.join(marker)) {
                                let _ = fs::create_dir_all(&dp);
                                let _ = fs::write(dp.join(marker), c);
                            }
                        }
                    }
                }
            }
        }
        let (parent, _name) = Self::parent_name(&dst_base);
        self.ensure_dir(&parent);
        Ok(())
    }

    fn watch(&mut self, key: &str, target_value: &XValue, tick_duration: Duration) -> XValue {
        watch_value(self, key, target_value, tick_duration)
    }

    fn mkindex(&mut self, path: &str, _capacity: u32) -> Result<(), String> {
        // 目录原生后端：成员即真实文件，无定宽矩阵可预留，capacity 无意义。
        if !Self::is_dir_key(path) {
            return Err(format!("{}: Mkindex {}", ERR_DIR_MUST_END_WITH_SLASH, path));
        }
        let resolved = self.resolve_path(path);
        self.ensure_dir(&resolved);
        if self.read_leaf(&resolved).is_none() {
            // Directory marker: inline, empty body, langtype "lib" in attr.
            let value = crate::headlenpow::to_wire(&[], "lib")
                .ok_or_else(|| format!("Mkindex: invalid directory {resolved}"))?;
            self.write_leaf(&resolved, &value);
        }
        Ok(())
    }

    fn ext_index(&mut self, path: &str, ext_path: &str) -> Result<(), String> {
        if !Self::is_dir_key(path) || !Self::is_dir_key(ext_path) {
            return Err(format!(
                "{}: ExtIndex path={} extpath={}",
                ERR_DIR_MUST_END_WITH_SLASH, path, ext_path
            ));
        }
        let resolved = self.resolve_parent(path);
        if !self.prefix_ext(ext_path).is_empty() {
            return Err(format!("{}: {}", ERR_EXT_CASCADE, ext_path));
        }
        if self.read_leaf(ext_path).is_none() {
            return Err(format!("ExtIndex target missing: {ext_path}"));
        }
        let langtype = self.langtype_of(ext_path);
        // External locator: langtype 带 `@` 前缀（扩展由类型表达，head 无 ptr）。
        let prefixed = if langtype.starts_with('@') {
            langtype.clone()
        } else {
            format!("@{langtype}")
        };
        let value = crate::headlenpow::to_wire(ext_path.as_bytes(), &prefixed)
            .ok_or_else(|| format!("ExtIndex locator invalid: {ext_path}"))?;
        self.ensure_dir(&resolved);
        self.write_leaf(&resolved, &value);
        Ok(())
    }

    fn del_ext_index(&mut self, path: &str) -> Result<(), String> {
        let resolved = self.resolve_parent(path);
        self.remove_leaf(&resolved);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::create_dir_all(&self.root);
        Ok(())
    }
}
