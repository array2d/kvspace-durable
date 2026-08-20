// ffi.rs — kvspace-durable 的 C ABI 暴露层。
//
// 目的：让 kvlang-layout（Rust）、kvlang-runtime（C/C++）等第三方语言只通过
// extern "C" 符号表调用本库，不接触 Rust 类型。所有 XValue 以 TLV 字节跨边界。
//
// 约定：
//   - 句柄：kvspaceConnect 返回 *mut Handle（Box<dyn KVSpace>），kvspaceFree 释放。
//   - 输入字符串：*const c_char（NUL 终止）；输入字节：*const u8 + u32 len。
//   - 输出字节：*mut *mut u8 + *mut u32，由 callee 分配，调用方用 kvspaceBytesFree(ptr, len) 释放。
//   - 错误：返回 c_int（0=成功，1=失败），失败信息写入 err 缓冲（err_cap 上限）。
//
// 注意：本层函数不得 panic 跨边界（panic 会 abort 进程）；调用方保证入参合法。

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::time::Duration;

use crate::conn::conn;
use crate::kvspace::{KVSpace, KVPair};
use crate::kvspace_common::get_one;
use crate::r#const::{KIND_CHAR_UTF8, KIND_UINT8};
use crate::xvalue::{
    body_bytes, decode_xvalue, decode_xvalue_head, encode_head, is_none, new_ptr,
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
pub struct kvspaceHead_t {
    pub kind: [u8; 32], // NUL 终止的 kind 字符串
    pub is_ptr: u8,     // ref==1
    pub array_len: i32, // 派生数组长度
    pub body_len: i32,  // body 字节数
    pub body_offset: i32, // body 在 data 内的起始偏移（= head_len）
    pub ndim: i32,      // 0=标量，N=N 维数组（唯一「是否数组」标志）
    pub dims: [i32; 8], // 各维长度（kind+ndim+dims 即完整 kindexp）
}

fn fill_head(head: &crate::xvalue::XValueHead, out: *mut kvspaceHead_t) {
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
        o.ndim = head.ndim;
        for (i, d) in head.dims.iter().take(8).enumerate() {
            o.dims[i] = *d;
        }
    }
}

// ── 生命周期 ─────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn kvspaceConnect(dsn: *const c_char) -> *mut Handle {
    let dsn = unsafe { cstr(dsn) };
    Box::into_raw(Box::new(conn(dsn)))
}

#[no_mangle]
pub extern "C" fn kvspaceFree(h: *mut Handle) {
    if !h.is_null() {
        unsafe { drop(Box::from_raw(h)) };
    }
}

#[no_mangle]
pub extern "C" fn kvspaceBytesFree(p: *mut u8, len: u32) {
    if !p.is_null() {
        let slice = unsafe { std::ptr::slice_from_raw_parts_mut(p, len as usize) };
        unsafe { drop(Box::from_raw(slice)) };
    }
}

