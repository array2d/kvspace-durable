// deltree.rs - kvspaceDelTree deletion semantics across fs and redis (C ABI).
//
// Deletion scope matches kvspace-c shm and store::is_subtree_key: `base` itself plus every
// descendant separated by `/`, `·`, or `‥` (including hidden `‥` runtime keys). A trailing
// `/` or `·` on `prefix` normalizes to `base`. If the final path component is a link, only
// the link is deleted (POSIX `rm` semantics); links on parent components are resolved.
//
// Redis tests skip when 127.0.0.1:6379 is unreachable and isolate under a unique prefix.
use kvspace_durable::ffi::*;
use std::ffi::CString;

/// 16 字节 head + 8 字节 int64 body + 空 attr（attrslen=0，无前缀）。
fn xvalue_i64(n: i64) -> Vec<u8> {
    let mut v = vec![0u8; 16 + 8];
    v[0..8].copy_from_slice(&8u64.to_le_bytes()); // bodylen @0-7
    // attrslen @8-11 = 0（已零填）
    v[16..24].copy_from_slice(&n.to_le_bytes());
    v
}

unsafe fn set_raw(h: *mut Handle, key: &str, v: &[u8]) {
    let k = CString::new(key).unwrap();
    let mut err = [0 as std::ffi::c_char; 256];
    let rc = kvspaceSetValue(
        h,
        k.as_ptr(),
        v.as_ptr(),
        v.len() as u32,
        err.as_mut_ptr(),
        256,
    );
    let msg = std::ffi::CStr::from_ptr(err.as_ptr())
        .to_string_lossy()
        .into_owned();
    assert_eq!(rc, 0, "set {key}: {msg}");
}

unsafe fn set(h: *mut Handle, key: &str, n: i64) {
    set_raw(h, key, &xvalue_i64(n));
}

/// `‥status` 槽：字符族 langtype（规范 04-CABI:33 的 wire 级契约）。
unsafe fn set_status(h: *mut Handle, key: &str, text: &str) {
    let v = kvspace_durable::xvalue::xvalue_to_wire("[]char/utf8", text.as_bytes(), 0);
    set_raw(h, key, &v);
}

unsafe fn exists(h: *mut Handle, key: &str) -> bool {
    let k = CString::new(key).unwrap();
    let mut p: *mut u8 = std::ptr::null_mut();
    let mut l = 0u32;
    kvspaceGet(h, k.as_ptr(), 0, &mut p, &mut l);
    !p.is_null() && l > 0
}

unsafe fn deltree(h: *mut Handle, prefix: &str) {
    let p = CString::new(prefix).unwrap();
    let mut err = [0 as std::ffi::c_char; 256];
    let rc = kvspaceDelTree(h, p.as_ptr(), err.as_mut_ptr(), 256);
    let msg = std::ffi::CStr::from_ptr(err.as_ptr())
        .to_string_lossy()
        .into_owned();
    assert_eq!(rc, 0, "DelTree({prefix}): {msg}");
}

unsafe fn list_len(h: *mut Handle, dir: &str) -> i32 {
    let d = CString::new(dir).unwrap();
    let mut n = -1;
    kvspaceListLen(h, d.as_ptr(), 0, 0, &mut n);
    n
}

