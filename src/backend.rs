// backend.rs — 对齐 redis/kvspace.go 的 KVSpace 实现逻辑，参数化于 KVStore 原语。
// redis 与 fs 后端共用这份逻辑，只替换底层 store。

use std::time::Duration;

use crate::kvspace::{KVPair, KVSpace};
use crate::kvspace_common::{
    is_descendant, is_frame_operand_key, join_path, sep_path, split_index, strip_dir_suf,
    validate_ptr, watch_value,
};
use crate::r#const::*;
use crate::store::KVStore;
use crate::xvalue::{decode_xvalue, decode_xvalue_head, is_ptr, ptr_target, XValue, REF_EXT};

pub struct Backend<S: KVStore> {
    store: S,
}

impl<S: KVStore> Backend<S> {
    pub fn new(store: S) -> Self {
        Backend { store }
    }

    fn sync_metadata(&mut self, key: &str, raw: &[u8]) -> Result<(), String> {
        let head = decode_xvalue_head(raw);
        if !raw.is_empty() && head.headlen == 0 {
            return Err(format!("invalid XValue at {key}"));
        }
        self.set_metadata(key, head.ro, head.vid)
    }

    fn metadata_at(&self, key: &str) -> Result<(bool, u32), String> {
        let meta =
            crate::metadata::key_for(key).ok_or_else(|| format!("reserved metadata key: {key}"))?;
        match self.store.get(&meta) {
            None => Ok((false, 0)),
            Some(data) => {
                crate::metadata::decode(&data).ok_or_else(|| format!("invalid metadata at {key}"))
            }
        }
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
        let mut names = std::collections::HashMap::<String, bool>::new();
        let scan_prefix = dir
            .strip_suffix('/')
            .or_else(|| dir.strip_suffix('·'))
            .unwrap_or(dir);
        for key in self.store.scan_keys(scan_prefix) {
            let Some(rest) = key.strip_prefix(dir) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let end = rest.find(['/', '·']).unwrap_or(rest.len());
            if end > 0 {
                let name = &rest[..end];
                if dir != PATH_SEP || name != META_ROOT_NAME {
                    let direct_dir =
                        rest.as_bytes().get(end) == Some(&b'/') && rest.len() == end + 1;
                    names
                        .entry(name.to_string())
                        .and_modify(|v| *v |= direct_dir)
                        .or_insert(direct_dir);
                }
            }
        }
        let mut result: Vec<String> = names
            .into_iter()
            .map(
                |(name, direct_dir)| {
                    if direct_dir {
                        format!("{name}/")
                    } else {
                        name
                    }
                },
            )
            .collect();
        result.sort_by(|a, b| crate::coord::cmp_coord(a, b));
        result
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
            if head.r#ref == REF_EXT {
                return String::from_utf8_lossy(head.body(&data)).into_owned();
            }
        }
        String::new()
    }
}

impl<S: KVStore> KVSpace for Backend<S> {
    fn set_metadata(&mut self, key: &str, ro: bool, vid: u32) -> Result<(), String> {
        let meta =
            crate::metadata::key_for(key).ok_or_else(|| format!("reserved metadata key: {key}"))?;
        if ro || vid != 0 {
            let value = crate::metadata::encode(ro, vid)
                .ok_or_else(|| format!("cannot encode metadata at {key}"))?;
            self.store.set(&meta, &value);
        } else {
            self.store.del(&[&meta]);
        }
        Ok(())
    }
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
            if crate::metadata::is_reserved(&full) {
                results[i] = Some(XValue::None);
                continue;
            }
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
        if crate::metadata::is_reserved(key) {
            return Vec::new();
        }
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if crate::metadata::is_reserved(&full) {
            return Vec::new();
        }
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

    fn get_metadata(&mut self, key: &str) -> Result<(bool, u32), String> {
        let resolved = self.resolve_path(key);
        self.metadata_at(&resolved)
    }

