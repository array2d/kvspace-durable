//! 存储层 ABI 第二阶段（`kvspace/include/kvspace/storage.h`）的 durable 实现。
//!
//! 与旧 ABI（`kvspace.h`）**并存**：旧符号一行不动，这里只新增 9 个全名符号。
//! 同一个 `Handle`、同一份读缓存与惰性写缓冲。
//!
//! **`resolve`/`realloc` 的偏移在 durable 上不可兑现**：`storage.h` 的契约是「上层用
//! 本进程基址 + 偏移读写」，那要求值在本进程映射区内——shm 成立，fs/redis/s3 不成立
//! （没有映射区，也没有「调用方写进映射后落盘」的提交点）。这两个符号因此**诚实失败**：
//! 返回 `KVSPACE_BACKEND` 并在 err 写明原因，绝不返回 0 偏移假装成功（0 是合法偏移）。
//! 读写仍走旧 ABI 的 `kvspaceGet`/`kvspaceSetValue` 等路径。
//!
//! 新路径**不实现 extindex**：`list`/`listlen` 只列物理直接子项。

use std::ffi::{c_char, c_int};

use crate::kvspace::KVSpace;

use crate::ffi::{cstr, kv_flush, panic_msg, write_err, Handle};

// ── kvspace_status_t（storage.h）：负数 errno，不是 0/-1 哨兵 ──────────────

const KVSPACE_OK: c_int = 0;
const KVSPACE_INVALID: c_int = -libc::EINVAL;
const KVSPACE_MISSING: c_int = -libc::ENOENT;
const KVSPACE_CAPACITY: c_int = -libc::ENOSPC;
const KVSPACE_NOT_INDEX: c_int = -libc::ENOTDIR;
const KVSPACE_HAS_CHILDREN: c_int = -libc::ENOTEMPTY;
/// 「这个后端做不到」≠「后端调用失败」：前者 `-ENOTSUP`，后者 `-EIO`。
const KVSPACE_UNSUPPORTED: c_int = -libc::ENOTSUP;
const KVSPACE_BACKEND: c_int = -libc::EIO;

/// 索引不存在时 `list_direct` 的 Err 前缀（与「后端调用失败」区分）。
const ERR_LIST_MISSING: &str = "kvspace: no such index";

// ── generation ────────────────────────────────────────────────────────
//
// 「key 集合变化」才前进，值修改不前进（验收 P2.6）。判据是**名字集合**本身：
// 每次列举重扫后端，把名字集合与上次快照比对——集合变了计数 +1。
// 不用「写操作递增」那种代理：写同一个 key 一百次集合没变，generation 就得停着。

/// 一个索引的列举快照：该集合对应的 generation + 上次见到的名字集合。
/// `generation == 0` 表示尚未列举过（0 不对外暴露，首次列举即 1）。
#[derive(Default)]
pub struct ListGen {
    generation: u64,
    names: Vec<String>,
}

/// 一次列举的结果：后端顺序的名字、总字节数、最长名字节数、generation。
pub struct Listing {
    pub names: Vec<String>,
    pub total_length: u32,
    pub max_name_length: u32,
    pub generation: u64,
}

impl Handle {
    /// 重扫该索引的直接子项，并按「集合是否变化」推进 generation。
    ///
    /// 集合用**排序后**的快照比对：后端顺序未定义，不排序会把顺序抖动误判成集合变化。
    ///
    /// 索引不存在 → `Err`（调用方转成 `KVSPACE_MISSING`）。判据是「根值存在 **或** 有子项」：
    /// 空索引有值（`makeindex` 建的空值），删根之后两者都空。
    fn list_direct(&mut self, key: &str) -> Result<Listing, String> {
        self.flush()?;
        let kv = &mut self.kv;
        let k = key.to_string();
        /* 新 ABI 不叠加 extindex：expand_ext = false；父路径 link 仍解析。 */
        let names = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            kv.list(&k, false, true)
        }))
        .map_err(panic_msg)?;
        if names.is_empty() {
            /* 空名字集有两种含义，必须分开：空索引 vs 索引不存在。 */
            let root_exists = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                !kv.get_raw(&k).is_empty()
            }))
            .map_err(panic_msg)?;
            if !root_exists {
                return Err(format!("{ERR_LIST_MISSING}:{key}"));
            }
        }

        let mut sorted = names.clone();
        sorted.sort();
        let entry = self.list_gen.entry(key.to_string()).or_default();
        if entry.generation == 0 {
            entry.generation = 1;
            entry.names = sorted;
        } else if entry.names != sorted {
            entry.generation += 1;
            entry.names = sorted;
        }
        let generation = entry.generation;

        /* 名字以 NUL 分隔：总长 = Σ(len + 1)；M 按同一字节串计（含目录项尾 /）。 */
        let total_length = names.iter().map(|n| n.len() + 1).sum::<usize>();
        let max_name_length = names.iter().map(String::len).max().unwrap_or(0);
        Ok(Listing {
            total_length: u32::try_from(total_length)
                .map_err(|_| "kvspace: list too large".to_string())?,
            max_name_length: u32::try_from(max_name_length)
                .map_err(|_| "kvspace: name too long".to_string())?,
            generation,
            names,
        })
    }
}

