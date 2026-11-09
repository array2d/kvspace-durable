// read_cache.rs — durable 每 key 常驻读缓存的指针寿命与失效范围。
//
// 直接走 C ABI（kvspaceConnect/Get/GetPart/SetValue/Del/DelTree/CpTree/Close），
// 在 fs 后端上验证：
//   1. 写别的 key 不影响已借出的指针；
//   2. 写/删同一个 key 使该 key 的指针失效（重新读能拿到新值）；
//   3. DelTree 别的 prefix 不影响 A；CpTree 到别的 dst 不影响 A；
//   4. DelTree 覆盖 A 时 A 失效；
//   5. GetPart 命中同一 key 的缓存。

use kvspace_durable::ffi::*;

/// attr 区：langtype 记录（#571 单 key 整体），**无长度前缀**（长度在 head 的
/// attrslen 里）；空 langtype = 空 attr（零字节）。
fn attr_of(lt: &str) -> Vec<u8> {
    if lt.is_empty() {
        return Vec::new();
    }
    let mut v = Vec::new();
    v.push(1); // tag = langtype
    v.extend_from_slice(&(lt.len() as u16).to_le_bytes());
    v.extend_from_slice(lt.as_bytes());
    v
}

/// 拼 16 字节 head + body + attr；head 的 bodylen/attrslen 由调用方给的实际长度决定。
/// 新 16 字节 head：
/// `[bodylen:u64][attrslen:u16][reserved:u16][layout:u8][padding:3]`。
/// `storetype`/`ptr` 已从 wire 删除——形态由 langtype 表达，故前两个参数不再使用。
fn wire(_storetype: u8, _ptr: u8, body: &[u8], lt: &str) -> Vec<u8> {
    let attr = attr_of(lt);
    let mut v = vec![0u8; 16];
    v[0..8].copy_from_slice(&(body.len() as u64).to_le_bytes());
    v[8..10].copy_from_slice(&u16::try_from(attr.len()).unwrap().to_le_bytes());
    v[12] = 0; /* layout = LENGTH */
    v.extend_from_slice(body);
    v.extend_from_slice(&attr);
    v
}

/// 64KB 级 char/utf8 值：storetype 1 变长、body 全 0（仅用于内存观测）+ 空 attr。
/// wire = [bodylen:u64 @0-7][attrslen:u16 @8-9][reserved:u16 @10-11][layout@12]
///        [padding@13-15][body][attr]，body 偏移恒 16。
fn xvalue_pad(n: usize) -> Vec<u8> {
    wire(1, 0, &vec![0u8; n], "")
}

fn xvalue_i64(n: i64) -> Vec<u8> {
    wire(0, 0, &n.to_le_bytes(), "int64")
}

unsafe fn set(h: *mut Handle, key: &str, n: i64) {
    let mut k = key.as_bytes().to_vec();
    k.push(0);
    /* 值含 attr 区（类型随值整体写，无 attr key）。 */
    let v = xvalue_i64(n);
    let mut err = [0i8; 128];
    let rc = kvspaceSetValue(
        h,
        k.as_ptr() as *const _,
        v.as_ptr(),
        v.len() as u32,
        err.as_mut_ptr(),
        err.len() as u32,
    );
    assert_eq!(rc, 0, "set {key}");
}

unsafe fn get_ptr(h: *mut Handle, key: &str) -> (*mut u8, u32) {
    let mut k = key.as_bytes().to_vec();
    k.push(0);
    let mut p: *mut u8 = std::ptr::null_mut();
    let mut l: u32 = 0;
    assert_eq!(kvspaceGet(h, k.as_ptr() as *const _, 0, &mut p, &mut l), 0);
    (p, l)
}

unsafe fn read_i64(p: *const u8, l: u32) -> i64 {
    /* head 16 字节，body 紧随其后。 */
    assert!(l >= 16 + 8, "len={l}");
    let mut b = [0u8; 8];
    b.copy_from_slice(std::slice::from_raw_parts(p.add(16), 8));
    i64::from_le_bytes(b)
}

unsafe fn get_val(h: *mut Handle, key: &str) -> Option<i64> {
    let (p, l) = get_ptr(h, key);
    if p.is_null() || l == 0 {
        None
    } else {
        Some(read_i64(p, l))
    }
}

unsafe fn ckey(key: &str) -> Vec<u8> {
    let mut k = key.as_bytes().to_vec();
    k.push(0);
    k
}

fn dsn() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    // 进程内计数器：并行测试的 pid 相同，时间戳在 macOS 上可能同值。
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("kvspace-rc-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = String::from("fs://");
    s.push_str(dir.to_str().unwrap());
    s
}

fn main() {}

