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
// durable 无常驻映射：0copy 适配下沉到句柄内。读把每笔结果单独存进 read_bufs 借用池并返回
// 其指针，池内缓冲同时存活至下一次写 flush（履约 kvspace.h「借用生命周期同该槽」，令单条指令
// 的多个读操作数可同时借用）；写把整条 TLV 攒进 write_buf、置 pending_key，在**下一个可观察
// 操作前**惰性 flush（内部实现，不进公开 ABI）。调用方从不 free、从不 commit。

pub struct Handle {
    kv: Box<dyn KVSpace>,
    read_bufs: Vec<Vec<u8>>,
    write_buf: Vec<u8>,
    pending_key: Option<String>,
}

impl Handle {
    /// 落盘上一笔惰性写（调用方已把 body 填进 write_buf）；写即写边界，回收读借用池。
    fn flush(&mut self) -> Result<(), String> {
        if let Some(key) = self.pending_key.take() {
            let tlv = std::mem::take(&mut self.write_buf);
            let val = decode_xvalue(&tlv);
            self.kv.set(&[KVPair {
                key,
                val,
                raw: Some(tlv),
            }])?;
            self.read_bufs.clear();
        }
        Ok(())
    }

    /// 借入一段读缓冲：存进借用池、返回其常驻指针（活到下一次写 flush，调用方不得 free）。
    fn lend(&mut self, buf: Vec<u8>, out: *mut *mut u8, out_len: *mut u32) {
        self.read_bufs.push(buf);
        let b = self.read_bufs.last_mut().unwrap();
        unsafe {
            *out = b.as_mut_ptr();
            *out_len = b.len() as u32;
        }
    }
}

/// flush 上一笔惰性写后返回后端引用。任一步失败 → Err（调用方转 err/返回 1）。
unsafe fn kv_flush<'a>(h: *mut Handle) -> Result<&'a mut dyn KVSpace, String> {
    let hd = h.as_mut().ok_or("kvspace: null handle")?;
    hd.flush()?;
    Ok(&mut *hd.kv)
}

/// 由 (ref, storetype, ro, vid, langtype) + body_len 直接构造三正交轴 head 并预留 body_len 零字节。
/// 与 kvspace-c kvspaceXvalueWriteHead 逐字节一致：ARRAYND 从 langtype 的 [dims] 落物理字段，
/// NONE/ATOM 无物理字段（index/extindex 不走本路径）。
fn build_tlv(
    r#ref: u8,
    storetype: u8,
    ro: u8,
    vid: u32,
    langtype: &str,
    body_len: usize,
) -> Vec<u8> {
    let dims = parse_langtype_dims(langtype);
    let lt = langtype.as_bytes();
    let has_dims = crate::xvalue::store_has_dims(storetype);
    let phys = if has_dims { 1 + 4 * dims.len() } else { 0 };
    let headlen = crate::xvalue::HEAD_PREFIX + phys + lt.len();
    let mut v = vec![0u8; headlen + body_len];
    v[0..2].copy_from_slice(&(headlen as u16).to_le_bytes());
    v[2] = r#ref;
    v[3] = storetype;
    v[4] = ro;
    v[5..9].copy_from_slice(&vid.to_le_bytes());
    v[9..13].copy_from_slice(&(body_len as u32).to_le_bytes());
    let mut o = crate::xvalue::HEAD_PREFIX;
    if has_dims {
        v[o] = dims.len() as u8;
        o += 1;
        for d in &dims {
            v[o..o + 4].copy_from_slice(&(*d as u32).to_le_bytes());
            o += 4;
        }
    }
    v[o..o + lt.len()].copy_from_slice(lt);
    v
}

/// 解析 langtype 前导 [dims]（仅 ARRAYND 携带；无则空）。
fn parse_langtype_dims(lt: &str) -> Vec<i32> {
    if lt.starts_with('[') {
        if let Some(end) = lt.find(']') {
            return lt[1..end]
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.parse().unwrap_or(0))
                .collect();
        }
    }
    Vec::new()
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

