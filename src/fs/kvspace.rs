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
    validate_ptr, watch_value,
};
use crate::r#const::*;
use crate::xvalue::*;
use crate::xvalue_byte::char_byte_to_wire;
const SELF_MARKER: &str = "__self__";
const DIR_MARKER: &str = "__dir__";

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

    /// langtype attr key：`x‥langtype`。
    fn langtype_key(key: &str) -> String {
        format!("{key}{RUNTIME_MEMBER_SEP}langtype")
    }

    /// 读 key 的 langtype attr key：attr key 值是字符串 XValue（char/utf8），
    /// 解出明文类型串；无/空/解析失败 → 空串。
    fn langtype_of(&self, key: &str) -> String {
        self.read_leaf(&Self::langtype_key(key))
            .map(|data| match from_wire(&data, "[]char/utf8") {
                XValue::CharByte(b) => String::from_utf8_lossy(&b.data).into_owned(),
                _ => String::new(),
            })
            .unwrap_or_default()
    }

    /// 写/删 key 的 langtype attr key：attr key 值存字符串 XValue（char/utf8）的 wire 字节；
    /// 空串 → 删除（无类型）。
    fn set_langtype(&self, key: &str, langtype: &str) {
        let ltkey = Self::langtype_key(key);
        if langtype.is_empty() {
            self.remove_leaf(&ltkey);
        } else {
            let wire = char_byte_to_wire(langtype.as_bytes(), &[]);
            self.write_leaf(&ltkey, &wire);
        }
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
            fs::write(p.join(Self::leaf_marker(key)), val)
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
                let v = from_wire(&data, &self.langtype_of(&cur));
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
            let head = from_wire_head(&data, &self.langtype_of(prefix));
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
                let fname = e.file_name().to_string_lossy().into_owned();
                if fname == SELF_MARKER
                    || fname == DIR_MARKER
                    || (dir_key == PATH_SEP && fname == META_ROOT_NAME)
                {
                    continue;
                }
                let name = Self::unescape_name(&fname);
                // RUNTIME_MEMBER_SEP（‥）前缀键是系统变量/langtype attr key，List 时隐藏。
                if name.contains(RUNTIME_MEMBER_SEP) {
                    continue;
                }
                if name.ends_with(OBJ_SEP) {
                    children.push(name.trim_end_matches(OBJ_SEP).to_string());
                } else if e.path().is_dir() {
                    children.push(format!("{}/", name));
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
                        .map(|raw| from_wire(&raw, &self.langtype_of(&full)))
                        .unwrap_or(XValue::None);
                }
                if let Some(data) = self.read_leaf(&full) {
                    return from_wire(&data, &self.langtype_of(&full));
                }
                if !ext_t.is_empty() {
                    let target = join_path(&ext_t, k);
                    if let Some(data) = self.read_leaf(&target) {
                        return from_wire(&data, &self.langtype_of(&target));
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
            if head.len as usize + crate::headlenpow::PREFIX != raw.len() {
                return Err(format!("invalid XValue at {key}"));
            }
            // 固定头无类型：langtype 走 x‥langtype attr key（值侧写自 val）。
            // ‥ 后缀 key（系统变量/attr key 本体）是叶子，无类型、不写嵌套 attr key。
            let langtype = if key.contains(RUNTIME_MEMBER_SEP) {
                String::new()
            } else {
                crate::xvalue::xvalue_langtype(&pair.val)
            };
            // 指针校验用声明的 target_kindexpr（val 为 Ptr 时可得；raw 直写无类型则跳过）。
            if let XValue::Ptr(ptr) = &pair.val {
                validate_ptr(self, &ptr.target, &ptr.target_kindexpr)?;
            }
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
                    && !(head.ptr == crate::xvalue::REF_PTR && is_frame_operand_key(&key))
                {
                    return Err(format!("{}: {}", ERR_EXT_WRITE, key));
                }
            }
            // 写序固定：attr 先落，x 后落（x 写 = 提交点，x 存在则类型必完整）。
            self.set_langtype(&key, &langtype);
            self.ensure_dir(&parent);
            if Self::is_dir_key(&key) {
                self.ensure_dir(&key);
            }
            self.write_leaf(&key, &raw);
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
            self.remove_leaf(&Self::langtype_key(&resolved));
            self.remove_leaf(&resolved);
        }
        Ok(())
    }

    fn del_tree(&mut self, prefix: &str) -> Result<(), String> {
        if crate::r#const::is_reserved(prefix) {
            return Err(format!("reserved metadata key: {prefix}"));
        }
        let resolved = self.resolve_path(prefix);
        if crate::r#const::is_reserved(&resolved) {
            return Err(format!("reserved metadata key: {resolved}"));
        }
        // 若 prefix 本身是链接（叶 Ptr），只删链接。
        let link_key = if Self::is_dir_key(&resolved) && resolved != PATH_SEP {
            strip_dir_suf(&resolved)
        } else {
            &resolved
        };
        if let Some(data) = self.read_leaf(link_key) {
            if from_wire_head(&data, &self.langtype_of(link_key)).is_ptr() {
                return self.del(&[resolved]);
            }
        }
        let base = if Self::is_dir_key(&resolved) && resolved != PATH_SEP {
            strip_dir_suf(&resolved)
        } else {
            &resolved
        };
        for key in [base.to_string(), format!("{base}{OBJ_SEP}")] {
            let node = self.node_path(&key);
            if node.is_dir() {
                fs::remove_dir_all(&node).map_err(|e| format!("DelTree {key}: {e}"))?;
            } else if node.is_file() {
                fs::remove_file(&node).map_err(|e| format!("DelTree {key}: {e}"))?;
            }
        }
        // base 值（非目录形态）的 langtype attr key 是 base 的兄弟文件，单独清除。
        self.remove_leaf(&Self::langtype_key(base));
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
        // attr key 与值同址：读解析后的物理 src 路径的类型。
        let v = from_wire(&raw, &self.langtype_of(&self.resolve_parent(src)));
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
        // base 值的 langtype attr key 是 base 的兄弟文件，单独复制。
        if let Some(lt) = self.read_leaf(&Self::langtype_key(&src_base)) {
            self.write_leaf(&Self::langtype_key(&dst_base), &lt);
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
        // base 值的 langtype attr key 是 base 的兄弟文件，单独复制。
        if let Some(lt) = self.read_leaf(&Self::langtype_key(&src_base)) {
            self.write_leaf(&Self::langtype_key(&dst_base), &lt);
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
            // 固定头空目录 marker：storetype 0、ptr 0、空 body；类型 "lib" 走 attr key。
            let value = crate::headlenpow::to_wire(0, 0, &[])
                .ok_or_else(|| format!("Mkindex: invalid directory {resolved}"))?;
            self.set_langtype(&resolved, "lib");
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
        // 扩展定位：storetype 3（ST_EXT）+ ptr 2（PTR_EXT）+ 定位串 body。
        let value = crate::headlenpow::to_wire(3, 2, ext_path.as_bytes())
            .ok_or_else(|| format!("ExtIndex locator invalid: {ext_path}"))?;
        self.set_langtype(&resolved, &langtype);
        self.ensure_dir(&resolved);
        self.write_leaf(&resolved, &value);
        Ok(())
    }

    fn del_ext_index(&mut self, path: &str) -> Result<(), String> {
        let resolved = self.resolve_parent(path);
        self.remove_leaf(&Self::langtype_key(&resolved));
        self.remove_leaf(&resolved);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::create_dir_all(&self.root);
        Ok(())
    }
}
