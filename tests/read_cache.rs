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

/// 64KB 级 char/utf8 值：[n]char/utf8，body 全 0（仅用于内存观测）。
fn tlv_pad(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; 64 + n];
    v[0] = 6; // headlen = 64
    v[1] = 1; // flags: array/storetype 1
    v[2..10].copy_from_slice(&(n as u64).to_le_bytes());
    v[10..18].copy_from_slice(&(n as u64).to_le_bytes());
    let lt = format!("[{n}]char/utf8");
    v[18..18 + lt.len()].copy_from_slice(lt.as_bytes());
    v
}

fn tlv_i64(n: i64) -> Vec<u8> {
    let mut v = vec![0u8; 40];
    v[0] = 5; // headlen = 32
    v[18..23].copy_from_slice(b"int64");
    v[32..40].copy_from_slice(&n.to_le_bytes());
    v
}

unsafe fn set(h: *mut Handle, key: &str, n: i64) {
    let mut k = key.as_bytes().to_vec();
    k.push(0);
    let v = tlv_i64(n);
    let mut err = [0i8; 128];
    let rc = kvspaceSetValue(
        h,
        k.as_ptr() as *const _,
        v.as_ptr(),
        v.len() as u32,
        0,
        0,
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
    assert!(l >= 40, "len={l}");
    let mut b = [0u8; 8];
    b.copy_from_slice(std::slice::from_raw_parts(p.add(32), 8));
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
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
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
            kvspaceGetPart(h, ckey("/a").as_ptr() as *const _, 32, 8, &mut pp, &mut pl),
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
            let v = tlv_pad(64 * 1024);
            let mut k = key.as_bytes().to_vec();
            k.push(0);
            assert_eq!(
                kvspaceSetValue(
                    h,
                    k.as_ptr() as *const _,
                    v.as_ptr(),
                    v.len() as u32,
                    0,
                    0,
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
            assert!(!p.is_null() && l == 64 + 64 * 1024);
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
        let tv = tlv_i64(7);
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
        assert!(!wp.is_null() && wl >= 40);
        assert_eq!(read_i64(p1, l1), 7, "old view must stay valid after watch");
        assert_eq!(read_i64(wp, wl), 7);

        kvspaceClose(h);
    }
    std::fs::remove_dir_all(dsn.trim_start_matches("fs://")).ok();
}

fn tlv_utf8(s: &str) -> Vec<u8> {
    let n = s.len();
    let mut v = vec![0u8; 64 + n];
    v[0] = 6;
    v[1] = 1;
    v[2..10].copy_from_slice(&(n as u64).to_le_bytes());
    v[10..18].copy_from_slice(&(n as u64).to_le_bytes());
    let lt = format!("[{n}]char/utf8");
    v[18..18 + lt.len()].copy_from_slice(lt.as_bytes());
    v[64..].copy_from_slice(s.as_bytes());
    v
}

unsafe fn read_str(p: *const u8, l: u32) -> String {
    assert!(l >= 64);
    String::from_utf8_lossy(std::slice::from_raw_parts(p.add(64), (l - 64) as usize)).into_owned()
}

unsafe fn set_str(h: *mut Handle, key: &str, s: &str) {
    let v = tlv_utf8(s);
    let mut k = key.as_bytes().to_vec();
    k.push(0);
    assert_eq!(
        kvspaceSetValue(
            h,
            k.as_ptr() as *const _,
            v.as_ptr(),
            v.len() as u32,
            0,
            0,
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
    let a = unsafe { kvspaceConnect(c.as_ptr() as *const _) };
    let b = unsafe { kvspaceConnect(c.as_ptr() as *const _) };
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

        // 再改回 running：A 看到 running；双缓冲只保证最近一次读（p4）的视图有效。
        set_str(b, st, "running");
        let (v3, _p3, _l3) = get_str(a, st).unwrap();
        assert_eq!(v3, "running");
        let (v4, p4, l4) = get_str(a, st).unwrap();
        assert_eq!(v4, "running");
        assert_eq!(read_str(p4, l4), "running");

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
    unsafe {
        kvspaceClose(a);
        kvspaceClose(b);
    }
    std::fs::remove_dir_all(dsn.trim_start_matches("fs://")).ok();
}
