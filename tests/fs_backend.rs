// fs_backend.rs — fs 后端（/tmp tmpfs）语义测试。

use kvspace_durable::*;

fn fresh_fs(tag: &str) -> Box<dyn KVSpace> {
    let root = format!("/tmp/kvspace-fs-test-{}-{}", tag, std::process::id());
    let _ = std::fs::remove_dir_all(&root);
    conn(&format!("fs://{}", root))
}

#[test]
fn test_set_get_roundtrip() {
    let mut kv = fresh_fs("rg");
    kv.set(&[KVPair { key: "/a".to_string(), val: new_int64(&[42]) }]).unwrap();
    let v = kv.get("/", &["a".to_string()], true).remove(0);
    assert_eq!(v, new_int64(&[42]));
}

#[test]
fn test_nested_set_list_get() {
    let mut kv = fresh_fs("nest");
    kv.set(&[KVPair { key: "/lib/math.sum/[0,0]".to_string(), val: new_char32(&['+' as u32]) }]).unwrap();
    kv.set(&[KVPair { key: "/lib/math.sum/[1,0]".to_string(), val: new_char32(&['-' as u32]) }]).unwrap();

    let children = kv.list("/lib/math.sum/", false, true);
    assert!(children.contains(&"[0,0]".to_string()));
    assert!(children.contains(&"[1,0]".to_string()));

    let v = kv.get("/lib/math.sum/", &["[0,0]".to_string()], true).remove(0);
    assert_eq!(v, new_char32(&['+' as u32]));
}

#[test]
fn test_dict_member() {
    let mut kv = fresh_fs("dict");
    kv.set(&[KVPair { key: "/lib/math.Pi".to_string(), val: new_float64(&[3.14]) }]).unwrap();
    let v = kv.get("/lib/math.", &["Pi".to_string()], true).remove(0);
    assert_eq!(v, new_float64(&[3.14]));

    let members = kv.list("/lib/math.", false, true);
    assert!(members.contains(&"Pi".to_string()));
}

#[test]
fn test_del_and_del_tree() {
    let mut kv = fresh_fs("del");
    kv.set(&[KVPair { key: "/a/b/c".to_string(), val: new_int64(&[1]) }]).unwrap();
    kv.set(&[KVPair { key: "/a/b/d".to_string(), val: new_int64(&[2]) }]).unwrap();

    kv.del(&["/a/b/c".to_string()]).unwrap();
    assert!(is_none(&kv.get("/a/b/", &["c".to_string()], true).remove(0)));

    kv.del_tree("/a/b/").unwrap();
    let remaining = kv.list("/a/", false, true);
    assert!(!remaining.contains(&"b".to_string()));
}

#[test]
fn test_soft_link() {
    let mut kv = fresh_fs("link");
    kv.set(&[KVPair { key: "/real/x".to_string(), val: new_int64(&[7]) }]).unwrap();
    kv.set(&[KVPair { key: "/alias".to_string(), val: new_ptr(KIND_INDEX, "/real/", 1) }]).unwrap();

    let v = kv.get("/alias/", &["x".to_string()], true).remove(0);
    assert_eq!(v, new_int64(&[7]));
}

#[test]
fn test_mkindex() {
    let mut kv = fresh_fs("mkidx");
    kv.mkindex("/x/y/z/").unwrap();
    let children = kv.list("/x/", false, true);
    assert!(children.contains(&"y/".to_string()));
}

#[test]
fn test_ext_index() {
    let mut kv = fresh_fs("ext");
    // 扩展层 /ext/ 里有 a；本地层 /loc/ 挂载 /ext/。
    kv.mkindex("/ext/").unwrap();
    kv.set(&[KVPair { key: "/ext/a".to_string(), val: new_int64(&[1]) }]).unwrap();

    kv.ext_index("/loc/", "/ext/").unwrap();
    // 本地写 /loc/b
    kv.set(&[KVPair { key: "/loc/b".to_string(), val: new_int64(&[2]) }]).unwrap();

    // 列 /loc/ 应同时看到本地 b 与扩展 a
    let children = kv.list("/loc/", true, true);
    assert!(children.contains(&"a".to_string()));
    assert!(children.contains(&"b".to_string()));

    // 经扩展读 a
    let v = kv.get("/loc/", &["a".to_string()], true).remove(0);
    assert_eq!(v, new_int64(&[1]));
}