// ── 9 个符号 ──────────────────────────────────────────────────────────

/// key 必须以 `/` 或 `·` 结尾才是索引（目录身份 = key 形状，不靠元数据）。
fn is_index_key(key: &str) -> bool {
    key.ends_with(crate::r#const::PATH_SEP) || key.ends_with(crate::r#const::OBJ_SEP)
}

/// 判 key 是否存在，**先于**索引形态判定。
///
/// 存在 = 有根值 **或** 有后代。两条缺一不可：只有子项的 `/only`（无根值）算存在
/// （它有后代），而 `/never` 两条都不满足。顺序很重要——先判形态会把「不存在」
/// 报成 `NOT_INDEX`，调用方据此以为「这个 key 在、只是不是索引」，判断全错。
///
/// 无根值时按父前缀列举后比对名字：索引前缀取 key 自身；普通 key 取最后一个
/// `/` 或 `·`（成员路径 `/m·k` 的父前缀就是 `/m·`）。
///
/// 切分走 `char_indices`：`·` 是 2 字节 UTF-8，拿字节下标切会落在码点中间 panic。
fn key_exists(hd: &mut Handle, key: &str) -> bool {
    let has_value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        !hd.kv.get_raw(key).is_empty()
    }))
    .unwrap_or(false);
    if has_value {
        return true;
    }
    let (probe, wanted) = if is_index_key(key) {
        (key.to_string(), None)
    } else {
        match key.char_indices().rfind(|(_, c)| *c == PATH_SEP_CHAR || *c == OBJ_SEP_CHAR) {
            Some((at, c)) => {
                let name = &key[at + c.len_utf8()..];
                if name.is_empty() {
                    return false;
                }
                (key[..at + c.len_utf8()].to_string(), Some(name))
            }
            None => return false,
        }
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hd.kv.list(&probe, false, true)))
        .map(|names| match wanted {
            /* 索引：有任一后代即存在。 */
            None => !names.is_empty(),
            /* 普通 key：名字必须出现在父前缀的列举里（目录项带尾 /）。 */
            Some(w) => names.iter().any(|n| n.trim_end_matches('/') == w),
        })
        .unwrap_or(false)
}

const PATH_SEP_CHAR: char = '/';
const OBJ_SEP_CHAR: char = '·';

/// `kvspaceXvalueInProcessAddress`：进程内值区基址。**durable 恒返回 NULL**——没有映射区，
/// 于是 `base + offset` 这条路在本后端不存在；调用方必须判空后回退旧 ABI 的读写。
/// 与 `resolve`/`realloc` 返回 `-ENOTSUP` 是同一件事的两面：没有基址，偏移就不可兑现。
#[no_mangle]
pub extern "C" fn kvspaceXvalueInProcessAddress(_h: *mut Handle) -> *mut u8 {
    std::ptr::null_mut()
}

/// `kvspaceResolve`：durable 无映射区，偏移不可兑现——诚实失败。
#[no_mangle]
pub extern "C" fn kvspaceResolve(
    h: *mut Handle,
    key: *const c_char,
    _parent_key_anchor: u64,
    out_value_offset: *mut u64,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let _ = (h, key, out_value_offset);
    write_err(
        err,
        err_cap,
        "kvspaceResolve: durable backend has no mapped value region (no offset→base accessor); read with kvspaceGet",
    );
    KVSPACE_UNSUPPORTED
}

