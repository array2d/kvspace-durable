// review_fixes.rs - Cross-backend C ABI compatibility regression tests (v0.3.23).
//
// Covers:
//   1. Unchecked Ptr storage in SetValue (dangling/forward Ptr, 2-hop slot -> ‥arg -> caller,
//      char/utf8 -> []char/utf32 parameter binding, pointer cycles, and `[1]·int64` map langtype).
//   2. kvspaceGet(resolve=0 vs 1) leaf Ptr chain resolution (up to 16 hops, coexisting raw and
//      resolved borrowed views, missing target -> 0 + NULL, cycle / >16 hops -> 1).
//   (deleted per p11: kvspaceGetHead coverage — the API no longer exists)
//   4. kvspaceWriteInPlace(resolve=1) mutating the terminal target through a Ptr chain.
//   (deleted per p11: untyped system slots — 2026-11-06 裁决要求系统槽必须带 langtype)
//   6. SetValue attr trailer validation and trailing padding trim.
//   7. /lib/<func>/‥ctime cross-handle hot-reload invalidation with retired view retention.
//   8. List(resolve=1) following Ptr links whose targets lack a trailing `/` or `·`.

use kvspace_durable::ffi::*;
use kvspace_durable::*;
use std::ffi::{c_char, CStr, CString};

mod common;

unsafe fn connect_c(dsn: &str) -> *mut Handle {
    let c = CString::new(dsn).unwrap();
    let h = kvspaceConnect(c.as_ptr());
    assert!(!h.is_null(), "connect failed: {dsn}");
    h
}

unsafe fn set_raw_checked(h: *mut Handle, key: &str, wire: &[u8]) {
    let k = CString::new(key).unwrap();
    let mut err = [0 as c_char; 256];
    let rc = kvspaceSetValue(
        h,
        k.as_ptr(),
        wire.as_ptr(),
        wire.len() as u32,
        err.as_mut_ptr(),
        err.len() as u32,
    );
    let msg = CStr::from_ptr(err.as_ptr()).to_string_lossy();
    assert_eq!(rc, 0, "SetValue({key}) failed: {msg}");
}

unsafe fn get_raw_c(h: *mut Handle, key: &str, resolve: i32) -> (i32, *mut u8, u32) {
    let k = CString::new(key).unwrap();
    let mut out: *mut u8 = std::ptr::null_mut();
    let mut len: u32 = 0;
    let rc = kvspaceGet(h, k.as_ptr(), resolve, &mut out, &mut len);
    (rc, out, len)
}

unsafe fn deltree_c(h: *mut Handle, prefix: &str) {
    let p = CString::new(prefix).unwrap();
    let mut err = [0 as c_char; 256];
    let rc = kvspaceDelTree(h, p.as_ptr(), err.as_mut_ptr(), err.len() as u32);
    let msg = CStr::from_ptr(err.as_ptr()).to_string_lossy();
    assert_eq!(rc, 0, "DelTree({prefix}) failed: {msg}");
}

