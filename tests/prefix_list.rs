use kvspace_durable::{Backend, KVPair, KVSpace, KVStore, XValue};
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
    let dir = kvspace_durable::headlenpow::to_wire(0, 0, &[]).unwrap();
    let code =
        kvspace_durable::headlenpow::to_wire(1, 0, &[0, 0, 0, 0, 0, b'x']).unwrap();
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
    let ptr = kvspace_durable::headlenpow::to_wire(1, 1, b"/x").unwrap();
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
fn lists_physical_children() {
    let store = MemoryStore::default();
    store.set(
        "/m",
        &kvspace_durable::xvalue::to_wire_head("[int64]·int64", 0, &[], &[]),
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
fn new_wire_directories_and_extensions_use_physical_keys() {
    let store = MemoryStore::default();
    let inspect = store.clone();
    let mut backend = Backend::new(store);
    backend.mkindex("/lib/", 32).unwrap();
    let dir = kvspace_durable::headlenpow::to_wire(0, 0, &[]).unwrap();
    let slot =
        kvspace_durable::headlenpow::to_wire(1, 0, &[0, 0, 0, 0, 0]).unwrap();
    let map = kvspace_durable::headlenpow::to_wire(0, 0, &[]).unwrap();
    let scalar =
        kvspace_durable::headlenpow::to_wire(0, 0, &7i64.to_le_bytes()).unwrap();
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
        .iter()
        .filter(|(key, _)| !key.ends_with("‥langtype"))
        .all(|(_, raw)| kvspace_durable::headlenpow::from_wire(raw).is_some()));
    assert!(inspect.get("/m·").is_none());
}
