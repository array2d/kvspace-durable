// ffi.rs — kvspace-durable 的 C ABI 暴露层。
//
// 目的：让 kvlang-layout（Rust）、kvlang-runtime（C/C++）等第三方语言只通过
// extern "C" 符号表调用本库，不接触 Rust 类型。所有 XValue 以 TLV 字节跨边界。
//
// 约定：
//   - 句柄：kvspaceConnect 返回 *mut Handle（Box<dyn KVSpace>），kvspaceClose 释放。
//   - 输入字符串：*const c_char（NUL 终止）；输入字节：*const u8 + u32 len。
//   - 输出字节：*mut *mut u8 + *mut u32，由 callee 分配，调用方用 kvspaceBytesFree(ptr, len) 释放。
//   - 错误：返回 c_int（0=成功，1=失败），失败信息写入 err 缓冲（err_cap 上限）。
//
// 注意：本层函数不得 panic 跨边界（panic 会 abort 进程）；调用方保证入参合法。

use std::ffi::CStr;
use std::os::raw::{c_char, c_int};
use std::time::Duration;

use crate::conn::conn;
use crate::kvspace::{KVPair, KVSpace};
use crate::kvspace_common::get_one;
use crate::xvalue::{decode_xvalue, decode_xvalue_head, encode_head, encode_head_perm, new_ptr};
use crate::xvalue_bool::new_bool;
use crate::xvalue_byte::new_char_byte;
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

/// 把 panic 转成错误消息：任何 panic 都不得跨 extern "C" 边界（否则 SIGABRT）。
fn panic_msg(p: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "kvspace: panic across C ABI boundary".to_string()
    }
}

/// 运行核心逻辑，捕获 panic 返回 Err。供各导出函数做统一兜底。
fn catch_panic<F>(f: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(panic_msg)?
}

/// 统一出口：Err → 写 err 缓冲并返回 1；Ok → 0。
fn result_to_code(r: Result<(), String>, err: *mut c_char, err_cap: u32) -> c_int {
    match r {
        Ok(()) => 0,
        Err(e) => {
            write_err(err, err_cap, &e);
            1
        }
    }
}

/// XValueHead 解码结果（repr(C)，供跨边界读取头元数据）。kindexpr 为唯一类型真相。
#[repr(C)]
pub struct kvspaceHead_t {
    pub kindexpr: [u8; 256], // NUL 终止（含 */@ 前缀与 [dims]，去 padding）
    pub ro: u8,              // 1=只读，0=可写
    pub vid: u32,            // vthread id
    pub body_len: i32,       // body 字节数
    pub body_offset: i32,    // body 在 data 内的起始偏移（= head_len）
}

fn fill_head(head: &crate::xvalue::XValueHead, out: *mut kvspaceHead_t) {
    unsafe {
        let mut o = &mut *out;
        let k = head.kindexpr.as_bytes();
        let n = k.len().min(255);
        o.kindexpr[..n].copy_from_slice(&k[..n]);
        o.kindexpr[n] = 0;
        o.ro = head.ro as u8;
        o.vid = head.vid;
        o.body_len = head.body_len;
        o.body_offset = head.head_len();
    }
}

