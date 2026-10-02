//! Read-only display maintenance costs; excludes capture, encoding and delivery.
#[cfg(not(windows))]
fn main() {
    eprintln!("This probe requires Windows.");
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use butterpollo_windows::{
        capture::{ComGuard, Device},
        timing::Timer,
    };
    use std::time::{Duration, Instant};
    let seconds: u64 = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "20".into())
        .parse()?;
    anyhow::ensure!(
        (1..=300).contains(&seconds),
        "duration must be 1..300 seconds"
    );
    let _com = ComGuard::new()?;
    let device = Device::new("")?;
    let timer = Timer::new()?;
    let mut monitors = vec![];
    let mut metadata = vec![];
    for _ in 0..seconds {
        let due = Instant::now() + Duration::from_secs(1);
        let start = Instant::now();
        let current = butterpollo_windows::display::monitors()?;
        monitors.push(start.elapsed().as_secs_f64() * 1000.);
        anyhow::ensure!(!current.is_empty(), "no active monitors");
        let start = Instant::now();
        let _ = device.hdr_metadata();
        metadata.push(start.elapsed().as_secs_f64() * 1000.);
        timer.until(due);
    }
    fn distribution(mut values: Vec<f64>) -> serde_json::Value {
        values.sort_by(f64::total_cmp);
        let count = values.len();
        serde_json::json!({"samples":count,"mean_ms":values.iter().sum::<f64>()/count as f64,
            "p95_ms":values[(count-1)*95/100],"max_ms":values[count-1],"values_ms":values})
    }
    println!(
        "{}",
        serde_json::json!({
            "scope":"read-only display maintenance; excludes capture, encoding, network and decoding",
            "display":device.display,"monitors":distribution(monitors),"hdr_metadata":distribution(metadata)
        })
    );
    Ok(())
}
