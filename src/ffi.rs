// ffi.rs — kvspace-durable 的 C ABI 暴露层。
//
// 目的：让 kvlang-layout（Rust）、kvlang-runtime（C/C++）等第三方语言只通过
// extern "C" 符号表调用本库，不接触 Rust 类型。所有 XValue 以 TLV 字节跨边界。
//
// 约定：
//   - 句柄：kvspace_conn 返回 *mut Handle（Box<dyn KVSpace>），kvspace_free 释放。
//   - 输入字符串：*const c_char（NUL 终止）；输入字节：*const u8 + u32 len。
//   - 输出字节：*mut *mut u8 + *mut u32，由 callee 分配，调用方用 kvspace_bytes_free(ptr, len) 释放。
//   - 错误：返回 c_int（0=成功，1=失败），失败信息写入 err 缓冲（err_cap 上限）。
//
// 注意：本层函数不得 panic 跨边界（panic 会 abort 进程）；调用方保证入参合法。

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::time::Duration;

use crate::conn::conn;
use crate::kvspace::{KVSpace, KVPair};
use crate::kvspace_common::get_one;
use crate::xvalue::{
    decode_xvalue, decode_xvalue_head, new_ptr, tlv_encode, tlv_encode_ptr,
};
use crate::xvalue_bool::new_bool;
use crate::xvalue_byte::{new_char, new_char_byte};
use crate::xvalue_float::new_float64;
use crate::xvalue_int::new_int64;

// ── 句柄 ─────────────────────────────────────────────────────────────

type Handle = Box<dyn KVSpace>;

// ── 内部助手 ─────────────────────────────────────────────────────────

#[inline]
unsafe fn cstr<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "";
    }
    CStr::from_ptr(p).to_str().unwrap_or("")
}

#[inline]
fn alloc(v: Vec<u8>, out: *mut *mut u8, out_len: *mut u32) -> c_int {
    let mut b = v.into_boxed_slice();
    let len = b.len() as u32;
    let p = b.as_mut_ptr();
    std::mem::forget(b);
    unsafe {
        *out = p;
        *out_len = len;
    }
    0
}

#[inline]
fn write_err(err: *mut c_char, err_cap: u32, msg: &str) {
    if err.is_null() || err_cap == 0 {
        return;
    }
    let bytes = msg.as_bytes();
    let n = bytes.len().min(err_cap as usize - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), err as *mut u8, n);
        *err.add(n) = 0;
    }
}

/// XValueHead 解码结果（repr(C)，供跨边界读取头元数据）。
#[repr(C)]
pub struct KVHead {
    pub kind: [u8; 32], // NUL 终止的 kind 字符串
    pub is_ptr: u8,     // ref==1
    pub array_len: i32, // 派生数组长度
    pub body_len: i32,  // body 字节数
    pub body_offset: i32, // body 在 data 内的起始偏移（= head_len）
}

fn fill_head(head: &crate::xvalue::XValueHead, out: *mut KVHead) {
    unsafe {
        let mut o = &mut *out;
        let k = head.kind.as_bytes();
        let n = k.len().min(31);
        o.kind[..n].copy_from_slice(&k[..n]);
        o.kind[n] = 0;
        o.is_ptr = head.is_ptr as u8;
        o.array_len = head.array_len;
        o.body_len = head.body_len;
        o.body_offset = head.head_len();
    }
}

// ── 生命周期 ─────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn kvspace_conn(dsn: *const c_char) -> *mut Handle {
    let dsn = unsafe { cstr(dsn) };
    Box::into_raw(Box::new(conn(dsn)))
}

#[no_mangle]
pub extern "C" fn kvspace_free(h: *mut Handle) {
    if !h.is_null() {
        unsafe { drop(Box::from_raw(h)) };
    }
}

#[no_mangle]
pub extern "C" fn kvspace_bytes_free(p: *mut u8, len: u32) {
    if !p.is_null() {
        let slice = unsafe { std::ptr::slice_from_raw_parts_mut(p, len as usize) };
        unsafe { drop(Box::from_raw(slice)) };
    }
}

