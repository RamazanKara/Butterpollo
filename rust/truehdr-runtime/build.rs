fn main() {
    if !std::env::var("TARGET").unwrap().ends_with("windows-msvc") {
        panic!("NVIDIA's vendor library requires --target x86_64-pc-windows-msvc");
    }
    println!("cargo:rerun-if-env-changed=NV_RTX_VIDEO_SDK");
    let sdk = std::path::PathBuf::from(
        std::env::var("NV_RTX_VIDEO_SDK").expect("set NV_RTX_VIDEO_SDK to RTX Video SDK 1.1.0"),
    );
    assert!(
        sdk.join("include/nvsdk_ngx_defs_truehdr.h").exists(),
        "SDK TrueHDR headers are missing"
    );
    println!(
        "cargo:rustc-link-search=native={}",
        sdk.join("lib/Windows/x64").display()
    );
    println!("cargo:rustc-link-lib=static=nvsdk_ngx_d");
    println!("cargo:rustc-link-lib=advapi32");
    println!("cargo:rustc-link-lib=user32");
}
