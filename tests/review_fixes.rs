// review_fixes.rs - Cross-backend C ABI compatibility regression tests (v0.3.23).
//
// Covers:
//   1. Unchecked Ptr storage in SetValue (dangling/forward Ptr, 2-hop slot -> ‥arg -> caller,
//      char/utf8 -> []char/utf32 parameter binding, pointer cycles, and `[1]·int64` map langtype).
//   2. kvspaceGet(resolve=0 vs 1) leaf Ptr chain resolution (up to 16 hops, coexisting raw and
//      resolved borrowed views, missing target -> 0 + NULL, cycle / >16 hops -> 1).
//   3. kvspaceGetHead / kvspaceGetPart / kvspaceSetPart leaf Ptr resolution and cache invalidation.
//   4. kvspaceWriteInPlace(resolve=1) mutating the terminal target through a Ptr chain.
//   5. Untyped variable-length system slots (ST_SLACK + empty langtype) round-trip and Watch.
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

    // kvspaceGetHead follows the 2-hop Ptr chain to read the terminal target header/langtype.
    let k_slot_c = CString::new(k_slot.as_str()).unwrap();
    let mut head_out: kvspaceHead_t = std::mem::zeroed();
    assert_eq!(kvspaceGetHead(h, k_slot_c.as_ptr(), &mut head_out), 0);
    assert_eq!(head_out.r#ref, REF_INLINE);
    assert_eq!(head_out.storetype, ST_SLACK);
    assert_eq!(head_out.body_len, 6);
    let lt_str = std::str::from_utf8(&head_out.langtype[..head_out.langtype_len as usize]).unwrap();
    assert_eq!(lt_str, "[6]char/utf8");

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
        kvspaceGetPart(h, k_wip_p2_c.as_ptr(), 8, 8, &mut part_out, &mut part_len),
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
            8,
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

    // 6. Untyped variable-length system slots (ST_SLACK + empty langtype) round-trip and Watch.
    let k_sys_lib = format!("{root}/vthread/1/[1]/\u{2025}lib");
    let sys_body = b"/lib/main\xc2\xb7test";
    let w_sys_lib = headlenpow::to_wire(ST_SLACK, REF_INLINE, sys_body, "").unwrap();
    set_raw_checked(h, &k_sys_lib, &w_sys_lib);

    let (rc_sys, p_sys, l_sys) = get_raw_c(h, &k_sys_lib, 0);
    assert_eq!(rc_sys, 0);
    let got_sys = std::slice::from_raw_parts(p_sys, l_sys as usize);
    assert_eq!(got_sys, w_sys_lib.as_slice(), "untyped ST_SLACK must round-trip verbatim");

    // from_wire must not treat untyped ST_SLACK as XValue::None or drop its body.
    let decoded_sys = from_wire(got_sys);
    assert!(!is_none(&decoded_sys), "ST_SLACK with empty langtype must not decode as None");
    assert_eq!(decoded_sys.to_wire(), w_sys_lib);

    // Canonical None (ST_SHORT + REF_INLINE + len=0 + empty langtype) still decodes to XValue::None.
    let w_none = headlenpow::to_wire(ST_SHORT, REF_INLINE, &[], "").unwrap();
    assert!(is_none(&from_wire(&w_none)));

    // Empty struct marker (ST_SHORT + len=0 + langtype="/lib/Point") preserves its wire bytes.
    let k_struct = format!("{root}/pt");
    let w_struct = headlenpow::to_wire(ST_SHORT, REF_INLINE, &[], "/lib/Point").unwrap();
    set_raw_checked(h, &k_struct, &w_struct);
    set_raw_checked(h, &format!("{k_struct}\u{b7}x"), &new_int64(&[3]).to_wire());
    let (_, p_st, l_st) = get_raw_c(h, &k_struct, 0);
    assert_eq!(
        std::slice::from_raw_parts(p_st, l_st as usize),
        w_struct.as_slice()
    );

    // Watch on an untyped status slot: blocks while "running", returns full ST_SLACK wire on "stopped".
    let k_status = format!("{root}/status_slot");
    let w_running = headlenpow::to_wire(ST_SLACK, REF_INLINE, b"running", "").unwrap();
    let w_stopped = headlenpow::to_wire(ST_SLACK, REF_INLINE, b"stopped", "").unwrap();
    set_raw_checked(h, &k_status, &w_running);

    let dsn_bg = dsn.to_string();
    let k_status_bg = k_status.clone();
    let w_stopped_bg = w_stopped.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(25));
        unsafe {
            let h2 = connect_c(&dsn_bg);
            set_raw_checked(h2, &k_status_bg, &w_stopped_bg);
            kvspaceClose(h2);
        }
    });

    let k_status_c = CString::new(k_status.as_str()).unwrap();
    let mut watch_out: *mut u8 = std::ptr::null_mut();
    let mut watch_len: u32 = 0;
    let rc_w = kvspaceWatch(
        h,
        k_status_c.as_ptr(),
        w_stopped.as_ptr(),
        w_stopped.len() as u32,
        5_000_000, // 5ms tick
        &mut watch_out,
        &mut watch_len,
    );
    writer.join().unwrap();
    assert_eq!(rc_w, 0);
    assert_eq!(
        std::slice::from_raw_parts(watch_out, watch_len as usize),
        w_stopped.as_slice(),
        "kvspaceWatch on untyped ST_SLACK must preserve body and ST_SLACK header"
    );

    // 7. SetValue attr trailer validation (matches kvspace-c).
    let k_bad = CString::new(format!("{root}/bad_attr")).unwrap();
    let noattr: [u8; 16] = [8, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0];
    assert_ne!(
        kvspaceSetValue(h, k_bad.as_ptr(), noattr.as_ptr(), noattr.len() as u32, std::ptr::null_mut(), 0),
        0,
        "wire value without attr trailer must be rejected"
    );
    let shortattr: [u8; 18] = [8, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 1, 0];
    assert_ne!(
        kvspaceSetValue(h, k_bad.as_ptr(), shortattr.as_ptr(), shortattr.len() as u32, std::ptr::null_mut(), 0),
        0,
        "attr_len < 2 must be rejected"
    );
    let padded: [u8; 20] = [
        8, 0, 0, 0, 0, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0xEE, 0xEE,
    ];
    let k_pad = format!("{root}/padded");
    set_raw_checked(h, &k_pad, &padded);
    let (_, p_pad, l_pad) = get_raw_c(h, &k_pad, 0);
    assert_eq!(l_pad, 18, "trailing padding bytes must be trimmed to 8 + len + attr");
    assert_eq!(std::slice::from_raw_parts(p_pad, l_pad as usize), &padded[..18]);

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
