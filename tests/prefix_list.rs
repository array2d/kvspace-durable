use kvspace_durable::{metadata, Backend, KVPair, KVSpace, KVStore, XValue};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Default, Clone)]
struct MemoryStore(Rc<RefCell<HashMap<String, Vec<u8>>>>);

impl KVStore for MemoryStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.0.borrow().get(key).cloned()
    }

    fn set(&self, key: &str, value: &[u8]) {
        self.0.borrow_mut().insert(key.to_string(), value.to_vec());
    }

    fn del(&self, keys: &[&str]) {
        let mut values = self.0.borrow_mut();
        for key in keys {
            values.remove(*key);
        }
    }

    fn scan_keys(&self, prefix: &str) -> Vec<String> {
        self.0
            .borrow()
            .keys()
            .filter(|key| {
                (key.len() > prefix.len()
                    && key.starts_with(prefix)
                    && (key[prefix.len()..].starts_with('/')
                        || key[prefix.len()..].starts_with('·')))
                    || *key == prefix
            })
            .cloned()
            .collect()
    }

    fn flush(&self) {
        self.0.borrow_mut().clear();
    }
}

#[test]
fn frame_pointer_shadows_code_operand() {
    let store = MemoryStore::default();
    let mut kv = Backend::new(store);
    let dir = kvspace_durable::headlenpow::encode(5, 0, 0, 0, "lib", &[], 0).unwrap();
    let code =
        kvspace_durable::headlenpow::encode(5, 1, 6, 6, "rwir", &[0, 0, 0, 0, 0, b'x'], 6).unwrap();
    for (key, raw) in [
        ("/lib/f/", dir),
        ("/lib/f/[1,-1]", code.clone()),
        ("/lib/f/[1,0]", code),
    ] {
        kv.set(&[KVPair {
            key: key.into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    }
    kv.ext_index("/vthread/1/[1]/", "/lib/f/").unwrap();
    let ptr = kvspace_durable::headlenpow::encode(5, 5, 2, 2, "int64", b"/x", 2).unwrap();
    assert!(kv
        .set(&[KVPair {
            key: "/vthread/1/[1]/[1,0]".into(),
            val: XValue::None,
            raw: Some(ptr.clone()),
        }])
        .is_err());
    kv.set(&[KVPair {
        key: "/vthread/1/[1]/[1,-1]".into(),
        val: XValue::None,
        raw: Some(ptr.clone()),
    }])
    .unwrap();
    assert_eq!(kv.get_raw("/vthread/1/[1]/[1,-1]"), ptr);
}

#[test]
fn stores_and_removes_metadata_sidecars() {
    let store = MemoryStore::default();
    let inspect = store.clone();
    let mut backend = Backend::new(store);
    let key = "/private";
    let meta = metadata::key_for(key).unwrap();
    let raw = kvspace_durable::xvalue::encode_head("int32", 0, &[], &7i32.to_le_bytes());
    backend
        .set(&[KVPair {
            key: key.into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    backend.set_metadata(key, true, 42).unwrap();
    assert_eq!(
        metadata::decode(&inspect.get(&meta).unwrap()),
        Some((true, 42))
    );
    assert_eq!(backend.get_metadata(key).unwrap(), (true, 42));
    backend.cp(key, "/copy").unwrap();
    let copy_meta = metadata::key_for("/copy").unwrap();
    assert_eq!(
        metadata::decode(&inspect.get(&copy_meta).unwrap()),
        Some((true, 42))
    );
    backend.del(&["/copy".into()]).unwrap();
    assert!(inspect.get(&copy_meta).is_none());
    let raw = kvspace_durable::xvalue::encode_head("int32", 0, &[], &8i32.to_le_bytes());
    backend
        .set(&[KVPair {
            key: key.into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    assert!(inspect.get(&meta).is_none());
    assert_eq!(backend.get_metadata(key).unwrap(), (false, 0));

    let raw = kvspace_durable::xvalue::encode_head("int32", 0, &[], &9i32.to_le_bytes());
    backend
        .set(&[KVPair {
            key: "/tree/a".into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    backend.set_metadata("/tree/a", true, 19).unwrap();
    backend.cp_tree("/tree", "/tree_copy").unwrap();
    let copied = metadata::key_for("/tree_copy/a").unwrap();
    assert_eq!(
        metadata::decode(&inspect.get(&copied).unwrap()),
        Some((true, 19))
    );
    backend.del_tree("/tree_copy").unwrap();
    assert!(inspect.get(&copied).is_none());
}

#[test]
fn lists_physical_children() {
    let store = MemoryStore::default();
    store.set(
        "/m",
        &kvspace_durable::xvalue::encode_head("[int64]·int64", 0, &[], &[]),
    );
    store.set("/m·[10]", b"value");
    store.set("/m·[2]", b"value");
    store.set("/m·[2]·nested", b"value");
    store.set("/d/x", b"value");
    store.set("/d/sub/", b"directory");
    store.set("/d/sub/y", b"value");
    store.set("/.kvspace-meta/2f", b"value");
    let mut backend = Backend::new(store);
    assert_eq!(backend.list("/m·", false, false), vec!["[2]", "[10]"]);
    assert_eq!(backend.list_len("/m·", false, false), 2);
    assert_eq!(backend.list_at("/m·", 1, false, false), Some("[10]".into()));
    assert_eq!(backend.list("/d/", false, false), vec!["sub/", "x"]);
    assert_eq!(backend.list("/", false, false), vec!["d", "m"]);
}

#[test]
fn copies_new_wire_metadata() {
    let store = MemoryStore::default();
    let inspect = store.clone();
    let mut backend = Backend::new(store);
    let raw =
        kvspace_durable::headlenpow::encode(5, 0, 0, 0, "int32", &7i32.to_le_bytes(), 4).unwrap();
    backend
        .set(&[KVPair {
            key: "/src/a".into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    backend.set_metadata("/src/a", true, 29).unwrap();
    backend.cp("/src/a", "/copy").unwrap();
    backend.cp_tree("/src", "/tree").unwrap();
    for key in ["/copy", "/tree/a"] {
        let meta = metadata::key_for(key).unwrap();
        assert_eq!(
            metadata::decode(&inspect.get(&meta).unwrap()),
            Some((true, 29))
        );
    }
}

#[test]
fn copies_pointer_slot_metadata() {
    let store = MemoryStore::default();
    let inspect = store.clone();
    let mut backend = Backend::new(store);
    let target =
        kvspace_durable::headlenpow::encode(5, 0, 0, 0, "int32", &7i32.to_le_bytes(), 4).unwrap();
    let pointer = kvspace_durable::headlenpow::encode(5, 5, 7, 7, "int32", b"/target", 7).unwrap();
    for (key, raw) in [("/target", target), ("/link", pointer)] {
        backend
            .set(&[KVPair {
                key: key.into(),
                val: XValue::None,
                raw: Some(raw),
            }])
            .unwrap();
    }
    backend.set_metadata("/target", true, 42).unwrap();
    backend.set_metadata("/link", true, 17).unwrap();
    backend.cp("/link", "/copy").unwrap();
    assert_eq!(backend.get_metadata("/copy").unwrap(), (true, 42));
    let key = metadata::key_for("/copy").unwrap();
    assert_eq!(
        metadata::decode(&inspect.get(&key).unwrap()),
        Some((true, 17))
    );
}

#[test]
fn new_wire_directories_and_extensions_use_physical_keys() {
    let store = MemoryStore::default();
    let inspect = store.clone();
    let mut backend = Backend::new(store);
    backend.mkindex("/lib/", 32).unwrap();
    let dir = kvspace_durable::headlenpow::encode(5, 0, 0, 0, "rwfunc", &[], 0).unwrap();
    let slot =
        kvspace_durable::headlenpow::encode(5, 1, 5, 5, "rwir", &[0, 0, 0, 0, 0], 5).unwrap();
    let map = kvspace_durable::headlenpow::encode(5, 0, 0, 0, "[int64]·int64", &[], 0).unwrap();
    let scalar =
        kvspace_durable::headlenpow::encode(5, 0, 0, 0, "int64", &7i64.to_le_bytes(), 8).unwrap();
    for (key, raw) in [
        ("/lib/f/", dir),
        ("/lib/f/[1,0]", slot.clone()),
        ("/m", map),
        ("/m·1", scalar),
    ] {
        backend
            .set(&[KVPair {
                key: key.into(),
                val: XValue::None,
                raw: Some(raw),
            }])
            .unwrap();
    }
    backend.ext_index("/stack/", "/lib/f/").unwrap();
    assert_eq!(backend.get_raw("/stack/[1,0]"), slot);
    assert_eq!(backend.list("/m·", false, false), vec!["1"]);
    assert_eq!(backend.list("/stack/", true, false), vec!["[1,0]"]);
    assert!(inspect
        .0
        .borrow()
        .values()
        .all(|raw| kvspace_durable::headlenpow::decode(raw).is_some()));
    assert!(inspect.get("/m·").is_none());
}
