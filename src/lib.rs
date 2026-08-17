#![allow(unused)]

// kvspace-durable — 严格翻译 kvspace-go。
// 文件对应：const.go → const.rs，xvalue*.go → xvalue*.rs，kvspace.go → kvspace.rs，
// kvspace_common.go → kvspace_common.rs，conn.go → conn.rs，redis/kvspace.go → backend.rs + redis/store.rs。
// 后端：redis、fs（goheap/shm 不属本项目）。

pub mod r#const;
pub mod xvalue;
pub mod xvalue_int;
pub mod xvalue_uint;
pub mod xvalue_float;
pub mod xvalue_bool;
pub mod xvalue_byte;
pub mod xvalue_dict;
pub mod xvalue_index;

pub mod kvspace;
pub mod kvspace_common;
pub mod store;
pub mod backend;
pub mod conn;
pub mod fs;
pub mod redis;

pub use r#const::*;
pub use xvalue::*;
pub use xvalue_int::*;
pub use xvalue_uint::*;
pub use xvalue_float::*;
pub use xvalue_bool::*;
pub use xvalue_byte::*;
pub use xvalue_dict::*;
pub use xvalue_index::*;

pub use backend::Backend;
pub use conn::conn;
pub use kvspace::{KVSpace, KVPair};
pub use kvspace_common::*;
pub use store::KVStore;
