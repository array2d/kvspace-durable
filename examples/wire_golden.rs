//! XValue wire 金标准（durable 侧）。
//!
//! 与 `kvspace/tests/xvalue_wire_golden.c`、`kvspace-c` 的同名程序产出**同一批
//! 操作**的字节，由 `kvspace/tests/compare_wire_golden.sh` 逐字节比对。
//! 输出格式必须与 C 版逐字一致：`<case-name> <total-len> <hex>`，顺序固定。
//!
//! 只比字节，不解释字节。任何一边"觉得自己是对的"都不算数。

use kvspace_durable::headlenpow::{reserve, to_wire};
use kvspace_durable::xvalue::{REF_EXT, REF_INLINE, REF_PTR, ST_EXT, ST_LARGE, ST_SHORT, ST_SLACK};

fn emit(name: &str, wire: &[u8]) {
    print!("{name} {} ", wire.len());
    for b in wire {
        print!("{b:02x}");
    }
    println!();
}

fn main() {
    /* none：ST_SHORT + INLINE + 空 body + 空 attr。 */
    emit("none", &to_wire(ST_SHORT, REF_INLINE, &[], "").unwrap());

    /* scalar-int64：body = 42i64 LE，langtype "int64"。 */
    emit(
        "scalar-int64",
        &to_wire(ST_SHORT, REF_INLINE, &42i64.to_le_bytes(), "int64").unwrap(),
    );

    /* scalar-bool：body = [1]，langtype "bool"。 */
    emit(
        "scalar-bool",
        &to_wire(ST_SHORT, REF_INLINE, &[1u8], "bool").unwrap(),
    );

    /* slack-utf8：body = "你😀" 的 UTF-8，langtype "[]char/utf8"。 */
    emit(
        "slack-utf8",
        &to_wire(ST_SLACK, REF_INLINE, "你😀".as_bytes(), "[]char/utf8").unwrap(),
    );

    /* slack-no-langtype：body = "abc"，空 langtype（attr 区零字节）。 */
    emit(
        "slack-no-langtype",
        &to_wire(ST_SLACK, REF_INLINE, b"abc", "").unwrap(),
    );

    /* tensor：body = 24×0xab，langtype "[2,3]float32"。 */
    emit(
        "tensor",
        &to_wire(ST_LARGE, REF_INLINE, &[0xabu8; 24], "[2,3]float32").unwrap(),
    );

    /* ptr：body = "/lib/f"，langtype "[]char/utf8"。 */
    emit(
        "ptr",
        &to_wire(ST_SLACK, REF_PTR, b"/lib/f", "[]char/utf8").unwrap(),
    );

    /* ext：body = "s3://b/k"，空 langtype。 */
    emit("ext", &to_wire(ST_EXT, REF_EXT, b"s3://b/k", "").unwrap());

    /* reserve：ST_SLACK、4 字节零 body、langtype "[]u8"。 */
    emit(
        "reserve",
        &reserve(ST_SLACK, REF_INLINE, 4, "[]u8").unwrap(),
    );
}
