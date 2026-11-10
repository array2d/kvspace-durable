// list_cache.rs - Visibility of per-handle ListLen/ListAt snapshots (C ABI, fs backend).
//
// Compares every step against a freshly opened handle without a cached snapshot:
//   1. ListLen + sequential ListAt(0..n) matches a fresh listing;
//   2. Same-handle mutations (SetValue / Del / DelTree / lazy WriteNewPlace) take effect immediately;
//   3. Cross-handle writes become visible on repeated `idx` or the next `ListLen`.
use kvspace_durable::ffi::*;
use kvspace_durable::storage_abi::*;
use std::ffi::CString;

/// 16 字节 head + 8 字节 int64 body + 空 attr（attrslen=0，无前缀）。
fn xvalue_i64(n: i64) -> Vec<u8> {
    let mut v = vec![0u8; 16 + 8];
    v[0..8].copy_from_slice(&8u64.to_le_bytes()); // bodylen @0-7
    // attrslen @8-11 = 0（已零填）
    v[16..24].copy_from_slice(&n.to_le_bytes());
    v
}

unsafe fn set(h: *mut Handle, key: &str, n: i64) {
    let k = CString::new(key).unwrap();
    let v = xvalue_i64(n);
    let rc = kvspaceSetValue(
        h,
        k.as_ptr(),
        v.as_ptr(),
        v.len() as u32,
        std::ptr::null_mut(),
        0,
    );
    assert_eq!(rc, 0, "set {key}");
}

unsafe fn del(h: *mut Handle, key: &str) {
    let k = CString::new(key).unwrap();
    let ks = [k.as_ptr()];
    assert_eq!(
        kvspaceDel(h, ks.as_ptr(), 1, std::ptr::null_mut(), 0),
        0,
        "del {key}"
    );
}

unsafe fn list_len(h: *mut Handle, dir: &str) -> i32 {
    let d = CString::new(dir).unwrap();
    let mut n = -1;
    assert_eq!(kvspaceListLen(h, d.as_ptr(), 0, 0, &mut n), 0);
    n
}

unsafe fn list_at(h: *mut Handle, dir: &str, idx: i32) -> Option<String> {
    let d = CString::new(dir).unwrap();
    let mut buf = [0u8; 256];
    let mut ol = 0u32;
    if kvspaceListAt(
        h,
        d.as_ptr(),
        0,
        0,
        idx,
        buf.as_mut_ptr(),
        buf.len() as u32,
        &mut ol,
    ) != 0
    {
        return None;
    }
    Some(String::from_utf8(buf[..ol as usize].to_vec()).unwrap())
}

unsafe fn traverse(h: *mut Handle, dir: &str) -> Vec<String> {
    let n = list_len(h, dir);
    (0..n)
        .map(|i| list_at(h, dir, i).expect("ListAt in range"))
        .collect()
}

/// Uncached reference listing via a fresh handle.
unsafe fn fresh(dsn: &CString, dir: &str) -> Vec<String> {
    let h = kvspaceConnect(dsn.as_ptr());
    assert!(!h.is_null());
    let v = traverse(h, dir);
    kvspaceClose(h);
    v
}

#[test]
fn list_cache_visibility_fs() {
    let dir = std::env::temp_dir().join(format!("kvspace-list-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dsn = CString::new(format!("fs://{}", dir.display())).unwrap();
    unsafe {
        let h = kvspaceConnect(dsn.as_ptr());
        let h2 = kvspaceConnect(dsn.as_ptr());
        assert!(!h.is_null() && !h2.is_null());
        let d = "/d/";
        let md = CString::new(d).unwrap();
        assert_eq!(
            kvspaceMkindex(h, md.as_ptr(), std::ptr::null_mut(), 0),
            0
        );
        for k in ["a", "b", "c"] {
            set(h, &format!("/d/{k}"), 1);
        }
        // 1. Sequential traversal matches a fresh listing.
        assert_eq!(traverse(h, d), fresh(&dsn, d));
        assert_eq!(traverse(h, d).len(), 3);

        // 2. Same-handle write mid-traversal is visible on the next index.
        assert_eq!(list_len(h, d), 3);
        let first = list_at(h, d, 0).unwrap();
        set(h, "/d/0", 1);
        let want = fresh(&dsn, d);
        assert_eq!(want.len(), 4);
        assert_eq!(
            list_at(h, d, 1),
            Some(want[1].clone()),
            "own SetValue mid-traversal"
        );
        assert_ne!(first, "");
        del(h, "/d/0");
        assert_eq!(list_at(h, d, 3), None, "own Del mid-traversal");
        assert_eq!(traverse(h, d), fresh(&dsn, d));

        // Lazy WriteNewPlace flushes on the next call and invalidates the snapshot.
        assert_eq!(list_len(h, d), 3);
        let k = CString::new("/d/e").unwrap();
        let lt = CString::new("int64").unwrap();
        let mut body: *mut u8 = std::ptr::null_mut();
        assert_eq!(
            kvspaceWriteNewPlace(
                h,
                k.as_ptr(),
                0,
                0,
                8,
                8,
                lt.as_ptr(),
                &mut body,
                std::ptr::null_mut(),
                0
            ),
            0
        );
        std::ptr::write_bytes(body, 0, 8);
        assert_eq!(
            list_at(h, d, 3),
            Some("e".to_string()),
            "lazy write visible"
        );

        // DelTree invalidates the snapshot.
        let sub = CString::new("/d/e").unwrap();
        assert_eq!(list_len(h, d), 4);
        assert_eq!(kvspaceDelTree(h, sub.as_ptr(), std::ptr::null_mut(), 0), 0);
        assert_eq!(list_at(h, d, 3), None, "DelTree mid-traversal");

        // 3. Cross-handle write becomes visible when repeating the same index.
        let before = traverse(h, d);
        assert_eq!(list_at(h, d, 0), Some(before[0].clone()));
        set(h2, "/d/0", 2);
        let want = fresh(&dsn, d);
        assert_eq!(
            list_at(h, d, 0),
            Some(want[0].clone()),
            "repeated idx re-lists"
        );
        del(h2, "/d/0");
        assert_eq!(list_len(h, d), 3, "ListLen re-lists");
        assert_eq!(traverse(h, d), fresh(&dsn, d));

        kvspaceClose(h2);
        kvspaceClose(h);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
