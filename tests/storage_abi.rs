//! 第二阶段存储层 ABI（`kvspace/include/kvspace/storage.h`）在 durable 后端的行为。
//!
//! 覆盖 P2.3–P2.9 的可执行判据：listlen 的 N/M、list 的四条语义、generation 三态、
//! free 三态、makeindex 不规定 body、以及 resolve/realloc 的诚实失败。
//!
//! 名字顺序**未定义**（后端顺序）：所有关于集合的断言只比排序后的结果。

use std::ffi::CString;

use kvspace_durable::ffi::*;
use kvspace_durable::storage_abi::*;

// ── 句柄与辅助 ───────────────────────────────────────────────────────

fn connect(label: &str) -> *mut Handle {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "kvspace-durable-sabi-{label}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let dsn = CString::new(format!("fs://{}/store", dir.display())).unwrap();
    let h = unsafe { kvspaceConnect(dsn.as_ptr()) };
    assert!(!h.is_null(), "connect must succeed");
    h
}

fn ckey(key: &str) -> CString {
    CString::new(key).unwrap()
}

/// 拼一条合法 wire：body = value，langtype 固定 `[]char/utf8`。
fn wire_str(value: &str) -> Vec<u8> {
    let lt = b"[]char/utf8";
    let attrslen = (3 + lt.len()) as u16;
    let mut v = Vec::with_capacity(16 + value.len() + attrslen as usize);
    v.extend_from_slice(&(value.len() as u64).to_le_bytes());
    v.extend_from_slice(&attrslen.to_le_bytes());
    v.extend_from_slice(&[0u8; 6]); /* padding */
    v.extend_from_slice(value.as_bytes());
    v.extend_from_slice(&[1, lt.len() as u8, 0]);
    v.extend_from_slice(lt);
    v
}