    fn get_part(&mut self, key: &str, off: u32, len: u32) -> Vec<u8> {
        if crate::metadata::is_reserved(key) {
            return Vec::new();
        }
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if crate::metadata::is_reserved(&full) {
            return Vec::new();
        }
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
        if crate::metadata::is_reserved(key) {
            return Err(format!("reserved metadata key: {key}"));
        }
        let (mut p, l) = sep_path(key);
        if p != PATH_SEP {
            p.push_str(DIR_INDEX_SUF);
        }
        let p = self.resolve_path(&p);
        let full = join_path(&p, &l);
        if crate::metadata::is_reserved(&full) {
            return Err(format!("reserved metadata key: {full}"));
        }
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
        for pair in pairs {
            if crate::metadata::is_reserved(&pair.key) {
                return Err(format!("reserved metadata key: {}", pair.key));
            }
            let key = self.resolve_parent(&pair.key);
            if key.contains("//") || crate::metadata::is_reserved(&key) {
                return Err(format!("invalid key: {key}"));
            }
            let raw = pair.raw.clone().unwrap_or_else(|| pair.val.encode());
            let head = crate::headlenpow::decode(&raw)
                .ok_or_else(|| format!("invalid XValue at {key}"))?;
            if head.total != raw.len() {
                return Err(format!("invalid XValue at {key}"));
            }
            if let XValue::Ptr(ptr) = decode_xvalue(&raw) {
                validate_ptr(self, &ptr.target, &ptr.target_kindexpr)?;
            }
            let (parent, _, _) = split_index(&key);
            if parent.ends_with(OBJ_SEP) {
                let base = strip_dir_suf(&parent);
                if !base.starts_with("/lib") && self.store.get(base).is_none() {
                    return Err(format!(
                        "{}: memhead {} does not exist — declare the container first",
                        ERR_MEMHEAD_MISSING, base
                    ));
                }
            }
            let ext_target = self.prefix_ext(&parent);
            if !ext_target.is_empty() && self.store.get(&key).is_none() {
                let suffix = key.strip_prefix(&parent).unwrap_or("");
                if self.store.get(&format!("{ext_target}{suffix}")).is_some()
                    && !(head.flags & 4 != 0 && is_frame_operand_key(&key))
                {
                    return Err(format!("{}: {}", ERR_EXT_WRITE, key));
                }
            }
            self.sync_metadata(&key, &raw)?;
            self.store.set(&key, &raw);
        }
        Ok(())
    }

