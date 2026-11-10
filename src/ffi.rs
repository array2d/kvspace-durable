// ffi.rs — kvspace-durable 的 C ABI 暴露层。
//
// 目的：让 kvlang-layout（Rust）、kvlang-runtime（C/C++）等第三方语言只通过
// extern "C" 符号表调用本库，不接触 Rust 类型。所有 XValue 以 head+body 字节跨边界。
//
// 约定：
//   - 句柄：kvspaceConnect 返回 *mut Handle（Box<dyn KVSpace>），kvspaceClose 释放。
//   - 输入字符串：*const c_char（NUL 终止）；输入字节：*const u8 + u32 len。
//   - 读出字节：kvspaceGet/ListAt 返回句柄内常驻/回收缓冲的借用偏移指针，调用方不得 free；
//     codec（New*）产出为 frontend malloc 缓冲，调用方以 libc free 释放。
//   - 错误：返回 c_int（0=成功，1=失败），失败信息写入 err 缓冲（err_cap 上限）。
//
// 注意：本层函数不得 panic 跨边界（panic 会 abort 进程）；调用方保证入参合法。

use std::collections::HashMap;
use std::ffi::CStr;
use std::io::Write;
use std::os::raw::{c_char, c_int};
use std::time::Duration;

use crate::conn::conn;
use crate::kvspace::{KVPair, KVSpace};
use crate::r#const::{DIR_INDEX_SUF, OBJ_SEP, PATH_SEP, RUNTIME_MEMBER_SEP};
use crate::xvalue::{from_wire, from_wire_head, REF_INLINE, REF_PTR};

// ── 句柄 ─────────────────────────────────────────────────────────────
//
// durable 无常驻映射：0copy 适配下沉到句柄内。读按 **key → Vec<u8>** 常驻缓存：命中直接返回
// 该副本指针，未命中读后端并缓存；指针寿命到该 key 被写/删或 handle close（履约 kvspace.h
// 「借用生命周期到该 key 写/删」）。写把整条 XValue 攒进 write_buf、置 pending_key，在**下一个
// 可观察操作前**惰性 flush——body 由调用方在返回后填，返回前无法落盘，故落盘点恒为「下一次
// op 或 Close」；漏 Close 的兜底由前端 atexit 兑现（见 kvspace.h 写原语注释、spec [[CABI]]
// 写侧落盘时机）。调用方从不 free、从不 commit。

/// Ring buffer for volatile control keys: reuses the current slot in place when unchanged,
/// and advances to the next slot on change so interleaved `Get` / `GetHead` views stay valid.
pub struct VolatileBuf {
    slots: [Vec<u8>; 4],
    which: usize,
}

/// 外部可改写、必须每读重读后端的控制键（normative：见 spec CABI）。
fn is_volatile(key: &str) -> bool {
    (key.starts_with("/vthread/") && key.contains('\u{2025}'))
        || (key.starts_with("/lib/")
            && (key.ends_with("/\u{2025}ctime") || key.contains("/vids/")))
}

pub struct Handle {
    pub(crate) kv: Box<dyn KVSpace>,
    /// key → 该 key 最近一次读到的完整 XValue（常驻快照，无 eviction）。
    read_cache: HashMap<String, Vec<u8>>,
    /// 控制键：每次读重读后端；双缓冲保住上一视图。
    volatile_cache: HashMap<String, VolatileBuf>,
    /// Watch 替换缓存项时旧快照的保活区（冷路径，活到 Close）。
    retired: Vec<Vec<u8>>,
    write_buf: Vec<u8>,
    pending_key: Option<String>,
    /// Cached child list for `ListLen` followed by sequential `ListAt(i)` traversal.
    list_cache: Option<ListCache>,
    /// 新 ABI 的 per-index 列举快照与 generation（`kvspaceList`/`kvspaceListlen`）。
    pub(crate) list_gen: HashMap<String, crate::storage_abi::ListGen>,
}

/// Per-traversal child list snapshot: refreshed by `ListLen` (`cursor = -1`) and reused by
/// `ListAt` while `(prefix, expand_ext, resolve)` match, `idx > cursor`, and no writes occur.
struct ListCache {
    prefix: String,
    expand_ext: bool,
    resolve: bool,
    names: Vec<String>,
    /// Last yielded index (`-1` right after `ListLen`).
    cursor: i32,
}

impl Handle {
    /// 落盘上一笔惰性写（调用方已把 body 填进 write_buf），并失效该 key 的读缓存。
    /// Catches backend panics (Redis disconnect/timeout, fs I/O error) and converts them into
    /// `Err` so they never unwind across `extern "C"`.
    pub(crate) fn flush(&mut self) -> Result<(), String> {
        if let Some(key) = self.pending_key.take() {
            let xv = std::mem::take(&mut self.write_buf);
            self.list_cache = None;
            let kv = &mut self.kv;
            let k = key.clone();
            let r = catch_panic(move || {
                let val = from_wire(&xv);
                kv.set(&[KVPair {
                    key: k,
                    val,
                    raw: Some(xv),
                }])
            });
            self.cache_invalidate(&key);
            r?;
        }
        Ok(())
    }

