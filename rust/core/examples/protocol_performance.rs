//! Compare the same wire-compatible FEC workload with the original nanors path.
#[cfg(not(windows))]
fn main() {
    eprintln!("This reference harness currently requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    benchmark::run()
}

#[cfg(windows)]
mod benchmark {
    use anyhow::{Context, Result, bail};
    use std::{
        ffi::c_void,
        hint::black_box,
        os::windows::ffi::OsStrExt,
        path::Path,
        time::{Duration, Instant},
    };

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryExW(path: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> i32;
    }
    type Encode = unsafe extern "C" fn(i32, i32, *mut *mut u8, i32) -> i32;
    struct Reference {
        module: *mut c_void,
        encode: Encode,
    }
    impl Reference {
        fn load(path: &Path) -> Result<Self> {
            let path = path.canonicalize()?;
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                let module = LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), 0x100 | 0x800);
                if module.is_null() {
                    bail!("load reference: {}", std::io::Error::last_os_error());
                }
                let initialize = GetProcAddress(module, c"fec_reference_init".as_ptr().cast());
                let encode = GetProcAddress(module, c"fec_reference_encode".as_ptr().cast());
                if initialize.is_null() || encode.is_null() {
                    FreeLibrary(module);
                    bail!("reference exports missing");
                }
                std::mem::transmute::<*mut c_void, unsafe extern "C" fn()>(initialize)();
                Ok(Self {
                    module,
                    encode: std::mem::transmute::<*mut c_void, Encode>(encode),
                })
            }
        }
        fn encode(
            &self,
            pointers: &mut [*mut u8],
            data: usize,
            parity: usize,
            bytes: usize,
        ) -> Result<()> {
            let code = unsafe {
                (self.encode)(
                    data as i32,
                    parity as i32,
                    pointers.as_mut_ptr(),
                    bytes as i32,
                )
            };
            if code != 0 {
                bail!("original FEC error {code}");
            }
            Ok(())
        }
    }
    impl Drop for Reference {
        fn drop(&mut self) {
            unsafe {
                FreeLibrary(self.module);
            }
        }
    }

    fn measure(mut encode: impl FnMut() -> Result<()>, seconds: f64) -> Result<f64> {
        let duration = Duration::from_secs_f64(seconds);
        let started = Instant::now();
        let mut count = 0;
        loop {
            for _ in 0..16 {
                encode()?;
                count += 1;
            }
            if started.elapsed() >= duration {
                break;
            }
        }
        Ok(started.elapsed().as_secs_f64() * 1_000_000. / f64::from(count))
    }
    fn median(values: &mut [f64]) -> f64 {
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    }
    pub fn run() -> Result<()> {
        let path = std::env::args_os()
            .nth(1)
            .context("pass the pinned C++ nanors reference DLL path")?;
        let reference = Reference::load(Path::new(&path))?;
        let mut cases = Vec::new();
        for (data, parity, bytes) in [(32, 7, 1416), (96, 20, 1416), (192, 39, 1416), (4, 2, 144)] {
            let mut rust: Vec<Vec<u8>> = (0..data + parity)
                .map(|i| {
                    (0..bytes)
                        .map(|b| ((i * 73 + b * 29 + (b >> 3)) & 255) as u8)
                        .collect()
                })
                .collect();
            let mut cpp = rust.clone();
            // Build the pointer view outside timing; the production matrix
            // constructor/encode/release sequence stays inside the reference.
            let mut cpp_pointers: Vec<_> = cpp.iter_mut().map(|s| s.as_mut_ptr()).collect();
            butterpollo_core::packet::cauchy_encode(&mut rust, data, parity)?;
            reference.encode(&mut cpp_pointers, data, parity, bytes)?;
            if rust != cpp {
                bail!("Rust/C++ parity mismatch for {data}+{parity}/{bytes}");
            }
            let mut rust_us = Vec::new();
            let mut cpp_us = Vec::new();
            for round in 0..7 {
                let rust_run = |shards: &mut Vec<Vec<u8>>| {
                    measure(
                        || {
                            butterpollo_core::packet::cauchy_encode(
                                black_box(shards),
                                data,
                                parity,
                            )?;
                            black_box(&*shards);
                            Ok(())
                        },
                        0.5,
                    )
                };
                let mut cpp_run = |shards: &mut Vec<Vec<u8>>| {
                    measure(
                        || {
                            reference.encode(black_box(&mut cpp_pointers), data, parity, bytes)?;
                            black_box(&*shards);
                            Ok(())
                        },
                        0.5,
                    )
                };
                if round % 2 == 0 {
                    rust_us.push(rust_run(&mut rust)?);
                    cpp_us.push(cpp_run(&mut cpp)?);
                } else {
                    cpp_us.push(cpp_run(&mut cpp)?);
                    rust_us.push(rust_run(&mut rust)?);
                }
                if rust != cpp {
                    bail!("timed parity mismatch");
                }
            }
            let rust_median = median(&mut rust_us);
            let cpp_median = median(&mut cpp_us);
            let result = serde_json::json!({"data":data,"parity":parity,"shard_bytes":bytes,"rust_median_us":rust_median,"cpp_median_us":cpp_median,"speedup":cpp_median/rust_median,"identical_parity":true,"rust_samples_us":rust_us,"cpp_samples_us":cpp_us});
            eprintln!("{result}");
            cases.push(result);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"reference_nanors_commit":"19f07b513e924e471cadd141943c1ec4adc8d0e0","reference_wrapper_commit":"f23ee0c9e7857887be7f774de6ac5153500a7e53","scope":"FEC only; original per-video-frame matrix allocation is included; caller shard and pointer-view allocations excluded; interleaved seven 0.5-second runs; small case uses generic Cauchy, not the fixed Moonlight audio matrix","cases":cases})
            )?
        );
        Ok(())
    }
}
