// kvspace.rs — 对齐 kvspace.go：KVSpace 接口 + KVPair

use std::time::Duration;

use crate::xvalue::XValue;

/// KVPair 用于批量写入，顺序确定（非 map）。
pub struct KVPair {
    pub key: String,
    pub val: XValue,
}

/// KVSpace KV 存储接口。
///
/// Watch 语义：阻塞等待 Get(key) == targetValue。
/// 先自旋（无 sleep），随后轮询间隔按指数回退，封顶 tickDuration。
/// 生产者只需 Set(key, targetValue)；无通知队列，跨进程/节点/后端通用。
///
/// 软链接透明穿透：Set 写入 Ptr 值（*kind:target）后，访问 /linkpath/x 透明地访问 target/x。
/// 删除语义例外（POSIX rm 式）：Del/DelTree 的最终组件作用于链接本体，不穿透 target。
pub trait KVSpace {
    /// 单点读：Get 返回完整 XValue，整存整取。
    fn get(&mut self, prefix: &str, keys: &[String], resolve: bool) -> Vec<XValue>;
    /// 单点写：Set 写完整 XValue，并维护目录索引；总是穿透 link 写入 target。
    fn set(&mut self, pairs: &[KVPair]) -> Result<(), String>;

    /// 列目录：resolve 是否穿透 link 列出 target 的子节点。
    fn list(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> Vec<String>;
    /// POSIX rm：最终组件是 link → 删 link 本体。
    fn del(&mut self, keys: &[String]) -> Result<(), String>;
    /// 递归删除；prefix 本身是链接则只删链接。
    fn del_tree(&mut self, prefix: &str) -> Result<(), String>;

    /// 阻塞等待 Get(key)==targetValue。
    fn watch(&mut self, key: &str, target_value: &XValue, tick_duration: Duration) -> XValue;

    /// 递归创建目录，类似 mkdir -p；path 须以 / 结尾。
    fn mkindex(&mut self, path: &str) -> Result<(), String>;

    /// 创建扩展索引，path 为写层，extpath 为只读扩展。
    fn ext_index(&mut self, path: &str, ext_path: &str) -> Result<(), String>;
    /// 移除 extindex。
    fn del_ext_index(&mut self, path: &str) -> Result<(), String>;

    fn clear(&mut self) -> Result<(), String>;
    fn dis_conn(&mut self) -> Result<(), String>;
}
