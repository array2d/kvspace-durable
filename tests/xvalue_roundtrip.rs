// xvalue_roundtrip.rs — XValue TLV 编解码 round-trip + 字节级格式锁定。

use kvspace_durable::*;

fn roundtrip(v: XValue) {
    let kind = v.kind().to_string();
    let b1 = v.encode();
    let d = decode_xvalue(&b1);
    let b2 = d.encode();
    assert_eq!(b1, b2, "roundtrip failed for kind {}", kind);
}

#[test]
fn test_roundtrip_all_types() {
    roundtrip(XValue::None);
    roundtrip(new_int8(&[0, 1, -1, 127, -128]));
    roundtrip(new_int16(&[0, 1, -1, 32767, -32768]));
    roundtrip(new_int32(&[0, 1, -1, i32::MAX, i32::MIN]));
    roundtrip(new_int64(&[0, 1, -1, i64::MAX, i64::MIN]));
    roundtrip(new_uint8(&[0, 1, 255]));
    roundtrip(new_uint16(&[0, 1, 65535]));
    roundtrip(new_uint32(&[0, 1, u32::MAX]));
    roundtrip(new_uint64(&[0, 1, u64::MAX]));
    roundtrip(new_float32(&[0.0, -1.5, f32::MAX, f32::MIN]));
    roundtrip(new_float64(&[0.0, -1.5, f64::MAX, f64::MIN]));
    roundtrip(new_bool(&[true, false, true]));
    roundtrip(new_char_byte(b"hello"));
    roundtrip(new_char_ascii(b"hello"));
    roundtrip(new_char32(&[0x48, 0x65, 0x6c, 0x6c, 0x6f]));
    roundtrip(new_dict());
    roundtrip(new_dict_index(&["a".to_string(), "b".to_string()]));
    roundtrip(new_index(&["a".to_string(), "b".to_string()]));
    roundtrip(new_ext_index(&["a".to_string()], "/lib/init/"));
    roundtrip(new_rwir(2, 1, "rwir add(A:int64, B:int64) -> (C:int64)"));
    roundtrip(new_rwfunc(5, 2, 1));
    roundtrip(new_rwfunc_with_types(5, 2, 1, &["int64".to_string(), "int64".to_string()]));
    roundtrip(new_time(&[0, 1_700_000_000_000_000_000]));
    roundtrip(new_duration(&[0, 1_500_000_000, 250_000_000]));
    roundtrip(new_ptr(KIND_CHAR, "[0,-1]", 1));
}

/// 锁定 int64 单值 TLV 字节：head = [kind_len][kind][ref][arr_flag][ndim][raw_len][raw]
#[test]
fn test_int64_one_encoding() {
    let b = new_int64(&[1]).encode();
    assert_eq!(
        b,
        vec![
            5, b'i', b'n', b't', b'6', b'4', // kind_len=5 + "int64"
            0,  // ref = 0（内联）
            0,  // arr_flag = 0（标量）
            0,  // ndim = 0
            8, 0, 0, 0, // raw_len = 8 LE
            1, 0, 0, 0, 0, 0, 0, 0, // 1 as i64 LE
        ]
    );
}