    /// Returns child names under `prefix`. `at = None` (`ListLen`) always re-lists with
    /// `cursor = -1`; `at = Some(idx)` (`ListAt`) reuses the snapshot when parameters match
    /// and `idx > cursor`, otherwise re-lists (so polling the same `idx` sees external writes).
    /// Any write on this handle invalidates `list_cache`.
    fn list_names(
        &mut self,
        prefix: &str,
        expand_ext: bool,
        resolve: bool,
        at: Option<i32>,
    ) -> Result<&[String], String> {
        let reuse = at.is_some_and(|idx| {
            self.list_cache.as_ref().is_some_and(|c| {
                c.prefix == prefix
                    && c.expand_ext == expand_ext
                    && c.resolve == resolve
                    && idx > c.cursor
            })
        });
        if !reuse {
            self.list_cache = None;
            let kv = &mut self.kv;
            let names = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                kv.list(prefix, expand_ext, resolve)
            }))
            .map_err(panic_msg)?;
            self.list_cache = Some(ListCache {
                prefix: prefix.to_string(),
                expand_ext,
                resolve,
                names,
                cursor: -1,
            });
        }
        let c = self.list_cache.as_mut().expect("list cache just set");
        if let Some(idx) = at {
            c.cursor = idx;
        }
        Ok(&c.names)
    }

    pub(crate) fn cache_invalidate(&mut self, key: &str) {
        self.read_cache.remove(key);
        self.volatile_cache.remove(key);
    }

    pub(crate) fn cache_invalidate_tree(&mut self, prefix: &str) {
        self.read_cache.retain(|k, _| !under_prefix(k, prefix));
        self.volatile_cache.retain(|k, _| !under_prefix(k, prefix));
    }
}

/// Returns whether `key` falls within the subtree affected by `DelTree`/`CpTree`/`CpList(prefix)`,
/// normalizing a trailing `/` or `·` to `base` and matching `/`, `·`, and `‥` boundaries.
fn under_prefix(key: &str, prefix: &str) -> bool {
    if key == prefix {
        return true;
    }
    if prefix.ends_with(RUNTIME_MEMBER_SEP) {
        return key.starts_with(prefix);
    }
    let base = if prefix == PATH_SEP {
        ""
    } else {
        prefix
            .strip_suffix(DIR_INDEX_SUF)
            .or_else(|| prefix.strip_suffix(OBJ_SEP))
            .unwrap_or(prefix)
    };
    if base.is_empty() {
        return key.starts_with(PATH_SEP);
    }
    key.strip_prefix(base).is_some_and(|rest| {
        rest.is_empty()
            || rest.starts_with(PATH_SEP)
            || rest.starts_with(OBJ_SEP)
            || rest.starts_with(RUNTIME_MEMBER_SEP)
    })
}

/// 控制键重读后端并写入双缓冲的另一块（上一块保活到该 key 下一次读）。
/// When `/lib/<func>/‥ctime` changes, moves cached entries under `<func>` into `retired`
/// so cross-handle relayout updates are visible immediately without invalidating borrowed pointers.
fn volatile_refresh(hd: &mut Handle, key: &str) -> bool {
    let raw = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hd.kv.get_raw(key))) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let mut ctime_changed_dir: Option<&str> = None;
    {
        let vb = hd
            .volatile_cache
            .entry(key.to_string())
            .or_insert_with(|| VolatileBuf {
                slots: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
                which: 0,
            });
        let prev = &vb.slots[vb.which];
        if prev.as_slice() != raw.as_slice() {
            if !prev.is_empty() {
                if let Some(func_dir) = key.strip_suffix("/\u{2025}ctime") {
                    if func_dir.starts_with("/lib/") && func_dir.len() > "/lib/".len() {
                        ctime_changed_dir = Some(func_dir);
                    }
                }
            }
            vb.which = (vb.which + 1) & 3;
            vb.slots[vb.which] = raw;
        }
    }
    if let Some(func_dir) = ctime_changed_dir {
        let stale_keys: Vec<String> = hd
            .read_cache
            .keys()
            .filter(|k| under_prefix(k, func_dir))
            .cloned()
            .collect();
        for k in stale_keys {
            if let Some(old_vec) = hd.read_cache.remove(&k) {
                hd.retired.push(old_vec);
            }
        }
        hd.list_cache = None;
    }
    true
}

