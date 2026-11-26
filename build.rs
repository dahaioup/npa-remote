// macOS: scrap 的 Display::scale 引用 BackingScaleFactor 符号，上游 rustdesk 由
// 主程序的 src/platform/macos.mm 提供；插件独立成二进制，这里补编最小实现。
// 同时编译 Vision OCR 的 Objective-C++ 实现（src/platform/macos_ocr.mm）。

#[cfg(target_os = "macos")]
fn main() {
    println!("cargo:rerun-if-changed=src/platform/macos_backing.mm");
    println!("cargo:rerun-if-changed=src/platform/macos_ocr.mm");
    cc::Build::new()
        .flag("-std=c++17")
        .file("src/platform/macos_backing.mm")
        .compile("npa_macos_backing");
    cc::Build::new()
        .flag("-std=c++17")
        .file("src/platform/macos_ocr.mm")
        .compile("npa_macos_ocr");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=Vision");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