/// `kvspaceRealloc`：同上——没有「调用方写进映射后落盘」的提交点。
#[no_mangle]
pub extern "C" fn kvspaceRealloc(
    h: *mut Handle,
    key: *const c_char,
    _body_length: u64,
    _attr_length: u64,
    out_value_offset: *mut u64,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let _ = (h, key, out_value_offset);
    write_err(
        err,
        err_cap,
        "kvspaceRealloc: durable backend has no mapped value region; write with kvspaceWriteNewPlace/kvspaceSetValue",
    );
    KVSPACE_UNSUPPORTED
}

/// `kvspaceFree`：删值；空索引连索引节点一起删；非空索引 HAS_CHILDREN。
#[no_mangle]
pub extern "C" fn kvspaceFree(
    h: *mut Handle,
    key: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let key = unsafe { cstr(key) }.to_string();
    let hd = match unsafe { h.as_mut() } {
        Some(hd) => hd,
        None => {
            write_err(err, err_cap, "kvspace: null handle");
            return KVSPACE_INVALID;
        }
    };
    if key.is_empty() {
        write_err(err, err_cap, "kvspaceFree: empty key");
        return KVSPACE_INVALID;
    }
    let index = is_index_key(&key);

    /* 非空索引拒绝：先看直接子项。判据用存储层自己的列举，不用缓存。 */
    if index {
        match hd.list_direct(&key) {
            Ok(listing) if !listing.names.is_empty() => {
                write_err(
                    err,
                    err_cap,
                    &format!(
                        "kvspaceFree: index {key} still has {} child(ren)",
                        listing.names.len()
                    ),
                );
                return KVSPACE_HAS_CHILDREN;
            }
            Ok(_) => {}
            Err(e) => {
                write_err(err, err_cap, &e);
                return KVSPACE_BACKEND;
            }
        }
    } else {
        /* 非索引：值不存在即 MISSING（resolve 语义的等价判据）。 */
        let exists = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            !hd.kv.get_raw(&key).is_empty()
        }))
        .unwrap_or(false);
        if !exists {
            write_err(err, err_cap, &format!("kvspaceFree: no such key {key}"));
            return KVSPACE_MISSING;
        }
    }

    let kv = match unsafe { kv_flush(h) } {
        Ok(kv) => kv,
        Err(e) => {
            write_err(err, err_cap, &e);
            return KVSPACE_BACKEND;
        }
    };
    let k = key.clone();
    let del_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        /* 索引：连 key 本体一起删（`/a/` 与 `/a` 两种写法都清）。 */
        if index {
            let base = k.trim_end_matches(['/', '·']).to_string();
            let mut keys = vec![k.clone()];
            if base != k && !base.is_empty() {
                keys.push(base);
            }
            kv.del(&keys)
        } else {
            kv.del(std::slice::from_ref(&k))
        }
    }));
    match del_result {
        Ok(Ok(())) => {
            if let Some(hd) = unsafe { h.as_mut() } {
                hd.cache_invalidate(&key);
            }
            KVSPACE_OK
        }
        Ok(Err(e)) => {
            write_err(err, err_cap, &e);
            KVSPACE_BACKEND
        }
        Err(p) => {
            write_err(err, err_cap, &panic_msg(p));
            KVSPACE_BACKEND
        }
    }
}

