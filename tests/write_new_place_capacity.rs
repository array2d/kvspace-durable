//! `kvspaceWriteNewPlace` 的 `body_cap` **不进 wire**：
//! `bodylen` 恒为有效长度，attr 里不出现 tag 2，容量属于后端的物理分配。
//!
//! 这条钉的是「容量不是 wire 事实」：曾经容量会改 `bodylen` 并写 tag 2 `reallen`，
//! 于是同一份值有两个长度量。tag 2 已删除，`bodylen` 是唯一长度来源。

use std::ffi::CString;

fn connect(label: &str) -> *mut kvspace_durable::ffi::Handle {
    let dir = std::env::temp_dir().join(format!("kvspace-durable-{label}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let dsn = CString::new(format!("fs://{}/store", dir.display())).unwrap();
    let h = unsafe { kvspace_durable::ffi::kvspaceConnect(dsn.as_ptr()) };
    assert!(!h.is_null(), "connect must succeed");
    h
}

/// 写一个 `body_len` 字节、`body_cap` 容量的值，读回整条 wire。
fn write_and_read(label: &str, body_len: u32, body_cap: u64) -> Vec<u8> {
    let handle = connect(label);
    let key = CString::new("/k").unwrap();
    let langtype = CString::new("[]char/utf8").unwrap();
    let mut body: *mut u8 = std::ptr::null_mut();
    let mut err = [0i8; 256];

    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            handle,
            key.as_ptr(),
            0, /* ref = inline */
            1, /* storetype：按裁决不分支，值无关 */
            body_len,
            body_cap,
            langtype.as_ptr(),
            &mut body,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    assert_eq!(rc, 0, "write must succeed");
    if body_len > 0 {
        let fill = vec![b'a'; body_len as usize];
        unsafe { std::ptr::copy_nonoverlapping(fill.as_ptr(), body, body_len as usize) };
    }

    /* WriteNewPlace 是惰性写：再写一笔把上一笔 flush 下去，才能读到。 */
    let probe = CString::new("/__commit_probe").unwrap();
    let int64 = CString::new("int64").unwrap();
    let mut probe_body: *mut u8 = std::ptr::null_mut();
    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            handle,
            probe.as_ptr(),
            0,
            1,
            8,
            8,
            int64.as_ptr(),
            &mut probe_body,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    assert_eq!(rc, 0, "commit probe must succeed");
    unsafe { std::ptr::copy_nonoverlapping(42i64.to_le_bytes().as_ptr(), probe_body, 8) };

    let mut out: *mut u8 = std::ptr::null_mut();
    let mut out_len: u32 = 0;
    let rc = unsafe { kvspace_durable::ffi::kvspaceGet(handle, key.as_ptr(), 1, &mut out, &mut out_len) };
    assert_eq!(rc, 0, "re-read must succeed");
    let wire = unsafe { std::slice::from_raw_parts(out, out_len as usize) }.to_vec();
    unsafe { kvspace_durable::ffi::kvspaceClose(handle) };
    wire
}

/// 遍历 attr 记录，返回遇到的 tag 列表（任何 tag 2 都是回归）。
fn attr_tags(wire: &[u8]) -> Vec<u8> {
    let bodylen = u64::from_le_bytes(wire[0..8].try_into().unwrap()) as usize;
    let attrslen = usize::from(u16::from_le_bytes(wire[8..10].try_into().unwrap()));
    let attrs = &wire[16 + bodylen..16 + bodylen + attrslen];
    let mut tags = Vec::new();
    let mut rest = attrs;
    while let [tag, l0, l1, tail @ ..] = rest {
        tags.push(*tag);
        rest = &tail[usize::from(u16::from_le_bytes([*l0, *l1]))..];
    }
    tags
}

#[test]
fn body_cap_does_not_enter_wire() {
    /* 请求容量 80、有效长度 2：wire 里的 bodylen 必须是 2，不是 80。 */
    let wire = write_and_read("cap-ignored", 2, 80);
    let bodylen = u64::from_le_bytes(wire[0..8].try_into().unwrap());
    assert_eq!(bodylen, 2, "bodylen 是有效长度，容量不进 wire");
    let attrslen = usize::from(u16::from_le_bytes(wire[8..10].try_into().unwrap()));
    assert_eq!(wire.len(), 16 + 2 + attrslen, "总长 = 16 + 有效长度 + attr");
    assert_eq!(&wire[16..18], b"aa", "body 就是那 2 字节内容");
    assert!(
        !attr_tags(&wire).contains(&2),
        "attr 里不得出现 tag 2（reallen 已删除）：tags={:?}",
        attr_tags(&wire)
    );
}

#[test]
fn body_cap_equal_to_len_also_omits_tag_two() {
    let wire = write_and_read("cap-equal", 2, 2);
    let bodylen = u64::from_le_bytes(wire[0..8].try_into().unwrap());
    assert_eq!(bodylen, 2, "bodylen 是有效长度");
    assert!(
        !attr_tags(&wire).contains(&2),
        "attr 里不得出现 tag 2：tags={:?}",
        attr_tags(&wire)
    );
}

#[test]
fn body_cap_smaller_than_len_is_rejected() {
    let handle = connect("cap-too-small");
    let key = CString::new("/k").unwrap();
    let langtype = CString::new("[]char/utf8").unwrap();
    let mut body: *mut u8 = std::ptr::null_mut();
    let mut err = [0i8; 256];
    let rc = unsafe {
        kvspace_durable::ffi::kvspaceWriteNewPlace(
            handle,
            key.as_ptr(),
            0,
            1,
            8,
            2,
            langtype.as_ptr(),
            &mut body,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    assert_ne!(rc, 0, "cap < len 是几何错，必须拒绝");
    unsafe { kvspace_durable::ffi::kvspaceClose(handle) };
}