// ── 生命周期 ─────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn kvspaceConnect(dsn: *const c_char) -> *mut Handle {
    let dsn = unsafe { cstr(dsn) };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| conn(dsn))) {
        Ok(kv) => Box::into_raw(Box::new(kv)),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn kvspaceClose(h: *mut Handle) {
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
    result_to_code(
        catch_panic(|| {
            let mut pairs = Vec::with_capacity(n as usize);
            let mut off = 0usize;
            for i in 0..n as usize {
                let key = unsafe { cstr(*keys.add(i)) }.to_string();
                let len = unsafe { *lens.add(i) } as usize;
                let bytes = unsafe { std::slice::from_raw_parts(vals.add(off), len) };
                off += len;
                pairs.push(KVPair {
                    key,
                    val: decode_xvalue(bytes),
                    raw: Some(bytes.to_vec()),
                });
            }
            kv.set(&pairs)
        }),
        err,
        err_cap,
    )
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
    let key = unsafe { cstr(key) }.to_string();
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    if catch_panic(|| {
        let raw = kv.get_raw(&key);
        if raw.is_empty() {
            unsafe {
                *out = std::ptr::null_mut();
                *out_len = 0;
            }
        } else {
            alloc(raw, out, out_len);
        }
        Ok(())
    })
    .is_err()
    {
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 1;
    }
    0
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
    let prefix = unsafe { cstr(prefix) }.to_string();
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    if let Err(e) = kv.validate_dir(&prefix) {
        let _ = e;
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 1;
    }
    let names: Vec<String> = (0..nnames as usize)
        .map(|i| unsafe { cstr(*names.add(i)) }.to_string())
        .collect();
    if catch_panic(|| {
        let vals = kv.get(&prefix, &names, true);
        let mut result = Vec::new();
        for v in vals {
            let bytes = v.encode();
            result.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            result.extend_from_slice(&bytes);
        }
        alloc(result, out, out_len);
        Ok(())
    })
    .is_err()
    {
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 1;
    }
    0
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
    let prefix = unsafe { cstr(prefix) }.to_string();
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    if let Err(e) = kv.validate_dir(&prefix) {
        let _ = e;
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 1;
    }
    if catch_panic(|| {
        let children = kv.list(&prefix, expand_ext != 0, resolve != 0);
        alloc(children.join("\n").into_bytes(), out, out_len);
        Ok(())
    })
    .is_err()
    {
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 1;
    }
    0
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
    result_to_code(catch_panic(|| kv.del(&keys)), err, err_cap)
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
    result_to_code(catch_panic(|| kv.del_tree(unsafe { cstr(prefix) })), err, err_cap)
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
    result_to_code(catch_panic(|| kv.mkindex(unsafe { cstr(path) })), err, err_cap)
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
    result_to_code(
        catch_panic(|| kv.ext_index(unsafe { cstr(path) }, unsafe { cstr(ext_path) })),
        err,
        err_cap,
    )
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
    result_to_code(catch_panic(|| kv.del_ext_index(unsafe { cstr(path) })), err, err_cap)
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
    let target_v =
        decode_xvalue(unsafe { std::slice::from_raw_parts(target, target_len as usize) });
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
    result_to_code(catch_panic(|| kv.clear()), err, err_cap)
}

#[no_mangle]
pub extern "C" fn kvspaceDisconnect(h: *mut Handle, err: *mut c_char, err_cap: u32) -> c_int {
    if h.is_null() {
        return 1;
    }
    let kv: &mut dyn KVSpace = unsafe { &mut **h };
    result_to_code(catch_panic(|| kv.dis_conn()), err, err_cap)
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
    alloc(
        encode_head(unsafe { cstr(kind) }, 0, dims, raw),
        out,
        out_len,
    )
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
    alloc(
        encode_head(unsafe { cstr(kind) }, 1, dims, raw),
        out,
        out_len,
    )
}

/// 带权限编码：显式指定 ref（0/1/2）、ro（1=只读）、vid。用于权限位落盘。
#[no_mangle]
pub extern "C" fn kvspaceTlvEncodeMode(
    kind: *const c_char,
    raw: *const u8,
    raw_len: u32,
    dims: *const i32,
    ndim: i32,
    r#ref: c_int,
    ro: u8,
    vid: u32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let raw = unsafe { std::slice::from_raw_parts(raw, raw_len as usize) };
    let dims = ffi_dims(dims, ndim);
    alloc(
        encode_head_perm(unsafe { cstr(kind) }, r#ref, dims, raw, ro != 0, vid),
        out,
        out_len,
    )
}

#[inline]
fn ffi_dims<'a>(dims: *const i32, ndim: i32) -> &'a [i32] {
    if dims.is_null() || ndim <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(dims, ndim as usize) }
    }
}

/// 解码 XValueHead（不解析 body）。返回 kindexpr/ro/vid/body_len/body_offset。
#[no_mangle]
pub extern "C" fn kvspaceDecodeHead(
    data: *const u8,
    data_len: u32,
    out: *mut kvspaceHead_t,
) -> c_int {
    if data.is_null() || out.is_null() {
        return 1;
    }
    let head = decode_xvalue_head(unsafe { std::slice::from_raw_parts(data, data_len as usize) });
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
    bytes: *const u8,
    len: u32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let v = new_char_byte(unsafe { std::slice::from_raw_parts(bytes, len as usize) });
    alloc(v.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewBool(v: u8, out: *mut *mut u8, out_len: *mut u32) -> c_int {
    let x = new_bool(&[v != 0]);
    alloc(x.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewInt64(v: i64, out: *mut *mut u8, out_len: *mut u32) -> c_int {
    let x = new_int64(&[v]);
    alloc(x.encode(), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewFloat64(v: f64, out: *mut *mut u8, out_len: *mut u32) -> c_int {
    let x = new_float64(&[v]);
    alloc(x.encode(), out, out_len)
}
