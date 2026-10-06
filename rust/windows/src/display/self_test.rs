//! Display self-test. The virtual display driver accepts only the SYSTEM
//! service, so `butterpollo-service.exe --display-self-test REPORT`, run as
//! SYSTEM, starts `butterpollo.exe --display-self-test REPORT` in the signed-in
//! console session. The desktop changes for about half a minute and is then
//! restored, including the driver's permanent display count.
//!
//! 1. A virtual display that Windows leaves off is switched on by the host,
//!    and the other displays keep their modes and positions.
//! 2. Two of the driver's permanent displays stand in for TVs in duplicate
//!    mode, as in issue #4, and a stream's virtual display still comes up.
//! 3. A saved layout with that clone group is restored after the group was
//!    broken up.
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Device, width, height, refresh and desktop position.
type Timing = (String, u32, u32, u32, i32, i32);

fn timings(skip: impl Fn(&Monitor) -> bool) -> Result<Vec<Timing>> {
    monitors()?
        .iter()
        .filter(|m| !skip(m))
        .map(|m| {
            let mode = mode(&m.display_name)?;
            let position = unsafe { mode.Anonymous1.Anonymous2.dmPosition };
            Ok((
                m.device_id.clone(),
                mode.dmPelsWidth,
                mode.dmPelsHeight,
                mode.dmDisplayFrequency,
                position.x,
                position.y,
            ))
        })
        .collect()
}

fn active_ids() -> Result<Vec<String>> {
    Ok(monitors()?.into_iter().map(|m| m.device_id).collect())
}

fn cloned(a: &str, b: &str) -> Result<bool> {
    Ok(Topology::query()?
        .clone_groups()
        .iter()
        .any(|group| group.iter().any(|id| id == a) && group.iter().any(|id| id == b)))
}

/// What Windows does when a layout it saved leaves the display off.
fn switch_off(target: &Monitor) -> Result<()> {
    let active = Topology::query()?;
    let on: Vec<_> = active
        .paths
        .iter()
        .filter(|p| !(p.targetInfo.adapterId == target.adapter && p.targetInfo.id == target.target))
        .copied()
        .collect();
    check(unsafe {
        SetDisplayConfig(
            Some(&on),
            Some(&active.modes),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES,
        )
    })
}

/// The check and the test display's device, which disappears asynchronously.
fn switch_on() -> Result<(Value, String)> {
    let display = VirtualDisplay::create("butterpollo-self-test-a", 1280, 720, 60)?;
    let target = display
        .resolved_target
        .clone()
        .context("virtual display unresolved")?;
    let ours = |m: &Monitor| m.adapter == target.adapter && m.target == target.target;
    let before = timings(ours)?;
    switch_off(&target)?;
    anyhow::ensure!(
        !monitors()?.iter().any(ours),
        "the virtual display stayed on after switching it off"
    );
    let kept_timings = activate_target(target.adapter, target.target)?;
    let on = monitors()?.iter().any(ours);
    let unchanged = timings(ours)? == before;
    let check = json!({
        "passed": on && unchanged,
        "switched_on": on,
        "kept_timings": kept_timings,
        "other_displays_unchanged": unchanged,
    });
    Ok((check, target.device_id))
}

