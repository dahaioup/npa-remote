// macOS: scrap 的 Display::scale 引用 BackingScaleFactor 符号，上游 rustdesk 由
// 主程序的 src/platform/macos.mm 提供；插件独立成二进制，这里补编最小实现。

#[cfg(target_os = "macos")]
fn main() {
    println!("cargo:rerun-if-changed=src/platform/macos_backing.mm");
    cc::Build::new()
        .flag("-std=c++17")
        .file("src/platform/macos_backing.mm")
        .compile("npa_macos_backing");
    println!("cargo:rustc-link-lib=framework=AppKit");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