/// flush 上一笔惰性写后返回后端引用。任一步失败 → Err（调用方转 err/err 返回 1）。
/// Used by mutating structure ops (`Del`/`DelTree`/`Cp*`/`Mkindex*`/`RmindexExt`/`Clear`); clears `list_cache`.
pub(crate) unsafe fn kv_flush<'a>(h: *mut Handle) -> Result<&'a mut dyn KVSpace, String> {
    let hd = h.as_mut().ok_or("kvspace: null handle")?;
    hd.list_cache = None;
    hd.flush()?;
    Ok(&mut *hd.kv)
}

/// 固定头预留：head + attr 区（langtype 尾部）一次写，body 偏移恒 = PREFIX。
/// 公共 FFI 的写入口：`ref` 转成 langtype 前缀，`storetype` **不分支、不校验、不下传**
/// （head 已无该字段；签名删减是另一笔，排在语料转绿之后）。
fn build_xvalue_head(
    r#ref: u8,
    storetype: u8,
    body_len: usize,
    body_cap: usize,
    langtype: &str,
) -> Option<Vec<u8>> {
    let _ = storetype;
    if body_len > body_cap {
        return None;
    }
    let prefixed = match r#ref {
        0 => {
            if langtype.starts_with('*') || langtype.starts_with('@') {
                /* 已带前缀却声明 inline：两个事实源，拒绝。 */
                return None;
            }
            langtype.to_string()
        }
        1 => {
            if langtype.starts_with('*') || langtype.starts_with('@') {
                return None;
            }
            format!("*{langtype}")
        }
        2 => {
            if langtype.starts_with('*') || langtype.starts_with('@') {
                return None;
            }
            format!("@{langtype}")
        }
        _ => return None,
    };
    /*
     * **容量不进 wire**：bodylen 恒为有效长度；容量是物理分配属性，由后端自行分配。
     * `body_cap` 只做「不小于 body_len」的上游断言，不参与 wire 编码。
     */
    let _ = body_cap;
    crate::headlenpow::to_wire_zeroed(body_len, &prefixed)
}

// ── 内部助手 ─────────────────────────────────────────────────────────

#[inline]
pub(crate) unsafe fn cstr<'a>(p: *const c_char) -> &'a str {
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
pub(crate) fn write_err(err: *mut c_char, err_cap: u32, msg: &str) {
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
pub(crate) fn panic_msg(p: Box<dyn std::any::Any + Send>) -> String {
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
    pub storetype: u8,       // Wire storage class.
    pub body_len: i32,       // body 字节数
    pub ndim: i32,           // 物理维数（标量=0）
    pub dims: [i32; 8],      // 物理字段 dims（X_MAX_NDIM=8）
    pub langtype: [u8; 256], // 完整 kindexpr 串（含 [dims]、无前缀），NUL 终止
    pub langtype_len: i32,   // langtype 字节数（不含 NUL）
    pub body_offset: i32,    // body 在 data 内的起始偏移（= head_len）
    pub body_cap: u64,
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
        o.body_len = head.body_len;
        o.body_offset = head.head_len();
        o.body_cap = head.body_cap;
    }
}

// ── 生命周期 ─────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn kvspaceConnect(dsn: *const c_char) -> *mut Handle {
    let dsn = unsafe { cstr(dsn) };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| conn(dsn))) {
        Ok(kv) => Box::into_raw(Box::new(Handle {
            kv,
            read_cache: HashMap::new(),
            volatile_cache: HashMap::new(),
            retired: Vec::new(),
            write_buf: Vec::new(),
            pending_key: None,
            list_cache: None,
            list_gen: HashMap::new(),
        })),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn kvspaceClose(h: *mut Handle) {
    if !h.is_null() {
        let mut hd = unsafe { Box::from_raw(h) };
        // 关闭前落盘未决写：本层不得 panic 跨边界，故写 stderr 而非 eprintln!（管道断裂会 panic）。
        if let Err(e) = hd.flush() {
            let _ = writeln!(std::io::stderr(), "kvspace: close: pending write lost: {e}");
        }
        drop(hd);
    }
}

// ── KVSpace 原语 ─────────────────────────────────────────────────────

/// Fetches raw bytes for a single key (using `read_cache`, or `volatile_refresh` for control keys).
/// Returns `Ok(true)` if present and non-empty, `Ok(false)` if missing/empty, or `Err(())` on backend failure.
fn fetch_raw_one(hd: &mut Handle, key: &str) -> Result<bool, ()> {
    if is_volatile(key) {
        if !volatile_refresh(hd, key) {
            return Err(());
        }
        let vb = hd.volatile_cache.get(key).unwrap();
        return Ok(!vb.slots[vb.which].is_empty());
    }
    if hd.read_cache.contains_key(key) {
        return Ok(true);
    }
    let raw = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hd.kv.get_raw(key)))
        .map_err(|_| ())?;
    if raw.is_empty() {
        return Ok(false);
    }
    hd.read_cache.insert(key.to_string(), raw);
    Ok(true)
}

