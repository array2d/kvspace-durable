// backend.rs — 对齐 redis/kvspace.go 的 KVSpace 实现逻辑，参数化于 KVStore 原语。
// redis 与 fs 后端共用这份逻辑，只替换底层 store。

use std::time::Duration;

use crate::coord::{cmp_coord, grow_coord_dims, is_coord};
use crate::kvspace::{KVPair, KVSpace};
use crate::kvspace_common::{
    dir_exists, get_one, join_path, mk_index_recursive, sep_path, split_index, strip_dir_suf,
    validate_ptr, watch_value,
};
use crate::r#const::*;
use crate::store::KVStore;
use crate::xvalue::{decode_xvalue, decode_xvalue_head, is_none, is_ptr, ptr_target, XValue};
use crate::xvalue_index::{new_ext_index, new_index, new_map_index, new_obj_index};

pub struct Backend<S: KVStore> {
    store: S,
}

impl<S: KVStore> Backend<S> {
    pub fn new(store: S) -> Self {
        Backend { store }
    }

    // ── 目录与路径工具 ──────────────────────────────────────────────

    fn is_dir(path: &str) -> bool {
        path.ends_with(DIR_INDEX_SUF) || path.ends_with(OBJ_SEP)
    }

    fn assert_dir(path: &str) {
        if path != PATH_SEP && !Self::is_dir(path) {
            panic!("{}: {}", ERR_DIR_MUST_END_WITH_SLASH, path);
        }
    }

    fn parent_name(path: &str) -> (String, String) {
        let clean = if Self::is_dir(path) && path != PATH_SEP {
            strip_dir_suf(path)
        } else {
            path
        };
        let (parent, name, _) = split_index(clean);
        (parent, name)
    }

    /// 确保父目录存在：成员目录（尾 ·）建 index，层级目录（尾 /）递归建。
    fn ensure_parent_dir(&mut self, dir: &str) {
        if dir == PATH_SEP {
            return;
        }
        if dir.ends_with(OBJ_SEP) {
            if self.store.get(dir).is_none() {
                self.store.set(dir, &new_index(&[]).encode());
            }
        } else {
            mk_index_recursive(self, dir);
        }
    }

    /// 写成员时兜底容器值链：leaf base + 沿父链全部中间层（object/stringkeymap）。
    /// parent 是尾 · 的成员父目录，name 是该成员名；逐层向上建容器值并注册成员到各自 memindex。
    fn ensure_member_chain(&mut self, parent: &str, name: &str, children: &mut Vec<(String, String)>) {
        let mut dir = parent.to_string();
        let mut child = name.to_string();
        loop {
            let base = strip_dir_suf(&dir).to_string();
            if self.store.get(&base).is_none() {
                if is_coord(&child) {
                    let dims = grow_coord_dims(&[], &[child.clone()]);
                    self.store.set(&base, &new_map_index(&dims).encode());
                } else {
                    self.store.set(&base, &new_obj_index().encode());
                }
            }
            self.ensure_parent_dir(&dir);
            children.push((dir.clone(), child));
            let (dp, dn) = Self::parent_name(&dir);
            if dp == PATH_SEP {
                children.push((PATH_SEP.to_string(), dn));
                break;
            }
            if !dp.ends_with(OBJ_SEP) {
                break;
            }
            dir = dp;
            child = dn;
        }
    }

