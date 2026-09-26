//! Compiles translations and embeds the icon and version information in
//! Windows executables.

fn main() {
    fastframe_i18n::build::compile_catalogs("assets/i18n");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // See the file for why the @try/@catch must be compiled C.
        println!("cargo:rerun-if-changed=build_support/touch_bar_guard.m");
        cc::Build::new()
            .file("build_support/touch_bar_guard.m")
            .compile("touch-bar-guard");
    }

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=packaging/windows/zapfast.ico");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("packaging/windows/zapfast.ico")
            .set("ProductName", "ZapFast")
            .set("FileDescription", "ZapFast");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=Windows resources not embedded: {error}");
        }
    }
}
