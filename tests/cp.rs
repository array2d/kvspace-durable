// cp.rs — kvspaceCp（单 key）/ kvspaceCpTree（递归子树）语义验证，覆盖 redis 与 fs 两后端。
// 重点：cp_tree 遇 extindex 成员，在 target 侧生成指向同一只读扩展的新 extindex。

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

fn run(dsn: &str, root: &str) {
    let mut kv = conn(dsn);
    let kv: &mut dyn KVSpace = kv.as_mut();
    kv.del_tree(root).unwrap();
    kv.mkindex(&format!("{root}/"), 0).unwrap();

    // 原型 stringkeymap 容器：两个叶成员 + 一个嵌套容器成员。
    set(
        kv,
        &format!("{root}/proto"),
        &new_map_langtype("[]char/utf32·int64"),
    );
    set(kv, &format!("{root}/proto·x"), &new_int64(&[10]));
    set(kv, &format!("{root}/proto·y"), &new_int64(&[20]));
    set(
        kv,
        &format!("{root}/proto·sub"),
        &new_map_langtype("[]char/utf32·int64"),
    );
    set(kv, &format!("{root}/proto·sub·z"), &new_int64(&[30]));

    kv.cp_tree(&format!("{root}/proto"), &format!("{root}/proto"))
        .unwrap();
    assert!(kv
        .cp_tree(&format!("{root}/proto"), &format!("{root}/proto/child"))
        .is_err());
    assert_eq!(get_one(kv, &format!("{root}/proto·x")), new_int64(&[10]));

    // Read-only ext source + extindex member `ov` overlaying `{root}/ext·` at `{root}/proto·ov/`.
    set(
        kv,
        &format!("{root}/ext"),
        &new_map_langtype("[]char/utf32·int64"),
    );
    let ext_loc = format!("{root}/ext·");
    set(
        kv,
        &ext_loc,
        &XValue::Opaque(Opaque {
            kind: "lib".into(),
            body: vec![],
            array_len: 1,
        }),
    );
    set(kv, &format!("{root}/ext·a"), &new_int64(&[99]));
    kv.ext_index(&format!("{root}/proto·ov/"), &ext_loc)
        .unwrap();

    // ── cp：单 key 拷贝，不带成员 ──
    kv.cp(&format!("{root}/proto·x"), &format!("{root}/px"))
        .unwrap();
    assert_eq!(
        get_one(kv, &format!("{root}/px")),
        new_int64(&[10]),
        "cp single key"
    );

    // ── cp_tree：递归子树拷贝 ──
    kv.cp_tree(&format!("{root}/proto"), &format!("{root}/inst"))
        .unwrap();
    assert_eq!(
        get_one(kv, &format!("{root}/inst·x")),
        new_int64(&[10]),
        "cp_tree leaf member x"
    );
    assert_eq!(
        get_one(kv, &format!("{root}/inst·y")),
        new_int64(&[20]),
        "cp_tree leaf member y"
    );
    assert_eq!(
        get_one(kv, &format!("{root}/inst·sub·z")),
        new_int64(&[30]),
        "cp_tree 嵌套成员 z"
    );
    match get_one(kv, &format!("{root}/inst")) {
        XValue::Map(_) => {}
        other => panic!("cp_tree 根值 kind 丢失: {:?}", other),
    }

    // 成员表完整（含嵌套与 extindex 成员）。
    let mut m = kv.list(&format!("{root}/inst·"), false, true);
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
        kv.list(&format!("{root}/"), false, true)
            .contains(&"inst".to_string()),
        "cp_tree 根登记父 index"
    );

    // ── 关键：extindex 成员在 target 侧成为新 extindex，overlay 读通 ──
    assert_eq!(
        kv.get(&format!("{root}/inst·ov/"), &["a".into()], true)
            .remove(0),
        new_int64(&[99]),
        "cp_tree 的 extindex 成员 overlay 读通"
    );
    match kv
        .get(&format!("{root}/inst·"), &["ov/".into()], true)
        .remove(0)
    {
        XValue::Ext(e) => assert_eq!(
            (e.langtype.as_str(), e.locator.as_str()),
            ("lib", ext_loc.as_str())
        ),
        other => panic!("target 侧应为 extindex: {:?}", other),
    }

    kv.cp_list(&format!("{root}/proto"), &format!("{root}/shallow"))
        .unwrap();
    assert_eq!(
        kv.get(&format!("{root}/shallow·ov/"), &["a".into()], true)
            .remove(0),
        new_int64(&[99])
    );

    // 改动源 extindex 目标，target 因共享同一只读扩展而同步可见（非深拷贝底层数据）。
    set(kv, &format!("{root}/ext·a"), &new_int64(&[123]));
    assert_eq!(
        kv.get(&format!("{root}/inst·ov/"), &["a".into()], true)
            .remove(0),
        new_int64(&[123]),
        "extindex 共享同一只读扩展"
    );

    kv.del_tree(root).unwrap();
}

#[test]
fn cp_redis() {
    let Some(dsn) = common::redis_test_dsn("cp") else {
        return;
    };
    run(&dsn, &format!("/kvspace-cp-test-{}", std::process::id()));
}

#[test]
fn cp_fs() {
    let dir = std::env::temp_dir().join(format!("kvspace-cp-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    run(&format!("fs://{}", dir.display()), "/r");
    let _ = std::fs::remove_dir_all(&dir);
}

/// S3 后端跑**同一套**语义测试 —— 与 redis、fs 用同一个 run()。
/// 默认忽略：需要真桶和 $KVSPACE，且 DSN 前缀必须带 `_test`（见 tests/common/mod.rs）。
#[test]
#[ignore = "需要真桶凭据，cargo test -- --ignored 才跑"]
fn cp_s3() {
    let Some(dsn) = common::s3_test_dsn() else { return };
    run(&dsn, &format!("/kvspace-cp-test-{}", std::process::id()));
}