    // ── link 解析 ───────────────────────────────────────────────────

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
        let dir_suf = Self::is_dir(path) && path != PATH_SEP;
        let clean = if dir_suf {
            strip_dir_suf(path)
        } else {
            path
        };
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
            if let Some(data) = self.store.get(&cur) {
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

    // ── 目录 index 读写 ─────────────────────────────────────────────

    fn read_dir_index(&self, dir: &str) -> Vec<String> {
        match self.store.get(dir) {
            None => Vec::new(),
            Some(data) => {
                let v = decode_xvalue(&data);
                if is_none(&v) {
                    return Vec::new();
                }
                match v {
                    XValue::Index(c) => normalize_children(c),
                    XValue::ExtIndex(e) => e.childs,
                    other => panic!("read_dir_index: unexpected kind {}", other.kind()),
                }
            }
        }
    }

    fn add_child(&self, parent: &str, name: &str) {
        match self.store.get(parent) {
            None => {
                if parent.ends_with(OBJ_SEP) {
                    let v = new_index_for_member(name);
                    self.store.set(parent, &v.encode());
                } else {
                    let v = new_index(&[name.to_string()]);
                    self.store.set(parent, &v.encode());
                }
            }
            Some(data) => {
                let v = decode_xvalue(&data);
                match v {
                    XValue::Index(nodes) => {
                        let mut nodes = normalize_children(nodes);
                        if nodes.iter().any(|n| n == name) {
                            return;
                        }
                        nodes.push(name.to_string());
                        let v = new_index(&nodes);
                        self.store.set(parent, &v.encode());
                    }
                    XValue::ExtIndex(e) => {
                        if e.childs.iter().any(|c| c == name) {
                            return;
                        }
                        let mut childs = e.childs.clone();
                        childs.push(name.to_string());
                        let v = new_ext_index(&childs, &e.ext_path);
                        self.store.set(parent, &v.encode());
                    }
                    other => panic!("add_child: unexpected kind {}", other.kind()),
                }
            }
        }
    }

    fn remove_child(&self, parent: &str, names: &[String]) {
        let is_removed = |n: &str| {
            names
                .iter()
                .any(|name| n == name || n == format!("{}{}", name, DIR_INDEX_SUF))
        };
        match self.store.get(parent) {
            None => {}
            Some(data) => {
                let v = decode_xvalue(&data);
                match v {
                    XValue::Index(nodes) => {
                        let nodes = normalize_children(nodes);
                        let filtered: Vec<String> =
                            nodes.into_iter().filter(|n| !is_removed(n)).collect();
                        let v = new_index(&filtered);
                        self.store.set(parent, &v.encode());
                    }
                    XValue::ExtIndex(e) => {
                        let filtered: Vec<String> =
                            e.childs.into_iter().filter(|n| !is_removed(n)).collect();
                        let v = new_ext_index(&filtered, &e.ext_path);
                        self.store.set(parent, &v.encode());
                    }
                    other => panic!("remove_child: unexpected kind {}", other.kind()),
                }
            }
        }
    }

    // ── Get 内部 ────────────────────────────────────────────────────

    fn get_dir(&self, dir: &str) -> XValue {
        match self.store.get(dir) {
            None => XValue::None,
            Some(data) => decode_xvalue(&data),
        }
    }

    fn prefix_ext(&self, prefix: &str) -> String {
        if let Some(data) = self.store.get(prefix) {
            let head = decode_xvalue_head(&data);
            if head.kind() == KIND_EXT_INDEX {
                let body = head.body(&data);
                return crate::xvalue_index::decode_ext_index(body).ext_path;
            }
        }
        String::new()
    }
}

/// 成员目录（memindex，`·` 结尾）新建时恒为 index；成员顺序/kind 由容器值 object/stringkeymap 决定。
fn new_index_for_member(name: &str) -> XValue {
    let _ = name;
    new_index(&[])
}

fn normalize_children(children: Vec<String>) -> Vec<String> {
    if children.len() == 1 && children[0].is_empty() {
        Vec::new()
    } else {
        children
    }
}

impl<S: KVStore> KVSpace for Backend<S> {
    fn get(&mut self, prefix: &str, keys: &[String], resolve: bool) -> Vec<XValue> {
        Self::assert_dir(prefix);
        let prefix = if resolve {
            self.resolve_path(prefix)
        } else {
            prefix.to_string()
        };
        let ext_t = self.prefix_ext(&prefix);

        let mut results: Vec<Option<XValue>> = vec![None; keys.len()];
        let mut full_keys: Vec<(usize, String)> = Vec::new();
        for (i, k) in keys.iter().enumerate() {
            let full = join_path(&prefix, k);
            if Self::is_dir(&full) {
                results[i] = Some(self.get_dir(&full));
            } else {
                full_keys.push((i, full));
            }
        }
        let full_refs: Vec<&str> = full_keys.iter().map(|(_, f)| f.as_str()).collect();
        let full_vals = self.store.get_many(&full_refs);
        let mut ext_keys: Vec<(usize, String)> = Vec::new();
        for (idx, (i, _)) in full_keys.iter().enumerate() {
            if let Some(data) = &full_vals[idx] {
                results[*i] = Some(decode_xvalue(data));
            } else if !ext_t.is_empty() {
                ext_keys.push((*i, join_path(&ext_t, &keys[*i])));
            }
        }
        if !ext_keys.is_empty() {
            let ext_refs: Vec<&str> = ext_keys.iter().map(|(_, t)| t.as_str()).collect();
            let ext_vals = self.store.get_many(&ext_refs);
            for (idx, (i, _)) in ext_keys.iter().enumerate() {
                results[*i] = Some(if let Some(data) = &ext_vals[idx] {
                    decode_xvalue(data)
                } else {
                    XValue::None
                });
            }
        }
        results
            .into_iter()
            .map(|r| r.unwrap_or(XValue::None))
            .collect()
    }

