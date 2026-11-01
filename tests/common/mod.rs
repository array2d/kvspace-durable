// tests/common/mod.rs — 各集成测试共用的小工具。
//
// Backend-independent helpers shared across integration tests; per-suite `run(dsn, root)`
// entry points stay in their respective test files (`cp.rs`, `map_coord.rs`, etc.).

/// Returns the Redis test DSN if reachable, or logs and returns `None` to skip.
/// Tests must never call `clear()` (`FLUSHDB`); always isolate under a unique prefix + `del_tree`.
#[allow(dead_code)]
pub fn redis_test_dsn(label: &str) -> Option<String> {
    let dsn = std::env::var("KVSPACE_TEST_REDIS_DSN")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let host_port = dsn
        .trim_start_matches("redis://")
        .split('/')
        .next()
        .unwrap_or("");
    let addr = host_port.rsplit('@').next().unwrap_or(host_port);
    let reachable = std::net::ToSocketAddrs::to_socket_addrs(&addr)
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|a| {
            std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_millis(300)).is_ok()
        });
    if !reachable {
        eprintln!("skipping redis {label} test: {addr} unreachable");
        return None;
    }
    Some(dsn)
}

/// S3 集成测试的 DSN 与安全守卫。
///
/// **为什么要守卫**：这些语义测试的第一步都是 `kv.clear()`，在 S3 后端上就是
/// `flush()` —— 把前缀之下的对象全删掉。指错前缀就是事故，所以这里硬性要求
/// 前缀里带 `_test`，否则跳过并说明原因。
///
/// 用法：
/// ```text
/// KVSPACE='s3://AK:SK@bucket/_test/run1/?endpoint=…&region=…' \
///   cargo test --test cp -- --ignored --nocapture
/// ```
#[allow(dead_code)]
pub fn s3_test_dsn() -> Option<String> {
    let dsn = match std::env::var("KVSPACE") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("跳过 S3 集成测试：环境变量 KVSPACE 未设置");
            return None;
        }
    };
    if !dsn.starts_with("s3://") {
        eprintln!("跳过 S3 集成测试：$KVSPACE 不是 s3:// 开头的 DSN");
        return None;
    }
    // 前缀是 `@<bucket>/` 与 `?` 之间那一段
    let after_bucket = dsn.split('@').next_back().unwrap_or("");
    let prefix = after_bucket
        .split_once('/')
        .map(|(_, p)| p.split('?').next().unwrap_or(""))
        .unwrap_or("");
    if !prefix.contains("_test") {
        eprintln!(
            "跳过 S3 集成测试：前缀 {:?} 里没有 `_test`。\n\
             这些测试会先 clear()（= 清空该前缀），不允许对着非测试前缀跑。",
            prefix
        );
        return None;
    }
    Some(dsn)
}
