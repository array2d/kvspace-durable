// map_coord.rs — strkeymapindex 坐标 key 布局（docs/strkeymapindex-ndarray.md）的端到端验证。
// 覆盖 redis 与 fs 两后端。

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

    // 显式创建 [2,3]strkeymapindex 目录。成员目录以 `·` 结尾，故标记在 /m·。
    set(kv, "/m·", &new_map_index(&[], &[2, 3]));

    // 乱序写坐标成员。
    set(kv, "/m·[1,2]", &new_float32(&[6.28]));
    set(kv, "/m·[0,1]", &new_float32(&[3.14]));
    set(kv, "/m·[0,0]", &new_float32(&[1.0]));

    // 目录标记 round-trip：kind + dims 保留。
    match get_one(kv, "/m·") {
        XValue::Map(m) => {
            assert_eq!(m.dims, vec![2, 3], "dims round-trip");
            assert_eq!(m.childs, vec!["[0,0]", "[0,1]", "[1,2]"], "childs");
        }
        other => panic!("map 目录 kind 丢失: {:?}", other),
    }

    // list 按 row-major 数值升序（先比 s0 再比 s1）。
    let names = kv.list("/m·", false, true);
    assert_eq!(names, vec!["[0,0]", "[0,1]", "[1,2]"], "list 顺序");

    // 坐标成员读回，缺席坐标读 None。
    assert_eq!(get_one(kv, "/m·[1,2]"), new_float32(&[6.28]));
    assert!(is_none(&get_one(kv, "/m·[9,9]")));

    // 未显式创建目录，直接写坐标成员 → 自动兜底为 map（维度由坐标推导）。
    set(kv, "/n·[2,3]", &new_int64(&[7]));
    match get_one(kv, "/n·") {
        XValue::Map(m) => assert_eq!(m.dims, vec![3, 4], "auto map dims"),
        other => panic!("自动兜底应为 map: {:?}", other),
    }

    // objindex 成员仍为裸名，与坐标段字面可分。
    set(kv, "/h/", &new_obj_index(&[]));
    set(kv, "/h·x", &new_int64(&[1]));
    set(kv, "/h·[0]", &new_int64(&[2]));
    let names = kv.list("/h·", false, true);
    assert!(names.contains(&"x".to_string()) && names.contains(&"[0]".to_string()));
}

#[test]
fn map_coord_redis() {
    run("redis://127.0.0.1:6379");
}

#[test]
fn map_coord_fs() {
    let dir = std::env::temp_dir().join("kvspace-map-coord-test");
    let _ = std::fs::remove_dir_all(&dir);
    run(&format!("fs://{}", dir.display()));
    let _ = std::fs::remove_dir_all(&dir);
}
