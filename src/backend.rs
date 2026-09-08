// backend.rs — 对齐 redis/kvspace.go 的 KVSpace 实现逻辑，参数化于 KVStore 原语。
// redis 与 fs 后端共用这份逻辑，只替换底层 store。

use std::time::Duration;

use crate::coord::{grow_coord_dims, is_coord};
use crate::kvspace::{KVPair, KVSpace};
use crate::kvspace_common::{
    dir_exists, get_one, join_path, mk_index_recursive, sep_path, split_index, strip_dir_suf,
    validate_ptr, watch_value,
};
use crate::r#const::*;
use crate::store::KVStore;
use crate::xvalue::{
    decode_xvalue, decode_xvalue_head, encode_head, is_none, is_ptr, ptr_target, XValue,
};
use crate::xvalue_index::{
    encode_ext_index_grow, encode_index_grow, grow_cap, matrix_cap, matrix_width, new_ext_index,
    new_index, new_map_index, new_obj_index,
};

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
    fn ensure_member_chain(
        &mut self,
        parent: &str,
        name: &str,
        children: &mut Vec<(String, String)>,
    ) {
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
            if dp.ends_with(OBJ_SEP) {
                // 父仍是成员目录：继续沿链上溯（多级 ·）。
                dir = dp;
                child = dn;
                continue;
            }
            // 父是目录（或根）：把成员名 dn 注册进父 index，链到此为止。
            children.push((dp, dn));
            break;
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
        // 一次 MGET 取所有祖先前缀——逐级 store.get 曾对每次读做 O(深度) 次网络往返，
        // 是 durable 上循环性程序 syscall 风暴之源。语义不变：仍返回 root→leaf 最先遇到的 Ptr。
        let mut prefixes: Vec<String> = Vec::with_capacity(parts.len());
        let mut cur = PATH_SEP.to_string();
        for p in &parts {
            cur = join_path(&cur, p);
            prefixes.push(cur.clone());
        }
        let refs: Vec<&str> = prefixes.iter().map(|s| s.as_str()).collect();
        let vals = self.store.get_many(&refs);
        for (i, data) in vals.iter().enumerate() {
            if let Some(data) = data {
                let v = decode_xvalue(data);
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
                let dims = decode_xvalue_head(&data).dims();
                let old_cap = matrix_cap(&dims);
                let old_m = matrix_width(&dims);
                let v = decode_xvalue(&data);
                match v {
                    XValue::Index(nodes) => {
                        let mut nodes = normalize_children(nodes);
                        if nodes.iter().any(|n| n == name) {
                            return;
                        }
                        nodes.push(name.to_string());
                        let cap = grow_cap(old_cap, nodes.len());
                        let (d, b) = encode_index_grow(&nodes, cap, old_m);
                        self.store.set(parent, &encode_head(KIND_INDEX, 0, &d, &b));
                    }
                    XValue::ExtIndex(e) => {
                        if e.childs.iter().any(|c| c == name) {
                            return;
                        }
                        let mut childs = e.childs.clone();
                        childs.push(name.to_string());
                        let cap = grow_cap(old_cap, childs.len());
                        let (d, b) = encode_ext_index_grow(&e.ext_path, &childs, cap, old_m);
                        self.store
                            .set(parent, &encode_head(KIND_EXT_INDEX, 0, &d, &b));
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
                let dims = decode_xvalue_head(&data).dims();
                let old_cap = matrix_cap(&dims); // 删除不缩 cap
                let old_m = matrix_width(&dims);
                let v = decode_xvalue(&data);
                match v {
                    XValue::Index(nodes) => {
                        let nodes = normalize_children(nodes);
                        let filtered: Vec<String> =
                            nodes.into_iter().filter(|n| !is_removed(n)).collect();
                        let (d, b) = encode_index_grow(&filtered, old_cap, old_m);
                        self.store.set(parent, &encode_head(KIND_INDEX, 0, &d, &b));
                    }
                    XValue::ExtIndex(e) => {
                        let filtered: Vec<String> =
                            e.childs.into_iter().filter(|n| !is_removed(n)).collect();
                        let (d, b) = encode_ext_index_grow(&e.ext_path, &filtered, old_cap, old_m);
                        self.store
                            .set(parent, &encode_head(KIND_EXT_INDEX, 0, &d, &b));
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
                return crate::xvalue_index::decode_ext_index(body, &head.dims()).ext_path;
            }
        }
        String::new()
    }

    /// listlen/listat O(1) 快路径的取值口：仅纯 index memindex 返 (dims=[N,M], body=N×M 矩阵)。
    /// ext_index（body 头部含 ext_path）与非目录/非 index 返 None，交回退全量 list()。
    fn index_head_body(&mut self, prefix: &str, resolve: bool) -> Option<(Vec<i32>, Vec<u8>)> {
        let resolved = if resolve {
            self.resolve_path(prefix)
        } else {
            prefix.to_string()
        };
        if !Self::is_dir(&resolved) {
            return None;
        }
        let data = self.store.get(&resolved)?;
        let head = decode_xvalue_head(&data);
        if head.kind() == KIND_INDEX {
            Some((head.dims(), head.body(&data).to_vec()))
        } else {
            None
        }
    }
}

/// 成员目录（memindex，`·` 结尾）新建时注册首个成员；成员顺序/kind 由容器值 object/stringkeymap 决定。
fn new_index_for_member(name: &str) -> XValue {
    new_index(&[name.to_string()])
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
        // ext-index 仅是主存未命中键的回退目标：延迟到确有缺失键才查 prefix_ext——
        // 否则每次读都为它多付一次 GET（frame 槽恒命中，曾是最热的一条无谓往返）。
        let mut missing: Vec<usize> = Vec::new();
        for (idx, (i, _)) in full_keys.iter().enumerate() {
            if let Some(data) = &full_vals[idx] {
                results[*i] = Some(decode_xvalue(data));
            } else {
                missing.push(*i);
            }
        }
        let ext_t = if missing.is_empty() {
            String::new()
        } else {
            self.prefix_ext(&prefix)
        };
        let ext_keys: Vec<(usize, String)> = if ext_t.is_empty() {
            Vec::new()
        } else {
            missing
                .iter()
                .map(|&i| (i, join_path(&ext_t, &keys[i])))
                .collect()
        };
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

    fn get_part(&mut self, key: &str, off: u32, len: u32) -> Vec<u8> {
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if self.store.exists(&full) {
            return self.store.get_part(&full, off, len).unwrap_or_default();
        }
        let ext_t = self.prefix_ext(&p);
        if !ext_t.is_empty() {
            let ext_full = join_path(&ext_t, &l);
            if self.store.exists(&ext_full) {
                return self.store.get_part(&ext_full, off, len).unwrap_or_default();
            }
        }
        Vec::new()
    }

    fn set_part(&mut self, key: &str, off: u32, buf: &[u8]) -> Result<(), String> {
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if self.store.exists(&full) {
            self.store.set_part(&full, off, buf);
            return Ok(());
        }
        let ext_t = self.prefix_ext(&p);
        if !ext_t.is_empty() {
            let ext_full = join_path(&ext_t, &l);
            if self.store.exists(&ext_full) {
                self.store.set_part(&ext_full, off, buf);
                return Ok(());
            }
        }
        Err(format!("set_part: missing key {}", key))
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
                validate_ptr(self, &ptr.target, &ptr.target_kindexpr)?;
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
                    let ext_t = crate::xvalue_index::decode_ext_index(body, &head.dims()).ext_path;
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
            let mut old_cap = 0usize;
            let mut old_m = 0usize;

            if let Some(data) = self.store.get(&parent) {
                let dims = decode_xvalue_head(&data).dims();
                old_cap = matrix_cap(&dims);
                old_m = matrix_width(&dims);
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

            let mut seen: std::collections::HashSet<String> = nodes.iter().cloned().collect();
            for n in &names {
                if seen.insert(n.clone()) {
                    nodes.push(n.clone());
                }
            }

            let cap = grow_cap(old_cap, nodes.len());
            if is_ext {
                let (d, b) = encode_ext_index_grow(&ext_path, &nodes, cap, old_m);
                self.store
                    .set(&parent, &encode_head(KIND_EXT_INDEX, 0, &d, &b));
            } else {
                let (d, b) = encode_index_grow(&nodes, cap, old_m);
                self.store.set(&parent, &encode_head(KIND_INDEX, 0, &d, &b));
            }
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

        // map 成员在 add_child 时已按坐标 row-major 有序存储，list/listat 一律信任存储序，无读时排序。
        let members = self.read_dir_index(&resolved);

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

    /// O(1) 覆写：无 ext 展开的 index memindex 直接读 head dims[0]=N；ext/其余回退全量。
    fn list_len(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> i32 {
        if !expand_ext {
            if let Some((dims, _)) = self.index_head_body(prefix, resolve) {
                return crate::xvalue_index::matrix_count(&dims) as i32;
            }
        }
        self.list(prefix, expand_ext, resolve).len() as i32
    }

    /// O(1) 覆写：无 ext 展开的 index memindex 取矩阵第 idx 行；ext/其余回退全量。
    fn list_at(
        &mut self,
        prefix: &str,
        idx: i32,
        expand_ext: bool,
        resolve: bool,
    ) -> Option<String> {
        if idx < 0 {
            return None;
        }
        if !expand_ext {
            if let Some((dims, body)) = self.index_head_body(prefix, resolve) {
                if idx as usize >= crate::xvalue_index::matrix_count(&dims) {
                    return None;
                }
                return crate::xvalue_index::matrix_at(
                    &body,
                    crate::xvalue_index::matrix_width(&dims),
                    idx as usize,
                );
            }
        }
        self.list(prefix, expand_ext, resolve)
            .into_iter()
            .nth(idx as usize)
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
                    let ext_t = crate::xvalue_index::decode_ext_index(body, &head.dims()).ext_path;
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

    /// 单 key 拷贝：src 处 XValue（head+body 原样）写到 dst，并注册进 dst 父 index；不触碰 src·/成员。
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

    /// 递归子树拷贝：以 src 为根，把整棵物理子树（base + 所有 ·/ 后代 key）字节级重映射到 dst。
    /// extindex 成员的 marker（含 ext_path）原样复制 → 在 dst 侧生成指向同一只读扩展的新 extindex。
    fn cp_tree(&mut self, src: &str, dst: &str) -> Result<(), String> {
        let src_res = self.resolve_path(src);
        let dst_res = self.resolve_path(dst);
        let mut src_scan = src_res.clone();
        if Self::is_dir(&src_scan) && src_scan != PATH_SEP {
            src_scan.pop();
        }
        let mut dst_base = dst_res.clone();
        if Self::is_dir(&dst_base) && dst_base != PATH_SEP {
            dst_base.pop();
        }
        if src_scan == dst_base {
            return Ok(());
        }
        let keys = self.store.scan_keys(&src_scan);
        if keys.is_empty() {
            return Err(format!("CpTree: source not found: {}", src));
        }
        // 覆盖语义确定：先清 dst 既有子树（也从父 index 摘除，随后重新登记）。
        let _ = self.del_tree(&dst_base);
        for k in &keys {
            let suffix = &k[src_scan.len()..];
            let new_key = format!("{}{}", dst_base, suffix);
            if let Some(data) = self.store.get(k) {
                self.store.set(&new_key, &data);
            }
        }
        // 按 dst 根物理形态登记进父 index（base 值 / 层级目录 / 成员目录）。
        let (parent, name) = Self::parent_name(&dst_base);
        self.ensure_parent_dir(&parent);
        if self.store.get(&dst_base).is_some() {
            self.add_child(&parent, &name);
        } else if self
            .store
            .get(&format!("{}{}", dst_base, DIR_INDEX_SUF))
            .is_some()
        {
            self.add_child(&parent, &format!("{}{}", name, DIR_INDEX_SUF));
        } else if self
            .store
            .get(&format!("{}{}", dst_base, OBJ_SEP))
            .is_some()
        {
            self.add_child(&parent, &format!("{}{}", name, OBJ_SEP));
        }
        Ok(())
    }

    /// 浅拷贝：base 值 + 一层 · 成员（不递归成员子树、不遍历 / 子节点）。用于单 struct/扁平容器。
    fn cp_list(&mut self, src: &str, dst: &str) -> Result<(), String> {
        let mut src_base = self.resolve_path(src);
        if Self::is_dir(&src_base) && src_base != PATH_SEP {
            src_base.pop();
        }
        let mut dst_base = self.resolve_path(dst);
        if Self::is_dir(&dst_base) && dst_base != PATH_SEP {
            dst_base.pop();
        }
        if src_base == dst_base {
            return Ok(());
        }
        let keys = self.store.scan_keys(&src_base);
        if keys.is_empty() {
            return Err(format!("CpList: source not found: {}", src));
        }
        let dst_mem = format!("{}{}", dst_base, OBJ_SEP);
        let _ = self.del_tree(&dst_base);
        for k in &keys {
            let suffix = &k[src_base.len()..];
            // 一层：base 自身、memindex 标记、无更深分隔的直接 · 成员；跳过 / 子节点与更深后代。
            let one_level = suffix.is_empty()
                || (suffix.starts_with(OBJ_SEP) && {
                    let rest = &suffix[OBJ_SEP.len()..];
                    !rest.contains(OBJ_SEP) && !rest.contains(PATH_SEP)
                });
            if !one_level {
                continue;
            }
            if let Some(data) = self.store.get(k) {
                self.store.set(&format!("{}{}", dst_base, suffix), &data);
            }
        }
        let (parent, name) = Self::parent_name(&dst_base);
        self.ensure_parent_dir(&parent);
        if self.store.get(&dst_base).is_some() {
            self.add_child(&parent, &name);
        } else if self.store.get(&dst_mem).is_some() {
            self.add_child(&parent, &format!("{}{}", name, OBJ_SEP));
        }
        Ok(())
    }

    fn watch(&mut self, key: &str, target_value: &XValue, tick_duration: Duration) -> XValue {
        watch_value(self, key, target_value, tick_duration)
    }

    fn mkindex(&mut self, path: &str, capacity: u32) -> Result<(), String> {
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
        // 叶目录预留容量：存 [0,cap,0]，首成员插入时 grow_cap(cap,1)=cap 物化 cap×M，不重分配。
        if capacity > 0 && cur != PATH_SEP {
            let nodes = self.read_dir_index(&cur);
            let old_m = self
                .store
                .get(&cur)
                .map(|d| matrix_width(&decode_xvalue_head(&d).dims()))
                .unwrap_or(0);
            let cap = grow_cap(capacity as usize, nodes.len());
            let (d, b) = encode_index_grow(&nodes, cap, old_m);
            self.store.set(&cur, &encode_head(KIND_INDEX, 0, &d, &b));
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