// ── KVSpace 原语 ─────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn kvspace_set(
    h: *mut Handle,
    keys: *const *const c_char,
    vals: *const u8,
    lens: *const u32,
    n: u32,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let mut pairs = Vec::with_capacity(n as usize);
    let mut off = 0usize;
    for i in 0..n as usize {
        let key = unsafe { cstr(*keys.add(i)) }.to_string();
        let len = unsafe { *lens.add(i) } as usize;
        let bytes = unsafe { std::slice::from_raw_parts(vals.add(off), len) };
        off += len;
        pairs.push(KVPair { key, val: decode_xvalue(bytes) });
    }
    match kv.set(&pairs) {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

/// 单点读（对齐 GetOne）：None 编码为空字节（out_len==0）。
#[no_mangle]
pub extern "C" fn kvspace_get_one(
    h: *mut Handle,
    key: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let v = get_one(kv, unsafe { cstr(key) });
    alloc(v.encode(), out, out_len)
}

/// 批量读：prefix 下 names 一次 MGET，返回每个值的 [4B len LE][TLV] 拼接。
/// None 编码为 len=0。names 数量须与 C 侧一致，按序对应。
#[no_mangle]
pub extern "C" fn kvspace_get_batch(
    h: *mut Handle,
    prefix: *const c_char,
    names: *const *const c_char,
    nnames: u32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let names: Vec<String> = (0..nnames as usize)
        .map(|i| unsafe { cstr(*names.add(i)) }.to_string())
        .collect();
    let vals = kv.get(unsafe { cstr(prefix) }, &names, true);
    let mut result = Vec::new();
    for v in vals {
        let bytes = v.encode();
        result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        result.extend_from_slice(&bytes);
    }
    alloc(result, out, out_len)
}

/// 列目录：子项以 \n 连接返回（子名不含 \n）。空目录返回 out_len==0。
#[no_mangle]
pub extern "C" fn kvspace_list(
    h: *mut Handle,
    prefix: *const c_char,
    expand_ext: c_int,
    resolve: c_int,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let children = kv.list(unsafe { cstr(prefix) }, expand_ext != 0, resolve != 0);
    alloc(children.join("\n").into_bytes(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspace_del(
    h: *mut Handle,
    keys: *const *const c_char,
    nkeys: u32,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let keys: Vec<String> = (0..nkeys as usize)
        .map(|i| unsafe { cstr(*keys.add(i)) }.to_string())
        .collect();
    match kv.del(&keys) {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspace_del_tree(
    h: *mut Handle,
    prefix: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    match kv.del_tree(unsafe { cstr(prefix) }) {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspace_mkindex(
    h: *mut Handle,
    path: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    match kv.mkindex(unsafe { cstr(path) }) {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspace_ext_index(
    h: *mut Handle,
    path: *const c_char,
    ext_path: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    match kv.ext_index(unsafe { cstr(path) }, unsafe { cstr(ext_path) }) {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspace_del_ext_index(
    h: *mut Handle,
    path: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    match kv.del_ext_index(unsafe { cstr(path) }) {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspace_watch(
    h: *mut Handle,
    key: *const c_char,
    target: *const u8,
    target_len: u32,
    tick_ns: u64,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let target_v = decode_xvalue(unsafe {
        std::slice::from_raw_parts(target, target_len as usize)
    });
    let v = kv.watch(
        unsafe { cstr(key) },
        &target_v,
        Duration::from_nanos(tick_ns),
    );
    alloc(v.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspace_clear(h: *mut Handle, err: *mut c_char, err_cap: u32) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    match kv.clear() {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspace_disconn(h: *mut Handle, err: *mut c_char, err_cap: u32) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    match kv.dis_conn() {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

// ── XValue 编解码（head/TLV + 标准标量构造器） ─────────────────────────

/// 通用 TLV 编码（内联，ref=0）。array_len 由 arr_flag/dims 推导。
/// kvlang 的自有 kind（rwir/rwfunc/scope）经此构造：body 由 kvlang 自己编码。
#[no_mangle]
pub extern "C" fn kvspace_tlv_encode(
    kind: *const c_char,
    raw: *const u8,
    raw_len: u32,
    array_len: i32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let raw = unsafe { std::slice::from_raw_parts(raw, raw_len as usize) };
    alloc(tlv_encode(unsafe { cstr(kind) }, raw, array_len), out, out_len)
}

/// 通用 TLV 编码（软链接，ref=1），body 为目标 key 路径。
#[no_mangle]
pub extern "C" fn kvspace_tlv_encode_ptr(
    kind: *const c_char,
    raw: *const u8,
    raw_len: u32,
    array_len: i32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let raw = unsafe { std::slice::from_raw_parts(raw, raw_len as usize) };
    alloc(tlv_encode_ptr(unsafe { cstr(kind) }, raw, array_len), out, out_len)
}

/// 解码 XValueHead（不解析 body）。返回 kind/is_ptr/array_len/body_len/body_offset。
#[no_mangle]
pub extern "C" fn kvspace_decode_head(
    data: *const u8,
    data_len: u32,
    out: *mut KVHead,
) -> c_int {
    if data.is_null() || out.is_null() {
        return 1;
    }
    let head = decode_xvalue_head(unsafe {
        std::slice::from_raw_parts(data, data_len as usize)
    });
    fill_head(&head, out);
    0
}

// ── 标准标量构造器（返回完整 TLV 字节） ───────────────────────────────

#[no_mangle]
pub extern "C" fn kvspace_new_ptr(
    kind: *const c_char,
    target: *const c_char,
    array_len: i32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let v = new_ptr(unsafe { cstr(kind) }, unsafe { cstr(target) }, array_len);
    alloc(v.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspace_new_char(
    kind: *const c_char,
    s: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let v = new_char(unsafe { cstr(kind) }, unsafe { cstr(s) });
    alloc(v.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspace_new_char_byte(
    bytes: *const u8,
    len: u32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let v = new_char_byte(unsafe { std::slice::from_raw_parts(bytes, len as usize) });
    alloc(v.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspace_new_bool(
    v: u8,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let x = new_bool(&[v != 0]);
    alloc(x.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspace_new_int64(
    v: i64,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let x = new_int64(&[v]);
    alloc(x.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspace_new_float64(
    v: f64,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let x = new_float64(&[v]);
    alloc(x.encode(), out, out_len)
}
