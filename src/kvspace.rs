// kvspace.rs — 对齐 kvspace.go：KVSpace 接口 + KVPair

use std::time::Duration;

use crate::xvalue::XValue;

/// KVPair 用于批量写入，顺序确定（非 map）。
pub struct KVPair {
    pub key: String,
    pub val: XValue,
    pub raw: Option<Vec<u8>>, // 原始 TLV 字节：Set 透传，保留 head 权限位（ro/vid）
}

/// KVSpace KV 存储接口。
///
/// Watch 语义：阻塞等待 Get(key) == targetValue。
/// 先自旋（无 sleep），随后轮询间隔按指数回退，封顶 tickDuration。
/// 生产者只需 Set(key, targetValue)；无通知队列，跨进程/节点/后端通用。
///
/// 路径寻址穿透：Set 写入 Ptr 值（*target_kindexpr，body=target）后，访问 /ptrpath/x 经目录前缀
/// 解析定位到 target/x（仅寻址，单跳；解引用取值只一跳，不连续追 Ptr 链）。
/// 删除语义例外（POSIX rm 式）：Del/DelTree 的最终组件作用于指针本体，不穿透 target。
pub trait KVSpace {
    /// 校验目录前缀（/、或以 / 或 · 结尾）。非法返回 Err，供 C ABI 边界在调用前短路。
    fn validate_dir(&self, path: &str) -> Result<(), String> {
        if path == crate::r#const::PATH_SEP
            || path.ends_with(crate::r#const::DIR_INDEX_SUF)
            || path.ends_with(crate::r#const::OBJ_SEP)
        {
            Ok(())
        } else {
            Err(format!(
                "{}: {}",
                crate::r#const::ERR_DIR_MUST_END_WITH_SLASH,
                path
            ))
        }
    }

    /// 单点读：Get 返回完整 XValue，整存整取。
    fn get(&mut self, prefix: &str, keys: &[String], resolve: bool) -> Vec<XValue>;
    /// 单点读原始字节（不 decode/re-encode，保 head 权限位 ro/vid）。无值返回空。
    fn get_raw(&mut self, key: &str) -> Vec<u8>;
    /// 单点写：Set 写完整 XValue，并维护目录索引；总是穿透 link 写入 target。
    fn set(&mut self, pairs: &[KVPair]) -> Result<(), String>;

    /// 列目录：resolve 是否穿透 link 列出 target 的子节点。
    fn list(&mut self, prefix: &str, expand_ext: bool, resolve: bool) -> Vec<String>;
    /// POSIX rm：最终组件是 link → 删 link 本体。
    fn del(&mut self, keys: &[String]) -> Result<(), String>;
    /// 递归删除；prefix 本身是链接则只删链接。
    fn del_tree(&mut self, prefix: &str) -> Result<(), String>;

    /// 单 key 拷贝（src → dst），XValue 原样，不含成员子树。对齐 unix cp。
    fn cp(&mut self, src: &str, dst: &str) -> Result<(), String>;
    /// 递归拷贝以 src 为根的整棵子树到 dst（含 memindex 与全部成员）。对齐 unix cp -r。
    /// extindex 成员复制其扩展句柄 → dst 侧生成指向同一只读扩展的新 extindex。
    fn cp_tree(&mut self, src: &str, dst: &str) -> Result<(), String>;

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
