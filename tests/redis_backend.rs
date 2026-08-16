// redis_backend.rs — redis 后端冒烟测试（需本地 redis，用唯一前缀 + del_tree 清理，不 FLUSHDB）。

use kvspace_durable::*;

fn redis_kv() -> Box<dyn KVSpace> {
    conn("redis://127.0.0.1:6379")
}

#[test]
fn test_redis_roundtrip() {
    let mut kv = redis_kv();
    let root = format!("/kvspace-rust-test-{}", std::process::id());
    let root_dir = format!("{}/", root);

    // 清理可能残留
    let _ = kv.del_tree(&root_dir);

    // 单点 set/get
    kv.set(&[KVPair { key: format!("{}/a", root), val: new_int64(&[42]) }]).unwrap();
    let v = kv.get(&root_dir, &["a".to_string()], true).remove(0);
    assert_eq!(v, new_int64(&[42]));

    // 嵌套 + list
    kv.set(&[KVPair { key: format!("{}/lib/f[0,0]", root), val: new_char32(&['+' as u32]) }]).unwrap();
    let children = kv.list(&format!("{}/lib/", root), false, true);
    assert!(children.contains(&"f[0,0]".to_string()));

    // dict 成员
    kv.set(&[KVPair { key: format!("{}/m.Pi", root), val: new_float64(&[3.14]) }]).unwrap();
    let v = kv.get(&format!("{}/m.", root), &["Pi".to_string()], true).remove(0);
    assert_eq!(v, new_float64(&[3.14]));

    // 清理
    kv.del_tree(&root_dir).unwrap();
}
