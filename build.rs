// 给 cdylib 设 SONAME：libkvspace_durable.so.1，供 kvspace dispatch 前端按名 dlopen。
fn main() {
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libkvspace_durable.so.1");
}