    fn get_raw(&mut self, key: &str) -> Vec<u8> {
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if let Some(data) = self.store.get(&full) {
            return data;
        }
        let ext_t = self.prefix_ext(&p);
        if !ext_t.is_empty() {
            let ext_full = join_path(&ext_t, &l);
            if let Some(data) = self.store.get(&ext_full) {
                return data;
            }
        }
        Vec::new()
    }

    fn set(&mut self, pairs: &[KVPair]) -> Result<(), String> {
        let mut children: Vec<(String, String)> = Vec::new();

        for p in pairs {
            let resolved = self.resolve_path(&p.key);
            if resolved.contains("//") {
                return Err(format!("Set: double-slash in key {:?}", resolved));
            }
            match &p.val {
                XValue::Index(_) | XValue::ExtIndex(_) => {
                    if !Self::is_dir(&resolved) {
                        return Err(format!(
                            "Set: directory-kind value at non-directory key {:?}",
                            resolved
                        ));
                    }
                }
                _ => {}
            }
            if let XValue::Ptr(ptr) = &p.val {
                validate_ptr(self, &ptr.target, &ptr.kind, ptr.array_len)?;
            }

            // 容器值（object/stringkeymap）：值存 p（无后缀），memindex 存 p·（空 index，成员后续写入维护）。
            if let XValue::Obj | XValue::Map(_) = &p.val {
                let base = if resolved == PATH_SEP {
                    resolved.clone()
                } else {
                    strip_dir_suf(&resolved).to_string()
                };
                let bytes = p.raw.clone().unwrap_or_else(|| p.val.encode());
                let mem = format!("{}{}", base, OBJ_SEP);
                self.store.set(&base, &bytes);
                self.store.set(&mem, &new_index(&[]).encode());
                let (parent, name) = Self::parent_name(&base);
                self.ensure_parent_dir(&parent);
                children.push((parent, name));
                continue;
            }

            if Self::is_dir(&resolved) {
                let (parent, name) = Self::parent_name(&resolved);
                self.ensure_parent_dir(&parent);
                let bytes = p.raw.clone().unwrap_or_else(|| p.val.encode());
                self.store.set(&resolved, &bytes);
                // 成员目录（尾 ·）注册裸 name（memindex 与容器值同名）；层级目录（尾 /）注册 name/。
                let child = if resolved.ends_with(OBJ_SEP) {
                    name
                } else {
                    format!("{}{}", name, DIR_INDEX_SUF)
                };
                children.push((parent, child));
                continue;
            }

            let (parent, name, _) = split_index(&resolved);
            if parent.ends_with(OBJ_SEP) {
                // 沿父链逐层兜底容器值（leaf base + 全部中间层 object/stringkeymap）并注册成员。
                self.ensure_member_chain(&parent, &name, &mut children);
            } else {
                mk_index_recursive(self, &parent);
            }

            // extindex 写保护：只读扩展层上的同名节点禁止写入。
            if let Some(data) = self.store.get(&parent) {
                let head = decode_xvalue_head(&data);
                if head.kind() == KIND_EXT_INDEX {
                    let body = head.body(&data);
                    let ext_t = crate::xvalue_index::decode_ext_index(body).ext_path;
                    let local_nodes = self.read_dir_index(&parent);
                    let local_exists = local_nodes.iter().any(|n| n == &name);
                    if !local_exists {
                        let ext_nodes = self.read_dir_index(&ext_t);
                        if ext_nodes.iter().any(|n| n == &name) {
                            return Err(format!("{}: {}", ERR_EXT_WRITE, resolved));
                        }
                    }
                }
            }

            let bytes = p.raw.clone().unwrap_or_else(|| p.val.encode());
            self.store.set(&resolved, &bytes);
            children.push((parent, name));
        }

        // 按 parent 分组，去重合并 children 进父目录 index。
        let mut parent_children: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for (parent, name) in children {
            parent_children.entry(parent).or_default().push(name);
        }
        for (parent, names) in parent_children {
            let mut nodes: Vec<String> = Vec::new();
            let mut ext_path = String::new();
            let mut is_ext = false;

            if let Some(data) = self.store.get(&parent) {
                let v = decode_xvalue(&data);
                match v {
                    XValue::Index(c) => nodes = normalize_children(c),
                    XValue::ExtIndex(e) => {
                        nodes = e.childs;
                        ext_path = e.ext_path;
                        is_ext = true;
                    }
                    other => panic!("Set parentChildren: unexpected kind {}", other.kind()),
                }
            }

            let mut seen = std::collections::HashSet::new();
            for n in &nodes {
                seen.insert(n.clone());
            }
            for n in &names {
                if !seen.contains(n) {
                    nodes.push(n.clone());
                    seen.insert(n.clone());
                }
            }

            let v = if is_ext {
                new_ext_index(&nodes, &ext_path)
            } else {
                new_index(&nodes)
            };
            self.store.set(&parent, &v.encode());
        }

        Ok(())
    }

