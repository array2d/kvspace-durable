// 给 cdylib 设 SONAME：libkvspace_durable.so.1，供 kvspace dispatch 前端按名 dlopen。
// macOS 的 ld64 无 -soname 选项（对应 -install_name，默认 @rpath 已可用），跳过。
fn main() {
    if !cfg!(target_os = "macos") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libkvspace_durable.so.1");
    }
}
