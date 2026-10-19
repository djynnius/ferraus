//! On macOS the prebuilt ONNX Runtime (with CoreML) references `___isPlatformVersionAtLeast`
//! from clang's compiler-rt. Debug builds pick it up implicitly, but release builds with LTO
//! and one codegen unit fail to link without it, so link `libclang_rt.osx.a` explicitly when
//! the toolchain provides it.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = std::process::Command::new("clang").arg("-print-runtime-dir").output();
    let Ok(out) = out else { return };
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if dir.is_empty() {
        return;
    }
    if std::path::Path::new(&dir).join("libclang_rt.osx.a").exists() {
        println!("cargo:rustc-link-search=native={dir}");
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}