/// `kvspaceMkindex`：只建 index 位置——空值（body 0、无 langtype），body 归上层。
///
/// 旧 `kvspaceMkindex` 塞 `langtype "lib"` 是后端自带的标记；新 ABI 不规定 body 内容，
/// 故这里写**规范 None**（空 body + 空 attr），上层随后用写路径填它。
#[no_mangle]
pub extern "C" fn kvspaceMkindex(
    h: *mut Handle,
    key: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let key = unsafe { cstr(key) }.to_string();
    if !is_index_key(&key) || key.is_empty() {
        write_err(
            err,
            err_cap,
            &format!("kvspaceMkindex: index key must end with / or ·: {key}"),
        );
        return KVSPACE_INVALID;
    }
    let kv = match unsafe { kv_flush(h) } {
        Ok(kv) => kv,
        Err(e) => {
            write_err(err, err_cap, &e);
            return KVSPACE_BACKEND;
        }
    };
    let k = key.clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        /* 已存在即幂等成功：不覆盖上层已写进去的 body。 */
        if !kv.get_raw(&k).is_empty() {
            return Ok(());
        }
        let value = crate::headlenpow::to_wire(&[], "")
            .ok_or_else(|| format!("kvspaceMkindex: invalid index {k}"))?;
        kv.set(&[crate::kvspace::KVPair {
            key: k.clone(),
            val: crate::xvalue::XValue::None,
            raw: Some(value),
        }])
    }));
    match r {
        Ok(Ok(())) => KVSPACE_OK,
        Ok(Err(e)) => {
            write_err(err, err_cap, &e);
            KVSPACE_BACKEND
        }
        Err(p) => {
            write_err(err, err_cap, &panic_msg(p));
            KVSPACE_BACKEND
        }
    }
}

/// `kvspaceDeltree`：递归删子树。
#[no_mangle]
pub extern "C" fn kvspaceDeltree(
    h: *mut Handle,
    key: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let key = unsafe { cstr(key) }.to_string();
    if key.is_empty() {
        write_err(err, err_cap, "kvspaceDeltree: empty key");
        return KVSPACE_INVALID;
    }
    let kv = match unsafe { kv_flush(h) } {
        Ok(kv) => kv,
        Err(e) => {
            write_err(err, err_cap, &e);
            return KVSPACE_BACKEND;
        }
    };
    let k = key.clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| kv.del_tree(&k)));
    match r {
        Ok(Ok(())) => {
            if let Some(hd) = unsafe { h.as_mut() } {
                hd.cache_invalidate_tree(&key);
            }
            KVSPACE_OK
        }
        Ok(Err(e)) => {
            write_err(err, err_cap, &e);
            KVSPACE_BACKEND
        }
        Err(p) => {
            write_err(err, err_cap, &panic_msg(p));
            KVSPACE_BACKEND
        }
    }
}

/// `kvspaceCpdir`：递归复制子树（目标已存在时先删再复制）。
#[no_mangle]
pub extern "C" fn kvspaceCpdir(
    h: *mut Handle,
    source_key: *const c_char,
    destination_key: *const c_char,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    let src = unsafe { cstr(source_key) }.to_string();
    let dst = unsafe { cstr(destination_key) }.to_string();
    if src.is_empty() || dst.is_empty() {
        write_err(err, err_cap, "kvspaceCpdir: empty key");
        return KVSPACE_INVALID;
    }
    let kv = match unsafe { kv_flush(h) } {
        Ok(kv) => kv,
        Err(e) => {
            write_err(err, err_cap, &e);
            return KVSPACE_BACKEND;
        }
    };
    let (s, d) = (src.clone(), dst.clone());
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| kv.cp_tree(&s, &d)));
    match r {
        Ok(Ok(())) => {
            if let Some(hd) = unsafe { h.as_mut() } {
                hd.cache_invalidate_tree(&dst);
            }
            KVSPACE_OK
        }
        Ok(Err(e)) => {
            let code = if e.contains("source not found") {
                KVSPACE_MISSING
            } else {
                KVSPACE_BACKEND
            };
            write_err(err, err_cap, &e);
            code
        }
        Err(p) => {
            write_err(err, err_cap, &panic_msg(p));
            KVSPACE_BACKEND
        }
    }
}