    fn list(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> Vec<String> {
        Self::assert_dir(prefix);
        let resolved = if resolve {
            self.resolve_path(prefix)
        } else {
            prefix.to_string()
        };
        if !Self::is_dir(&resolved) {
            return Vec::new();
        }

        let mut members = self.read_dir_index(&resolved);

        // stringkeymap：容器值（无后缀 p）的 kind 决定 row-major 升序。
        if resolved.ends_with(OBJ_SEP) {
            let base = strip_dir_suf(&resolved);
            if let Some(data) = self.store.get(base) {
                if decode_xvalue_head(&data).kind() == KIND_MAP {
                    members.sort_by(|a, b| cmp_coord(a, b));
                }
            }
        }

        let mut ext_members: Vec<String> = Vec::new();
        if expand_ext {
            let ext_t = self.prefix_ext(&resolved);
            if !ext_t.is_empty() {
                ext_members = self.read_dir_index(&ext_t);
            }
        }

        let mut local_set = std::collections::HashSet::new();
        let mut result = Vec::new();
        for m in members {
            local_set.insert(m.clone());
            result.push(m);
        }
        for m in ext_members {
            if local_set.contains(&m) {
                continue;
            }
            result.push(m);
        }
        result
    }

    fn del(&mut self, keys: &[String]) -> Result<(), String> {
        for key in keys {
            let resolved = self.resolve_parent(key);
            let (parent, name) = Self::parent_name(&resolved);

            // extindex 删除保护：只读扩展层上的同名节点禁止删除。
            if let Some(data) = self.store.get(&parent) {
                let head = decode_xvalue_head(&data);
                if head.kind() == KIND_EXT_INDEX {
                    let body = head.body(&data);
                    let ext_t = crate::xvalue_index::decode_ext_index(body).ext_path;
                    let local_nodes = self.read_dir_index(&parent);
                    let local_exists = local_nodes.iter().any(|n| n == &name);
                    if !local_exists {
                        let ext_nodes = self.read_dir_index(&ext_t);
                        if ext_nodes.iter().any(|n| n == &name) {
                            return Err(format!("{}: {}", ERR_EXT_DEL, resolved));
                        }
                    }
                }
            }

            if Self::is_dir(&resolved) {
                let link_key = strip_dir_suf(&resolved);
                self.store.del(&[link_key, &resolved]);
            } else {
                self.store.del(&[&resolved]);
            }
            self.remove_child(&parent, &[name]);
        }
        Ok(())
    }

    fn del_tree(&mut self, prefix: &str) -> Result<(), String> {
        let mut link_key = prefix;
        if Self::is_dir(link_key) && link_key != PATH_SEP {
            link_key = strip_dir_suf(prefix);
        }
        if let Some(data) = self.store.get(link_key) {
            let head = decode_xvalue_head(&data);
            if head.is_ptr() {
                return self.del(&[prefix.to_string()]);
            }
        }

        let resolved = self.resolve_path(prefix);
        // scan 用去尾斜杠/点的前缀：scan_keys 匹配 k[prefix.len()..] 以 '/' 或 '·' 开头，
        // 尾斜杠会使子节点首字符（如 f）落空，导致子树孩子扫不到。
        let mut scan = resolved.clone();
        if Self::is_dir(&scan) && scan != PATH_SEP {
            scan.pop();
        }
        let keys = self.store.scan_keys(&scan);

        self.store.del(&[&resolved]);
        for k in &keys {
            self.store.del(&[k]);
        }

        let (parent, name) = Self::parent_name(&resolved);
        let names = vec![name.clone(), format!("{}{}", name, OBJ_SEP)];
        self.remove_child(&parent, &names);
        Ok(())
    }

    fn watch(&mut self, key: &str, target_value: &XValue, tick_duration: Duration) -> XValue {
        watch_value(self, key, target_value, tick_duration)
    }

    fn mkindex(&mut self, path: &str) -> Result<(), String> {
        if !Self::is_dir(path) {
            return Err(format!("{}: Mkindex {}", ERR_DIR_MUST_END_WITH_SLASH, path));
        }
        let resolved = self.resolve_path(path);

        let trimmed = resolved.trim_matches('/');
        let parts: Vec<&str> = if trimmed.is_empty() {
            Vec::new()
        } else {
            trimmed.split('/').collect()
        };
        let mut cur = PATH_SEP.to_string();
        for p in parts {
            cur = format!("{}{}", join_path(&cur, p), DIR_INDEX_SUF);
            if self.read_dir_index(&cur).is_empty() {
                let (parent, name) = Self::parent_name(&cur);
                self.add_child(&parent, &format!("{}{}", name, DIR_INDEX_SUF));
            }
        }
        Ok(())
    }

    fn ext_index(&mut self, path: &str, ext_path: &str) -> Result<(), String> {
        if !Self::is_dir(path) || !Self::is_dir(ext_path) {
            return Err(format!(
                "{}: ExtIndex path={} extpath={}",
                ERR_DIR_MUST_END_WITH_SLASH, path, ext_path
            ));
        }
        if let Some(data) = self.store.get(ext_path) {
            let head = decode_xvalue_head(&data);
            if head.kind() == KIND_EXT_INDEX {
                return Err(format!("{}: {}", ERR_EXT_CASCADE, ext_path));
            }
        }

        let resolved = self.resolve_parent(path);
        let (parent, name) = Self::parent_name(&resolved);
        self.ensure_parent_dir(&parent);

        let v = new_ext_index(&[], ext_path);
        self.store.set(&resolved, &v.encode());
        self.add_child(&parent, &format!("{}{}", name, DIR_INDEX_SUF));
        Ok(())
    }

    fn del_ext_index(&mut self, path: &str) -> Result<(), String> {
        let resolved = self.resolve_parent(path);

        let mut link_key = resolved.as_str();
        if Self::is_dir(link_key) {
            link_key = strip_dir_suf(&resolved);
        }
        if let Some(data) = self.store.get(link_key) {
            let head = decode_xvalue_head(&data);
            if head.is_ptr() {
                self.store.del(&[link_key]);
                let (parent, name) = Self::parent_name(&resolved);
                self.remove_child(&parent, &[name]);
                return Ok(());
            }
        }

        self.store.del(&[&resolved]);
        let (parent, name) = Self::parent_name(&resolved);
        self.remove_child(&parent, &[name]);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        self.store.flush();
        Ok(())
    }

    fn dis_conn(&mut self) -> Result<(), String> {
        Ok(())
    }
}