    fn list(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> Vec<String> {
        if crate::metadata::is_reserved(prefix) {
            return Vec::new();
        }
        Self::assert_dir(prefix);
        let resolved = if resolve {
            self.resolve_path(prefix)
        } else {
            prefix.to_string()
        };
        if !Self::is_dir(&resolved) {
            return Vec::new();
        }
        if crate::metadata::is_reserved(&resolved) {
            return Vec::new();
        }

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
        result.sort_by(|a, b| crate::coord::cmp_coord(a, b));
        result
    }

    fn list_len(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> i32 {
        self.list(prefix, expand_ext, resolve).len() as i32
    }

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
        self.list(prefix, expand_ext, resolve)
            .into_iter()
            .nth(idx as usize)
    }

    fn del(&mut self, keys: &[String]) -> Result<(), String> {
        for key in keys {
            if crate::metadata::is_reserved(key) {
                return Err(format!("reserved metadata key: {key}"));
            }
            let resolved = self.resolve_parent(key);
            if crate::metadata::is_reserved(&resolved) {
                return Err(format!("reserved metadata key: {resolved}"));
            }
            let (parent, _, _) = split_index(&resolved);
            let ext_target = self.prefix_ext(&parent);
            if !ext_target.is_empty() && self.store.get(&resolved).is_none() {
                let suffix = resolved.strip_prefix(&parent).unwrap_or("");
                if self.store.get(&format!("{ext_target}{suffix}")).is_some() {
                    return Err(format!("{}: {}", ERR_EXT_DEL, resolved));
                }
            }

            if Self::is_dir(&resolved) {
                let link_key = strip_dir_suf(&resolved);
                self.store.del(&[link_key, &resolved]);
                self.sync_metadata(link_key, &[])?;
                self.sync_metadata(&resolved, &[])?;
            } else {
                self.store.del(&[&resolved]);
                self.sync_metadata(&resolved, &[])?;
            }
        }
        Ok(())
    }

    fn del_tree(&mut self, prefix: &str) -> Result<(), String> {
        if crate::metadata::is_reserved(prefix) {
            return Err(format!("reserved metadata key: {prefix}"));
        }
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
        if crate::metadata::is_reserved(&resolved) {
            return Err(format!("reserved metadata key: {resolved}"));
        }
        let mut scan = resolved.clone();
        if Self::is_dir(&scan) && scan != PATH_SEP {
            scan.pop();
        }
        let keys: Vec<_> = self
            .store
            .scan_keys(&scan)
            .into_iter()
            .filter(|key| !crate::metadata::is_reserved(key))
            .collect();

        self.store.del(&[&resolved]);
        self.sync_metadata(&resolved, &[])?;
        for k in &keys {
            self.store.del(&[k]);
            self.sync_metadata(k, &[])?;
        }

        Ok(())
    }

    /// 单 key 拷贝：src 处 XValue（head+body 原样）写到 dst，并注册进 dst 父 index；不触碰 src·/成员。
    fn cp(&mut self, src: &str, dst: &str) -> Result<(), String> {
        if crate::metadata::is_reserved(src) || crate::metadata::is_reserved(dst) {
            return Err("reserved metadata key".into());
        }
        let raw = self.get_raw(src);
        if raw.is_empty() {
            return Err(format!("Cp: source not found: {}", src));
        }
        let (ro, vid) = self.metadata_at(src)?;
        let v = decode_xvalue(&raw);
        self.set(&[KVPair {
            key: dst.to_string(),
            val: v,
            raw: Some(raw),
        }])?;
        self.set_metadata(dst, ro, vid)
    }

    /// 递归子树拷贝：以 src 为根，把整棵物理子树（base + 所有 ·/ 后代 key）字节级重映射到 dst。
    /// extindex 成员的 marker（含 ext_path）原样复制 → 在 dst 侧生成指向同一只读扩展的新 extindex。
    fn cp_tree(&mut self, src: &str, dst: &str) -> Result<(), String> {
        if crate::metadata::is_reserved(src) || crate::metadata::is_reserved(dst) {
            return Err("reserved metadata key".into());
        }
        let src_res = self.resolve_path(src);
        let dst_res = self.resolve_path(dst);
        if crate::metadata::is_reserved(&src_res) || crate::metadata::is_reserved(&dst_res) {
            return Err("reserved metadata key".into());
        }
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
        if is_descendant(&src_scan, &dst_base) {
            return Err("CpTree: destination is inside source".into());
        }
        let keys: Vec<_> = self
            .store
            .scan_keys(&src_scan)
            .into_iter()
            .filter(|key| !crate::metadata::is_reserved(key))
            .collect();
        if keys.is_empty() {
            return Err(format!("CpTree: source not found: {}", src));
        }
        // Replace the destination subtree.
        let _ = self.del_tree(&dst_base);
        for k in &keys {
            let suffix = &k[src_scan.len()..];
            let new_key = format!("{}{}", dst_base, suffix);
            if let Some(data) = self.store.get(k) {
                let (ro, vid) = self.metadata_at(k)?;
                self.store.set(&new_key, &data);
                self.set_metadata(&new_key, ro, vid)?;
            }
        }
        Ok(())
    }

    /// 浅拷贝：base 值 + 一层 · 成员（不递归成员子树、不遍历 / 子节点）。用于单 struct/扁平容器。
    fn cp_list(&mut self, src: &str, dst: &str) -> Result<(), String> {
        if crate::metadata::is_reserved(src) || crate::metadata::is_reserved(dst) {
            return Err("reserved metadata key".into());
        }
        let mut src_base = self.resolve_path(src);
        if Self::is_dir(&src_base) && src_base != PATH_SEP {
            src_base.pop();
        }
        let mut dst_base = self.resolve_path(dst);
        if crate::metadata::is_reserved(&src_base) || crate::metadata::is_reserved(&dst_base) {
            return Err("reserved metadata key".into());
        }
        if Self::is_dir(&dst_base) && dst_base != PATH_SEP {
            dst_base.pop();
        }
        if src_base == dst_base {
            return Ok(());
        }
        let keys: Vec<_> = self
            .store
            .scan_keys(&src_base)
            .into_iter()
            .filter(|key| !crate::metadata::is_reserved(key))
            .collect();
        if keys.is_empty() {
            return Err(format!("CpList: source not found: {}", src));
        }
        let _ = self.del_tree(&dst_base);
        for k in &keys {
            let suffix = &k[src_base.len()..];
            // Copy the base and direct members, including ext directories.
            let one_level = suffix.is_empty()
                || (suffix.starts_with(OBJ_SEP) && {
                    let rest = &suffix[OBJ_SEP.len()..];
                    let member = rest.strip_suffix(PATH_SEP).unwrap_or(rest);
                    !member.is_empty() && !member.contains(OBJ_SEP) && !member.contains(PATH_SEP)
                });
            if !one_level {
                continue;
            }
            if let Some(data) = self.store.get(k) {
                let new_key = format!("{}{}", dst_base, suffix);
                let (ro, vid) = self.metadata_at(k)?;
                self.store.set(&new_key, &data);
                self.set_metadata(&new_key, ro, vid)?;
            }
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
        let _ = capacity;
        let resolved = self.resolve_path(path);
        let value = crate::headlenpow::encode(5, 0, 0, 0, "lib", &[], 0)
            .ok_or_else(|| format!("Mkindex: invalid directory {resolved}"))?;
        if self.store.get(&resolved).is_none() {
            self.store.set(&resolved, &value);
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
        if !self.prefix_ext(ext_path).is_empty() {
            return Err(format!("{}: {}", ERR_EXT_CASCADE, ext_path));
        }

        let resolved = self.resolve_parent(path);
        let source = self
            .store
            .get(ext_path)
            .ok_or_else(|| format!("ExtIndex target missing: {ext_path}"))?;
        let langtype = decode_xvalue_head(&source).langtype;
        let pow = (5..=31)
            .find(|&p| 18 + langtype.len() <= 1usize << p)
            .ok_or_else(|| format!("ExtIndex type too long: {langtype}"))?;
        let len = ext_path.len() as u64;
        let value = crate::headlenpow::encode(
            pow,
            3,
            len,
            len,
            &langtype,
            ext_path.as_bytes(),
            ext_path.len(),
        )
        .ok_or_else(|| format!("ExtIndex locator invalid: {ext_path}"))?;
        self.store.set(&resolved, &value);
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
                self.set_metadata(link_key, false, 0)?;
                return Ok(());
            }
        }

        self.store.del(&[&resolved]);
        self.set_metadata(&resolved, false, 0)?;
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        self.store.flush();
        Ok(())
    }
}
