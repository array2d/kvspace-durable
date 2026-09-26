// s3/mod.rs — S3 兼容对象存储后端（腾讯 COS / AWS S3）。
//
// 与其他两个后端不同的两点，都是刻意的：
//   1. **有第三方依赖**（ureq + sha2 + hmac）。HTTPS 绕不过 TLS，而 std 没有 TLS。
//   2. **凭据在 DSN 里**，所以 DSN 不能原样打印 —— 一切打印走 `redact_dsn`。

pub mod dsn;
pub mod sigv4;
pub mod store;

pub use dsn::redact as redact_dsn;
pub use store::{connect, S3Store};