/// `root` is a top-level prefix exclusive to this test run (e.g. `"/dt123"`).
unsafe fn run(dsn: &str, root: &str) {
    let c = CString::new(dsn).unwrap();
    let h = kvspaceConnect(c.as_ptr());
    let h2 = kvspaceConnect(c.as_ptr());
    assert!(!h.is_null() && !h2.is_null(), "connect {dsn}");
    let d = format!("{root}/d");
    let inside: Vec<String> = [
        "",
        "/a",
        "/\u{2025}status",
        "/sub/x",
        "/sub/\u{2025}pc",
        "\u{b7}k",
        "\u{b7}k/y",
        "\u{b7}k\u{2025}x",
        "\u{2025}x",
        "\u{2025}x/y",
    ]
    .iter()
    .map(|s| format!("{d}{s}"))
    .collect();
    let outside: Vec<String> = ["x", "2/a", "2\u{2025}pc"]
        .iter()
        .map(|s| format!("{d}{s}"))
        .chain([format!("{root}/e/\u{2025}status")])
        .collect();
    for form in [d.clone(), format!("{d}/"), format!("{d}\u{b7}")] {
        let mk = CString::new(format!("{d}/")).unwrap();
        assert_eq!(
            kvspaceMkindex(h, mk.as_ptr(), 0, std::ptr::null_mut(), 0),
            0
        );
        for (i, k) in inside.iter().chain(outside.iter()).enumerate() {
            /* `‥status` 是 wire 级契约槽（规范 04-CABI:33）：必须是字符族、内容 ≤16。
             * 用 int64 播种会被后端正确拒绝——那是判据在起作用，不是夹具该绕开的东西。 */
            if k.ends_with("\u{2025}status") {
                set_status(h, k, "ok");
            } else {
                set(h, k, i as i64);
            }
        }
        for k in inside.iter().chain(outside.iter()) {
            assert!(exists(h, k), "{dsn}: seeded {k}");
        }
        // Populate list snapshots on both handles; DelTree must invalidate/refresh them.
        assert_eq!(list_len(h, &format!("{d}/")), 2, "{dsn}: /d/ lists a, sub/");
        assert_eq!(
            list_len(h2, &format!("{d}/sub/")),
            1,
            "{dsn}: /d/sub/ lists x"
        );
        deltree(h, &form);
        for k in &inside {
            assert!(!exists(h, k), "{dsn}: DelTree({form}) left {k}");
            assert!(
                !exists(h2, k),
                "{dsn}: DelTree({form}) left {k} (other handle)"
            );
        }
        assert!(
            !exists(h, &format!("{d}/")),
            "{dsn}: DelTree({form}) left {d}/"
        );
        for k in &outside {
            assert!(exists(h, k), "{dsn}: DelTree({form}) removed sibling {k}");
        }
        assert_eq!(
            list_len(h, &format!("{d}/")),
            0,
            "{dsn}: list after DelTree({form})"
        );
        assert_eq!(
            list_len(h2, &format!("{d}/sub/")),
            0,
            "{dsn}: other handle after DelTree"
        );
    }

    // Link final component: delete only the link itself (with or without trailing '/'), leaving target intact.
    let t = format!("{root}/t");
    set(h, &format!("{t}/a"), 1);
    set(h, &format!("{t}/\u{2025}pc"), 2);
    set(h, &format!("{t}\u{2025}x"), 3);
    let tc = CString::new(t.clone()).unwrap();
    let empty = CString::new("").unwrap();
    let mut lv: *mut u8 = std::ptr::null_mut();
    let mut ln = 0u32;
    assert_eq!(
        kvspaceNewPtr(empty.as_ptr(), tc.as_ptr(), &mut lv, &mut ln),
        0
    );
    let link = std::slice::from_raw_parts(lv, ln as usize).to_vec();
    // Reclaim the Box<[u8]> leaked by kvspaceNewPtr within the same Rust allocator.
    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
        lv,
        ln as usize,
    )));
    for (key, form) in [("p", "p"), ("q", "q/")] {
        let lk = format!("{root}/{key}");
        set_raw(h, &lk, &link);
        deltree(h, &format!("{root}/{form}"));
        assert!(!exists(h, &lk), "{dsn}: DelTree({form}) left the link");
        for k in [
            format!("{t}/a"),
            format!("{t}/\u{2025}pc"),
            format!("{t}\u{2025}x"),
        ] {
            assert!(
                exists(h, &k),
                "{dsn}: DelTree(link {form}) removed target key {k}"
            );
        }
    }

    // Resolve links on parent components without following the final link: /p2 -> /t2, /t2/q -> /t;
    // DelTree("/p2/q") removes only /t2/q.
    let t2 = CString::new(format!("{root}/t2")).unwrap();
    let mut l2: *mut u8 = std::ptr::null_mut();
    let mut n2 = 0u32;
    assert_eq!(
        kvspaceNewPtr(empty.as_ptr(), t2.as_ptr(), &mut l2, &mut n2),
        0
    );
    let link2 = std::slice::from_raw_parts(l2, n2 as usize).to_vec();
    drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
        l2,
        n2 as usize,
    )));
    set(h, &format!("{root}/t2/keep"), 5);
    set_raw(h, &format!("{root}/t2/q"), &link);
    set_raw(h, &format!("{root}/p2"), &link2);
    deltree(h, &format!("{root}/p2/q"));
    assert!(
        !exists(h, &format!("{root}/t2/q")),
        "{dsn}: DelTree via parent link left the link"
    );
    assert!(
        exists(h, &format!("{root}/t2/keep")),
        "{dsn}: DelTree via parent link removed a sibling"
    );
    assert!(
        exists(h, &format!("{root}/p2")),
        "{dsn}: DelTree via parent link removed the parent link"
    );
    for k in [
        format!("{t}/a"),
        format!("{t}/\u{2025}pc"),
        format!("{t}\u{2025}x"),
    ] {
        assert!(
            exists(h, &k),
            "{dsn}: DelTree via parent link removed target key {k}"
        );
    }

    deltree(h, root);
    for k in outside
        .iter()
        .chain([format!("{t}/a"), format!("{t}\u{2025}x")].iter())
    {
        assert!(!exists(h, k), "{dsn}: cleanup left {k}");
    }
    kvspaceClose(h2);
    kvspaceClose(h);
}

#[test]
fn deltree_fs() {
    let dir = std::env::temp_dir().join(format!("kvspace-deltree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dsn = format!("fs://{}", dir.display());
    unsafe {
        run(&dsn, "/r");
        // Root "/": delete all keys (including `‥` keys) while keeping the root directory usable.
        let c = CString::new(dsn.as_str()).unwrap();
        let h = kvspaceConnect(c.as_ptr());
        set(h, "/a/b", 1);
        set(h, "/a\u{2025}x", 2);
        set(h, "/\u{2025}top", 3);
        deltree(h, "/");
        for k in ["/a/b", "/a\u{2025}x", "/\u{2025}top"] {
            assert!(!exists(h, k), "DelTree(/) left {k}");
        }
        set(h, "/after", 4);
        assert!(exists(h, "/after"), "store usable after DelTree(/)");
        kvspaceClose(h);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deltree_redis() {
    let dsn = std::env::var("KVSPACE_TEST_REDIS_DSN")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let addr = dsn
        .trim_start_matches("redis://")
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    let reachable = std::net::ToSocketAddrs::to_socket_addrs(&addr.as_str())
        .ok()
        .and_then(|mut a| a.next())
        .is_some_and(|a| {
            std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_millis(300)).is_ok()
        });
    if !reachable {
        eprintln!("skipping redis DelTree test: {addr} unreachable");
        return;
    }
    unsafe {
        run(
            &dsn,
            &format!("/kvspace-deltree-test-{}", std::process::id()),
        )
    };
}
