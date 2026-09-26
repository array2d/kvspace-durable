// map_coord.rs — stringkeymap 坐标 key 布局（docs/stringkeymap-ndarray.md）的端到端验证。
// 覆盖 redis 与 fs 两后端。

use kvspace_durable::*;

mod common;

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

    set(kv, "/m", &new_map_langtype("[int64,int64]·float32"));

    // 乱序写坐标成员。
    set(kv, "/m·[1,2]", &new_float32(&[6.28]));
    set(kv, "/m·[0,1]", &new_float32(&[3.14]));
    set(kv, "/m·[0,0]", &new_float32(&[1.0]));

    // The map type is stored at /m; members are physical /m· keys.
    match get_one(kv, "/m") {
        XValue::Map(m) => assert_eq!(m.langtype, "[int64,int64]·float32"),
        other => panic!("容器值 kind 丢失: {:?}", other),
    }

    // list 按 row-major 数值升序（先比 s0 再比 s1）。
    let names = kv.list("/m·", false, true);
    assert_eq!(names, vec!["[0,0]", "[0,1]", "[1,2]"], "list 顺序");

    // 坐标成员读回，缺席坐标读 None。
    assert_eq!(get_one(kv, "/m·[1,2]"), new_float32(&[6.28]));
    assert!(is_none(&get_one(kv, "/m·[9,9]")));

    // 未声明容器直接写坐标成员 → **拒绝**：memhead 不存在则禁止写 memitem。
    // 这条规则两侧同源——kvspace（本处）与 runtime 的 kvlangBuiltinCheckMemhead；
    // 曾经只有 redis 后端会兜底自动建容器，fs 不会，同一份代码两后端分叉，故铲掉兜底。
    let e = kv.set(&[KVPair {
        key: "/n·[2,3]".to_string(),
        val: new_int64(&[7]),
        raw: None,
    }]);
    assert!(e.is_err(), "未声明 memhead 的成员写必须被拒: {e:?}");
    assert!(is_none(&get_one(kv, "/n")), "被拒后不应留下容器值");

    // 命名成员仍为裸名，与坐标段字面可分。
    set(kv, "/h", &new_map_langtype("[]char/utf32·int64"));
    set(kv, "/h·x", &new_int64(&[1]));
    set(kv, "/h·[0]", &new_int64(&[2]));
    let names = kv.list("/h·", false, true);
    assert!(names.contains(&"x".to_string()) && names.contains(&"[0]".to_string()));
}

#[test]
fn map_coord_redis() {
    let dsn = std::env::var("KVSPACE_TEST_REDIS_DSN")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    run(&dsn);
}

#[test]
fn map_coord_fs() {
    let dir = std::env::temp_dir().join("kvspace-map-coord-test");
    let _ = std::fs::remove_dir_all(&dir);
    run(&format!("fs://{}", dir.display()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// S3 后端跑**同一套**语义测试 —— 与 redis、fs 用同一个 run()。
/// 默认忽略：需要真桶和 $KVSPACE，且 DSN 前缀必须带 `_test`（见 tests/common/mod.rs）。
#[test]
#[ignore = "需要真桶凭据，cargo test -- --ignored 才跑"]
fn map_coord_s3() {
    let Some(dsn) = common::s3_test_dsn() else { return };
    run(&dsn);
}