unsafe fn run_backend_suite(dsn: &str, root: &str) {
    let h = connect_c(dsn);
    deltree_c(h, root);

    // 1. Forward/dangling Ptr write + resolve=0 vs resolve=1.
    let k_dangling = format!("{root}/dangling_ptr");
    let k_late_target = format!("{root}/late_target");
    let w_dangling = new_ptr("int64", &k_late_target).to_wire();
    set_raw_checked(h, &k_dangling, &w_dangling);

    // Before target exists: resolve=0 returns raw Ptr wire; resolve=1 returns 0 with NULL/0.
    let (rc0, p0, l0) = get_raw_c(h, &k_dangling, 0);
    assert_eq!(rc0, 0);
    assert!(!p0.is_null() && l0 as usize == w_dangling.len());
    assert_eq!(
        std::slice::from_raw_parts(p0, l0 as usize),
        w_dangling.as_slice()
    );

    let (rc1, p1, l1) = get_raw_c(h, &k_dangling, 1);
    assert_eq!(rc1, 0, "dangling Ptr with resolve=1 must return 0 like a missing key");
    assert!(p1.is_null() && l1 == 0, "dangling Ptr with resolve=1 must clear output");

    // Once the forward target is written, resolve=1 resolves to the new value.
    let w_val42 = new_int64(&[42]).to_wire();
    set_raw_checked(h, &k_late_target, &w_val42);
    let (rc1b, p1b, l1b) = get_raw_c(h, &k_dangling, 1);
    assert_eq!(rc1b, 0);
    assert!(!p1b.is_null() && l1b as usize == w_val42.len());
    assert_eq!(
        std::slice::from_raw_parts(p1b, l1b as usize),
        w_val42.as_slice()
    );

    // 2. 2-hop Ptr (slot -> ‥arg -> caller), heterogeneous kindexpr, and coexisting borrowed views.
    let k_caller = format!("{root}/caller_slot");
    let k_arg = format!("{root}/\u{2025}arg[0,-1]");
    let k_slot = format!("{root}/[1,-1]");
    let w_utf8 = new_char_byte("你好".as_bytes()).to_wire();
    // Actual value is char/utf8 while parameter kindexpr is []char/utf32 across 2 hops.
    set_raw_checked(h, &k_caller, &w_utf8);
    let w_arg_ptr = new_ptr("[]char/utf32", &k_caller).to_wire();
    let w_slot_ptr = new_ptr("[]char/utf32", &k_arg).to_wire();
    set_raw_checked(h, &k_arg, &w_arg_ptr);
    set_raw_checked(h, &k_slot, &w_slot_ptr);

    let (rc_raw, ptr_raw, len_raw) = get_raw_c(h, &k_slot, 0);
    let (rc_res, ptr_res, len_res) = get_raw_c(h, &k_slot, 1);
    assert_eq!((rc_raw, rc_res), (0, 0));
    assert!(!ptr_raw.is_null() && !ptr_res.is_null());
    // Raw Ptr view and resolved target view stay valid simultaneously in read_cache.
    assert_eq!(
        std::slice::from_raw_parts(ptr_raw, len_raw as usize),
        w_slot_ptr.as_slice()
    );
    assert_eq!(
        std::slice::from_raw_parts(ptr_res, len_res as usize),
        w_utf8.as_slice()
    );

    // 3. 16-hop limit and cycle detection (>16 hops or cycle returns 1).
    let k_leaf16 = format!("{root}/hop_leaf");
    let w_leaf99 = new_int64(&[99]).to_wire();
    set_raw_checked(h, &k_leaf16, &w_leaf99);
    // hop_1 -> hop_leaf (1 hop), ..., hop_16 -> hop_15 (16 hops, valid); hop_17 -> hop_16 (17 hops, error).
    let mut prev = k_leaf16.clone();
    for i in 1..=17 {
        let cur = format!("{root}/hop_{i}");
        let wp = new_ptr("int64", &prev).to_wire();
        set_raw_checked(h, &cur, &wp);
        prev = cur;
    }
    let (rc_16, p_16, l_16) = get_raw_c(h, &format!("{root}/hop_16"), 1);
    assert_eq!(rc_16, 0, "16-hop Ptr chain must succeed");
    assert_eq!(
        std::slice::from_raw_parts(p_16, l_16 as usize),
        w_leaf99.as_slice()
    );

    let (rc_17, p_17, l_17) = get_raw_c(h, &format!("{root}/hop_17"), 1);
    assert_eq!(rc_17, 1, "17-hop Ptr chain (>16) must return 1");
    assert!(p_17.is_null() && l_17 == 0);

    // 2-node cycle: SetValue accepts cyclic pointers; resolve=0 reads raw Ptr, resolve=1 returns 1.
    let k_cyc_a = format!("{root}/cyc_a");
    let k_cyc_b = format!("{root}/cyc_b");
    set_raw_checked(h, &k_cyc_a, &new_ptr("int64", &k_cyc_b).to_wire());
    set_raw_checked(h, &k_cyc_b, &new_ptr("int64", &k_cyc_a).to_wire());
    let (rc_cyc0, p_cyc0, _) = get_raw_c(h, &k_cyc_a, 0);
    assert_eq!(rc_cyc0, 0);
    assert!(!p_cyc0.is_null());
    let (rc_cyc1, p_cyc1, l_cyc1) = get_raw_c(h, &k_cyc_a, 1);
    assert_eq!(rc_cyc1, 1, "Ptr cycle with resolve=1 must return 1");
    assert!(p_cyc1.is_null() && l_cyc1 == 0);

    // 4. kvspaceWriteInPlace(resolve=1) mutates terminal target through a 2-hop Ptr.
    let k_wip_val = format!("{root}/wip_val");
    let k_wip_p1 = format!("{root}/wip_p1");
    let k_wip_p2 = format!("{root}/wip_p2");
    let w_init = new_int64(&[100]).to_wire();
    let w_p1 = new_ptr("int64", &k_wip_val).to_wire();
    let w_p2 = new_ptr("int64", &k_wip_p1).to_wire();
    set_raw_checked(h, &k_wip_val, &w_init);
    set_raw_checked(h, &k_wip_p1, &w_p1);
    set_raw_checked(h, &k_wip_p2, &w_p2);

    // Prime read_cache with wip_val first to verify WriteInPlace(wip_p2, resolve=1) invalidates it.
    let (rc_pre, p_pre, l_pre) = get_raw_c(h, &k_wip_val, 0);
    assert_eq!(rc_pre, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_pre, l_pre as usize),
        w_init.as_slice()
    );

    let k_wip_p2_c = CString::new(k_wip_p2.as_str()).unwrap();
    let mut body_ptr: *mut u8 = std::ptr::null_mut();
    let mut err = [0 as c_char; 256];
    let rc_wip = kvspaceWriteInPlace(
        h,
        k_wip_p2_c.as_ptr(),
        1,
        8,
        &mut body_ptr,
        err.as_mut_ptr(),
        err.len() as u32,
    );
    assert_eq!(rc_wip, 0, "WriteInPlace(resolve=1) through 2-hop Ptr failed");
    assert!(!body_ptr.is_null());
    std::ptr::copy_nonoverlapping(777i64.to_le_bytes().as_ptr(), body_ptr, 8);

    // Next read flushes: terminal target becomes 777 while wip_p2 / wip_p1 keep their Ptr wire.
    let w_777 = new_int64(&[777]).to_wire();
    let (rc_post_res, p_post_res, l_post_res) = get_raw_c(h, &k_wip_p2, 1);
    assert_eq!(rc_post_res, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_post_res, l_post_res as usize),
        w_777.as_slice()
    );
    let (_, p_post_val, l_post_val) = get_raw_c(h, &k_wip_val, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_post_val, l_post_val as usize),
        w_777.as_slice()
    );
    let (_, p_post_p2, l_post_p2) = get_raw_c(h, &k_wip_p2, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_post_p2, l_post_p2 as usize),
        w_p2.as_slice()
    );

    // 5. GetPart / SetPart follow Ptr chains.
    let mut part_out: *mut u8 = std::ptr::null_mut();
    let mut part_len: u32 = 0;
    assert_eq!(
        /* body 偏移 16（head 16 字节）。 */
        kvspaceGetPart(h, k_wip_p2_c.as_ptr(), 16, 8, &mut part_out, &mut part_len),
        0
    );
    assert_eq!(part_len, 8);
    assert_eq!(
        std::slice::from_raw_parts(part_out, 8),
        &777i64.to_le_bytes()
    );

    let new_body = 888i64.to_le_bytes();
    assert_eq!(
        kvspaceSetPart(
            h,
            k_wip_p2_c.as_ptr(),
            16,
            new_body.as_ptr(),
            8,
            err.as_mut_ptr(),
            err.len() as u32
        ),
        0
    );
    let (_, p_after_sp, l_after_sp) = get_raw_c(h, &k_wip_val, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_after_sp, l_after_sp as usize),
        new_int64(&[888]).to_wire().as_slice()
    );

    // Canonical None（空 langtype + 空 body + 空 attr）still decodes to XValue::None.
    let w_none = headlenpow::to_wire(&[], "").unwrap();
    assert!(is_none(&from_wire(&w_none)));

    // Empty struct marker（len=0 + langtype="/lib/Point"）preserves its wire bytes.
    let k_struct = format!("{root}/pt");
    let w_struct = headlenpow::to_wire(&[], "/lib/Point").unwrap();
    set_raw_checked(h, &k_struct, &w_struct);
    set_raw_checked(h, &format!("{k_struct}\u{b7}x"), &new_int64(&[3]).to_wire());
    let (_, p_st, l_st) = get_raw_c(h, &k_struct, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_st, l_st as usize),
        w_struct.as_slice()
    );

    // 7. SetValue head/attr validation（16 字节 head，attrslen 在 head 里、无前缀）。
    let k_bad = CString::new(format!("{root}/bad_attr")).unwrap();
    /* bodylen=8 @0-7，attrslen=0 @8-9，padding@10-15 全零，
       后接 8 字节 body。空 attr 在新布局里**合法**（旧布局要求 [2,0] 前缀）。 */
    let noattr: [u8; 24] = [8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                            42, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        kvspaceSetValue(h, k_bad.as_ptr(), noattr.as_ptr(), noattr.len() as u32, std::ptr::null_mut(), 0),
        0,
        "空 attr（attrslen=0）在新布局里合法"
    );

    /* 声明 attrslen=4 但只给 2 字节 attr → 总长不足 → 拒绝。 */
    let mut shortattr = noattr;
    shortattr[8..10].copy_from_slice(&4u16.to_le_bytes());
    assert_ne!(
        kvspaceSetValue(h, k_bad.as_ptr(), shortattr.as_ptr(), (16 + 8 + 2) as u32, std::ptr::null_mut(), 0),
        0,
        "与 head 声明的 attrslen 不符的 wire 必须被拒绝"
    );

    /* padding 非零 → 拒绝（它是 ABI 的一部分）；10-15 六个字节同等对待。 */
    for at in 10usize..16 {
        let mut badpad = noattr;
        badpad[at] = 1;
        assert_ne!(
            kvspaceSetValue(h, k_bad.as_ptr(), badpad.as_ptr(), badpad.len() as u32, std::ptr::null_mut(), 0),
            0,
            "非零 padding@{at} 必须被拒绝"
        );
    }

    /* 尾部多余字节按 16 + bodylen + attrslen 截断。 */
    let mut padded = noattr.to_vec();
    padded.extend_from_slice(&[0xEE, 0xEE]);
    let k_pad = format!("{root}/padded");
    set_raw_checked(h, &k_pad, &padded);
    let (_, p_pad, l_pad) = get_raw_c(h, &k_pad, 0);
    assert_eq!(l_pad, 24, "尾部多余字节须截断到 16 + bodylen + attrslen");
    assert_eq!(std::slice::from_raw_parts(p_pad, l_pad as usize), &noattr[..]);

    // 8. Map langtype normalization in validate_ptr (preserve `[1]·int64` key type).
    {
        let mut kv = conn(dsn);
        let k_map = format!("{root}/num_key_map");
        kv.set(&[KVPair {
            key: k_map.clone(),
            val: new_map_langtype("[1]·int64"),
            raw: None,
        }])
        .unwrap();
        assert!(
            validate_ptr(kv.as_mut(), &k_map, "[1]·int64").is_ok(),
            "map langtype [1]·int64 must match itself"
        );
        assert!(
            validate_ptr(kv.as_mut(), &k_map, "[2]·int64").is_err(),
            "distinct map key shapes [1]·int64 and [2]·int64 must not be stripped by norm_langtype"
        );
    }

    // 9. /lib/<func>/‥ctime cross-handle hot-reload detection and retired snapshot retention.
    let func_dir = format!("/lib/rf_hot_{}", std::process::id());
    let k_ctime = format!("{func_dir}/\u{2025}ctime");
    let k_insn = format!("{func_dir}/[1,0]");
    let w_ctime1 = new_int64(&[1000]).to_wire();
    let w_insn1 = headlenpow::to_wire(b"ret 10", "").unwrap();
    set_raw_checked(h, &k_ctime, &w_ctime1);
    set_raw_checked(h, &k_insn, &w_insn1);

    // Prime handle `h` with ‥ctime and [1,0], keeping a borrowed pointer to the old [1,0] view.
    let (rc_ct1, p_ct1, l_ct1) = get_raw_c(h, &k_ctime, 0);
    let (rc_in1, p_in1, l_in1) = get_raw_c(h, &k_insn, 0);
    assert_eq!((rc_ct1, rc_in1), (0, 0));
    assert_eq!(std::slice::from_raw_parts(p_ct1, l_ct1 as usize), w_ctime1.as_slice());
    assert_eq!(std::slice::from_raw_parts(p_in1, l_in1 as usize), w_insn1.as_slice());

    // Simulate external relayout via `h2` updating [1,0] and ‥ctime.
    let h2 = connect_c(dsn);
    let w_ctime2 = new_int64(&[2000]).to_wire();
    let w_insn2 = headlenpow::to_wire(b"ret 20", "").unwrap();
    set_raw_checked(h2, &k_insn, &w_insn2);
    set_raw_checked(h2, &k_ctime, &w_ctime2);
    kvspaceClose(h2);

    // Re-reading ‥ctime on `h` observes the new timestamp, retires cached [1,0], and keeps p_in1 valid.
    let (rc_ct2, p_ct2, l_ct2) = get_raw_c(h, &k_ctime, 0);
    assert_eq!(rc_ct2, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_ct2, l_ct2 as usize),
        w_ctime2.as_slice(),
        "cross-handle /lib/*/‥ctime update must be visible immediately"
    );
    let (rc_in2, p_in2, l_in2) = get_raw_c(h, &k_insn, 0);
    assert_eq!(rc_in2, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_in2, l_in2 as usize),
        w_insn2.as_slice(),
        "instructions under func_dir must reload after ‥ctime changes"
    );
    assert_eq!(
        std::slice::from_raw_parts(p_in1, l_in1 as usize),
        w_insn1.as_slice(),
        "retired instruction snapshot must remain valid until Close"
    );
    deltree_c(h, &func_dir);

    // 10. List(resolve=1) follows Ptr links to bare targets (e.g. /lib/<op>/vids -> /vthread/‥vids).
    let k_vids_target = format!("{root}/vthread/‥vids");
    let k_vids_entry = format!("{k_vids_target}/89");
    let k_vids_link = format!("{root}/lib/numpy·zeros/vids");
    let w_vids_head = headlenpow::to_wire(&[], "[]char/utf32·[]char/utf32").unwrap();
    let w_vids_pc = headlenpow::to_wire(b"[1,0]", "[5]char/utf8").unwrap();
    let w_vids_ptr = new_ptr("[]char/utf32·[]char/utf32", &k_vids_target).to_wire();
    set_raw_checked(h, &k_vids_target, &w_vids_head);
    set_raw_checked(h, &k_vids_link, &w_vids_ptr);
    set_raw_checked(h, &format!("{k_vids_link}/89"), &w_vids_pc);

    let pfx_vids = CString::new(format!("{k_vids_link}/")).unwrap();
    let mut cnt: i32 = -1;
    assert_eq!(kvspaceListLen(h, pfx_vids.as_ptr(), 0, 1, &mut cnt), 0);
    assert_eq!(cnt, 1, "ListLen(resolve=1) must list children through a bare-target Ptr");
    let mut name_buf = [0u8; 64];
    let mut name_len: u32 = 0;
    assert_eq!(
        kvspaceListAt(
            h,
            pfx_vids.as_ptr(),
            0,
            1,
            0,
            name_buf.as_mut_ptr(),
            name_buf.len() as u32,
            &mut name_len,
        ),
        0
    );
    assert_eq!(&name_buf[..name_len as usize], b"89");
    let (rc_ve, p_ve, l_ve) = get_raw_c(h, &k_vids_entry, 0);
    assert_eq!(rc_ve, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_ve, l_ve as usize),
        w_vids_pc.as_slice()
    );

    // `·` member enumeration also follows a bare-target Ptr (e.g. /rf/m_link· -> /rf/m_real).
    let k_m_real = format!("{root}/m_real");
    let k_m_link = format!("{root}/m_link");
    let w_m_head = headlenpow::to_wire(&[], "[]char/utf8·int64").unwrap();
    let w_m_val = new_int64(&[42]).to_wire();
    let w_m_ptr = new_ptr("[]char/utf8·int64", &k_m_real).to_wire();
    set_raw_checked(h, &k_m_real, &w_m_head);
    set_raw_checked(h, &format!("{k_m_real}·n"), &w_m_val);
    set_raw_checked(h, &k_m_link, &w_m_ptr);
    let pfx_m = CString::new(format!("{k_m_link}·")).unwrap();
    let mut m_cnt: i32 = -1;
    assert_eq!(kvspaceListLen(h, pfx_m.as_ptr(), 0, 1, &mut m_cnt), 0);
    assert_eq!(m_cnt, 1, "ListLen(resolve=1) must list `·` members through a bare map Ptr");

    deltree_c(h, root);
    kvspaceClose(h);
}

#[test]
fn review_fixes_fs() {
    let dir = std::env::temp_dir().join(format!("kvspace-review-fixes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let dsn = format!("fs://{}", dir.display());
    unsafe {
        run_backend_suite(&dsn, "/rf");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn review_fixes_redis() {
    let Some(dsn) = common::redis_test_dsn("review_fixes") else {
        return;
    };
    let root = format!("/kvspace-review-fixes-test-{}", std::process::id());
    unsafe {
        run_backend_suite(&dsn, &root);
    }
}
