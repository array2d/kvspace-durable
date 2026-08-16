// store.rs — 后端存储原语。redis/fs 各自实现，generic backend 只依赖它。

/// 单 key 字节级存储原语（无目录索引、无 link 语义，纯 get/set/del/scan/flush）。
pub trait KVStore {
    /// 读 key 的原始字节；None = 不存在。
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn set(&self, key: &str, val: &[u8]);
    fn del(&self, keys: &[&str]);
    /// 返回所有以 prefix 开头的 key（含 prefix 自身，若存在）。
    fn scan_keys(&self, prefix: &str) -> Vec<String>;
    fn flush(&self);
}
