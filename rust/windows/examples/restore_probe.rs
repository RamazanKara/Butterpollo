//! Capture the current display layout and restore it, naming any step that
//! fails. The restored layout is the one already active.
//!
//! `--restore` runs `Snapshot::restore`; `--steps` runs its steps one at a
//! time and prints every display's rate after each.
use butterpollo_windows::display::{Snapshot, Topology, monitors, set_dpi_scale, set_hdr};

fn rates(step: &str) -> anyhow::Result<()> {
    let topology = Topology::query()?;
    let mut line = format!("{step:<28}");
    for m in monitors()? {
        let rate = topology.refresh(&m.device_id).map_or(0, |r| r.0);
        line += &format!(
            " | {} {:.3} Hz hdr={}",
            m.display_name,
            f64::from(rate) / 1000.,
            m.hdr_enabled
        );
    }
    println!("{line}");
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let snapshot = Snapshot::capture()?;
    println!("{}", serde_json::to_string(&snapshot)?);
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|a| a == "--restore") {
        match snapshot.restore() {
            Ok(()) => println!("RESTORE ok"),
            Err(error) => println!("RESTORE failed: {error:#}"),
        }
        println!("{}", serde_json::to_string(&Snapshot::capture()?)?);
    }
    if args.iter().any(|a| a == "--steps") {
        rates("captured")?;
        for n in &snapshot.nodes {
            let Some(m) = monitors()?.into_iter().find(|m| m.device_id == n.device_id) else {
                continue;
            };
            let rate =
                butterpollo_core::framegen::Rate((n.mode.refresh_hz * 1000.0).round() as u32);
            let result =
                Topology::set_mode_rate(&m.display_name, n.mode.width, n.mode.height, rate);
            rates(&format!(
                "mode {} {:?}",
                n.label,
                result.err().map(|e| e.to_string())
            ))?;
            if let Some(enabled) = snapshot.hdr.get(&n.device_id) {
                let result = set_hdr(&m, *enabled);
                rates(&format!(
                    "hdr {} {:?}",
                    n.label,
                    result.err().map(|e| e.to_string())
                ))?;
            }
            if let Some(percent) = snapshot.scale.get(&n.device_id) {
                let result = set_dpi_scale(&m, *percent);
                rates(&format!(
                    "scale {} {:?}",
                    n.label,
                    result.err().map(|e| e.to_string())
                ))?;
            }
        }
        let result = Topology::query()?.set_positions(
            &snapshot
                .nodes
                .iter()
                .map(|n| (n.device_id.clone(), n.desired_position))
                .collect(),
        );
        rates(&format!(
            "positions {:?}",
            result.err().map(|e| e.to_string())
        ))?;
    }
    // --set <display> <width> <height> <millihertz>
    if let Some(i) = args.iter().position(|a| a == "--set") {
        let rate = butterpollo_core::framegen::Rate(args[i + 4].parse()?);
        Topology::set_mode_rate(
            &args[i + 1],
            args[i + 2].parse()?,
            args[i + 3].parse()?,
            rate,
        )?;
        rates("set")?;
    }
    Ok(())
}
