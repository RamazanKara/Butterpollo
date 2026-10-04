//! Probe: does adding a second virtual display break duplication of the first,
//! and does re-creating a display restore it? Run as LocalSystem in the
//! signed-in session (the virtual display driver is only open to SYSTEM).
use anyhow::{Context, Result};
use butterpollo_windows::{capture::Duplication, display::VirtualDisplay};
use std::time::{Duration, Instant};

struct Target {
    label: &'static str,
    id: &'static str,
    mode: (u32, u32, u32),
    display: Option<VirtualDisplay>,
    dup: Option<Duplication>,
    frames: u32,
    lost: u32,
}
impl Target {
    fn create(&mut self) -> Result<()> {
        self.dup = None;
        self.display = None;
        let (w, h, f) = self.mode;
        self.display = Some(VirtualDisplay::create_rate(self.id, w, h, f).context(self.label)?);
        Ok(())
    }
    fn name(&self) -> String {
        self.display
            .as_ref()
            .map_or_else(String::new, |d| d.name.clone())
    }
    fn poll(&mut self) {
        let Some(display) = self.display.as_mut() else {
            return;
        };
        let _ = display.feed();
        let name = display.name.clone();
        if self.dup.is_none() {
            self.dup = Duplication::new_format(&name, false).ok();
        }
        if let Some(dup) = self.dup.as_mut() {
            match dup.next_gpu() {
                Ok(Some(_)) => self.frames += 1,
                Ok(None) => {}
                Err(_) => {
                    self.lost += 1;
                    self.dup = None;
                }
            }
        }
    }
}
fn phase(name: &str, targets: &mut [&mut Target], seconds: f64) {
    for t in targets.iter_mut() {
        t.frames = 0;
        t.lost = 0;
    }
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline {
        for t in targets.iter_mut() {
            t.poll();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let report: Vec<_> = targets
        .iter()
        .filter(|t| t.display.is_some())
        .map(|t| {
            format!(
                "{}({}) frames={} lost={}",
                t.label,
                t.name(),
                t.frames,
                t.lost
            )
        })
        .collect();
    println!("PHASE {name}: {}", report.join(" | "));
}
fn main() -> Result<()> {
    butterpollo_windows::capture::enable_dpi_awareness();
    let _com = butterpollo_windows::capture::ComGuard::new()?;
    let mut a = Target {
        label: "A",
        id: "multi-ddx-probe-a",
        mode: (1280, 720, 60_000),
        display: None,
        dup: None,
        frames: 0,
        lost: 0,
    };
    let mut b = Target {
        label: "B",
        id: "multi-ddx-probe-b",
        mode: (1920, 1080, 120_000),
        display: None,
        dup: None,
        frames: 0,
        lost: 0,
    };
    a.create()?;
    std::thread::sleep(Duration::from_secs(2));
    phase("A_alone", &mut [&mut a, &mut b], 3.);
    b.create()?;
    std::thread::sleep(Duration::from_secs(2));
    phase("B_added", &mut [&mut a, &mut b], 3.);
    a.create()?;
    std::thread::sleep(Duration::from_secs(2));
    phase("A_recreated", &mut [&mut a, &mut b], 3.);
    b.create()?;
    std::thread::sleep(Duration::from_secs(2));
    phase("B_recreated", &mut [&mut a, &mut b], 3.);
    Ok(())
}
