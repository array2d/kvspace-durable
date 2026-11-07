pub(crate) fn is_subtree_key(prefix: &str, key: &str) -> bool {
    if prefix.is_empty() || prefix == "/" {
        return key.starts_with('/');
    }
    // '/' (hierarchy), '·' (member), and '‥' (runtime system var) bound a subtree.
    key.strip_prefix(prefix).is_some_and(|rest| {
        rest.is_empty() || rest.starts_with('/') || rest.starts_with('·') || rest.starts_with('‥')
    })
}

pub trait KVStore {
    /// 读 key 的原始字节；None = 不存在。
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// 批量读，与 get 顺序对应；None = 不存在。默认逐条 get，后端可覆盖为 MGET。
    fn get_many(&self, keys: &[&str]) -> Vec<Option<Vec<u8>>> {
        keys.iter().map(|k| self.get(k)).collect()
    }
    fn set(&self, key: &str, val: &[u8]);
    fn del(&self, keys: &[&str]);
    fn scan_keys(&self, prefix: &str) -> Vec<String>;
    /// key 是否存在。默认整读判定，后端可覆盖为 EXISTS。
    fn exists(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    /// 定位读 key 的 [off, off+len) 字节；不存在 → None。默认整读后切片，后端可覆盖为 GETRANGE。
    fn get_part(&self, key: &str, off: u32, len: u32) -> Option<Vec<u8>> {
        self.get(key).map(|v| {
            let s = (off as usize).min(v.len());
            let e = (s + len as usize).min(v.len());
            v[s..e].to_vec()
        })
    }
    /// 定位写 buf 到 key 的 [off, off+buf.len())（key 须已存在）。默认整读改写，后端可覆盖为 SETRANGE。
    ///
    /// **越界即失败，不扩容**：原地写的语义是「改已存在的内容」，不是「把值撑大」。
    /// 曾用 `resize(e, 0)` 兜底，于是越界写会静默扩展 wire——读方拿到的
    /// 长度比写方以为的长，而错在写入侧、不在读侧，排查时方向全反。
    fn set_part(&self, key: &str, off: u32, buf: &[u8]) {
        /* 越界判据在 `backend.rs::set_part` 唯一入口，这里只负责写。
         * **不 resize**：越界若走到这里说明入口漏判，宁可什么都不写也不要静默扩容。 */
        let Some(mut v) = self.get(key) else {
            return;
        };
        let s = off as usize;
        let e = s + buf.len();
        if e > v.len() {
            return;
        }
        v[s..e].copy_from_slice(buf);
        self.set(key, &v);
    }
    fn flush(&self);
}

#[cfg(test)]
mod tests {
    use super::is_subtree_key;

    #[test]
    fn subtree_scan_respects_key_boundaries() {
        assert!(is_subtree_key("/proto", "/proto"));
        assert!(is_subtree_key("/proto", "/proto/member"));
        assert!(is_subtree_key("/proto", "/proto·field"));
        assert!(!is_subtree_key("/proto", "/protocol"));
        assert!(is_subtree_key("/", "/proto/member"));
        assert!(is_subtree_key("", "/proto/member"));
    }
}