fn cached_slice<'a>(hd: &'a Handle, key: &str) -> Option<&'a [u8]> {
    if is_volatile(key) {
        let vb = hd.volatile_cache.get(key)?;
        let s = &vb.slots[vb.which];
        (!s.is_empty()).then_some(s.as_slice())
    } else {
        hd.read_cache.get(key).map(|v| v.as_slice())
    }
}

fn cached_mut_slice<'a>(hd: &'a mut Handle, key: &str) -> Option<&'a mut [u8]> {
    if is_volatile(key) {
        let vb = hd.volatile_cache.get_mut(key)?;
        let s = &mut vb.slots[vb.which];
        (!s.is_empty()).then_some(s.as_mut_slice())
    } else {
        hd.read_cache.get_mut(key).map(|v| v.as_mut_slice())
    }
}

/// Maximum leaf `Ptr` hops to follow (matches `kvspace-c`).
const MAX_PTR_HOPS: usize = 16;

/// Resolves `key` in cache (`resolve = true` follows leaf `Ptr` chains up to 16 hops).
/// Returns `Ok(Some(final_key))` if found, `Ok(None)` if missing/dangling, or `Err(())` on error/cycle/hop limit.
fn resolve_cached_key(hd: &mut Handle, key: &str, resolve: bool) -> Result<Option<String>, ()> {
    if !resolve {
        return match fetch_raw_one(hd, key)? {
            true => Ok(Some(key.to_string())),
            false => Ok(None),
        };
    }
    let mut cur = key.to_string();
    for hop in 0..=MAX_PTR_HOPS {
        if cur.is_empty() {
            return Ok(None);
        }
        if !fetch_raw_one(hd, &cur)? {
            return Ok(None);
        }
        let next_target = {
            let bytes = cached_slice(hd, &cur).ok_or(())?;
            let w = crate::headlenpow::from_wire(bytes).ok_or(())?;
            if w.reference != crate::headlenpow::Reference::Pointer {
                None
            } else {
                if hop == MAX_PTR_HOPS {
                    return Err(());
                }
                let t = std::str::from_utf8(w.body).map_err(|_| ())?;
                Some(t.to_string())
            }
        };
        match next_target {
            None => return Ok(Some(cur)),
            Some(t) => cur = t,
        }
    }
    Err(())
}

/// Borrowed read: `*out` points into the handle's cache (valid until the key is overwritten,
/// deleted, or the handle is closed; caller must not `free`). `resolve != 0` follows leaf `Ptr`
/// chains up to 16 hops (cycle/limit returns 1; missing/dangling sets `*out = NULL`, `*out_len = 0`, returns 0).
#[no_mangle]
pub extern "C" fn kvspaceGet(
    h: *mut Handle,
    key: *const c_char,
    resolve: c_int,
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
    match resolve_cached_key(hd, &key, resolve != 0) {
        Ok(Some(final_key)) => {
            if let Some(b) = cached_mut_slice(hd, &final_key) {
                unsafe {
                    *out = b.as_mut_ptr();
                    *out_len = b.len() as u32;
                }
            } else {
                unsafe {
                    *out = std::ptr::null_mut();
                    *out_len = 0;
                }
            }
            0
        }
        Ok(None) => {
            unsafe {
                *out = std::ptr::null_mut();
                *out_len = 0;
            }
            0
        }
        Err(()) => {
            unsafe {
                *out = std::ptr::null_mut();
                *out_len = 0;
            }
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn kvspaceSetValue(
    h: *mut Handle,
    key: *const c_char,
    value: *const u8,
    value_len: u32,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let Some(hd) = (unsafe { h.as_mut() }) else {
        return 1;
    };
    if key.is_null() || value.is_null() || value_len == 0 {
        write_err(err, err_cap, "invalid SetValue input");
        return 1;
    }
    if let Err(e) = hd.flush() {
        write_err(err, err_cap, &e);
        return 1;
    }
    let key = unsafe { cstr(key) }.to_string();
    let raw = unsafe { std::slice::from_raw_parts(value, value_len as usize) };
    let Some(whead) = crate::headlenpow::from_wire(raw) else {
        write_err(err, err_cap, "invalid XValue");
        return 1;
    };
    /* attr 区长度从 head 读（无前缀）；空 attr 合法（=0）。 */
    let Some(attr) = crate::headlenpow::attr_len(raw) else {
        write_err(err, err_cap, "invalid XValue");
        return 1;
    };
    let Some(total) = (whead.len as usize)
        .checked_add(crate::headlenpow::PREFIX)
        .and_then(|x| x.checked_add(attr as usize))
    else {
        write_err(err, err_cap, "invalid XValue");
        return 1;
    };
    if total > raw.len() {
        write_err(err, err_cap, "invalid XValue");
        return 1;
    }
    let raw = raw[..total].to_vec();
    hd.list_cache = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let val = from_wire(&raw);
        hd.kv.set(&[KVPair {
            key: key.clone(),
            val,
            raw: Some(raw),
        }])
    }));
    match result {
        Ok(result) => {
            let code = result_to_code(result, err, err_cap);
            if code == 0 {
                /* 写后失效：不把写值留在读缓存里，避免写密集负载无界占用。 */
                hd.cache_invalidate(&key);
            }
            code
        }
        Err(_) => {
            write_err(err, err_cap, "SetValue backend panic");
            1
        }
    }
}

