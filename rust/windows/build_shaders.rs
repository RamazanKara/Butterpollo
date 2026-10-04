//! Compile fixed shaders once when building the Windows host, rather than on
//! the customer's first frame or when a capture device is recreated.
use std::path::Path;

#[cfg(windows)]
pub fn compile(out: &Path) {
    use std::ffi::CString;
    use windows::{
        Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile},
        core::PCSTR,
    };
    println!("cargo:rerun-if-changed=src/shaders/color.hlsl");
    const SOURCE: &str = include_str!("src/shaders/color.hlsl");
    let mut generated = String::from(
        "fn shader_bytecode(entry: &[u8], target: &[u8]) -> Option<&'static [u8]> {\nmatch (entry, target) {\n",
    );
    for (entry, target) in [
        ("vertex", "vs_5_0"),
        ("luma", "ps_5_0"),
        ("chroma", "ps_5_0"),
        ("packed444", "ps_5_0"),
        ("planar444", "ps_5_0"),
        ("pyro_y", "ps_5_0"),
        ("pyro_u", "ps_5_0"),
        ("pyro_v", "ps_5_0"),
        ("luma_cs", "cs_5_0"),
        ("chroma_cs", "cs_5_0"),
    ] {
        let name = CString::new(entry).unwrap();
        let profile = CString::new(target).unwrap();
        unsafe {
            let mut code = None;
            let mut errors = None;
            if let Err(error) = D3DCompile(
                SOURCE.as_ptr().cast(),
                SOURCE.len(),
                PCSTR::null(),
                None,
                None,
                PCSTR(name.as_ptr().cast()),
                PCSTR(profile.as_ptr().cast()),
                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                0,
                &mut code,
                Some(&mut errors),
            ) {
                let detail = errors
                    .map(|blob| {
                        String::from_utf8_lossy(std::slice::from_raw_parts(
                            blob.GetBufferPointer().cast(),
                            blob.GetBufferSize(),
                        ))
                        .into_owned()
                    })
                    .unwrap_or_default();
                panic!("compile {entry}/{target}: {error}: {detail}");
            }
            let code = code.expect("shader compiler returned no bytecode");
            std::fs::write(
                out.join(format!("{entry}.dxbc")),
                std::slice::from_raw_parts(
                    code.GetBufferPointer().cast::<u8>(),
                    code.GetBufferSize(),
                ),
            )
            .expect("write compiled shader");
        }
        generated.push_str(&format!(
            "(b\"{entry}\\0\", b\"{target}\\0\") => Some(include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{entry}.dxbc\"))),\n",
        ));
    }
    generated.push_str("_ => None,\n}\n}\n");
    std::fs::write(out.join("shader_bytecode.rs"), generated).expect("write shader bindings");
}

#[cfg(not(windows))]
pub fn compile(out: &Path) {
    // Non-Windows cross builds keep the existing runtime compiler fallback.
    std::fs::write(
        out.join("shader_bytecode.rs"),
        "fn shader_bytecode(_: &[u8], _: &[u8]) -> Option<&'static [u8]> { None }\n",
    )
    .expect("write cross-build shader fallback");
}
