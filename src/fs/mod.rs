// fs/mod.rs — 文件系统后端（结构感知），root 目录通常指向 /tmp tmpfs。

pub mod kvspace;

pub use kvspace::{connect, FsKVSpace};