#[test]
fn read_cache_lifetime_and_invalidation() {
    let dsn = dsn();
    let mut c = dsn.as_bytes().to_vec();
    c.push(0);
    let h = kvspaceConnect(c.as_ptr() as *const _);
    assert!(!h.is_null());
    unsafe {
        set(h, "/a", 1);
        set(h, "/b", 2);

        // 1. 读 A 后写 B：A 的指针仍有效。
        let (pa, la) = get_ptr(h, "/a");
        assert_eq!(read_i64(pa, la), 1);
        set(h, "/b", 3);
        assert_eq!(read_i64(pa, la), 1, "write other key must not invalidate A");

        // 2. 同 key 写：重新读拿新值。
        set(h, "/a", 5);
        assert_eq!(get_val(h, "/a"), Some(5));

        // 3. DelTree 别的 prefix 不影响 A；CpTree 到别的 dst 不影响 A。
        set(h, "/t/x", 7);
        set(h, "/t/y", 8);
        let (pa, la) = get_ptr(h, "/a");
        assert_eq!(
            kvspaceDelTree(h, ckey("/t/").as_ptr() as *const _, std::ptr::null_mut(), 0),
            0
        );
        assert_eq!(
            read_i64(pa, la),
            5,
            "DelTree other prefix must not invalidate A"
        );
        set(h, "/src/x", 9);
        let dst = ckey("/dst/");
        let src = ckey("/src/");
        assert_eq!(
            kvspaceCpTree(
                h,
                src.as_ptr() as *const _,
                dst.as_ptr() as *const _,
                std::ptr::null_mut(),
                0
            ),
            0
        );
        assert_eq!(
            read_i64(pa, la),
            5,
            "CpTree to other dst must not invalidate A"
        );

        // 4. DelTree 覆盖 A 时 A 失效。
        get_ptr(h, "/t/x");
        assert_eq!(
            kvspaceDelTree(h, ckey("/t/").as_ptr() as *const _, std::ptr::null_mut(), 0),
            0
        );
        assert_eq!(get_val(h, "/t/x"), None);

        // 5. GetPart 命中同一 key 缓存：返回 [off, off+len) 的字节。
        let (pa, la) = get_ptr(h, "/a");
        let mut pp: *mut u8 = std::ptr::null_mut();
        let mut pl: u32 = 0;
        assert_eq!(
            /* body 偏移 16（head 16 字节）；读 8 字节 int64 body。 */
            kvspaceGetPart(h, ckey("/a").as_ptr() as *const _, 16, 8, &mut pp, &mut pl),
            0
        );
        let mut b = [0u8; 8];
        b.copy_from_slice(std::slice::from_raw_parts(pp, pl as usize));
        assert_eq!(i64::from_le_bytes(b), 5);
        assert_eq!(read_i64(pa, la), 5);

        kvspaceClose(h);
    }
    std::fs::remove_dir_all(dsn.trim_start_matches("fs://")).ok();
}

/// 无 eviction 的内存增长观测（手动运行）：
/// `cargo test --release --test read_cache -- --ignored --nocapture memory_growth`
#[test]
#[ignore]
fn memory_growth() {
    fn rss_kb() -> u64 {
        let s = std::fs::read_to_string("/proc/self/statm").unwrap();
        let pages: u64 = s.split_whitespace().nth(1).unwrap().parse().unwrap();
        pages * 4
    }
    let dsn = dsn();
    let mut c = dsn.as_bytes().to_vec();
    c.push(0);
    let h = kvspaceConnect(c.as_ptr() as *const _);
    assert!(!h.is_null());
    const N: usize = 2000;
    unsafe {
        for i in 0..N {
            let key = format!("/k{i}");
            let v = xvalue_pad(64 * 1024);
            let mut k = key.as_bytes().to_vec();
            k.push(0);
            assert_eq!(
                kvspaceSetValue(
                    h,
                    k.as_ptr() as *const _,
                    v.as_ptr(),
                    v.len() as u32,
                    std::ptr::null_mut(),
                    0
                ),
                0
            );
        }
        let base = rss_kb();
        for i in 0..N {
            let key = format!("/k{i}");
            let (p, l) = get_ptr(h, &key);
            assert!(!p.is_null() && l == 8 + 64 * 1024);
            if i + 1 == 500 || i + 1 == 1000 || i + 1 == N {
                eprintln!(
                    "rss after {} reads: {} KB (+{})",
                    i + 1,
                    rss_kb(),
                    rss_kb() - base
                );
            }
        }
        kvspaceClose(h);
    }
    std::fs::remove_dir_all(dsn.trim_start_matches("fs://")).ok();
}