/// `kvspaceListlen`：直接子项数 N 与最长子项名字节数 M（含目录项尾 `/`）。
#[no_mangle]
pub extern "C" fn kvspaceListlen(
    h: *mut Handle,
    key: *const c_char,
    out_count: *mut i64,
    out_max_name_length: *mut u32,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if !out_count.is_null() {
        unsafe { *out_count = 0 };
    }
    if !out_max_name_length.is_null() {
        unsafe { *out_max_name_length = 0 };
    }
    let key = unsafe { cstr(key) }.to_string();
    let hd = match unsafe { h.as_mut() } {
        Some(hd) => hd,
        None => {
            write_err(err, err_cap, "kvspace: null handle");
            return KVSPACE_INVALID;
        }
    };
    if key.is_empty() {
        write_err(err, err_cap, "kvspaceListlen: empty key");
        return KVSPACE_INVALID;
    }
    /* **先存在、后形态**：不存在的 key 谈不上「不是索引」。 */
    if !key_exists(hd, &key) {
        write_err(err, err_cap, &format!("kvspaceListlen: no such key: {key}"));
        return KVSPACE_MISSING;
    }
    if !is_index_key(&key) {
        write_err(
            err,
            err_cap,
            &format!("kvspaceListlen: not an index: {key}"),
        );
        return KVSPACE_NOT_INDEX;
    }
    match hd.list_direct(&key) {
        Ok(listing) => {
            if !out_count.is_null() {
                unsafe { *out_count = listing.names.len() as i64 };
            }
            if !out_max_name_length.is_null() {
                unsafe { *out_max_name_length = listing.max_name_length };
            }
            KVSPACE_OK
        }
        Err(e) => list_error(err, err_cap, &e),
    }
}

/// 「索引不存在」与「后端调用失败」是两件事：前者 `-ENOENT`，后者 `-EIO`。
fn list_error(err: *mut c_char, err_cap: u32, message: &str) -> c_int {
    write_err(err, err_cap, message);
    if message.starts_with(ERR_LIST_MISSING) {
        KVSPACE_MISSING
    } else {
        KVSPACE_BACKEND
    }
}

/// `kvspaceList`：一次复制全部直接子项名（NUL 分隔，后端顺序，不排序）。
///
/// `out_names = NULL` 只回报长度/个数/generation；容量不足返回 CAPACITY，**仍写出**
/// 所需长度与个数（不截断、不写半截缓冲）。
#[no_mangle]
pub extern "C" fn kvspaceList(
    h: *mut Handle,
    key: *const c_char,
    out_names: *mut c_char,
    out_capacity: u32,
    out_length: *mut u32,
    out_count: *mut u32,
    out_generation: *mut u64,
    err: *mut c_char,
    err_cap: u32,
) -> c_int {
    if !out_length.is_null() {
        unsafe { *out_length = 0 };
    }
    if !out_count.is_null() {
        unsafe { *out_count = 0 };
    }
    if !out_generation.is_null() {
        unsafe { *out_generation = 0 };
    }
    let key = unsafe { cstr(key) }.to_string();
    let hd = match unsafe { h.as_mut() } {
        Some(hd) => hd,
        None => {
            write_err(err, err_cap, "kvspace: null handle");
            return KVSPACE_INVALID;
        }
    };
    if key.is_empty() {
        write_err(err, err_cap, "kvspaceList: empty key");
        return KVSPACE_INVALID;
    }
    /* **先存在、后形态**：与 listlen 同一判据，两个入口不许有第二套顺序。 */
    if !key_exists(hd, &key) {
        write_err(err, err_cap, &format!("kvspaceList: no such key: {key}"));
        return KVSPACE_MISSING;
    }
    if !is_index_key(&key) {
        write_err(err, err_cap, &format!("kvspaceList: not an index: {key}"));
        return KVSPACE_NOT_INDEX;
    }
    let listing = match hd.list_direct(&key) {
        Ok(listing) => listing,
        Err(e) => return list_error(err, err_cap, &e),
    };
    /* 三个回报先写全：容量不足时调用方要靠它们重试。 */
    if !out_length.is_null() {
        unsafe { *out_length = listing.total_length };
    }
    if !out_count.is_null() {
        unsafe { *out_count = listing.names.len() as u32 };
    }
    if !out_generation.is_null() {
        unsafe { *out_generation = listing.generation };
    }
    /* NULL 缓冲 = 只探测。 */
    if out_names.is_null() {
        return KVSPACE_OK;
    }
    if out_capacity < listing.total_length {
        return KVSPACE_CAPACITY;
    }
    /* 按 `name\0name\0...` 紧凑写入，末尾不留多余 NUL。 */
    let mut at = 0usize;
    for name in &listing.names {
        unsafe {
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                out_names.add(at) as *mut u8,
                name.len(),
            );
            *out_names.add(at + name.len()) = 0;
        }
        at += name.len() + 1;
    }
    KVSPACE_OK
}