/// 定位读：借用读 key 值的 [offset, offset+len) 字节，*out 指向该 key 读缓存（活到该 key 被写/删或 close，调用方不得 free）。
/// Always follows leaf `Ptr` chains; sets `*out = NULL` and `*out_len = 0` when missing, empty, or out of range.
#[no_mangle]
pub extern "C" fn kvspaceGetPart(
    h: *mut Handle,
    key: *const c_char,
    offset: u32,
    len: u32,
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
    let final_key = match resolve_cached_key(hd, &key, true) {
        Ok(Some(k)) => k,
        Ok(None) => {
            unsafe {
                *out = std::ptr::null_mut();
                *out_len = 0;
            }
            return 0;
        }
        Err(()) => {
            unsafe {
                *out = std::ptr::null_mut();
                *out_len = 0;
            }
            return 1;
        }
    };
    let Some(b) = cached_mut_slice(hd, &final_key) else {
        unsafe {
            *out = std::ptr::null_mut();
            *out_len = 0;
        }
        return 0;
    };
    let s = (offset as usize).min(b.len());
    let e = s.saturating_add(len as usize).min(b.len());
    unsafe {
        if e <= s {
            *out = std::ptr::null_mut();
            *out_len = 0;
        } else {
            *out = b.as_mut_ptr().add(s);
            *out_len = (e - s) as u32;
        }
    }
    0
}

/// Positional write into `[offset, offset + buf_len)` of an existing key (follows leaf `Ptr` chains; synchronous).
#[no_mangle]
pub extern "C" fn kvspaceSetPart(
    h: *mut Handle,
    key: *const c_char,
    offset: u32,
    buf: *const u8,
    buf_len: u32,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    if buf_len > 0 && buf.is_null() {
        write_err(err, err_cap, "invalid SetPart buffer");
        return 1;
    }
    if let Err(e) = hd.flush() {
        write_err(err, err_cap, &e);
        return 1;
    }
    let key = unsafe { cstr(key) }.to_string();
    let target_key = match resolve_cached_key(hd, &key, true) {
        Ok(Some(k)) => k,
        Ok(None) => {
            write_err(err, err_cap, &format!("set_part: missing key {key}"));
            return 1;
        }
        Err(()) => {
            write_err(err, err_cap, &format!("set_part: ptr resolve failed at {key}"));
            return 1;
        }
    };
    /* 判界不在这里：`backend.rs::set_part` 是唯一入口，三个后端共用同一份判据。
     * 在那之前判一次，等于把「同一个量」写成两处——两处迟早会分叉。 */
    let data: &[u8] = if buf_len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(buf, buf_len as usize) }
    };
    hd.list_cache = None;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hd.kv.set_part(&target_key, offset, data)
    }))
    .map_err(panic_msg)
    .and_then(|x| x);
    let code = result_to_code(r, err, err_cap);
    if code == 0 {
        hd.cache_invalidate(&target_key);
    }
    code
}

/// Decodes the value header using `read_cache` and following leaf `Ptr` chains (up to 16 hops).
/// Returns 1 when missing, empty, dangling, or exceeding the hop limit.
#[no_mangle]
pub extern "C" fn kvspaceGetHead(
    h: *mut Handle,
    key: *const c_char,
    out: *mut kvspaceHead_t,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    if out.is_null() || hd.flush().is_err() {
        return 1;
    }
    let key = unsafe { cstr(key) }.to_string();
    let final_key = match resolve_cached_key(hd, &key, true) {
        Ok(Some(k)) => k,
        _ => return 1,
    };
    let Some(value) = cached_slice(hd, &final_key) else {
        return 1;
    };
    /* langtype 在值 attr 区（单 key 整体）；无类型 = 空 attr。 */
    let head = crate::xvalue::from_wire_head(value);
    if head.headlen == 0 {
        return 1;
    }
    fill_head(&head, out);
    0
}