/// Two more permanent displays from the driver, on and duplicated.
fn stand_in_tvs(permanent: u32, before: &BTreeSet<String>) -> Result<(String, String)> {
    anyhow::ensure!(
        permanent <= 2,
        "the driver already has {permanent} permanent displays"
    );
    set_permanent_display_count(permanent + 2)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let added = loop {
        let added: Vec<_> = Topology::query_all()?
            .monitors()
            .into_iter()
            .map(|m| m.device_id)
            .filter(|id| !before.contains(id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if added.len() >= 2 {
            break added;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "the driver's permanent displays did not appear"
        );
        std::thread::sleep(Duration::from_millis(250));
    };
    let (a, b) = (added[0].clone(), added[1].clone());
    let mut on = active_ids()?;
    on.extend([a.clone(), b.clone()]);
    on.sort();
    on.dedup();
    Topology::set_active(&on)?;
    Topology::query()?.restore_clone_groups(&[vec![a.clone(), b.clone()]])?;
    anyhow::ensure!(cloned(&a, &b)?, "the stand-in TVs could not be duplicated");
    Ok((a, b))
}

fn beside_duplicated_tvs(a: &str, b: &str) -> Result<Value> {
    let others = |m: &Monitor| m.device_id == a || m.device_id == b;
    let before = timings(others)?;
    let display = VirtualDisplay::create("butterpollo-self-test-b", 1280, 720, 60)?;
    let target = display
        .resolved_target
        .clone()
        .context("virtual display unresolved")?;
    let on = monitors()?
        .iter()
        .any(|m| m.adapter == target.adapter && m.target == target.target);
    // The TVs may move to make room; everything else keeps its timing.
    let unchanged =
        timings(|m| others(m) || (m.adapter == target.adapter && m.target == target.target))?
            == before;
    // Windows lays out the displays again when one arrives and may break up
    // the duplicate; that is reported, not failed.
    let still_cloned = cloned(a, b)?;
    Ok(json!({
        "passed": on && unchanged,
        "stream_display_on": on,
        "switched_on_by_host": display.switched_on,
        "tvs_still_duplicated": still_cloned,
        "other_displays_unchanged": unchanged,
    }))
}

fn clone_layout_restored(a: &str, b: &str) -> Result<Value> {
    if !cloned(a, b)? {
        Topology::query()?.restore_clone_groups(&[vec![a.to_owned(), b.to_owned()]])?;
    }
    let saved = Snapshot::capture()?;
    anyhow::ensure!(
        saved
            .clone_groups
            .iter()
            .any(|g| g.iter().any(|id| id == a) && g.iter().any(|id| id == b)),
        "the saved layout has no clone group"
    );
    // Give every display its own desktop, as set_active does.
    Topology::set_active(&active_ids()?)?;
    let broken = !cloned(a, b)?;
    let restored = saved.restore();
    let rejoined = cloned(a, b)?;
    Ok(json!({
        "passed": broken && restored.is_ok() && rejoined,
        "clone_group_broken_first": broken,
        "restore": restored.err().map(|e| format!("{e:#}")),
        "clone_group_restored": rejoined,
    }))
}

/// Run the checks, restore the desktop and write `report`. True when every
/// check passed.
pub fn run(report: &std::path::Path) -> Result<bool> {
    let original = Snapshot::capture()?;
    let permanent = permanent_display_count()?;
    let before: BTreeSet<_> = Topology::query_all()?
        .monitors()
        .into_iter()
        .map(|m| m.device_id)
        .collect();
    let mut checks = serde_json::Map::new();
    let result = (|| -> Result<()> {
        let (check, test_display) = switch_on()?;
        checks.insert("switch_on".into(), check);
        let mut before = before.clone();
        before.insert(test_display);
        let (a, b) = stand_in_tvs(permanent, &before)?;
        checks.insert(
            "beside_duplicated_tvs".into(),
            beside_duplicated_tvs(&a, &b)?,
        );
        checks.insert(
            "clone_layout_restored".into(),
            clone_layout_restored(&a, &b)?,
        );
        Ok(())
    })();
    let mut cleanup = Vec::new();
    if let Err(error) = set_permanent_display_count(permanent) {
        cleanup.push(format!("permanent display count: {error:#}"));
    }
    std::thread::sleep(Duration::from_secs(2));
    if let Err(error) = original.restore() {
        cleanup.push(format!("original layout: {error:#}"));
    }
    let passed = result.is_ok()
        && cleanup.is_empty()
        && checks.values().all(|check| check["passed"] == true);
    let document = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "passed": passed,
        "checks": checks,
        "error": result.err().map(|e| format!("{e:#}")),
        "cleanup_errors": cleanup,
        "permanent_display_count": permanent,
    });
    std::fs::write(report, serde_json::to_vec_pretty(&document)?)?;
    Ok(passed)
}
