//! Lets the host load libcef from beside itself once installed, and from the unpacked CEF
//! directory when it runs from the build output.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    if let Ok(cef_directory) = std::env::var("DEP_CEF_DLL_WRAPPER_CEF_DIR") {
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,{cef_directory}");
        println!("cargo::rustc-env=TUFO_CEF_BUILD_DIRECTORY={cef_directory}");
    }
}