/// 就地写：key 必须已存在、body_len 必须等于原 body_len——把原 head+body 攒进 write_buf、
/// 置 pending，返回 write_buf 内 body 偏移指针供直接写；违反前置条件 → 非 0 + err。
/// When `resolve != 0`, follows leaf `Ptr` chains (up to 16 hops) to mutate the terminal target in place.
#[no_mangle]
pub extern "C" fn kvspaceWriteInPlace(
    h: *mut Handle,
    key: *const c_char,
    resolve: c_int,
    body_len: u32,
    body: *mut *mut u8,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let hd = match unsafe { h.as_mut() } {
        Some(x) => x,
        None => return 1,
    };
    if body.is_null() {
        write_err(err, err_cap, "kvspace: null body out-pointer");
        return 1;
    }
    if let Err(e) = hd.flush() {
        write_err(err, err_cap, &e);
        return 1;
    }
    let key = unsafe { cstr(key) }.to_string();
    let target_key = match resolve_cached_key(hd, &key, resolve != 0) {
        Ok(Some(k)) => k,
        Ok(None) => {
            write_err(err, err_cap, "kvspace: write-in-place on missing key");
            return 1;
        }
        Err(()) => {
            write_err(err, err_cap, "kvspace: write-in-place ptr resolve failed");
            return 1;
        }
    };
    let existing = match hd.read_cache.remove(&target_key) {
        Some(v) => v,
        None => match cached_slice(hd, &target_key).map(|s| s.to_vec()) {
            Some(v) => {
                hd.volatile_cache.remove(&target_key);
                v
            }
            None => {
                write_err(err, err_cap, "kvspace: write-in-place on missing key");
                return 1;
            }
        },
    };
    let head = from_wire_head(&existing);
    let head_len = head.head_len() as usize;
    let blen = head.body_len.max(0) as usize;
    let attr = crate::headlenpow::attr_len(&existing).unwrap_or(0) as usize;
    if head.headlen == 0
        || head.body_len < 0
        || head.body_len as u32 != body_len
        || head_len.checked_add(blen).and_then(|x| x.checked_add(attr)) != Some(existing.len())
    {
        write_err(err, err_cap, "kvspace: write-in-place body_len mismatch");
        return 1;
    }
    hd.cache_invalidate(&target_key);
    hd.write_buf = existing;
    hd.pending_key = Some(target_key);
    /* body 指针供调用方直接写 8 字节标量：对齐是前条件，不是假设。 */
    let body_ptr = unsafe { hd.write_buf.as_mut_ptr().add(head_len) };
    debug_assert_eq!(
        body_ptr as usize % 8,
        0,
        "kvspace: write-in-place body pointer is not 8-byte aligned"
    );
    unsafe { *body = body_ptr };
    0
}

