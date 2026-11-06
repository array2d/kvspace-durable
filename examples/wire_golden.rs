//! XValue wire 金标准（durable 侧）。
//!
//! 与 `kvspace/tests/xvalue_wire_golden.c`、`kvspace-c` 的同名程序产出**同一批
//! 操作**的字节，由 `kvspace/tests/compare_wire_golden.sh` 逐字节比对。
//! 输出格式必须与 C 版逐字一致：`<case-name> <total-len> <hex>`，顺序固定。
//!
//! 只比字节，不解释字节。任何一边"觉得自己是对的"都不算数。
//!
//! **本文件按新 16 字节 head 重推**（`[bodylen:u64][attrslen:u32][layout:u8][padding:3]`），
//! 不拿旧布局改偏移——那会把旧假设（storetype/ptr 两个字段）不知不觉带过来。
//! 「指针」「扩展」「张量」都不再是 head 字段，而是 langtype 前缀/形状。

use kvspace_durable::headlenpow::{reserve, to_wire};

fn emit(name: &str, wire: &[u8]) {
    print!("{name} {} ", wire.len());
    for b in wire {
        print!("{b:02x}");
    }
    println!();
}

fn main() {
    /* none：空 body + 空 langtype（attr 区零字节）→ layout=LENGTH, padding 全零。 */
    emit("none", &to_wire(&[], "").unwrap());

    /* scalar-int64：body = 42i64 LE，langtype "int64"。 */
    emit("scalar-int64", &to_wire(&42i64.to_le_bytes(), "int64").unwrap());

    /* scalar-bool：body = [1]，langtype "bool"。 */
    emit("scalar-bool", &to_wire(&[1u8], "bool").unwrap());

    /* slack-utf8：body = "你😀" 的 UTF-8，langtype "[]char/utf8"。 */
    emit(
        "slack-utf8",
        &to_wire("你😀".as_bytes(), "[]char/utf8").unwrap(),
    );

    /* slack-no-langtype：body = "abc"，空 langtype（attr 区零字节）。 */
    emit("slack-no-langtype", &to_wire(b"abc", "").unwrap());

    /* tensor：body = 24×0xab，langtype "[2,3]float32"（形状在类型里，不在 head）。 */
    emit("tensor", &to_wire(&[0xabu8; 24], "[2,3]float32").unwrap());

    /* ptr：body = "/lib/f"，langtype 带 `*` 前缀（指针性是类型，不是 head 字段）。 */
    emit("ptr", &to_wire(b"/lib/f", "*[]char/utf8").unwrap());

    /* ext：body = "s3://b/k"，langtype 带 `@` 前缀，其余部分空 → `@`。 */
    emit("ext", &to_wire(b"s3://b/k", "@").unwrap());

    /* reserve：4 字节零 body、langtype "[]u8"。 */
    emit("reserve", &reserve(4, "[]u8").unwrap());

    /* varlen：layout=CAPACITY，容量 8、有效 3 → attr 带 tag 2。 */
    emit(
        "varlen",
        &kvspace_durable::headlenpow::to_wire_varlen(8, 3, "[]char/utf8").unwrap(),
    );
}
