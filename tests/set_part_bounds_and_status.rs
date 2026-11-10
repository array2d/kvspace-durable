//! `SetPart` 上界与 `‥status` wire 级校验。
//!
//! 两组判据各钉一个「静默错误」：
//!
//! 1. **SetPart 越界**：原地写的语义是「改已存在的内容」，不是「把值撑大」。
//!    曾用 `resize(e, 0)` 兜底、`SETRANGE`/`pwrite` 也都会自己扩——于是越界写
//!    静默扩展 wire，读方拿到的长度比写方以为的长，而错在写入侧、不在读侧。
//! 2. **status 槽形状**：规范 `02-kvspace模型/04-CABI.kv:33-36` 要求
//!    base kind 是 char 族 + 有效长度 ≤16，**所有后端都必须校验**。
//!    不校验则 `‥status` 能被写成任意形状，运行时按文本读会拿到垃圾。

use std::ffi::CString;
use kvspace_durable::{KVPair, KVSpace, XValue};

fn connect(label: &str) -> *mut kvspace_durable::ffi::Handle {
    let dir = std::env::temp_dir().join(format!(
        "kvspace-durable-setpart-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let dsn = CString::new(format!("fs://{}/store", dir.display())).unwrap();
    let h = unsafe { kvspace_durable::ffi::kvspaceConnect(dsn.as_ptr()) };
    assert!(!h.is_null(), "connect must succeed");
    h
}

/// 写一个 LENGTH 形态的值：body = `data`，langtype = `lt`。
/// 把上一笔惰性写推下去，并**观察它是否成功**。
///
/// `kvspaceWriteNewPlace` 只是把值攒进 `write_buf` + `pending_key`；真正的
/// `kv.set` 发生在**下一次 flush**（下一次写、或 close）。所以想看见写失败，
/// 必须再写一笔——那一笔的入口会先 flush，flush 失败就直接返回非 0。
///
/// 返回 `true` 表示上一笔成功落盘。
fn commit_previous(h: *mut kvspace_durable::ffi::Handle) -> bool {
    let k = CString::new("/__commit_probe").unwrap();
    let l = CString::new("int64").unwrap();
    let mut body: *mut u8 = std::ptr::null_mut();
    let mut err = [0i8; 256];
    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            h, k.as_ptr(), 0, 1, 8, 8, l.as_ptr(),
            &mut body, err.as_mut_ptr(), err.len() as u32,
        )
    };
    if rc != 0 {
        return false;
    }
    unsafe { std::ptr::copy_nonoverlapping(42i64.to_le_bytes().as_ptr(), body, 8) };
    true
}

fn write_value(h: *mut kvspace_durable::ffi::Handle, key: &str, data: &[u8], lt: &str) -> bool {
    let k = CString::new(key).unwrap();
    let l = CString::new(lt).unwrap();
    let mut body: *mut u8 = std::ptr::null_mut();
    let mut err = [0i8; 256];
    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            h,
            k.as_ptr(),
            0,
            1,
            data.len() as u32,
            data.len() as u64,
            l.as_ptr(),
            &mut body,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    if rc != 0 {
        return false;
    }
    if !data.is_empty() {
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), body, data.len()) };
    }
    true
}

fn read_value(h: *mut kvspace_durable::ffi::Handle, key: &str) -> Vec<u8> {
    let k = CString::new(key).unwrap();
    let mut out: *mut u8 = std::ptr::null_mut();
    let mut out_len: u32 = 0;
    let rc = unsafe {
        kvspace_durable::ffi::kvspaceGet(h, k.as_ptr(), 1, &mut out, &mut out_len)
    };
    if rc != 0 || out_len == 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(out, out_len as usize).to_vec() }
}

