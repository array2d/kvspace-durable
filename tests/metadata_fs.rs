use kvspace_durable::{conn, metadata, new_map_langtype, KVPair, KVSpace, XValue};
use std::fs;

fn set(kv: &mut dyn KVSpace, key: &str, ro: bool, vid: u32) {
    let raw = kvspace_durable::xvalue::encode_head("int32", 0, &[], &7i32.to_le_bytes());
    kv.set(&[KVPair {
        key: key.into(),
        val: XValue::None,
        raw: Some(raw),
    }])
    .unwrap();
    kv.set_metadata(key, ro, vid).unwrap();
}

#[test]
fn metadata_tracks_fs_subtrees() {
    let root = std::env::temp_dir().join(format!("kvspace-meta-fs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut kv = conn(&format!("fs://{}", root.display()));
    let kv = kv.as_mut();
    set(kv, "/src/a", true, 7);
    set(kv, "/src/sub/b", true, 8);
    let meta = |key: &str| root.join(metadata::key_for(key).unwrap().trim_start_matches('/'));
    assert_eq!(
        metadata::decode(&fs::read(meta("/src/a")).unwrap()),
        Some((true, 7))
    );
    assert_eq!(kv.get_metadata("/src/a").unwrap(), (true, 7));
    assert!(kv
        .list("/", false, false)
        .iter()
        .all(|name| !name.contains(".kvspace-meta")));

    kv.cp_tree("/src", "/dst").unwrap();
    assert_eq!(
        metadata::decode(&fs::read(meta("/dst/a")).unwrap()),
        Some((true, 7))
    );
    assert_eq!(
        metadata::decode(&fs::read(meta("/dst/sub/b")).unwrap()),
        Some((true, 8))
    );
    kv.del_tree("/src").unwrap();
    assert!(!meta("/src/a").exists());
    assert!(!meta("/src/sub/b").exists());
    assert!(meta("/dst/a").exists());
    kv.del_tree("/dst").unwrap();
    assert!(!meta("/dst/a").exists());
    assert!(!meta("/dst/sub/b").exists());

    kv.set(&[KVPair {
        key: "/m".into(),
        val: new_map_langtype("[int64]·[]char/utf32"),
        raw: None,
    }])
    .unwrap();
    set(kv, "/m·x", true, 9);
    kv.cp_list("/m", "/n").unwrap();
    assert_eq!(
        metadata::decode(&fs::read(meta("/n·x")).unwrap()),
        Some((true, 9))
    );
    kv.del_tree("/m").unwrap();
    assert!(!meta("/m·x").exists());
    assert!(meta("/n·x").exists());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reads_new_wire_directory_value() {
    let root = std::env::temp_dir().join(format!("kvspace-new-dir-fs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut kv = conn(&format!("fs://{}", root.display()));
    let raw = kvspace_durable::headlenpow::encode(5, 0, 0, 0, "rwfunc", &[], 0).unwrap();
    kv.set(&[KVPair {
        key: "/lib/f/".into(),
        val: kvspace_durable::xvalue::decode_xvalue(&raw),
        raw: Some(raw.clone()),
    }])
    .unwrap();
    assert_eq!(kv.get_raw("/lib/f/"), raw);
    assert_eq!(kv.get_part("/lib/f/", 0, 32), raw);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn copies_pointer_slot_metadata() {
    let root = std::env::temp_dir().join(format!("kvspace-pointer-meta-fs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut kv = conn(&format!("fs://{}", root.display()));
    let kv = kv.as_mut();
    let target =
        kvspace_durable::headlenpow::encode(5, 0, 0, 0, "int32", &7i32.to_le_bytes(), 4).unwrap();
    let pointer = kvspace_durable::headlenpow::encode(5, 5, 7, 7, "int32", b"/target", 7).unwrap();
    for (key, raw) in [("/target", target), ("/link", pointer)] {
        kv.set(&[KVPair {
            key: key.into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    }
    kv.set_metadata("/target", true, 42).unwrap();
    kv.set_metadata("/link", true, 17).unwrap();
    kv.cp("/link", "/copy").unwrap();
    assert_eq!(kv.get_metadata("/copy").unwrap(), (true, 42));
    let key = metadata::key_for("/copy").unwrap();
    assert_eq!(
        metadata::decode(&fs::read(root.join(key.trim_start_matches('/'))).unwrap()),
        Some((true, 17))
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn value_and_directory_keys_coexist() {
    let root = std::env::temp_dir().join(format!("kvspace-dir-value-fs-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut kv = conn(&format!("fs://{}", root.display()));
    let value = kvspace_durable::xvalue::encode_head("int64", 0, &[], &7i64.to_le_bytes());
    kv.set(&[KVPair {
        key: "/node".into(),
        val: XValue::None,
        raw: Some(value.clone()),
    }])
    .unwrap();
    kv.mkindex("/node/", 0).unwrap();
    let directory = kv.get_raw("/node/");
    assert_eq!(
        kvspace_durable::headlenpow::decode(&directory)
            .unwrap()
            .langtype,
        "lib"
    );
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
