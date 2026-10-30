use kvspace_durable::{conn, KVPair, XValue};
use std::fs;

#[test]
fn reads_new_wire_directory_value() {
    let root = std::env::temp_dir().join(format!("kvspace-new-dir-fs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut kv = conn(&format!("fs://{}", root.display()));
    let raw = kvspace_durable::headlenpow::to_wire(0, 0, &[], "").unwrap();
    kv.set(&[KVPair {
        key: "/lib/f/".into(),
        val: kvspace_durable::xvalue::from_wire(&raw),
        raw: Some(raw.clone()),
    }])
    .unwrap();
    assert_eq!(kv.get_raw("/lib/f/"), raw);
    assert_eq!(kv.get_part("/lib/f/", 0, 32), raw);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn value_and_directory_keys_coexist() {
    let root = std::env::temp_dir().join(format!("kvspace-dir-value-fs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut kv = conn(&format!("fs://{}", root.display()));
    let value = kvspace_durable::xvalue::to_wire_head("int64", 0, &[], &7i64.to_le_bytes());
    kv.set(&[KVPair {
        key: "/node".into(),
        val: XValue::None,
        raw: Some(value.clone()),
    }])
    .unwrap();
    kv.mkindex("/node/", 0).unwrap();
    let directory = kv.get_raw("/node/");
    let dh = kvspace_durable::headlenpow::from_wire(&directory).unwrap();
    assert_eq!(dh.storetype, 0); /* 目录 = storetype 0 空 body；langtype 在 attr key */
    assert_eq!(dh.ptr, 0);
    assert_eq!(kv.get_raw("/node"), value);
    for name in ["__dir__", "__self__", "~abc"] {
        kv.set(&[KVPair {
            key: format!("/node/{name}"),
            val: XValue::None,
            raw: Some(value.clone()),
        }])
        .unwrap();
        assert_eq!(kv.get_raw(&format!("/node/{name}")), value);
        kv.set(&[KVPair {
            key: format!("/node·{name}"),
            val: XValue::None,
            raw: Some(value.clone()),
        }])
        .unwrap();
        assert_eq!(kv.get_raw(&format!("/node·{name}")), value);
    }
    let names = kv.list("/node/", false, false);
    for name in ["__dir__", "__self__", "~abc"] {
        assert!(names.contains(&name.to_string()));
        assert!(kv.list("/node·", false, false).contains(&name.to_string()));
    }
    assert_eq!(kv.get_raw("/node/"), directory);
    kv.cp_tree("/node", "/copy").unwrap();
    assert_eq!(kv.get_raw("/copy"), value);
    assert_eq!(kv.get_raw("/copy/"), directory);
    for name in ["__dir__", "__self__", "~abc"] {
        assert_eq!(kv.get_raw(&format!("/copy/{name}")), value);
        assert_eq!(kv.get_raw(&format!("/copy·{name}")), value);
    }
    let _ = fs::remove_dir_all(&root);
}
