// fs/kvspace.rs — 结构感知的文件系统 KVSpace。
// 编码：kvspace 的 '.' 一律替换为 './'（父目录名带尾点 + "/" 分隔成员），反向 './' → '.'。
// "/" 与 "." 的 index 都从 readdir 派生；ExtIndex 用目录内 __extindex__ 文件存 ext_target_path（第一行）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::kvspace::{KVSpace, KVPair};
use crate::kvspace_common::{join_path, sep_path, split_index, validate_ptr, watch_value, SepKind};
use crate::r#const::*;
use crate::xvalue::*;
use crate::xvalue_index::{new_dict_index, new_ext_index, new_index};

const EXTINDEX_MARKER: &str = "__extindex__";

pub struct FsKVSpace {
    root: PathBuf,
}

pub fn connect(root: &str) -> FsKVSpace {
    let root = if root.is_empty() { "/tmp/kvspace-fs" } else { root };
    FsKVSpace::new(root)
}

impl FsKVSpace {
    pub fn new(root: &str) -> Self {
        fs::create_dir_all(root).unwrap_or_else(|e| panic!("kvspace-fs: create root {}: {}", root, e));
        FsKVSpace { root: PathBuf::from(root) }
    }

    /// kvspace key → fs 路径：'.' → './'
    fn fs_path(&self, key: &str) -> PathBuf {
        let rel = key.replace('.', "./");
        self.root.join(rel.trim_start_matches('/'))
    }

    /// fs 路径 → kvspace key：'./' → '.'
    fn key_of(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let s = rel.to_string_lossy().replace("./", ".");
        if s.is_empty() {
            PATH_SEP.to_string()
        } else {
            format!("/{}", s)
        }
    }

    fn is_dir_key(key: &str) -> bool {
        key.ends_with(DIR_INDEX_SUF) || key.ends_with(DICT_SEP)
    }

    fn read_leaf(&self, key: &str) -> Option<Vec<u8>> {
        fs::read(self.fs_path(key)).ok()
    }

    fn write_leaf(&self, key: &str, val: &[u8]) {
        let p = self.fs_path(key);
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(p, val).unwrap_or_else(|e| panic!("kvspace-fs: set {}: {}", key, e));
    }

    fn remove_leaf(&self, key: &str) {
        let _ = fs::remove_file(self.fs_path(key));
    }

    // ── link 解析（读叶值，同 backend.rs） ─────────────────────────────

    fn resolve_path(&self, path: &str) -> String {
        let mut path = path.to_string();
        loop {
            let (resolved, changed) = self.resolve_one(&path);
            if !changed {
                return resolved;
            }
            path = resolved;
        }
    }

    fn resolve_parent(&self, path: &str) -> String {
        let dir_suf = Self::is_dir_key(path) && path != PATH_SEP;
        let clean = if dir_suf { &path[..path.len() - 1] } else { path };
        let (parent, last) = sep_path(clean);
        if parent == clean {
            return path.to_string();
        }
        let resolved = self.resolve_path(&parent);
        let mut result = join_path(&resolved, &last);
        if dir_suf {
            result.push_str(DIR_INDEX_SUF);
        }
        result
    }

    fn resolve_one(&self, path: &str) -> (String, bool) {
        if path == PATH_SEP {
            return (path.to_string(), false);
        }
        let trimmed = path.trim_matches('/');
        let parts: Vec<&str> = if trimmed.is_empty() { Vec::new() } else { trimmed.split('/').collect() };
        let mut cur = PATH_SEP.to_string();
        for (i, p) in parts.iter().enumerate() {
            cur = join_path(&cur, p);
            if let Some(data) = self.read_leaf(&cur) {
                let v = decode_xvalue(&data);
                if is_ptr(&v) {
                    let target = ptr_target(&v);
                    if i + 1 < parts.len() {
                        return (join_path(&target, &parts[i + 1..].join("/")), true);
                    }
                    return (target, true);
                }
            }
        }
        (path.to_string(), false)
    }