/// 新位置写：按 (ref, storetype, body_len, body_cap) 攒好 head 到 write_buf、置 pending，返回 body 偏移指针。
/// langtype（可为空）随 head + attr 区整体写入——单 key 一次写。
#[no_mangle]
pub extern "C" fn kvspaceWriteNewPlace(
    h: *mut Handle,
    key: *const c_char,
    r#ref: u8,
    storetype: u8,
    body_len: u32,
    body_cap: u64,
    langtype: *const c_char,
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
    let Ok(cap) = usize::try_from(body_cap) else {
        write_err(err, err_cap, "capacity exceeds platform size");
        return 1;
    };
    let Some(xv) = build_xvalue_head(r#ref, storetype, body_len as usize, cap, lt) else {
        write_err(err, err_cap, "invalid XValue geometry");
        return 1;
    };
    /* 固定头：body 偏移恒 = PREFIX（=8）。 */
    let head_len = crate::headlenpow::PREFIX;
    hd.cache_invalidate(&key);
    hd.write_buf = xv;
    hd.pending_key = Some(key);
    /* body 指针供调用方直接写 8 字节标量：对齐是前条件，不是假设。 */
    let body_ptr = unsafe { hd.write_buf.as_mut_ptr().add(head_len) };
    debug_assert_eq!(
        body_ptr as usize % 8,
        0,
        "kvspace: write-new-place body pointer is not 8-byte aligned"
    );
    unsafe { *body = body_ptr };
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
    /* Always re-list so callers polling child count observe external additions; snapshot serves subsequent ListAt(0..). */
    match hd.list_names(&prefix, expand_ext != 0, resolve != 0, None) {
        Ok(names) => {
            unsafe { *out_count = names.len() as i32 };
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
    if idx < 0 || hd.flush().is_err() || hd.kv.validate_dir(&prefix).is_err() {
        return 1;
    }
    // Reuses the cached list when `(prefix, expand_ext, resolve)` match and `idx` strictly
    // increases without intervening writes; repeated or non-increasing `idx` triggers a re-list.
    let name = match hd.list_names(&prefix, expand_ext != 0, resolve != 0, Some(idx)) {
        Ok(names) => match names.get(idx as usize) {
            Some(n) => n.clone(),
            None => return 1,
        },
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
    let code = result_to_code(catch_panic(|| kv.del(&keys)), err, err_cap);
    if let Some(hd) = unsafe { h.as_mut() } {
        let mut is_handoff = false;
        for k in &keys {
            hd.cache_invalidate(k);
            if (k.starts_with("/lib/") && k.contains("/vids/"))
                || k.starts_with("/vthread/\u{2025}vids/")
            {
                is_handoff = true;
            }
        }
        if is_handoff {
            for (_, old) in hd.read_cache.drain() {
                hd.retired.push(old);
            }
            hd.list_cache = None;
        }
    }
    code
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
    let prefix = unsafe { cstr(prefix) }.to_string();
    let code = result_to_code(catch_panic(|| kv.del_tree(&prefix)), err, err_cap);
    if let Some(hd) = unsafe { h.as_mut() } {
        hd.cache_invalidate_tree(&prefix);
    }
    code
}

/// 单键复制。**新存储 ABI 的 `kvspaceCp` 就是这一个符号**：`storage.h` 的签名
/// `(handle, old_key, new_key, err, err_cap)` 与旧 `kvspace.h` 逐参相同，
/// 不另写第二份实现（两份同名不同义的副本是二义性的温床）。
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
    let src = unsafe { cstr(src) }.to_string();
    let dst = unsafe { cstr(dst) }.to_string();
    /* 新 ABI 的 `kvspaceCp` 就是本符号：源不存在必须 `-ENOENT`，不是一律 1。
       旧调用方只看 0/非 0，故这条改动不破旧语义。 */
    let r = catch_panic(|| kv.cp(&src, &dst));
    let code = match &r {
        Err(e) if e.contains("source not found") => {
            write_err(err, err_cap, e);
            -libc::ENOENT
        }
        _ => result_to_code(r, err, err_cap),
    };
    if let Some(hd) = unsafe { h.as_mut() } {
        hd.cache_invalidate(&dst);
    }
    code
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
    let src = unsafe { cstr(src) }.to_string();
    let dst = unsafe { cstr(dst) }.to_string();
    let code = result_to_code(catch_panic(|| kv.cp_tree(&src, &dst)), err, err_cap);
    if let Some(hd) = unsafe { h.as_mut() } {
        hd.cache_invalidate_tree(&dst);
    }
    code
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
    let src = unsafe { cstr(src) }.to_string();
    let dst = unsafe { cstr(dst) }.to_string();
    let code = result_to_code(catch_panic(|| kv.cp_list(&src, &dst)), err, err_cap);
    if let Some(hd) = unsafe { h.as_mut() } {
        hd.cache_invalidate_tree(&dst);
    }
    code
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
    let key = unsafe { cstr(key) }.to_string();
    /* watch 目标与 key 值同型（相等判据）：langtype 在值 attr 区。 */
    let target_v = from_wire(unsafe {
        std::slice::from_raw_parts(target, target_len as usize)
    });
    let kv = &mut hd.kv;
    let v = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        kv.watch(&key, &target_v, Duration::from_nanos(tick_ns))
    })) {
        Ok(v) => v,
        Err(_) => {
            unsafe {
                *out = std::ptr::null_mut();
                *out_len = 0;
            }
            return 1;
        }
    };
    for (_, old) in hd.read_cache.drain() {
        hd.retired.push(old); /* Cross-process sync boundary: keep old views alive until Close and force fresh reads. */
    }
    hd.list_cache = None;
    hd.read_cache.insert(key.clone(), v.to_wire());
    let b = hd.read_cache.get_mut(&key).unwrap();
    unsafe {
        *out = b.as_mut_ptr();
        *out_len = b.len() as u32;
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
    let code = result_to_code(catch_panic(|| kv.clear()), err, err_cap);
    if code == 0 {
        if let Some(hd) = unsafe { h.as_mut() } {
            hd.read_cache.clear();
        }
    }
    code
}

// ── 标准标量构造器（返回完整 XValue 字节） ─────────────────────────────
//
// Signatures and wire output match `kvspace.h`, frontend `codec_abi.c`, and `kvspace-c`:
// `langtype` is written verbatim into the attr trailer (NULL/empty -> empty attr).

/// Hands ownership of `wire` to the caller (`free()`-able), or returns 1 if invalid/oversized.
fn emit(wire: Option<Vec<u8>>, out: *mut *mut u8, out_len: *mut u32) -> c_int {
    if out.is_null() || out_len.is_null() {
        return 1;
    }
    match wire {
        Some(w) if u32::try_from(w.len()).is_ok() => alloc(w, out, out_len),
        _ => 1,
    }
}

