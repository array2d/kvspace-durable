#![allow(unused)]
#![allow(non_snake_case)]

// kvspace-durable — 严格翻译 kvspace-go。
// 文件对应：const.go → const.rs，xvalue*.go → xvalue*.rs，kvspace.go → kvspace.rs，
// kvspace_common.go → kvspace_common.rs，conn.go → conn.rs，redis/kvspace.go → backend.rs + redis/store.rs。
// 后端：redis、fs（goheap/shm 不属本项目）。

pub mod r#const;
pub mod coord;
pub mod xvalue;
pub mod xvalue_bool;
pub mod xvalue_byte;
pub mod xvalue_float;
pub mod xvalue_index;
pub mod xvalue_int;
pub mod xvalue_obj;
pub mod xvalue_uint;

pub mod backend;
pub mod conn;
pub mod ffi;
pub mod fs;
pub mod kvspace;
pub mod kvspace_common;
pub mod redis;
pub mod store;

pub use coord::*;
pub use r#const::*;
pub use xvalue::*;
pub use xvalue_bool::*;
pub use xvalue_byte::*;
pub use xvalue_float::*;
pub use xvalue_index::*;
pub use xvalue_int::*;
pub use xvalue_obj::*;
pub use xvalue_uint::*;

pub use backend::Backend;
pub use conn::conn;
pub use kvspace::{KVPair, KVSpace};
pub use kvspace_common::*;
pub use store::KVStore;