    fn prefix_ext(&self, prefix: &str) -> String {
        if let Some(data) = self.read_leaf(prefix) {
            let head = decode_xvalue_head(&data);
            if head.kind == KIND_EXT_INDEX {
                let body = head.body(&data);
                return crate::xvalue_index::decode_ext_index(body).ext_path;
            }
        }
        // 结构感知：ext 也可能存在 marker 文件里
        let marker = self.fs_path(prefix).join(EXTINDEX_MARKER);
        if let Ok(b) = fs::read(&marker) {
            return String::from_utf8_lossy(&b).into_owned();
        }
        String::new()
    }

    // ── 目录 children 派生 ────────────────────────────────────────────

    fn dir_children(&self, dir_key: &str) -> Vec<String> {
        let p = self.fs_path(dir_key);
        let is_dict = dir_key.ends_with(DICT_SEP);
        let mut children = Vec::new();
        if let Ok(entries) = fs::read_dir(&p) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == EXTINDEX_MARKER {
                    continue;
                }
                let decoded = name.replace("./", ".");
                if e.path().is_dir() {
                    if is_dict {
                        children.push(decoded.trim_end_matches('.').to_string());
                    } else {
                        children.push(format!("{}/", decoded));
                    }
                } else {
                    children.push(decoded);
                }
            }
        }
        children
    }

    /// 目录 key 的 XValue：dict → DictIndex，hierarchy → ExtIndex（有 marker）或 Index。
    fn dir_value(&self, dir_key: &str) -> XValue {
        let children = self.dir_children(dir_key);
        if dir_key.ends_with(DICT_SEP) {
            return new_dict_index(&children);
        }
        let marker = self.fs_path(dir_key).join(EXTINDEX_MARKER);
        if let Ok(b) = fs::read(&marker) {
            let ext_path = String::from_utf8_lossy(&b).into_owned();
            return new_ext_index(&children, &ext_path);
        }
        new_index(&children)
    }
}

impl KVSpace for FsKVSpace {
    fn get(&mut self, prefix: &str, keys: &[String], resolve: bool) -> Vec<XValue> {
        if prefix != PATH_SEP && !Self::is_dir_key(prefix) {
            panic!("{}: {}", ERR_DIR_MUST_END_WITH_SLASH, prefix);
        }
        let prefix = if resolve { self.resolve_path(prefix) } else { prefix.to_string() };
        let ext_t = self.prefix_ext(&prefix);

        keys.iter()
            .map(|k| {
                let full = join_path(&prefix, k);
                if Self::is_dir_key(&full) {
                    return self.dir_value(&full);
                }
                if let Some(data) = self.read_leaf(&full) {
                    return decode_xvalue(&data);
                }
                if !ext_t.is_empty() {
                    let target = join_path(&ext_t, k);
                    if let Some(data) = self.read_leaf(&target) {
                        return decode_xvalue(&data);
                    }
                }
                XValue::None
            })
            .collect()
    }

    fn set(&mut self, pairs: &[KVPair]) -> Result<(), String> {
        for p in pairs {
            let resolved = self.resolve_path(&p.key);
            if resolved.contains("//") {
                panic!("Set: double-slash in key {:?}", resolved);
            }
            match &p.val {
                XValue::Index(_) | XValue::ExtIndex(_) => {
                    if !Self::is_dir_key(&resolved) {
                        panic!("Set: index/extindex value at non-directory key {:?}", resolved);
                    }
                }
                _ => {}
            }
            if let XValue::Ptr(ptr) = &p.val {
                validate_ptr(self, &ptr.target, &ptr.kind, ptr.array_len)?;
            }

            // 目录 index 值：结构派生，无需存；ExtIndex 写 marker。
            if let XValue::ExtIndex(e) = &p.val {
                let dir = self.fs_path(&resolved);
                let _ = fs::create_dir_all(&dir);
                let marker = dir.join(EXTINDEX_MARKER);
                fs::write(&marker, e.ext_path.as_bytes())
                    .map_err(|e| format!("kvspace-fs: extindex {}: {}", resolved, e))?;
                continue;
            }
            if let XValue::Index(_) | XValue::Dict(_) = &p.val {
                let _ = fs::create_dir_all(self.fs_path(&resolved));
                continue;
            }

            // 叶值：确保父目录存在，写文件。
            let (parent, _name, _) = split_index(&resolved);
            let parent_dir = self.fs_path(&parent);
            let _ = fs::create_dir_all(&parent_dir);
            self.write_leaf(&resolved, &p.val.encode());
        }
        Ok(())
    }

