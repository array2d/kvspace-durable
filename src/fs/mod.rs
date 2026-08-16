// fs/mod.rs — 文件系统后端（KVStore 原语），root 目录通常指向 /tmp tmpfs。

pub mod store;

pub use store::{connect, FsStore};
