//! How late a high-resolution waitable timer wakes for short waits.
use butterpollo_windows::timing::{Signal, Timer};
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    // Pass --streaming to measure with the stream's timer resolution and priority.
    let _scope = std::env::args()
        .any(|a| a == "--streaming")
        .then(butterpollo_windows::timing::StreamingScope::enter);
    let timer = Timer::new()?;
    let signal = Signal::new()?;
    for wait_us in [100u64, 250, 500, 1000, 2000] {
        for (name, mode) in [("until_or_signal", 0), ("until", 1), ("until_precise", 2)] {
            let mut late = Vec::with_capacity(400);
            for _ in 0..400 {
                let deadline = Instant::now() + Duration::from_micros(wait_us);
                match mode {
                    0 => {
                        timer.until_or_signal(deadline, &signal)?;
                    }
                    1 => timer.until(deadline),
                    _ => timer.until_precise(deadline),
                }
                late.push(
                    Instant::now()
                        .saturating_duration_since(deadline)
                        .as_micros() as u64,
                );
            }
            late.sort_unstable();
            let mean = late.iter().sum::<u64>() as f64 / late.len() as f64;
            println!(
                "{name:16} wait {wait_us:5} us: late mean {mean:7.1} p50 {:5} p95 {:5} max {:5} us",
                late[late.len() / 2],
                late[late.len() * 95 / 100],
                late[late.len() - 1]
            );
        }
    }
    Ok(())
}