    fn list(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> Vec<String> {
        if prefix != PATH_SEP && !Self::is_dir_key(prefix) {
            panic!("{}: {}", ERR_DIR_MUST_END_WITH_SLASH, prefix);
        }
        let resolved = if resolve { self.resolve_path(prefix) } else { prefix.to_string() };
        if !Self::is_dir_key(&resolved) {
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
            let resolved = self.resolve_parent(key);
            if Self::is_dir_key(&resolved) {
                let _ = fs::remove_dir_all(self.fs_path(&resolved));
            } else {
                self.remove_leaf(&resolved);
            }
        }
        Ok(())
    }

    fn del_tree(&mut self, prefix: &str) -> Result<(), String> {
        let resolved = self.resolve_path(prefix);
        // 若 prefix 本身是链接（叶 Ptr），只删链接。
        let link_key = if Self::is_dir_key(&resolved) && resolved != PATH_SEP {
            &resolved[..resolved.len() - 1]
        } else {
            &resolved
        };
        if let Some(data) = self.read_leaf(link_key) {
            if decode_xvalue_head(&data).is_ptr {
                return self.del(&[resolved]);
            }
        }
        let _ = fs::remove_dir_all(self.fs_path(&resolved));
        Ok(())
    }

    fn watch(&mut self, key: &str, target_value: &XValue, tick_duration: Duration) -> XValue {
        watch_value(self, key, target_value, tick_duration)
    }

    fn mkindex(&mut self, path: &str) -> Result<(), String> {
        if !Self::is_dir_key(path) {
            return Err(format!("{}: Mkindex {}", ERR_DIR_MUST_END_WITH_SLASH, path));
        }
        let resolved = self.resolve_path(path);
        let _ = fs::create_dir_all(self.fs_path(&resolved));
        Ok(())
    }

    fn ext_index(&mut self, path: &str, ext_path: &str) -> Result<(), String> {
        if !Self::is_dir_key(path) || !Self::is_dir_key(ext_path) {
            return Err(format!("{}: ExtIndex path={} extpath={}", ERR_DIR_MUST_END_WITH_SLASH, path, ext_path));
        }
        let resolved = self.resolve_parent(path);
        let dir = self.fs_path(&resolved);
        let _ = fs::create_dir_all(&dir);
        let marker = dir.join(EXTINDEX_MARKER);
        fs::write(&marker, ext_path.as_bytes())
            .map_err(|e| format!("kvspace-fs: extindex {}: {}", resolved, e))?;
        Ok(())
    }

    fn del_ext_index(&mut self, path: &str) -> Result<(), String> {
        let resolved = self.resolve_parent(path);
        let marker = self.fs_path(&resolved).join(EXTINDEX_MARKER);
        let _ = fs::remove_file(marker);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::create_dir_all(&self.root);
        Ok(())
    }

    fn dis_conn(&mut self) -> Result<(), String> {
        Ok(())
    }
}

// 供测试用：返回 root 下的顶层条目数。
pub fn top_level_count(kv: &FsKVSpace) -> usize {
    fs::read_dir(&kv.root).map(|d| d.count()).unwrap_or(0)
}
