//! Compare the old capture timeout with the interruptible media timer.
use anyhow::Result;
use butterpollo_windows::timing::{Signal, Timer};
use serde_json::json;
use std::{
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let timer = Timer::new()?;
    let signal = Signal::new()?;
    let lock = Mutex::new(());
    let condition = Condvar::new();
    for micros in [250, 500, 16_667] {
        for method in ["condition_variable", "capture_event_and_timer"] {
            let mut samples = Vec::with_capacity(40);
            for _ in 0..40 {
                let duration = Duration::from_micros(micros);
                let start = Instant::now();
                if method == "condition_variable" {
                    let _guard = condition
                        .wait_timeout(lock.lock().unwrap(), duration)
                        .unwrap();
                } else {
                    timer.until_or_signal(start + duration, &signal)?;
                }
                samples.push(start.elapsed().as_secs_f64() * 1000.);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{}",
                json!({"method":method,"requested_us":micros,"samples":samples.len(),"mean_ms":samples.iter().sum::<f64>()/samples.len() as f64,"p95_ms":samples[samples.len()*95/100],"max_ms":samples.last()})
            );
        }
    }
    Ok(())
}
