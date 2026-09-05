// ffi.rs — kvspace-durable 的 C ABI 暴露层。
//
// 目的：让 kvlang-layout（Rust）、kvlang-runtime（C/C++）等第三方语言只通过
// extern "C" 符号表调用本库，不接触 Rust 类型。所有 XValue 以 TLV 字节跨边界。
//
// 约定：
//   - 句柄：kvspaceConnect 返回 *mut Handle（Box<dyn KVSpace>），kvspaceClose 释放。
//   - 输入字符串：*const c_char（NUL 终止）；输入字节：*const u8 + u32 len。
//   - 读出字节：kvspaceGet/ListAt 返回句柄内常驻/回收缓冲的借用偏移指针，调用方不得 free；
//     codec（TlvEncode/New*）产出为 frontend malloc 缓冲，调用方以 libc free 释放。
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
//
// durable 无常驻映射：0copy 适配下沉到句柄内。读复用 read_buf 返回借用指针（活到下一次
// 读/写为止）；写把整条 TLV 攒进 write_buf、置 pending_key，在**下一个可观察操作前**惰性
// flush（内部实现，不进公开 ABI）。调用方从不 free、从不 commit。

pub struct Handle {
    kv: Box<dyn KVSpace>,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
    list_buf: Vec<u8>,
    pending_key: Option<String>,
}

impl Handle {
    /// 落盘上一笔惰性写（调用方已把 body 填进 write_buf）。
    fn flush(&mut self) -> Result<(), String> {
        if let Some(key) = self.pending_key.take() {
            let tlv = std::mem::take(&mut self.write_buf);
            let val = decode_xvalue(&tlv);
            self.kv.set(&[KVPair {
                key,
                val,
                raw: Some(tlv),
            }])?;
        }
        Ok(())
    }
}

/// flush 上一笔惰性写后返回后端引用。任一步失败 → Err（调用方转 err/返回 1）。
unsafe fn kv_flush<'a>(h: *mut Handle) -> Result<&'a mut dyn KVSpace, String> {
    let hd = h.as_mut().ok_or("kvspace: null handle")?;
    hd.flush()?;
    Ok(&mut *hd.kv)
}

/// 由 kindexpr 串 + body_len 直接构造 head（ro=0 vid=0）并预留 body_len 零字节。
/// 与 kvspace-c kvspaceXvalueWriteHead 逐字节一致。
fn build_tlv(kindexpr: &str, body_len: usize) -> Vec<u8> {
    let kx = kindexpr.as_bytes();
    let slot = kx.len() + 1;
    let mut v = Vec::with_capacity(1 + slot + 9 + body_len);
    v.push(slot as u8);
    v.extend_from_slice(kx);
    v.push(0); // kindexpr NUL
    v.push(0); // ro
    v.extend_from_slice(&0u32.to_le_bytes()); // vid
    v.extend_from_slice(&(body_len as u32).to_le_bytes()); // body_len
    v.resize(1 + slot + 9 + body_len, 0); // body 占位
    v
}

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
        Ok(kv) => Box::into_raw(Box::new(Handle {
            kv,
            read_buf: Vec::new(),
            write_buf: Vec::new(),
            list_buf: Vec::new(),
            pending_key: None,
        })),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn kvspaceClose(h: *mut Handle) {
    if !h.is_null() {
        let mut hd = unsafe { Box::from_raw(h) };
        let _ = hd.flush(); // 关闭前落盘未决写
        drop(hd);
    }
}

// ── KVSpace 原语 ─────────────────────────────────────────────────────

/// 借用读：*out 指向句柄内复用的 read_buf（活到下一次读/写），调用方不得 free。
/// resolve 由 get_raw 内部按路径解析（durable 恒穿透父路径 link）；空值 → *out=NULL、out_len=0。
#[no_mangle]
pub extern "C" fn kvspaceGet(
    h: *mut Handle,
    key: *const c_char,
    _resolve: c_int,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    let key = unsafe { cstr(key) }.to_string();
    if hd.flush().is_err() {
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 1;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hd.kv.get_raw(&key))) {
        Ok(raw) => {
            if raw.is_empty() {
                unsafe {
                    *out = std::ptr::null_mut();
                    *out_len = 0;
                }
            } else {
                hd.read_buf = raw;
                unsafe {
                    *out = hd.read_buf.as_mut_ptr();
                    *out_len = hd.read_buf.len() as u32;
                }
            }
            0
        }
        Err(_) => {
            unsafe {
                *out = std::ptr::null_mut();
                *out_len = 0;
            }
            1
        }
    }
}

/// 就地写：key 必须已存在、body_len 必须等于原 body_len——把原 head+body 攒进 write_buf、
/// 置 pending，返回 write_buf 内 body 偏移指针供直接写；违反前置条件 → 非 0 + err。
#[no_mangle]
pub extern "C" fn kvspaceWriteInPlace(
    h: *mut Handle,
    key: *const c_char,
    _resolve: c_int,
    body_len: u32,
    body: *mut *mut u8,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    if let Err(e) = hd.flush() {
        write_err(err, err_cap, &e);
        return 1;
    }
    let key = unsafe { cstr(key) }.to_string();
    let existing = hd.kv.get_raw(&key);
    if existing.is_empty() {
        write_err(err, err_cap, "kvspace: write-in-place on missing key");
        return 1;
    }
    let head = decode_xvalue_head(&existing);
    let head_len = head.head_len() as usize;
    if head.body_len as u32 != body_len || existing.len() != head_len + body_len as usize {
        write_err(err, err_cap, "kvspace: write-in-place body_len mismatch");
        return 1;
    }
    hd.write_buf = existing;
    hd.pending_key = Some(key);
    unsafe { *body = hd.write_buf.as_mut_ptr().add(head_len) };
    0
}

