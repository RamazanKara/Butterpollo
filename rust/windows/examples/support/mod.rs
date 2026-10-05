//! Internal capture-worker dispatch for probes that create a Capture.
pub fn wgc_worker() -> Option<anyhow::Result<()>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--wgc-worker") {
        return None;
    }
    Some((|| {
        anyhow::ensure!(
            args.len() == 4 && args[2] == "--wgc-parent",
            "invalid WGC worker arguments"
        );
        butterpollo_windows::capture::enable_dpi_awareness();
        butterpollo_windows::capture::run_wgc_worker(&args[1], args[3].parse()?)
    })())
}