/// 锁定数组（>1 元素）的 head 编码：arr_flag=1, ndim=1, dims=[N]。
#[test]
fn test_int64_array_encoding() {
    let b = new_int64(&[1, 2, 3]).encode();
    let h = decode_xvalue_head(&b);
    assert_eq!(h.kind, "int64");
    assert_eq!(h.r#ref, 0);
    assert_eq!(h.arr_flag, 1);
    assert_eq!(h.ndim, 1);
    assert_eq!(h.dims, vec![3]);
    assert_eq!(h.array_len, 3);
    assert_eq!(h.body_len, 24);
}

/// 锁定 Ptr 编码：ref=1，body=target 路径。
#[test]
fn test_ptr_encoding() {
    let b = new_ptr(KIND_CHAR, "[0,-1]", 1).encode();
    let h = decode_xvalue_head(&b);
    assert_eq!(h.kind, KIND_CHAR);
    assert_eq!(h.is_ptr, true);
    assert_eq!(h.r#ref, 1);
    let d = decode_xvalue(&b);
    assert_eq!(ptr_target(&d), "[0,-1]");
}

/// duration 显示格式对齐 Go time.Duration.String()。
#[test]
fn test_duration_string() {
    assert_eq!(duration_string(0), "0s");
    assert_eq!(duration_string(700), "700ns");
    assert_eq!(duration_string(1500), "1.5\u{00B5}s");
    assert_eq!(duration_string(500_000), "500\u{00B5}s");
    assert_eq!(duration_string(250_000_000), "250ms");
    assert_eq!(duration_string(1_500_000_000), "1.5s");
    assert_eq!(duration_string(90_000_000_000), "1m30s");
    assert_eq!(duration_string(3_661_000_000_000), "1h1m1s");
    assert_eq!(duration_string(-1_500_000_000), "-1.5s");
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// 字节级对齐 kvspace-go：以下 hex 是 Go 侧 `v.Encode()` 的实际输出，逐字节锁定。
#[test]
fn test_byte_compat_with_go() {
    assert_eq!(new_int64(&[1]).encode(), hex("05696e743634000000080000000100000000000000"));
    assert_eq!(
        new_int64(&[1, 2, 3]).encode(),
        hex("05696e7436340001010300000018000000010000000000000002000000000000000300000000000000")
    );
    assert_eq!(new_int8(&[-1]).encode(), hex("04696e743800000001000000ff"));
    assert_eq!(new_uint64(&[u64::MAX]).encode(), hex("0675696e74363400000008000000ffffffffffffffff"));
    assert_eq!(new_float64(&[1.5]).encode(), hex("07666c6f6174363400000008000000000000000000f83f"));
    assert_eq!(new_bool(&[true, false]).encode(), hex("04626f6f6c00010102000000020000000100"));
    assert_eq!(
        new_char32(&[0x68, 0x65, 0x6c, 0x6c, 0x6f]).encode(),
        hex("0a636861722f7574663332000101050000001400000068000000650000006c0000006c0000006f000000")
    );
    assert_eq!(new_char_byte(b"hello").encode(), hex("09636861722f75746638000101050000000500000068656c6c6f"));
    assert_eq!(new_char_ascii(b"hi").encode(), hex("0a636861722f617363696900010102000000020000006869"));
    assert_eq!(new_ptr(KIND_CHAR, "[0,-1]", 1).encode(), hex("0a636861722f7574663332010000060000005b302c2d315d"));
    assert_eq!(new_dict().encode(), hex("046469637400000000000000"));
    assert_eq!(
        new_dict_index(&["a".to_string(), "b".to_string()]).encode(),
        hex("046469637400000003000000610a62")
    );
    assert_eq!(
        new_index(&["a".to_string(), "b".to_string()]).encode(),
        hex("05696e64657800000003000000610a62")
    );
    assert_eq!(
        new_ext_index(&["a".to_string()], "/lib/init/").encode(),
        hex("08657874696e6465780000000f000000e280a62f6c69622f696e69742f0a61")
    );
    assert_eq!(
        new_rwir(2, 1, "add(A,B)->(C)").encode(),
        hex("0472776972000000110000000200010061646428412c42292d3e284329")
    );
    assert_eq!(new_rwfunc(5, 2, 1).encode(), hex("06727766756e63000101050000000400000002000100"));
    assert_eq!(
        new_rwfunc_with_types(5, 2, 1, &["int64".to_string(), "int64".to_string()]).encode(),
        hex("06727766756e63000101050000000f00000002000100696e7436340a696e743634")
    );
    assert_eq!(new_time(&[0]).encode(), hex("0474696d65000000080000000000000000000000"));
    assert_eq!(new_duration(&[0]).encode(), hex("086475726174696f6e000000080000000000000000000000"));
    assert_eq!(XValue::None.encode(), hex(""));
}