/// 新位置写：按 (kindexpr, body_len) 攒好 head 到 write_buf、置 pending，返回 body 偏移指针。
#[no_mangle]
pub extern "C" fn kvspaceWriteNewPlace(
    h: *mut Handle,
    key: *const c_char,
    kindexpr: *const c_char,
    body_len: u32,
    body: *mut *mut u8,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    if let Err(e) = hd.flush() {
        write_err(err, err_cap, &e);
        return 1;
    }
    let key = unsafe { cstr(key) }.to_string();
    let kx = unsafe { cstr(kindexpr) };
    let tlv = build_tlv(kx, body_len as usize);
    let head_len = tlv.len() - body_len as usize;
    hd.write_buf = tlv;
    hd.pending_key = Some(key);
    unsafe { *body = hd.write_buf.as_mut_ptr().add(head_len) };
    0
}

/// 只返回前缀下子项计数，无缓冲、无需释放。
#[no_mangle]
pub extern "C" fn kvspaceListLen(
    h: *mut Handle,
    prefix: *const c_char,
    expand_ext: c_int,
    resolve: c_int,
    out_count: *mut i32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    let prefix = unsafe { cstr(prefix) }.to_string();
    if hd.flush().is_err() || hd.kv.validate_dir(&prefix).is_err() {
        unsafe { *out_count = 0 };
        return 1;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hd.kv.list(&prefix, expand_ext != 0, resolve != 0).len()
    })) {
        Ok(n) => {
            unsafe { *out_count = n as i32 };
            0
        }
        Err(_) => {
            unsafe { *out_count = 0 };
            1
        }
    }
}

/// 借用索引取项：返回前缀下第 idx 个直接子项名，*out 指向句柄内复用的 list_buf（活到下次
/// 同句柄 ListAt），调用方不得 free。idx 越界 → *out=NULL、*out_len=0、返回非 0。配合
/// kvspaceListLen 遍历，不再一次性返回整段名单缓冲。
#[no_mangle]
pub extern "C" fn kvspaceListAt(
    h: *mut Handle,
    prefix: *const c_char,
    expand_ext: c_int,
    resolve: c_int,
    idx: i32,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    unsafe {
        *out = std::ptr::null_mut();
        *out_len = 0;
    }
    let prefix = unsafe { cstr(prefix) }.to_string();
    if hd.flush().is_err() || hd.kv.validate_dir(&prefix).is_err() {
        return 1;
    }
    let names = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hd.kv.list(&prefix, expand_ext != 0, resolve != 0)
    })) {
        Ok(n) => n,
        Err(_) => return 1,
    };
    if idx < 0 || idx as usize >= names.len() {
        return 1;
    }
    hd.list_buf = names[idx as usize].clone().into_bytes();
    unsafe {
        *out = hd.list_buf.as_mut_ptr();
        *out_len = hd.list_buf.len() as u32;
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
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
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
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
    result_to_code(
        catch_panic(|| kv.del_tree(unsafe { cstr(prefix) })),
        err,
        err_cap,
    )
}

#[no_mangle]
pub extern "C" fn kvspaceCp(
    h: *mut Handle,
    src: *const c_char,
    dst: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
    result_to_code(
        catch_panic(|| kv.cp(unsafe { cstr(src) }, unsafe { cstr(dst) })),
        err,
        err_cap,
    )
}

#[no_mangle]
pub extern "C" fn kvspaceCpTree(
    h: *mut Handle,
    src: *const c_char,
    dst: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
    result_to_code(
        catch_panic(|| kv.cp_tree(unsafe { cstr(src) }, unsafe { cstr(dst) })),
        err,
        err_cap,
    )
}

#[no_mangle]
pub extern "C" fn kvspaceMkindex(
    h: *mut Handle,
    path: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
    result_to_code(
        catch_panic(|| kv.mkindex(unsafe { cstr(path) })),
        err,
        err_cap,
    )
}

#[no_mangle]
pub extern "C" fn kvspaceMkindexExt(
    h: *mut Handle,
    path: *const c_char,
    ext_path: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
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
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
    result_to_code(
        catch_panic(|| kv.del_ext_index(unsafe { cstr(path) })),
        err,
        err_cap,
    )
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
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    if hd.flush().is_err() {
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 1;
    }
    let target_v =
        decode_xvalue(unsafe { std::slice::from_raw_parts(target, target_len as usize) });
    let v = hd.kv.watch(
        unsafe { cstr(key) },
        &target_v,
        Duration::from_nanos(tick_ns),
    );
    hd.read_buf = v.encode();
    unsafe {
        *out = hd.read_buf.as_mut_ptr();
        *out_len = hd.read_buf.len() as u32;
    }
    0
}

#[no_mangle]
pub extern "C" fn kvspaceClear(h: *mut Handle, err: *mut c_char, err_cap: u32) -> c_int {
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
    result_to_code(catch_panic(|| kv.clear()), err, err_cap)
}

#[no_mangle]
pub extern "C" fn kvspaceDisconnect(h: *mut Handle, err: *mut c_char, err_cap: u32) -> c_int {
    let kv: &mut dyn KVSpace = match unsafe { kv_flush(h) } {
        Ok(k) => k,
        Err(e) => {
            write_err(err, err_cap, &e);
            return 1;
        }
    };
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
    target_kindexpr: *const c_char,
    target: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    let v = new_ptr(unsafe { cstr(target_kindexpr) }, unsafe { cstr(target) });
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
