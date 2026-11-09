//! `kvspaceWriteNewPlace(cap > len)` 必须产 CAPACITY 布局：
//! `bodylen = 容量`、`layout = 1`、attr tag 2 记 `reallen = len`、总长含容量。
//!
//! 这条钉的是「容量必须进 wire」。丢掉它等于把容量降级成物理预留——
//! 读方拿不到有效长度，与 kvspace-c 的 `NewSlack`/`Reserve` 语义分叉。

use std::ffi::CString;

fn connect(label: &str) -> *mut kvspace_durable::ffi::Handle {
    let dir = std::env::temp_dir().join(format!("kvspace-durable-{label}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let dsn = CString::new(format!("fs://{}/store", dir.display())).unwrap();
    let h = unsafe { kvspace_durable::ffi::kvspaceConnect(dsn.as_ptr()) };
    assert!(!h.is_null(), "connect must succeed");
    h
}

#[test]
fn write_new_place_records_capacity_when_cap_exceeds_len() {
    let handle = connect("write_new_place_capacity");
    let key = CString::new("/cap").unwrap();
    let langtype = CString::new("[]char/utf8").unwrap();
    let mut body: *mut u8 = std::ptr::null_mut();
    let mut err = [0i8; 256];

    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            handle,
            key.as_ptr(),
            0, /* ref = inline */
            1, /* storetype：按裁决不分支，值无关 */
            2,  /* body_len */
            80, /* body_cap > body_len ⇒ CAPACITY */
            langtype.as_ptr(),
            &mut body,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    assert_eq!(rc, 0, "write must succeed");

    /* 读回整条 wire：bodylen = 容量 80，layout = 1，attrslen 含 tag 2。 */
    let mut out: *mut u8 = std::ptr::null_mut();
    let mut out_len: u32 = 0;
    let rc = unsafe {
        kvspace_durable::ffi::kvspaceGet(
            handle,
            key.as_ptr(),
            1,
            &mut out,
            &mut out_len,
        )
    };
    assert_eq!(rc, 0, "read must succeed");
    let wire = unsafe { std::slice::from_raw_parts(out, out_len as usize) };

    let bodylen = u64::from_le_bytes(wire[0..8].try_into().unwrap());
    assert_eq!(bodylen, 80, "head.bodylen 必须是**容量**，不是有效长度");
    assert_eq!(wire[12], 1, "layout 必须是 CAPACITY(1)");
    /* reserved 两字节与 padding 三字节必须为零 */
    assert_eq!(&wire[10..12], &[0, 0], "reserved 必须为零");
    assert_eq!(&wire[13..16], &[0, 0, 0], "padding 必须为零");
    /* 总长 = 16 + 容量 + attr */
    let attrslen = u16::from_le_bytes(wire[8..10].try_into().unwrap()) as usize;
    assert_eq!(wire.len(), 16 + 80 + attrslen, "总长必须含容量");
    /* attr 里必须有 tag 2（reallen = 2） */
    let attrs = &wire[16 + 80..];
    let mut found = false;
    let mut rest = attrs;
    while let [tag, l0, l1, tail @ ..] = rest {
        let rlen = usize::from(u16::from_le_bytes([*l0, *l1]));
        let rec = &tail[..rlen];
        if *tag == 2 {
            assert_eq!(rlen, 8, "tag 2 的 data 恒 8 字节");
            let reallen = u64::from_le_bytes(rec.try_into().unwrap());
            assert_eq!(reallen, 2, "tag 2 记的是有效长度");
            found = true;
        }
        rest = &tail[rlen..];
    }
    assert!(found, "CAPACITY 布局必须带 attr tag 2");

    unsafe { kvspace_durable::ffi::kvspaceClose(handle) };
}

#[test]
fn write_new_place_uses_length_layout_when_cap_equals_len() {
    let handle = connect("write_new_place_length");
    let key = CString::new("/len").unwrap();
    let langtype = CString::new("[]char/utf8").unwrap();
    let mut body: *mut u8 = std::ptr::null_mut();
    let mut err = [0i8; 256];

    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            handle,
            key.as_ptr(),
            0,
            1,
            2,
            2, /* cap == len ⇒ LENGTH */
            langtype.as_ptr(),
            &mut body,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    assert_eq!(rc, 0);

    let mut out: *mut u8 = std::ptr::null_mut();
    let mut out_len: u32 = 0;
    unsafe {
        kvspace_durable::ffi::kvspaceGet(handle, key.as_ptr(), 1, &mut out, &mut out_len)
    };
    let wire = unsafe { std::slice::from_raw_parts(out, out_len as usize) };
    let bodylen = u64::from_le_bytes(wire[0..8].try_into().unwrap());
    assert_eq!(bodylen, 2, "cap == len 时 bodylen 是有效长度");
    assert_eq!(wire[12], 0, "layout 必须是 LENGTH(0)");

    unsafe { kvspace_durable::ffi::kvspaceClose(handle) };
}