/// 同指令两次读同一 key（add(a,a) 形态）：两份视图必须共存且都正确。
/// 先读再 watch 同 key：旧视图保活，不能悬空。
#[test]
fn same_key_two_reads_and_watch_retire() {
    let dsn = dsn();
    let mut c = dsn.as_bytes().to_vec();
    c.push(0);
    let h = kvspaceConnect(c.as_ptr() as *const _);
    assert!(!h.is_null());
    unsafe {
        set(h, "/a", 7);
        let (p1, l1) = get_ptr(h, "/a");
        let (p2, l2) = get_ptr(h, "/a");
        assert_eq!(p1, p2, "second read must return the same cached view");
        assert_eq!(read_i64(p1, l1), 7);
        assert_eq!(read_i64(p2, l2), 7);

        // watch 同 key：目标值就是当前值，立即返回；旧视图必须仍然有效。
        let tv = xvalue_i64(7);
        let mut wp: *mut u8 = std::ptr::null_mut();
        let mut wl: u32 = 0;
        let rc = kvspaceWatch(
            h,
            ckey("/a").as_ptr() as *const _,
            tv.as_ptr(),
            tv.len() as u32,
            1_000_000,
            &mut wp,
            &mut wl,
        );
        assert_eq!(rc, 0);
        assert!(!wp.is_null() && wl >= 16);
        assert_eq!(read_i64(p1, l1), 7, "old view must stay valid after watch");
        assert_eq!(read_i64(wp, wl), 7);

        kvspaceClose(h);
    }
    std::fs::remove_dir_all(dsn.trim_start_matches("fs://")).ok();
}

fn xvalue_utf8(s: &str) -> Vec<u8> {
    wire(1, 0, s.as_bytes(), "")
}

unsafe fn read_str(p: *const u8, l: u32) -> String {
    assert!(l >= 16);
    /* bodylen 是 head 的 u64 @0-7；body 从 offset 16 开始（attr 区不读入）。 */
    let mut b = [0u8; 8];
    b.copy_from_slice(std::slice::from_raw_parts(p, 8));
    let n = u64::from_le_bytes(b) as usize;
    String::from_utf8_lossy(std::slice::from_raw_parts(p.add(16), n)).into_owned()
}

unsafe fn set_str(h: *mut Handle, key: &str, s: &str) {
    let v = xvalue_utf8(s);
    let mut k = key.as_bytes().to_vec();
    k.push(0);
    assert_eq!(
        kvspaceSetValue(
            h,
            k.as_ptr() as *const _,
            v.as_ptr(),
            v.len() as u32,
            std::ptr::null_mut(),
            0
        ),
        0
    );
}

unsafe fn get_str(h: *mut Handle, key: &str) -> Option<(String, *mut u8, u32)> {
    let (p, l) = get_ptr(h, key);
    if p.is_null() || l == 0 {
        None
    } else {
        Some((read_str(p, l), p, l))
    }
}

/// 控制键 `/vthread/*‥status` 是 volatile：外部 handle 的写必须立即被观测；
/// 双缓冲保证上一视图在下一次读之后仍有效。普通 key 仍是 per-handle 快照。
#[test]
fn volatile_status_observed_and_double_buffered() {
    let dsn = dsn();
    let mut c = dsn.as_bytes().to_vec();
    c.push(0);
    let a = kvspaceConnect(c.as_ptr() as *const _);
    let b = kvspaceConnect(c.as_ptr() as *const _);
    assert!(!a.is_null() && !b.is_null());
    let st = "/vthread/1\u{2025}status";
    unsafe {
        set_str(a, st, "running");
        let (v1, p1, l1) = get_str(a, st).unwrap();
        assert_eq!(v1, "running");

        // 外部 handle B 改状态：A 下一次读必须看到 paused；旧视图仍为 running。
        set_str(b, st, "paused");
        let (v2, _p2, _l2) = get_str(a, st).unwrap();
        assert_eq!(v2, "paused", "external status write must be observed");
        assert_eq!(
            read_str(p1, l1),
            "running",
            "previous view must survive one more read"
        );

        // Switch back to running: handle A sees running, and repeated unchanged reads keep the same pointer.
        set_str(b, st, "running");
        let (v3, p3, l3) = get_str(a, st).unwrap();
        assert_eq!(v3, "running");
        for _ in 0..6 {
            let (vn, pn, ln) = get_str(a, st).unwrap();
            assert_eq!(vn, "running");
            assert_eq!(pn, p3, "unchanged volatile read must keep pointer stable");
            assert_eq!(ln, l3);
        }
        assert_eq!(read_str(p3, l3), "running");

        // 外部删除该 key：volatile 每读重读，必须看到 None。
        let del_keys = [ckey(st)];
        let del_ptrs = [del_keys[0].as_ptr() as *const _];
        assert_eq!(
            kvspaceDel(b, del_ptrs.as_ptr(), 1, std::ptr::null_mut(), 0),
            0
        );
        assert!(get_str(a, st).is_none(), "external delete must be observed");

        // 普通 key 仍是 per-handle 快照：B 写 /x 不被 A 观测。
        set_str(a, "/x", "one");
        assert_eq!(get_str(a, "/x").unwrap().0, "one");
        set_str(b, "/x", "two");
        assert_eq!(
            get_str(a, "/x").unwrap().0,
            "one",
            "snapshot keys ignore external writes"
        );
    }
    kvspaceClose(a);
    kvspaceClose(b);
    std::fs::remove_dir_all(dsn.trim_start_matches("fs://")).ok();
}
