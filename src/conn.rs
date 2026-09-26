// conn.rs — 对齐 conn.go：Conn 用 dsn 的 scheme 选择后端。
// 说明：Go 用 registry map 支持动态注册，此处简化为显式 match（后端集固定）。

use crate::backend::Backend;
use crate::kvspace::KVSpace;
use crate::s3::redact_dsn;

/// `$KVSPACE` 环境变量名。`conn("")` 时读它。
pub const ENV_DSN: &str = "KVSPACE";

/// Conn 用 dsn 创建 KVSpace。默认 scheme 为 redis。
/// 例：conn("redis://127.0.0.1:6379")、conn("fs:///tmp/kvspace")、
///     conn("s3://AK:SK@bucket/prefix/?endpoint=cos.ap-beijing.myqcloud.com&region=ap-beijing")。
///
/// 传空串读 `$KVSPACE` —— 让「凭据放在环境里」不必把 DSN 写进命令行参数。
pub fn conn(dsn: &str) -> Box<dyn KVSpace> {
    let owned;
    let dsn = if dsn.is_empty() {
        owned = std::env::var(ENV_DSN).unwrap_or_else(|_| {
            panic!("kvspace: dsn 为空，且环境变量 {} 未设置", ENV_DSN)
        });
        owned.as_str()
    } else {
        dsn
    };

    let (scheme, addr) = match dsn.find("://") {
        Some(i) => (&dsn[..i], &dsn[i + 3..]),
        None => ("redis", dsn),
    };
    match scheme {
        "redis" => Box::new(Backend::new(crate::redis::connect(addr))),
        "fs" => Box::new(crate::fs::connect(addr)),
        // s3 传整串：它的 DSN 带 userinfo 与 query，剥掉 scheme 反而不好解析。
        "s3" => Box::new(Backend::new(crate::s3::connect(dsn))),
        // 脱敏后再打印：s3 的 DSN 里有凭据，原样进日志等于泄密钥。
        _ => panic!(
            "kvspace: unknown scheme {:?} in dsn {:?}",
            scheme,
            redact_dsn(dsn)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空_dsn_读环境变量() {
        // 只验证「设了就认得」；不给环境变量时的 panic 行为不在这里测（会污染测试进程）。
        std::env::set_var(ENV_DSN, "fs:///tmp/kvspace-conn-env-test");
        let _ = conn("");
        std::env::remove_var(ENV_DSN);
    }

    /// `unknown scheme` 的 panic 消息里绝不能出现凭据 —— 那行会进日志。
    #[test]
    fn 未知_scheme_崩掉且不泄露凭据() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // 静音默认的 panic 打印
        let r = std::panic::catch_unwind(|| conn("weird://AK:SECRET@h/p"));
        std::panic::set_hook(prev);

        let msg = r
            .err()
            .and_then(|e| {
                e.downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            })
            .unwrap_or_default();
        assert!(msg.contains("unknown scheme"), "{}", msg);
        assert!(!msg.contains("SECRET"), "panic 消息泄露了凭据: {}", msg);
    }
}