fn set_part(
    h: *mut kvspace_durable::ffi::Handle,
    key: &str,
    offset: u32,
    buf: &[u8],
) -> i32 {
    let k = CString::new(key).unwrap();
    let mut err = [0i8; 256];
    unsafe {
        kvspace_durable::ffi::kvspaceSetPart(
            h,
            k.as_ptr(),
            offset,
            buf.as_ptr(),
            buf.len() as u32,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    }
}

// ── SetPart 四组 ─────────────────────────────────────────────────────

#[test]
fn set_part_inside_bounds_succeeds() {
    let h = connect("inside");
    assert!(write_value(h, "/v", b"abcdef", "[]char/utf8"));
    /* body 6 字节，落在 [16, 22)。写 [17,20) 是界内。 */
    assert_eq!(set_part(h, "/v", 17, b"XYZ"), 0);
    let wire = read_value(h, "/v");
    assert_eq!(&wire[17..20], b"XYZ");
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

#[test]
fn set_part_exactly_at_boundary_succeeds() {
    let h = connect("boundary");
    assert!(write_value(h, "/v", b"abcdef", "[]char/utf8"));
    /* 上界 = 16 + 6 = 22；写 [22-len, 22) 正好贴边。 */
    assert_eq!(set_part(h, "/v", 20, b"YZ"), 0);
    let wire = read_value(h, "/v");
    assert_eq!(wire.len(), 22 + 14, "整条 wire = 16 head + 6 body + 14 attr；边界内写不得改变它");
    assert_eq!(&wire[20..22], b"YZ");
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

#[test]
fn set_part_one_byte_past_boundary_fails_and_leaves_value_intact() {
    let h = connect("past");
    assert!(write_value(h, "/v", b"abcdef", "[]char/utf8"));
    let before = read_value(h, "/v");
    /* 上界 22；写 [21, 24) 越 2 字节。 */
    assert_ne!(set_part(h, "/v", 21, b"XYZ"), 0, "越界必须失败");
    let after = read_value(h, "/v");
    assert_eq!(before, after, "失败不得改动原值（不许扩容、不许部分写）");
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

#[test]
fn set_part_bounds_ignore_body_capacity_entirely() {
    let h = connect("capacity");
    /* 请求 32 字节容量、6 字节有效长度：容量不进 wire，上界仍是 16+6=22。 */
    let k = CString::new("/cap").unwrap();
    let l = CString::new("[]char/utf8").unwrap();
    let mut body: *mut u8 = std::ptr::null_mut();
    let mut err = [0i8; 256];
    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            h,
            k.as_ptr(),
            0,
            1,
            6,
            32,
            l.as_ptr(),
            &mut body,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    assert_eq!(rc, 0);
    unsafe { std::ptr::copy_nonoverlapping(b"abcdef".as_ptr(), body, 6) };

    /* 22 处界内：成功。 */
    assert_eq!(set_part(h, "/cap", 20, b"YZ"), 0, "有效长度内的写必须成功");
    /* 24 处已超出有效长度 6（上界 22），必须失败。 */
    assert_ne!(set_part(h, "/cap", 22, b"Z"), 0, "有效长度之外必须失败");
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

// ── status wire 级校验 ──────────────────────────────────────────────

#[test]
fn status_rejects_content_longer_than_16() {
    let h = connect("status-long");
    /* 17 字节 > 16：必须被拒。 */
    /* 第一笔只是攒进缓冲（入口不落盘），第二笔的入口会 flush 它 ⇒ 失败可见。 */
    assert!(write_value(h, "/vthread/1/\u{2025}status", b"0123456789abcdefg", "[]char/utf8"));
    assert!(!commit_previous(h), "status 内容 >16 必须被拒（flush 时红）");
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

#[test]
fn status_rejects_non_character_langtype() {
    let h = connect("status-int");
    assert!(write_value(h, "/vthread/1/\u{2025}status", &42i64.to_le_bytes(), "int64"));
    assert!(!commit_previous(h), "status 必须是字符族，int64 必须被拒");
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

#[test]
fn status_accepts_compliant_wire() {
    let h = connect("status-ok");
    /* 正向对照：合规的 status 必须写得进去。
     * 缺这条的话，一个「全拒绝」的实现也能让上面两条全绿。 */
    assert!(write_value(h, "/vthread/1/\u{2025}status", b"running", "[]char/utf8"));
    assert!(commit_previous(h), "合规 status 必须被接受");
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

#[test]
fn status_shape_rule_only_applies_to_status_key() {
    let h = connect("status-scope");
    /* 反向对照：同一条 17 字节 + int64 的规则**不该**溢出到普通 key。 */
    assert!(
        write_value(h, "/ordinary", b"0123456789abcdefg", "[]char/utf8"),
        "普通 key 不受 status 规则约束"
    );
    assert!(
        write_value(h, "/ordinary-int", &42i64.to_le_bytes(), "int64"),
        "普通 key 不受 status 字符族约束"
    );
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

// ── 判界收进唯一入口后，mem 与 fs 各自越界 ─────────────────────────
//
// 判界在 `backend.rs::set_part` 唯一入口，三个后端共用同一份判据。
// 这两条各走一条不同的后端（mem 走 `store.rs` 默认实现、fs 走 `fs/kvspace.rs`），
// 证明判据不依赖具体后端。

mod mem_backend {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use kvspace_durable::{Backend, KVPair, KVSpace, KVStore, XValue};

    #[derive(Default, Clone)]
    pub struct MemoryStore(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl KVStore for MemoryStore {
        fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(key).cloned()
        }
        fn set(&self, key: &str, value: &[u8]) {
            self.0.borrow_mut().insert(key.to_string(), value.to_vec());
        }
        fn del(&self, keys: &[&str]) {
            let mut v = self.0.borrow_mut();
            for k in keys {
                v.remove(*k);
            }
        }
        fn scan_keys(&self, prefix: &str) -> Vec<String> {
            self.0
                .borrow()
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect()
        }
        fn flush(&self) {}
    }

    pub fn backend() -> Backend<MemoryStore> {
        Backend::new(MemoryStore::default())
    }

    pub fn seed(kv: &mut Backend<MemoryStore>, key: &str, body: &[u8]) {
        let raw = kvspace_durable::xvalue::xvalue_to_wire("[]char/utf8", body, 0);
        kv.set(&[KVPair {
            key: key.into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    }
}

#[test]
fn set_part_out_of_range_fails_on_memory_backend() {
    let mut kv = mem_backend::backend();
    mem_backend::seed(&mut kv, "/m", b"abcdef");
    /* 上界 = 16 + 6 = 22；写 [21,24) 越界。 */
    assert!(
        KVSpace::set_part(&mut kv, "/m", 21, b"XYZ").is_err(),
        "mem 后端越界必须失败"
    );
    /* 界内仍可写，且没被上一条波及。 */
    assert!(
        KVSpace::set_part(&mut kv, "/m", 17, b"XY").is_ok(),
        "mem 后端界内必须成功"
    );
}

#[test]
fn set_part_out_of_range_fails_on_fs_backend() {
    let h = connect("fs-out-of-range");
    assert!(write_value(h, "/f", b"abcdef", "[]char/utf8"));
    assert!(commit_previous(h), "先落盘，确保 /f 真的在 store 里");
    assert_ne!(
        set_part(h, "/f", 21, b"XYZ"),
        0,
        "fs 后端越界必须失败"
    );
    unsafe { kvspace_durable::ffi::kvspaceClose(h) };
}

// ── 跨后端等价对照 ──────────────────────────────────────────────────
//
// 「两处判界用同一个量」需要一条**可执行的**判据，而不是注释里的承诺。
// 这组用同一批 (offset, buf) 在 mem 与 fs 上各跑一遍，要求结论逐条相同。
// 判界若在某一边算了容量（而非 `bodylen`），带 body_cap 那一条会立刻分叉。

/// 三种界相对上界（16 + 6 = 22）的位置：界内、刚好贴边、越 1 字节。
const PROBES: [(u32, &[u8]); 3] = [(17, b"XY"), (20, b"YZ"), (21, b"XYZ")];

fn mem_verdicts() -> Vec<bool> {
    let mut out = Vec::new();
    for (off, buf) in PROBES {
        let mut kv = mem_backend::backend();
        mem_backend::seed(&mut kv, "/x", b"abcdef");
        out.push(KVSpace::set_part(&mut kv, "/x", off, buf).is_ok());
    }
    out
}

fn fs_verdicts(label: &str) -> Vec<bool> {
    let mut out = Vec::new();
    for (off, buf) in PROBES {
        let h = connect(label);
        assert!(write_value(h, "/x", b"abcdef", "[]char/utf8"));
        assert!(commit_previous(h), "先落盘，确保 /x 真的在 store 里");
        out.push(set_part(h, "/x", off, buf) == 0);
        unsafe { kvspace_durable::ffi::kvspaceClose(h) };
    }
    out
}

#[test]
fn set_part_bounds_agree_across_backends() {
    let mem = mem_verdicts();
    let fs = fs_verdicts("cross-backend");
    assert_eq!(
        mem, fs,
        "mem 与 fs 的判界结论必须逐条相同（两处用同一个量）；mem={mem:?} fs={fs:?}"
    );
    /* 同时钉住绝对结论，防止两边一起错：界内真、边界真、越界假。 */
    assert_eq!(mem, vec![true, true, false], "三种位置的绝对结论");
}

#[test]
fn set_part_bounds_with_body_cap_agree_across_backends() {
    /* 请求容量 32、有效 6：容量不进 wire，两侧上界都必须是 16+6=22。
     * 这条钉的是一条**已删除的歧义**：曾经容量会改 bodylen，两侧加起来
     * 就是「同一份值有两个长度量」。 */
    let mem = {
        let mut kv = mem_backend::backend();
        let raw = kvspace_durable::headlenpow::to_wire_zeroed(6, "[]char/utf8").unwrap();
        kv.set(&[KVPair {
            key: "/c".into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
        vec![
            KVSpace::set_part(&mut kv, "/c", 20, b"YZ").is_ok(),
            KVSpace::set_part(&mut kv, "/c", 22, b"Z").is_ok(),
        ]
    };
    let fs = {
        let h = connect("cross-capacity");
        let k = CString::new("/c").unwrap();
        let l = CString::new("[]char/utf8").unwrap();
        let mut body: *mut u8 = std::ptr::null_mut();
        let mut err = [0i8; 256];
        let rc = unsafe {
            kvspace_durable::ffi::kvspaceWriteNewPlace(
                h, k.as_ptr(), 0, 1, 6, 32, l.as_ptr(),
                &mut body, err.as_mut_ptr(), err.len() as u32,
            )
        };
        assert_eq!(rc, 0);
        unsafe { std::ptr::copy_nonoverlapping(b"abcdef".as_ptr(), body, 6) };
        assert!(commit_previous(h));
        let v = vec![set_part(h, "/c", 20, b"YZ") == 0, set_part(h, "/c", 22, b"Z") == 0];
        unsafe { kvspace_durable::ffi::kvspaceClose(h) };
        v
    };
    assert_eq!(mem, fs, "判界必须跨后端相同；mem={mem:?} fs={fs:?}");
    assert_eq!(mem, vec![true, false], "bodylen 只有 6：22 处必须失败");
}