/// Scalar constructor: inline, `body = raw`.
fn new_scalar(raw: &[u8], langtype: *const c_char, out: *mut *mut u8, out_len: *mut u32) -> c_int {
    let lt = unsafe { cstr(langtype) };
    emit(crate::headlenpow::to_wire(raw, lt), out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewPtr(
    target_kindexpr: *const c_char,
    target: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    if target_kindexpr.is_null() || target.is_null() {
        return 1;
    }
    let kindexpr = unsafe { cstr(target_kindexpr) };
    // `any` 不是类型名（空 kindexpr = 不约束，见 spec [[文法与合法性]]）。
    if kindexpr == "any" {
        return 1;
    }
    // Write raw target path bytes as body; reject empty target (matches kvspaceXvalueHeadNewPtr).
    let path = unsafe { CStr::from_ptr(target) }.to_bytes();
    if path.is_empty() {
        return 1;
    }
    /* 指针 body 是目标 key 路径，指针性由 langtype 的 `*` 前缀表达。 */
    let prefixed = if kindexpr.starts_with('*') {
        kindexpr.to_string()
    } else {
        format!("*{kindexpr}")
    };
    emit(
        crate::headlenpow::to_wire(path, &prefixed),
        out,
        out_len,
    )
}

#[no_mangle]
pub extern "C" fn kvspaceNewChar(
    bytes: *const u8,
    len: u32,
    langtype: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    if len != 0 && bytes.is_null() {
        return 1;
    }
    let body: &[u8] = if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(bytes, len as usize) }
    };
    let lt = unsafe { cstr(langtype) };
    emit(
        crate::headlenpow::to_wire(body, lt),
        out,
        out_len,
    )
}

#[no_mangle]
pub extern "C" fn kvspaceNewBool(
    v: u8,
    langtype: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    new_scalar(&[u8::from(v != 0)], langtype, out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewInt64(
    v: i64,
    langtype: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    new_scalar(&v.to_le_bytes(), langtype, out, out_len)
}

#[no_mangle]
pub extern "C" fn kvspaceNewFloat64(
    v: f64,
    langtype: *const c_char,
    out: *mut *mut u8,
    out_len: *mut u32,
) -> c_int {
    new_scalar(&v.to_bits().to_le_bytes(), langtype, out, out_len)
}

// ── slot view（shm-only）─────────────────────────────────────────────

/// 与 kvspace/include/kvspace/kvspace.h 的 kvspaceXvalueBodyView_t 逐字段一致。
#[repr(C)]
pub struct KvspaceXvalueBodyView {
    pub head: *mut u8,
    pub headlen: u32,
    pub body: *mut u8,
    pub body_cap: u64,
    pub cap: u64,
}

/// durable 后端没有映射内槽：恒返回 -2（不支持），调用方回退既有读写 API。
#[no_mangle]
pub extern "C" fn kvspaceXvalueBodyView(
    _h: *mut Handle,
    _r: *const std::ffi::c_void,
    _key: *const c_char,
    _out: *mut KvspaceXvalueBodyView,
) -> c_int {
    -2
}

// ── P2 view token（shm-only）────────────────────────────────────────
//
// 与 kvspace/include/kvspace/kvspace.h 的 kvspaceViewToken_t 逐字段一致。
// durable 后端没有映射内槽与垃圾环，全部恒返回 -2（不支持）；调用方（runtime）
// 见 -2 即回退既有读写 API，且每个句柄只判定一次。没有 kvspaceViewRelease：
// 历史上从未导出该符号。

#[repr(C)]
pub struct KvspaceViewToken {
    pub head: *mut u8,
    pub body: *mut u8,
    pub headlen: u32,
    pub value_len: u32,
    pub cap: u64,
    pub body_cap: u64,
    pub struct_gen: u32,
    pub value_gen: u32,
    pub store_id: u64,
    pub node_id: u32,
    pub _pad: u32,
}

#[no_mangle]
pub extern "C" fn kvspaceViewSweep(_h: *mut Handle) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewResolvePrefix(
    _h: *mut Handle,
    _path: *const c_char,
    _ref: *mut std::ffi::c_void,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewResolve(
    _h: *mut Handle,
    _key: *const c_char,
    _tok: *mut KvspaceViewToken,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewFromRef(
    _h: *mut Handle,
    _r: *const std::ffi::c_void,
    _key: *const c_char,
    _tok: *mut KvspaceViewToken,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewResolveFrom(
    _h: *mut Handle,
    _parent: *const std::ffi::c_void,
    _rel: *const c_char,
    _tok: *mut KvspaceViewToken,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewCheck(
    _h: *mut Handle,
    _tok: *const KvspaceViewToken,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewWrite(
    _h: *mut Handle,
    _tok: *mut KvspaceViewToken,
    _data: *const u8,
    _len: u32,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewDel(
    _h: *mut Handle,
    _tok: *mut KvspaceViewToken,
    _key: *const c_char,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewRefresh(
    _h: *mut Handle,
    _tok: *mut KvspaceViewToken,
    _key: *const c_char,
) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewAck(_h: *mut Handle, _epoch: *mut u32) -> c_int {
    -2
}

#[no_mangle]
pub extern "C" fn kvspaceViewQuiesce(_h: *mut Handle, _on: c_int) -> c_int {
    -2
}
