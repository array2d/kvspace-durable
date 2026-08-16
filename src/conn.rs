// conn.rs — 对齐 conn.go：Conn 用 dsn 的 scheme 选择后端。
// 说明：Go 用 registry map 支持动态注册，此处简化为显式 match（后端集固定）。

use crate::backend::Backend;
use crate::kvspace::KVSpace;

/// Conn 用 dsn 创建 KVSpace。默认 scheme 为 redis。
/// 例：conn("redis://127.0.0.1:6379")、conn("fs:///tmp/kvspace")。
pub fn conn(dsn: &str) -> Box<dyn KVSpace> {
    let (scheme, addr) = match dsn.find("://") {
        Some(i) => (&dsn[..i], &dsn[i + 3..]),
        None => ("redis", dsn),
    };
    match scheme {
        "redis" => Box::new(Backend::new(crate::redis::connect(addr))),
        "fs" => Box::new(Backend::new(crate::fs::connect(addr))),
        _ => panic!("kvspace: unknown scheme {:?} in dsn {:?}", scheme, dsn),
    }
}
