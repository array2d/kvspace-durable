// store.rs — 后端存储原语。redis/fs 各自实现，generic backend 只依赖它。
//
// 注意这里**没有** `scan_keys` 这类「前缀扫描」原语。子树枚举由 `Backend::collect_subtree`
// 沿目录索引递归完成 —— 后端只需要 get/set/del 这几个点操作。
// 这样 S3 后端不必依赖 `ListObjects(Prefix=)`（那是裸字节前缀，与 KVSpace 要的词边界
// 匹配不是一回事），也不必为「有没有前缀扫描能力」做适配。

/// 单 key 字节级存储原语（无目录索引、无 link 语义，纯 get/set/del/scan/flush）。
pub trait KVStore {
    /// 读 key 的原始字节；None = 不存在。
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// 批量读，与 get 顺序对应；None = 不存在。默认逐条 get，后端可覆盖为 MGET。
    fn get_many(&self, keys: &[&str]) -> Vec<Option<Vec<u8>>> {
        keys.iter().map(|k| self.get(k)).collect()
    }
    fn set(&self, key: &str, val: &[u8]);
    fn del(&self, keys: &[&str]);
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
    fn set_part(&self, key: &str, off: u32, buf: &[u8]) {
        if let Some(mut v) = self.get(key) {
            let s = off as usize;
            let e = s + buf.len();
            if e > v.len() {
                v.resize(e, 0);
            }
            v[s..e].copy_from_slice(buf);
            self.set(key, &v);
        }
    }
    fn flush(&self);
}
