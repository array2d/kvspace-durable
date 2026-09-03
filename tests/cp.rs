// cp.rs — kvspaceCp（单 key）/ kvspaceCpTree（递归子树）语义验证，覆盖 redis 与 fs 两后端。
// 重点：cp_tree 遇 extindex 成员，在 target 侧生成指向同一只读扩展的新 extindex。

use kvspace_durable::*;

fn set(kv: &mut dyn KVSpace, key: &str, v: &XValue) {
    kv.set(&[KVPair {
        key: key.to_string(),
        val: v.clone(),
        raw: None,
    }])
    .unwrap();
}

fn get_one(kv: &mut dyn KVSpace, key: &str) -> XValue {
    let (mut p, l) = sep_path(key);
    if p != PATH_SEP {
        p.push_str(DIR_INDEX_SUF);
    }
    kv.get(&p, &[l], true).remove(0)
}

fn run(dsn: &str) {
    let mut kv = conn(dsn);
    let kv: &mut dyn KVSpace = kv.as_mut();
    kv.clear().unwrap();

    // 原型 object 容器：两个叶成员 + 一个嵌套容器成员。
    set(kv, "/proto", &XValue::Obj);
    set(kv, "/proto·x", &new_int64(&[10]));
    set(kv, "/proto·y", &new_int64(&[20]));
    set(kv, "/proto·sub", &XValue::Obj);
    set(kv, "/proto·sub·z", &new_int64(&[30]));

    // 只读扩展源 + 原型上的 extindex 成员 ov 覆盖 /ext·（extindex 节点落 `/` 目录键 /proto·ov/）。
    set(kv, "/ext·", &new_obj_index());
    set(kv, "/ext·a", &new_int64(&[99]));
    kv.ext_index("/proto·ov·", "/ext·").unwrap();

    // ── cp：单 key 拷贝，不带成员 ──
    kv.cp("/proto·x", "/px").unwrap();
    assert_eq!(get_one(kv, "/px"), new_int64(&[10]), "cp 单 key");

    // ── cp_tree：递归子树拷贝 ──
    kv.cp_tree("/proto", "/inst").unwrap();
    assert_eq!(get_one(kv, "/inst·x"), new_int64(&[10]), "cp_tree 叶成员 x");
    assert_eq!(get_one(kv, "/inst·y"), new_int64(&[20]), "cp_tree 叶成员 y");
    assert_eq!(
        get_one(kv, "/inst·sub·z"),
        new_int64(&[30]),
        "cp_tree 嵌套成员 z"
    );
    match get_one(kv, "/inst") {
        XValue::Obj => {}
        other => panic!("cp_tree 根值 kind 丢失: {:?}", other),
    }

    // 成员表完整（含嵌套与 extindex 成员）。
    let mut m = kv.list("/inst·", false, true);
    m.sort();
    assert!(
        m.contains(&"x".to_string())
            && m.contains(&"y".to_string())
            && m.contains(&"sub".to_string()),
        "cp_tree 成员表: {:?}",
        m
    );

    // 根登记进父 index。
    assert!(
        kv.list("/", false, true).contains(&"inst".to_string()),
        "cp_tree 根登记父 index"
    );

    // ── 关键：extindex 成员在 target 侧成为新 extindex，overlay 读通 ──
    assert_eq!(
        kv.get("/inst·ov/", &["a".into()], true).remove(0),
        new_int64(&[99]),
        "cp_tree 的 extindex 成员 overlay 读通"
    );
    match kv.get("/inst·", &["ov/".into()], true).remove(0) {
        XValue::ExtIndex(e) => assert_eq!(e.ext_path, "/ext·", "extindex 指向同一只读扩展"),
        other => panic!("target 侧应为 extindex: {:?}", other),
    }

    // 改动源 extindex 目标，target 因共享同一只读扩展而同步可见（非深拷贝底层数据）。
    set(kv, "/ext·a", &new_int64(&[123]));
    assert_eq!(
        kv.get("/inst·ov/", &["a".into()], true).remove(0),
        new_int64(&[123]),
        "extindex 共享同一只读扩展"
    );
}

#[test]
fn cp_redis() {
    run("redis://127.0.0.1:6379");
}

#[test]
fn cp_fs() {
    let dir = std::env::temp_dir().join("kvspace-cp-test");
    let _ = std::fs::remove_dir_all(&dir);
    run(&format!("fs://{}", dir.display()));
    let _ = std::fs::remove_dir_all(&dir);
}
