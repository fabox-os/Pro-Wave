fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let supported = (os == "macos" && arch == "aarch64") || (os == "windows" && arch == "x86_64");
    println!("cargo:rustc-check-cfg=cfg(plugin_host)");
    if supported {
        println!("cargo:rustc-cfg=plugin_host");
    }
}
