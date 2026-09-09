// fs/kvspace.rs — 结构感知的文件系统 KVSpace。
// 编码：kvspace 的 '·'（成员分隔）一律替换为 '·/'（父目录名带尾中点 + "/" 分隔成员），反向 '·/' → '·'。
// "/" 与 "·" 的 index 都从 readdir 派生；ExtIndex 用目录内 __extindex__ 文件存 ext_target_path（第一行）。

use std::fs;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::coord::{cmp_coord, grow_coord_dims, is_coord, parse_coord};
use crate::kvspace::{KVPair, KVSpace};
use crate::kvspace_common::{
    join_path, sep_path, split_index, strip_dir_suf, validate_ptr, watch_value, SepKind,
};
use crate::r#const::*;
use crate::xvalue::*;
use crate::xvalue_index::{new_ext_index, new_index, new_map_index, new_obj_index};

const EXTINDEX_MARKER: &str = "__extindex__";
const SELF_MARKER: &str = "__self__";
const ORDER_MARKER: &str = "__order__";
/// stringkeymap 成员目录标记，内容为 dims（逗号分隔）。readdir 派生不出 kind 与 dims，故显式落盘。
const MAP_MARKER: &str = "__map__";

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
        self.root.join(rel.trim_start_matches('/'))
    }

    /// fs 路径 → kvspace key：'·/' → '·'
    fn key_of(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let pat = [OBJ_SEP, "/"].concat();
        let s = rel.to_string_lossy().replace(pat.as_str(), OBJ_SEP);
        if s.is_empty() {
            PATH_SEP.to_string()
        } else {
            format!("/{}", s)
        }
    }

    fn is_dir_key(key: &str) -> bool {
        key.ends_with(DIR_INDEX_SUF) || key.ends_with(OBJ_SEP)
    }

    fn read_leaf(&self, key: &str) -> Option<Vec<u8>> {
        if key.contains("//") {
            return None;
        }
        let p = self.fs_path(key);
        // 目录带值：值落在目录内的保留文件 __self__，readdir 派生的 index 之外。
        if p.is_dir() {
            fs::read(p.join(SELF_MARKER)).ok()
        } else {
            fs::read(p).ok()
        }
    }
    /// 叶值实际所在文件（目录带值时为 __self__），供定位读写 pread/pwrite。存在才返回。
    fn leaf_file(&self, key: &str) -> Option<PathBuf> {
        if key.contains("//") {
            return None;
        }
        let p = self.fs_path(key);
        let f = if p.is_dir() { p.join(SELF_MARKER) } else { p };
        if f.is_file() {
            Some(f)
        } else {
            None
        }
    }
    /// 解析逻辑 key 到叶文件（先 meta，再 ext 扩展存储）。
    fn physical_file(&mut self, key: &str) -> Option<PathBuf> {
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        if let Some(f) = self.leaf_file(&join_path(&p, &l)) {
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
        let p = self.fs_path(key);
        if p.is_dir() {
            fs::write(p.join(SELF_MARKER), val)
                .unwrap_or_else(|e| panic!("kvspace-fs: set {}: {}", key, e));
        } else {
            if let Some(parent) = p.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(p, val).unwrap_or_else(|e| panic!("kvspace-fs: set {}: {}", key, e));
        }
    }

    fn remove_leaf(&self, key: &str) {
        let p = self.fs_path(key);
        if p.is_dir() {
            let _ = fs::remove_file(p.join(SELF_MARKER));
        } else {
            let _ = fs::remove_file(p);
        }
    }

    fn suffix_for(key: &str) -> &'static str {
        if key.ends_with(OBJ_SEP) && !key.ends_with(DIR_INDEX_SUF) {
            OBJ_SEP
        } else {
            DIR_INDEX_SUF
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
        let clean = if dir_suf { strip_dir_suf(path) } else { path };
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
        let parts: Vec<&str> = if trimmed.is_empty() {
            Vec::new()
        } else {
            trimmed.split('/').collect()
        };
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
            if head.kind() == KIND_EXT_INDEX {
                let body = head.body(&data);
                return crate::xvalue_index::decode_ext_index(body, &head.dims()).ext_path;
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
        if dir_key.contains("//") {
            return Vec::new();
        }
        let p = self.fs_path(dir_key);
        // 目录名（末段）是否以 '·' 结尾：成员前缀目录（"math·"）内的成员不再扁平化。
        let name = dir_key
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("");
        let is_member_dir = name.ends_with(OBJ_SEP);
        let mut children = Vec::new();
        if let Ok(entries) = fs::read_dir(&p) {
            for e in entries.flatten() {
                let fname = e.file_name().to_string_lossy().into_owned();
                if fname == EXTINDEX_MARKER
                    || fname == SELF_MARKER
                    || fname == ORDER_MARKER
                    || fname == MAP_MARKER
                {
                    continue;
                }
                if fname.ends_with(OBJ_SEP) {
                    if is_member_dir {
                        // 成员目录内：obj 是完整 key，发射为 "init."
                        children.push(fname);
                    } else {
                        // 普通目录内：成员前缀目录（"math."）扁平化展开为 "math.<成员>"
                        let sub_key = format!("{}{}/", dir_key, fname);
                        for sub in self.dir_children(&sub_key) {
                            children.push(format!("{}{}", fname, sub));
                        }
                    }
                } else if e.path().is_dir() {
                    children.push(format!("{}/", fname));
                    // 目录带值：额外发射无尾斜杠的叶名（对应 __self__）
                    if e.path().join(SELF_MARKER).is_file() {
                        children.push(fname.clone());
                    }
                } else {
                    children.push(fname);
                }
            }
        }
        // memindex 统一按 cmp_coord 规范排序（坐标 row-major 数值序、字符串键字典序），三后端一致。
        children.sort_by(|a, b| cmp_coord(a, b));
        children
    }

    /// 目录 key 的 XValue：统一为 index（memindex）；ExtIndex 有 marker 时为 extindex。
    /// 不存在的目录返回 None（对齐 redis 后端：目录 key 不存在 → None）。
    fn dir_value(&self, dir_key: &str) -> XValue {
        if dir_key.contains("//") || !self.fs_path(dir_key).is_dir() {
            return XValue::None;
        }
        let children = self.dir_children(dir_key);
        let marker = self.fs_path(dir_key).join(EXTINDEX_MARKER);
        if let Ok(b) = fs::read(&marker) {
            let ext_path = String::from_utf8_lossy(&b).into_owned();
            return new_ext_index(&children, &ext_path);
        }
        new_index(&children)
    }

    /// 递归复制文件/目录（extindex/self/order/map marker 作普通文件一并复制）。
    fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
        if src.is_dir() {
            fs::create_dir_all(dst)?;
            for e in fs::read_dir(src)? {
                let e = e?;
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
                if Self::is_dir_key(&full) {
                    return self.dir_value(&full);
                }
                if let Some(data) = self.read_leaf(&full) {
                    return decode_xvalue(&data);
                }
                // dict 形式回落：读 seen 回落 seen.
                let dict_key = format!("{}{}", full, OBJ_SEP);
                if self.fs_path(&dict_key).is_dir() {
                    return self.dir_value(&dict_key);
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

    fn get_raw(&mut self, key: &str) -> Vec<u8> {
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
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
        let pf = self
            .physical_file(key)
            .ok_or_else(|| format!("set_part: missing key {}", key))?;
        let f = fs::OpenOptions::new()
            .write(true)
            .open(&pf)
            .map_err(|e| format!("kvspace-fs: set_part open {:?}: {}", pf, e))?;
        f.write_at(buf, off as u64)
            .map_err(|e| format!("kvspace-fs: set_part write {:?}: {}", pf, e))?;
        Ok(())
    }

    fn set(&mut self, pairs: &[KVPair]) -> Result<(), String> {
        for p in pairs {
            let resolved = self.resolve_path(&p.key);
            if resolved.contains("//") {
                return Err(format!("Set: double-slash in key {:?}", resolved));
            }
            match &p.val {
                XValue::Index(_) | XValue::ExtIndex(_) => {
                    if !Self::is_dir_key(&resolved) {
                        return Err(format!(
                            "Set: directory-kind value at non-directory key {:?}",
                            resolved
                        ));
                    }
                }
                _ => {}
            }
            if let XValue::Ptr(ptr) = &p.val {
                validate_ptr(self, &ptr.target, &ptr.target_kindexpr)?;
            }

            // 目录 index 值：结构派生，无需存；ExtIndex 写 marker。
            if let XValue::ExtIndex(e) = &p.val {
                self.ensure_dir(&resolved);
                let marker = self.fs_path(&resolved).join(EXTINDEX_MARKER);
                fs::write(&marker, e.ext_path.as_bytes())
                    .map_err(|e| format!("kvspace-fs: extindex {}: {}", resolved, e))?;
                continue;
            }
            if let XValue::Obj = &p.val {
                let base = if resolved == PATH_SEP {
                    resolved.clone()
                } else {
                    strip_dir_suf(&resolved).to_string()
                };
                let (parent, _name) = Self::parent_name(&base);
                self.ensure_dir(&parent); // 父可能是同名叶文件（如 /lib/input def rwir）→ 提升为目录
                self.write_leaf(&base, &p.raw.clone().unwrap_or_else(|| p.val.encode()));
                self.ensure_dir(&format!("{}{}", base, OBJ_SEP));
                continue;
            }
            if let XValue::Map(dims) = &p.val {
                let base = if resolved == PATH_SEP {
                    resolved.clone()
                } else {
                    strip_dir_suf(&resolved).to_string()
                };
                let (parent, _name) = Self::parent_name(&base);
                self.ensure_dir(&parent); // 父可能是同名叶文件（如 /lib/input def rwir）→ 提升为目录
                self.write_leaf(&base, &p.raw.clone().unwrap_or_else(|| p.val.encode()));
                self.ensure_dir(&format!("{}{}", base, OBJ_SEP));
                let _ = dims;
                continue;
            }
            if let XValue::Index(_) = &p.val {
                self.ensure_dir(&resolved);
                continue;
            }

            // 叶值：确保父是目录（父可能是同名叶文件，如 /lib/println），写文件。
            let (parent, name, _) = split_index(&resolved);
            self.ensure_dir(&parent);
            // extindex 写保护：只读扩展层上的同名节点禁止写入（对齐 backend.rs）。
            let marker = self.fs_path(&parent).join(EXTINDEX_MARKER);
            if let Ok(b) = fs::read(&marker) {
                let ext_t = String::from_utf8_lossy(&b).into_owned();
                if !self.dir_children(&parent).iter().any(|n| n == &name)
                    && self.dir_children(&ext_t).iter().any(|n| n == &name)
                {
                    return Err(format!("{}: {}", ERR_EXT_WRITE, resolved));
                }
            }
            let bytes = p.raw.clone().unwrap_or_else(|| p.val.encode());
            self.write_leaf(&resolved, &bytes);
            // 坐标段成员写入未显式创建容器 → 自动建 stringkeymap 值（dims 由坐标推导）。
            if parent.ends_with(OBJ_SEP) && is_coord(&name) {
                let base = strip_dir_suf(&parent);
                if !self.fs_path(base).exists() {
                    let mut names = self.dir_children(&parent);
                    if !names.contains(&name) {
                        names.push(name.clone());
                    }
                    let dims = grow_coord_dims(&[], &names);
                    self.write_leaf(base, &new_map_index(&dims).encode());
                }
            }
        }
        Ok(())
    }

    fn list(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> Vec<String> {
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
            strip_dir_suf(&resolved)
        } else {
            &resolved
        };
        if let Some(data) = self.read_leaf(link_key) {
            if decode_xvalue_head(&data).is_ptr() {
                return self.del(&[resolved]);
            }
        }
        let _ = fs::remove_dir_all(self.fs_path(&resolved));
        Ok(())
    }

    fn cp(&mut self, src: &str, dst: &str) -> Result<(), String> {
        let raw = self.get_raw(src);
        if raw.is_empty() {
            return Err(format!("Cp: source not found: {}", src));
        }
        let v = decode_xvalue(&raw);
        self.set(&[KVPair {
            key: dst.to_string(),
            val: v,
            raw: Some(raw),
        }])
    }

    fn cp_tree(&mut self, src: &str, dst: &str) -> Result<(), String> {
        let src_res = self.resolve_path(src);
        let dst_res = self.resolve_path(dst);
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
        let src_res = self.resolve_path(src);
        let dst_res = self.resolve_path(dst);
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
                    let sp = e.path();
                    let dp = dmem.join(e.file_name());
                    if sp.is_file() {
                        let _ = fs::copy(&sp, &dp);
                    } else if let Ok(c) = fs::read(sp.join(SELF_MARKER)) {
                        let _ = fs::create_dir_all(&dp);
                        let _ = fs::write(dp.join(SELF_MARKER), c);
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
        let _ = fs::create_dir_all(self.fs_path(&resolved));
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
        // 级联检查：ext_path 本身是 extindex → 不容许（对齐 backend.rs）
        if self.fs_path(ext_path).join(EXTINDEX_MARKER).is_file() {
            return Err(format!("{}: {}", ERR_EXT_CASCADE, ext_path));
        }
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