fn set_str(h: *mut Handle, key: &str, value: &str) {
    let k = ckey(key);
    let v = wire_str(value);
    let mut err = [0i8; 256];
    let rc = unsafe {
        kvspaceSetValue(
            h,
            k.as_ptr(),
            v.as_ptr(),
            v.len() as u32,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    assert_eq!(rc, 0, "set {key}");
}

fn err_str(err: &[i8; 256]) -> String {
    let bytes: Vec<u8> = err
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `kvspaceList` 一次取全部名字。返回 (成功?, 名字, 长度, 个数, generation)。
fn list_all(h: *mut Handle, key: &str) -> (i32, Vec<String>, u32, u32, u64) {
    let k = ckey(key);
    /* 先探长度，再按长度分配——与调用方的正确用法一致。 */
    let mut length = 0u32;
    let mut count = 0u32;
    let mut gen = 0u64;
    let mut err = [0i8; 256];
    let rc = unsafe {
        kvspaceList(
            h,
            k.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut length,
            &mut count,
            &mut gen,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    if rc != 0 {
        return (rc, Vec::new(), length, count, gen);
    }
    let mut buf = vec![0u8; length as usize];
    let rc = unsafe {
        kvspaceList(
            h,
            k.as_ptr(),
            buf.as_mut_ptr() as *mut std::ffi::c_char,
            length,
            &mut length,
            &mut count,
            &mut gen,
            err.as_mut_ptr(),
            err.len() as u32,
        )
    };
    let names = buf
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    (rc, names, length, count, gen)
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn close(h: *mut Handle) {
    unsafe { kvspaceClose(h) };
}

// ── resolve / realloc：诚实失败（UNSUPPORTED，不是 BACKEND） ───────────

#[test]
fn resolve_and_realloc_report_unsupported_not_backend() {
    let h = connect("unsupported");
    let key = ckey("/a");
    let mut err = [0i8; 256];
    let mut off = 0u64;

    let rc = unsafe { kvspaceResolve(h, key.as_ptr(), 0, &mut off, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!(rc, -95, "resolve 必须返回 -ENOTSUP：`后端做不到` 不等于 `后端调用失败`；err={}", err_str(&err));

    let rc = unsafe {
        kvspaceRealloc(h, key.as_ptr(), 8, 0, &mut off, err.as_mut_ptr(), err.len() as u32)
    };
    assert_eq!(rc, -95, "realloc 必须返回 -ENOTSUP；err={}", err_str(&err));
    close(h);
}

// ── deltree：连根删，key 不存在也算成功 ───────────────────────────────

#[test]
fn deltree_removes_the_root_itself() {
    let h = connect("deltree-root");
    let err_free = |rc: i32, e: &[i8; 256]| format!("rc={rc} err={}", err_str(e));
    let mut err = [0i8; 256];

    /* 空索引：deltree 后根也没了 ⇒ listlen 必须是 MISSING，不是 0。 */
    let x = ckey("/x/");
    assert_eq!(unsafe { kvspaceMkindex(h, x.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    let (mut n, mut m) = (0i64, 0u32);
    assert_eq!(
        unsafe { kvspaceListlen(h, x.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) },
        0,
        "空索引必须存在且 N=M=0"
    );
    assert_eq!((n, m), (0, 0));

    assert_eq!(
        unsafe { kvspaceDeltree(h, x.as_ptr(), err.as_mut_ptr(), err.len() as u32) },
        0,
        "deltree {}",
        err_free(0, &err)
    );
    let rc = unsafe { kvspaceListlen(h, x.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!(rc, -2, "连根删后 listlen 必须 -ENOENT（不是 0）；err={}", err_str(&err));

    /* 带子项的索引：子项与根一起消失。 */
    let y = ckey("/y/");
    assert_eq!(unsafe { kvspaceMkindex(h, y.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    set_str(h, "/y/c1", "hi");
    set_str(h, "/y/c2", "yo");
    let rc = unsafe { kvspaceListlen(h, y.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!((rc, n), (0, 2), "两子项");
    assert_eq!(unsafe { kvspaceDeltree(h, y.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    let rc = unsafe { kvspaceListlen(h, y.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!(rc, -2, "deltree 后子树与根都必须消失");

    /* 不存在的 key：视为成功（幂等）。 */
    let ghost = ckey("/ghost/");
    assert_eq!(
        unsafe { kvspaceDeltree(h, ghost.as_ptr(), err.as_mut_ptr(), err.len() as u32) },
        0,
        "删除不存在的 key 必须成功：err={}",
        err_str(&err)
    );
    close(h);
}

// ── free 三态 ─────────────────────────────────────────────────────────

#[test]
fn free_three_states() {
    let h = connect("free");
    let mut err = [0i8; 256];

    /* ① 有值的普通 key：删掉。 */
    set_str(h, "/v", "abc");
    assert_eq!(
        unsafe { kvspaceFree(h, ckey("/v").as_ptr(), err.as_mut_ptr(), err.len() as u32) },
        0,
        "有值 key 必须删成功；err={}",
        err_str(&err)
    );

    /* ② 空索引：连索引节点一起删。 */
    let a = ckey("/a/");
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    assert_eq!(
        unsafe { kvspaceFree(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) },
        0,
        "空索引必须删成功；err={}",
        err_str(&err)
    );
    let (mut n, mut m) = (0i64, 0u32);
    assert_eq!(
        unsafe { kvspaceListlen(h, a.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) },
        -2,
        "空索引被 free 后必须不存在"
    );

    /* ③ 非空索引：HAS_CHILDREN，子项一个不动。 */
    let b = ckey("/b/");
    assert_eq!(unsafe { kvspaceMkindex(h, b.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    set_str(h, "/b/one", "1");
    let rc = unsafe { kvspaceFree(h, b.as_ptr(), err.as_mut_ptr(), err.len() as u32) };
    assert_eq!(rc, -39, "非空索引必须 -ENOTEMPTY；err={}", err_str(&err));
    let (_, names, _, count, _) = list_all(h, "/b/");
    assert_eq!(count, 1, "被拒的 free 不得动子项");
    assert_eq!(sorted(names), vec!["one".to_string()]);

    close(h);
}

// ── makeindex 不规定 body ────────────────────────────────────────────

#[test]
fn makeindex_creates_an_empty_value_and_keeps_existing_body() {
    let h = connect("makeindex");
    let mut err = [0i8; 256];
    let a = ckey("/m/");
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);

    /* 新 ABI 不写 langtype：建出来的就是规范 None（空 body + 空 attr）。 */
    let mut out: *mut u8 = std::ptr::null_mut();
    let mut out_len = 0u32;
    let rc = unsafe { kvspaceGet(h, a.as_ptr(), 1, &mut out, &mut out_len) };
    assert_eq!(rc, 0);
    let wire = unsafe { std::slice::from_raw_parts(out, out_len as usize) };
    assert_eq!(u64::from_le_bytes(wire[0..8].try_into().unwrap()), 0, "body 必须为空");
    assert_eq!(u16::from_le_bytes(wire[8..10].try_into().unwrap()), 0, "attr 必须为空——不塞 langtype \"lib\"");

    /* 幂等：已存在时不覆盖上层写进去的 body。 */
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    assert_eq!(u64::from_le_bytes(wire[0..8].try_into().unwrap()), 0);

    /* 非索引 key 必须拒绝。 */
    let bad = ckey("/notindex");
    assert_eq!(
        unsafe { kvspaceMkindex(h, bad.as_ptr(), err.as_mut_ptr(), err.len() as u32) },
        -22,
        "非索引 key 必须 -EINVAL"
    );
    close(h);
}

// ── listlen 的 N / M（P2.3） ─────────────────────────────────────────

#[test]
fn listlen_max_name_length_matches_list_and_includes_dir_slash() {
    let h = connect("listlen");
    let mut err = [0i8; 256];
    let a = ckey("/a/");
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    set_str(h, "/a/x", "1");
    set_str(h, "/a/yyy", "2");
    set_str(h, "/a/[10]", "3");
    /* 子索引：名字带尾 `/`，M 必须按带斜杠的那个长度算。 */
    let sub = ckey("/a/sub/");
    assert_eq!(unsafe { kvspaceMkindex(h, sub.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);

    let (mut n, mut m) = (0i64, 0u32);
    let rc = unsafe { kvspaceListlen(h, a.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!(rc, 0, "err={}", err_str(&err));
    assert_eq!(n, 4, "子项数");

    let (rc, names, length, count, _) = list_all(h, "/a/");
    assert_eq!(rc, 0);
    assert_eq!(count, 4);
    let actual_max = names.iter().map(String::len).max().unwrap();
    assert_eq!(
        m as usize, actual_max,
        "M 必须等于 list 输出的最长名字节数（含目录项尾 /）：listlen M={m} 实际 M={actual_max} names={names:?}"
    );
    assert!(names.contains(&"sub/".to_string()), "子索引名字必须带尾 /：names={names:?}");
    let expect_total: usize = names.iter().map(|s| s.len() + 1).sum();
    assert_eq!(length as usize, expect_total, "out_length 必须等于 Σ(len+1)");

    /* 非索引与非存在 key 的两种拒绝必须分开：先建一个普通值 key 才有「非索引」可言。 */
    set_str(h, "/plain", "v");
    let plain = ckey("/plain");
    let rc = unsafe { kvspaceListlen(h, plain.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!(rc, -20, "普通值 key 必须 -ENOTDIR；err={}", err_str(&err));
    let ghost = ckey("/ghost/");
    let rc = unsafe { kvspaceListlen(h, ghost.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!(rc, -2, "不存在的索引必须 -ENOENT；err={}", err_str(&err));
    close(h);
}

// ── list 的四个语义（P2.5） ──────────────────────────────────────────

#[test]
fn list_null_buffer_probe_capacity_and_format() {
    let h = connect("list-semantics");
    let mut err = [0i8; 256];
    let a = ckey("/a/");
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    set_str(h, "/a/one", "1");
    set_str(h, "/a/two", "2");

    /* a) out_names = NULL：只回报，不写缓冲，返回 0。 */
    let (mut length, mut count, mut gen) = (0u32, 0u32, 0u64);
    let rc = unsafe {
        kvspaceList(
            h, a.as_ptr(), std::ptr::null_mut(), 0,
            &mut length, &mut count, &mut gen, err.as_mut_ptr(), err.len() as u32,
        )
    };
    assert_eq!(rc, 0, "NULL 缓冲必须成功；err={}", err_str(&err));
    assert_eq!(count, 2);
    assert_eq!(length as usize, "one".len() + 1 + "two".len() + 1, "长度含所有 NUL");
    assert!(gen > 0, "generation 必须非 0");

    /* b) 容量不足：CAPACITY，且仍写出所需长度与个数，不截断。 */
    let mut small = vec![0xAAu8; 3];
    let (mut l2, mut c2, mut g2) = (0u32, 0u32, 0u64);
    let rc = unsafe {
        kvspaceList(
            h, a.as_ptr(), small.as_mut_ptr() as *mut std::ffi::c_char, 3,
            &mut l2, &mut c2, &mut g2, err.as_mut_ptr(), err.len() as u32,
        )
    };
    assert_eq!(rc, -28, "容量不足必须 -ENOSPC；err={}", err_str(&err));
    assert_eq!(l2, length, "容量不足时仍须写出所需长度");
    assert_eq!(c2, count, "容量不足时仍须写出个数");
    assert!(small.iter().all(|b| *b == 0xAA), "容量不足时不得写半截缓冲：{small:?}");

    /* d) 格式：NUL 分隔，无多余尾 NUL。 */
    let (rc, names, length, _, _) = list_all(h, "/a/");
    assert_eq!(rc, 0);
    assert_eq!(length as usize, names.iter().map(|s| s.len() + 1).sum::<usize>());
    assert_eq!(sorted(names), vec!["one".to_string(), "two".to_string()]);
    close(h);
}

// ── generation 三态（P2.6） ──────────────────────────────────────────

#[test]
fn generation_advances_on_insert_and_delete_but_not_on_value_write() {
    let h = connect("generation");
    let mut err = [0i8; 256];
    let a = ckey("/g/");
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);

    let gen_now = |h: *mut Handle, err: &mut [i8; 256]| -> u64 {
        let mut length = 0u32;
        let mut count = 0u32;
        let mut gen = 0u64;
        let rc = unsafe {
            kvspaceList(
                h, a.as_ptr(), std::ptr::null_mut(), 0,
                &mut length, &mut count, &mut gen, err.as_mut_ptr(), err.len() as u32,
            )
        };
        assert_eq!(rc, 0);
        gen
    };

    let g0 = gen_now(h, &mut err);
    /* 插入子项 ⇒ 前进。 */
    set_str(h, "/g/one", "1");
    let g1 = gen_now(h, &mut err);
    assert!(g1 > g0, "插入子项后 generation 必须前进：{g0} → {g1}");

    /* 只改子项的值、集合不变 ⇒ 不前进。关键判据：body 变化不该丢上层排序缓存。 */
    set_str(h, "/g/one", "changed");
    let g2 = gen_now(h, &mut err);
    assert_eq!(g2, g1, "只改值不前进：集合没变，generation 必须停住");

    /* 再插入 ⇒ 前进。 */
    set_str(h, "/g/two", "2");
    let g3 = gen_now(h, &mut err);
    assert!(g3 > g2, "第二次插入后必须前进：{g2} → {g3}");

    /* 删除 ⇒ 前进（即使删完变空）。 */
    unsafe { kvspaceFree(h, ckey("/g/two").as_ptr(), err.as_mut_ptr(), err.len() as u32) };
    let g4 = gen_now(h, &mut err);
    assert!(g4 > g3, "删除子项后必须前进：{g3} → {g4}");
    close(h);
}

// ── cp / cpdir ───────────────────────────────────────────────────────

#[test]
fn cp_copies_one_key_and_cpdir_copies_the_subtree() {
    let h = connect("cp");
    let mut err = [0i8; 256];

    /* 单键复制。 */
    set_str(h, "/src", "value");
    assert_eq!(
        unsafe {
            kvspaceCp(h, ckey("/src").as_ptr(), ckey("/dst").as_ptr(), err.as_mut_ptr(), err.len() as u32)
        },
        0,
        "cp err={}",
        err_str(&err)
    );
    let (rc, names, ..) = list_all(h, "/");
    assert_eq!(rc, 0);
    let n = sorted(names);
    assert!(n.contains(&"src".to_string()) && n.contains(&"dst".to_string()), "{n:?}");

    /* 源不存在必须 MISSING，不是 BACKEND。 */
    let rc = unsafe {
        kvspaceCp(h, ckey("/nope").as_ptr(), ckey("/x").as_ptr(), err.as_mut_ptr(), err.len() as u32)
    };
    assert_eq!(rc, -2, "源不存在必须 -ENOENT；err={}", err_str(&err));

    /* 递归复制子树：根 + 子项全过去。 */
    let a = ckey("/tree/");
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    set_str(h, "/tree/k1", "1");
    set_str(h, "/tree/sub", "2");
    assert_eq!(
        unsafe {
            kvspaceCpdir(
                h, ckey("/tree/").as_ptr(), ckey("/copy/").as_ptr(),
                err.as_mut_ptr(), err.len() as u32,
            )
        },
        0,
        "cpdir err={}",
        err_str(&err)
    );
    let (rc, names, _, count, _) = list_all(h, "/copy/");
    assert_eq!(rc, 0, "复制后的索引必须存在");
    assert_eq!(count, 2);
    assert_eq!(sorted(names), vec!["k1".to_string(), "sub".to_string()]);
    close(h);
}

// ── 内存后端：判据不依赖具体存储 ─────────────────────────────────────

mod memory_backend {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    use kvspace_durable::{KVStore, KVPair, KVSpace, XValue};

    #[derive(Default, Clone)]
    pub struct MemoryStore(Rc<RefCell<HashMap<String, Vec<u8>>>>);

    impl KVStore for MemoryStore {
        fn get(&self, key: &str) -> Option<Vec<u8>> {
            self.0.borrow().get(key).cloned()
        }
        fn set(&self, key: &str, value: &[u8]) {
            self.0.borrow_mut().insert(key.to_string(), value.to_vec());
        }
        fn del(&self, keys: &[&str]) {
            let mut v = self.0.borrow_mut();
            for k in keys {
                v.remove(*k);
            }
        }
        fn scan_keys(&self, prefix: &str) -> Vec<String> {
            self.0
                .borrow()
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect()
        }
        fn flush(&self) {}
    }

    pub fn backend() -> kvspace_durable::Backend<MemoryStore> {
        kvspace_durable::Backend::new(MemoryStore::default())
    }

    pub fn set_str(kv: &mut kvspace_durable::Backend<MemoryStore>, key: &str, value: &str) {
        let raw = kvspace_durable::xvalue::xvalue_to_wire("[]char/utf8", value.as_bytes(), 0);
        kv.set(&[KVPair {
            key: key.into(),
            val: XValue::None,
            raw: Some(raw),
        }])
        .unwrap();
    }
}

/// 同一组行为在内存后端上复现：N/M、空 vs 不存在、deltree 连根删。
/// 判据在这里是 `KVSpace` trait 语义，不是 fs 的文件布局。
#[test]
fn memory_backend_agrees_on_enumeration_and_deltree() {
    use kvspace_durable::KVSpace;

    let mut kv = memory_backend::backend();
    let a = "/m/";
    kv.mkindex(a, 0).unwrap();
    memory_backend::set_str(&mut kv, "/m/one", "1");
    memory_backend::set_str(&mut kv, "/m/longer", "2");

    /* list 与 list_len 必须给同一个集合；M 按同一字节串算。 */
    let names = kv.list(a, false, true);
    let mut sorted_names = names.clone();
    sorted_names.sort();
    assert_eq!(sorted_names, vec!["longer".to_string(), "one".to_string()]);
    assert_eq!(kv.list_len(a, false, true) as usize, names.len());
    assert_eq!(
        names.iter().map(String::len).max().unwrap(),
        "longer".len(),
        "M 的判据与 fs 后端一致"
    );

    /* deltree 连根删：trait 层就该如此，不是 fs 特有的文件操作。 */
    kv.del_tree(a).unwrap();
    assert!(
        kv.list(a, false, true).is_empty(),
        "deltree 后不得再有子项"
    );
    assert!(
        kv.get_raw(a).is_empty(),
        "deltree 后根值也必须消失——`/m/` 不能只剩一个空壳"
    );
}

// ── kvspaceXvalueInProcessAddress：durable 恒 NULL（P2.9b e） ──────────────────────

#[test]
fn database_is_null_because_durable_has_no_mapped_region() {
    let h = connect("database");
    /* 没有基址就必须给 NULL，不能给一个「看着能用」的假指针：
       调用方判空后回退旧 ABI；假指针会让它算出一个指向垃圾的地址。 */
    assert!(
        unsafe { kvspaceXvalueInProcessAddress(h) }.is_null(),
        "durable 没有映射区，kvspaceXvalueInProcessAddress 必须返回 NULL"
    );
    /* 两次调用都为 NULL（这里不承诺稳定性，只承诺空）。 */
    assert!(unsafe { kvspaceXvalueInProcessAddress(h) }.is_null());
    close(h);
}

// ── 三态顺序：先存在、后形态（P2.4） ─────────────────────────────────

#[test]
fn existence_is_checked_before_index_shape() {
    let h = connect("three-states");
    let mut err = [0i8; 256];
    let (mut n, mut m) = (0i64, 0u32);

    let probe = |h: *mut Handle, key: &str, err: &mut [i8; 256]| -> i32 {
        let k = ckey(key);
        unsafe { kvspaceListlen(h, k.as_ptr(), &mut 0i64, &mut 0u32, err.as_mut_ptr(), err.len() as u32) }
    };

    /* ① 不存在：**尾带不带 `/` 必须同答案**，都是 MISSING。
       曾经先判形态，于是 `/never` 报 NOT_INDEX —— 一个根本不存在的 key
       被说成「不是索引」，调用方会以为它在。 */
    assert_eq!(probe(h, "/never", &mut err), -2, "不存在的 /never 必须 MISSING；err={}", err_str(&err));
    assert_eq!(probe(h, "/never/", &mut err), -2, "不存在的 /never/ 必须 MISSING");
    assert_eq!(probe(h, "/never\u{b7}x", &mut err), -2, "不存在的成员路径也必须 MISSING");
    assert_eq!(probe(h, "/a/b/c", &mut err), -2, "不存在的深层路径也必须 MISSING");

    /* ② 存在但非索引：NOT_INDEX。 */
    set_str(h, "/plain", "v");
    assert_eq!(probe(h, "/plain", &mut err), -20, "存在非索引必须 NOT_INDEX；err={}", err_str(&err));

    /* ③ 存在且是空索引：OK / 0 / 0。 */
    let a = ckey("/empty/");
    assert_eq!(unsafe { kvspaceMkindex(h, a.as_ptr(), err.as_mut_ptr(), err.len() as u32) }, 0);
    let rc = unsafe { kvspaceListlen(h, a.as_ptr(), &mut n, &mut m, err.as_mut_ptr(), err.len() as u32) };
    assert_eq!((rc, n, m), (0, 0, 0), "空索引必须 OK/0/0");

    /* ④ 只有子项、没有根值：算存在（它有后代），不是 MISSING。 */
    set_str(h, "/only/c1", "x");
    assert_eq!(probe(h, "/only", &mut err), -20, "有后代但无根值 ⇒ 存在 ⇒ NOT_INDEX");
    assert_eq!(probe(h, "/only/", &mut err), 0, "有后代 ⇒ 索引存在");

    /* list 与 listlen 必须同一套顺序（不许两个入口各有一套）。 */
    let k = ckey("/never2");
    let mut length = 0u32;
    let mut count = 0u32;
    let mut gen = 0u64;
    let rc = unsafe {
        kvspaceList(
            h, k.as_ptr(), std::ptr::null_mut(), 0,
            &mut length, &mut count, &mut gen, err.as_mut_ptr(), err.len() as u32,
        )
    };
    assert_eq!(rc, -2, "list 对不存在的 key 也必须 MISSING；err={}", err_str(&err));

    close(h);
}