// ── KVSpace 原语 ─────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn kvspaceSet(
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
pub extern "C" fn kvspaceGet(
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
pub extern "C" fn kvspaceGetBatch(
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
pub extern "C" fn kvspaceList(
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
pub extern "C" fn kvspaceDel(
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
pub extern "C" fn kvspaceDelTree(
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
pub extern "C" fn kvspaceMkindex(
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
pub extern "C" fn kvspaceMkindexExt(
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
pub extern "C" fn kvspaceRmindexExt(
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
pub extern "C" fn kvspaceWatch(
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
pub extern "C" fn kvspaceClear(h: *mut Handle, err: *mut c_char, err_cap: u32) -> c_int {
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
pub extern "C" fn kvspaceDisconnect(h: *mut Handle, err: *mut c_char, err_cap: u32) -> c_int {
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

/// 通用 TLV 编码（内联，ref=0）。dims/ndim 直接落盘：ndim=0 标量，dims 可为 NULL。
/// kvlang 的自有 kind（rwir/rwfunc/scope）经此构造：body 由 kvlang 自己编码。
#[no_mangle]
pub extern "C" fn kvspaceTlvEncode(
    kind: *const c_char,
    raw: *const u8,
    raw_len: u32,
    dims: *const i32,
    ndim: i32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let raw = unsafe { std::slice::from_raw_parts(raw, raw_len as usize) };
    let dims = ffi_dims(dims, ndim);
    alloc(encode_head(unsafe { cstr(kind) }, 0, dims, raw), out, out_len)
}

/// 通用 TLV 编码（软链接，ref=1），body 为目标 key 路径。
#[no_mangle]
pub extern "C" fn kvspaceTlvEncodePtr(
    kind: *const c_char,
    raw: *const u8,
    raw_len: u32,
    dims: *const i32,
    ndim: i32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let raw = unsafe { std::slice::from_raw_parts(raw, raw_len as usize) };
    let dims = ffi_dims(dims, ndim);
    alloc(encode_head(unsafe { cstr(kind) }, 1, dims, raw), out, out_len)
}

#[inline]
fn ffi_dims<'a>(dims: *const i32, ndim: i32) -> &'a [i32] {
    if dims.is_null() || ndim <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(dims, ndim as usize) }
    }
}

/// 解码 XValueHead（不解析 body）。返回 kind/is_ptr/array_len/body_len/body_offset。
#[no_mangle]
pub extern "C" fn kvspaceDecodeHead(
    data: *const u8,
    data_len: u32,
    out: *mut kvspaceHead_t,
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
pub extern "C" fn kvspaceNewPtr(
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
pub extern "C" fn kvspaceNewChar(
    kind: *const c_char,
    s: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let v = new_char(unsafe { cstr(kind) }, unsafe { cstr(s) });
    alloc(v.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewCharByte(
    bytes: *const u8,
    len: u32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let v = new_char_byte(unsafe { std::slice::from_raw_parts(bytes, len as usize) });
    alloc(v.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewBool(
    v: u8,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let x = new_bool(&[v != 0]);
    alloc(x.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewInt64(
    v: i64,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let x = new_int64(&[v]);
    alloc(x.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewFloat64(
    v: f64,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let x = new_float64(&[v]);
    alloc(x.encode(), out, out_len)
}

fn nq_key(key: &str) -> String {
    format!("/\u{2025}notify{key}")
}

fn nq_load(kv: &mut dyn KVSpace, qk: &str) -> Vec<u8> {
    let v = get_one(kv, qk);
    if is_none(&v) {
        return Vec::new();
    }
    body_bytes(&v)
}

fn nq_frames(body: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= body.len() {
        let n = u32::from_le_bytes([body[i], body[i + 1], body[i + 2], body[i + 3]]) as usize;
        i += 4;
        if i + n > body.len() {
            break;
        }
        out.push(body[i..i + n].to_vec());
        i += n;
    }
    out
}

fn nq_save(kv: &mut dyn KVSpace, qk: &str, frames: &[Vec<u8>]) -> Result<(), String> {
    if frames.is_empty() {
        return kv.del(&[qk.to_string()]);
    }
    let mut raw = Vec::new();
    for f in frames {
        raw.extend_from_slice(&(f.len() as u32).to_le_bytes());
        raw.extend_from_slice(f);
    }
    let tlv = encode_head(KIND_UINT8, 0, &[raw.len() as i32], &raw);
    kv.set(&[KVPair {
        key: qk.to_string(),
        val: decode_xvalue(&tlv),
    }])
}

#[no_mangle]
pub extern "C" fn kvspaceNotify(
    h: *mut Handle,
    key: *const c_char,
    val: *const u8,
    len: u32,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() || val.is_null() || len == 0 {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let key = unsafe { cstr(key) };
    let frame = unsafe { std::slice::from_raw_parts(val, len as usize) }.to_vec();
    let qk = nq_key(key);
    let mut frames = nq_frames(&nq_load(kv, &qk));
    frames.push(frame);
    match nq_save(kv, &qk, &frames) {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspaceTake(
    h: *mut Handle,
    key: *const c_char,
    timeout_ns: u64,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    if out.is_null() || out_len.is_null() {
        return 1;
    }
    unsafe {
        *out = std::ptr::null_mut();
        *out_len = 0;
    }
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let key = unsafe { cstr(key) };
    let qk = nq_key(key);
    let deadline = std::time::Instant::now() + Duration::from_nanos(timeout_ns);
    loop {
        let mut frames = nq_frames(&nq_load(kv, &qk));
        if !frames.is_empty() {
            let item = frames.remove(0);
            let _ = nq_save(kv, &qk, &frames);
            return alloc(item, out, out_len);
        }
        if std::time::Instant::now() >= deadline {
            return 0;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[no_mangle]
pub extern "C" fn kvspaceIncr(
    h: *mut Handle,
    key: *const c_char,
    out: *mut i64,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if h.is_null() || out.is_null() {
        write_err(err, err_cap, "Incr: bad args");
        return 1;
    }
    unsafe { *out = 0; }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    let key = unsafe { cstr(key) };
    let cur = get_one(kv, key);
    let mut n: i64 = 0;
    if !is_none(&cur) {
        if !cur.kind().starts_with("char/") {
            write_err(err, err_cap, "Incr: counter is not a Char");
            return 1;
        }
        let s = cur.value_string();
        match s.parse::<i64>() {
            Ok(v) => n = v,
            Err(_) => {
                write_err(err, err_cap, "Incr: unparsable counter");
                return 1;
            }
        }
    }
    if n == i64::MAX {
        write_err(err, err_cap, "Incr: overflow");
        return 1;
    }
    n += 1;
    let val = new_char(KIND_CHAR_UTF8, &n.to_string());
    match kv.set(&[KVPair { key: key.to_string(), val }]) {
        Ok(()) => {
            unsafe { *out = n; }
            0
        }
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}
