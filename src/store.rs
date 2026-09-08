// store.rs — 后端存储原语。redis/fs 各自实现，generic backend 只依赖它。

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
    /// 返回所有以 prefix 开头的 key（含 prefix 自身，若存在）。
    fn scan_keys(&self, prefix: &str) -> Vec<String>;
    fn flush(&self);
}
