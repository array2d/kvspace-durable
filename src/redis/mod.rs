// redis/mod.rs — Redis 后端（KVStore 原语），内置最小 RESP 客户端，零第三方依赖。

pub mod store;

pub use store::{connect, RedisStore};