/// XValueHead 解码结果（repr(C)，供跨边界读取头元数据）。逐字段对齐 kvspace-c 三正交轴 kvspaceHead_t。
#[repr(C)]
pub struct kvspaceHead_t {
    pub headlen: u16,        // head 总字节数
    pub r#ref: u8,           // 存储位置：0=inline 1=ptr 2=@ext
    pub storetype: u8,       // 物理布局：NONE/ATOM/ARRAYND/index/extindex
    pub ro: u8,              // 1=只读，0=可写
    pub vid: u32,            // vthread id
    pub body_len: i32,       // body 字节数
    pub ndim: i32,           // 物理维数（标量=0）
    pub dims: [i32; 8],      // 物理字段 dims（X_MAX_NDIM=8）
    pub langtype: [u8; 256], // 完整 kindexpr 串（含 [dims]、无前缀），NUL 终止
    pub langtype_len: i32,   // langtype 字节数（不含 NUL）
    pub body_offset: i32,    // body 在 data 内的起始偏移（= head_len）
}

fn fill_head(head: &crate::xvalue::XValueHead, out: *mut kvspaceHead_t) {
    unsafe {
        let o = &mut *out;
        o.headlen = head.headlen;
        o.r#ref = head.r#ref;
        o.storetype = head.storetype;
        let lt = head.langtype.as_bytes();
        let n = lt.len().min(255);
        o.langtype = [0; 256];
        o.langtype[..n].copy_from_slice(&lt[..n]);
        o.langtype[n] = 0;
        o.langtype_len = n as i32;
        let dims = head.dims();
        o.ndim = dims.len().min(8) as i32;
        o.dims = [0; 8];
        for (i, d) in dims.iter().take(8).enumerate() {
            o.dims[i] = *d;
        }
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
            read_bufs: Vec::new(),
            write_buf: Vec::new(),
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

/// 借用读：*out 指向读借用池内本笔缓冲（活到下一次写 flush），调用方不得 free。
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
                hd.lend(raw, out, out_len);
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

/// 新位置写：按 (ref, storetype, langtype, body_len) 攒好 head 到 write_buf、置 pending，返回 body 偏移指针。
#[no_mangle]
pub extern "C" fn kvspaceWriteNewPlace(
    h: *mut Handle,
    key: *const c_char,
    r#ref: u8,
    storetype: u8,
    ro: u8,
    vid: u32,
    langtype: *const c_char,
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
    let lt = unsafe { cstr(langtype) };
    let tlv = build_tlv(r#ref, storetype, ro, vid, lt, body_len as usize);
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
        hd.kv.list_len(&prefix, expand_ext != 0, resolve != 0)
    })) {
        Ok(n) => {
            unsafe { *out_count = n };
            0
        }
        Err(_) => {
            unsafe { *out_count = 0 };
            1
        }
    }
}

/// 索引取项：把前缀下第 idx 个直接子项名写进调用方自备缓冲 buf（容量 buf_cap），*out_len
/// 置该名长度（不含 NUL）。库侧零状态、调用方不得 free。idx 越界或缓冲不足 → 返回非 0
/// （缓冲不足时 *out_len 仍为所需长度，不静默截断）。配合 kvspaceListLen 遍历。
#[no_mangle]
pub extern "C" fn kvspaceListAt(
    h: *mut Handle,
    prefix: *const c_char,
    expand_ext: c_int,
    resolve: c_int,
    idx: i32,
    buf: *mut u8,
    buf_cap: u32,
    out_len: *mut u32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    unsafe {
        *out_len = 0;
    }
    let prefix = unsafe { cstr(prefix) }.to_string();
    if hd.flush().is_err() || hd.kv.validate_dir(&prefix).is_err() {
        return 1;
    }
    let name = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hd.kv.list_at(&prefix, idx, expand_ext != 0, resolve != 0)
    })) {
        Ok(Some(n)) => n,
        Ok(None) => return 1,
        Err(_) => return 1,
    };
    let name = name.as_bytes();
    unsafe {
        *out_len = name.len() as u32;
    }
    if buf.is_null() || name.len() as u32 > buf_cap {
        return 1;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(name.as_ptr(), buf, name.len());
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
pub extern "C" fn kvspaceCpList(
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
        catch_panic(|| kv.cp_list(unsafe { cstr(src) }, unsafe { cstr(dst) })),
        err,
        err_cap,
    )
}

#[no_mangle]
pub extern "C" fn kvspaceMkindex(
    h: *mut Handle,
    path: *const c_char,
    capacity: u32,
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
        catch_panic(|| kv.mkindex(unsafe { cstr(path) }, capacity)),
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
    hd.lend(v.encode(), out, out_len);
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
