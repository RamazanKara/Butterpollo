//! Protect targets that were deliberately inactive before an owned VDD hotplug.
//! Windows can recall another saved topology while a virtual target arrives.
//! This guard exists only during startup/recovery, never throughout a stream.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

type TargetKey = (u32, i32, u32);

fn key(path: &DISPLAYCONFIG_PATH_INFO) -> TargetKey {
    (
        path.targetInfo.adapterId.LowPart,
        path.targetInfo.adapterId.HighPart,
        path.targetInfo.id,
    )
}

fn monitor_key(monitor: &Monitor) -> TargetKey {
    (
        monitor.adapter.LowPart,
        monitor.adapter.HighPart,
        monitor.target,
    )
}

fn target_identity(path: &DISPLAYCONFIG_PATH_INFO) -> Result<String> {
    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
            path.targetInfo.adapterId,
            path.targetInfo.id,
        ),
        ..Default::default()
    };
    // SAFETY: The initialized target-name structure has the matching header type
    // and size and remains writable throughout the query.
    check(unsafe { DisplayConfigGetDeviceInfo(&mut name.header) })?;
    Ok(wide(&name.monitorDevicePath).to_ascii_lowercase())
}

fn identities(paths: &[DISPLAYCONFIG_PATH_INFO]) -> Result<BTreeMap<TargetKey, String>> {
    let mut result = BTreeMap::new();
    for path in paths
        .iter()
        .filter(|p| p.targetInfo.targetAvailable.as_bool())
    {
        if let std::collections::btree_map::Entry::Vacant(entry) = result.entry(key(path)) {
            let identity = target_identity(path)?;
            if !identity.is_empty() {
                entry.insert(identity);
            }
        }
    }
    Ok(result)
}

fn dormant_targets(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    identities: &BTreeMap<TargetKey, String>,
) -> BTreeMap<TargetKey, String> {
    // ALL_PATHS contains inactive alternative routes to active monitors. A
    // target is dormant only when none of its routes is currently active.
    let active: BTreeSet<_> = paths
        .iter()
        .filter(|p| p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0)
        .map(key)
        .collect();
    paths
        .iter()
        .filter(|p| p.targetInfo.targetAvailable.as_bool() && !active.contains(&key(p)))
        .filter_map(|p| identities.get(&key(p)).map(|id| (key(p), id.clone())))
        .collect()
}

fn unwanted_targets(
    dormant: &BTreeMap<TargetKey, String>,
    current: &BTreeMap<TargetKey, String>,
    owned: TargetKey,
) -> BTreeSet<TargetKey> {
    current
        .iter()
        .filter(|(target, identity)| {
            **target != owned && dormant.get(*target).is_some_and(|old| old == *identity)
        })
        .map(|(target, _)| *target)
        .collect()
}

fn protected_paths(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    unwanted: &BTreeSet<TargetKey>,
    owned: TargetKey,
) -> Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
    anyhow::ensure!(
        paths
            .iter()
            .any(|p| key(p) == owned && p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0),
        "owned virtual display disappeared during hotplug protection"
    );
    // Keep existing source/target mode indices and clone relationships. Do not
    // use set_active(), which deliberately lets Windows choose new timings.
    Ok(paths
        .iter()
        .filter(|p| key(p) == owned || !unwanted.contains(&key(p)))
        .copied()
        .collect())
}

/// The modes the kept paths use, renumbered. Modes left behind by a pruned
/// path make Windows refuse the supplied layout (ERROR_INVALID_PARAMETER).
fn referenced_modes(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    modes: &[DISPLAYCONFIG_MODE_INFO],
) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    let mut kept = Vec::new();
    let mut renumbered = BTreeMap::new();
    let mut remap = |index: u32| -> Result<u32> {
        if index == DISPLAYCONFIG_PATH_MODE_IDX_INVALID {
            return Ok(index);
        }
        if let Some(new) = renumbered.get(&index) {
            return Ok(*new);
        }
        kept.push(
            *modes
                .get(index as usize)
                .context("display mode index out of range")?,
        );
        let new = kept.len() as u32 - 1;
        renumbered.insert(index, new);
        Ok(new)
    };
    let paths = paths
        .iter()
        .map(|path| {
            let mut path = *path;
            // SAFETY: These paths were queried without virtual-mode awareness, so
            // the source modeInfoIdx union field is initialized; remap checks bounds.
            path.sourceInfo.Anonymous.modeInfoIdx =
                remap(unsafe { path.sourceInfo.Anonymous.modeInfoIdx })?;
            // SAFETY: The same non-virtual query initialized the target modeInfoIdx
            // union field, and remap checks it against the mode table.
            path.targetInfo.Anonymous.modeInfoIdx =
                remap(unsafe { path.targetInfo.Anonymous.modeInfoIdx })?;
            Ok(path)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((paths, kept))
}

// Compare values, never raw struct bytes (unions/padding are not comparable).
// A reordered mode table is conservatively treated as a change as well.
fn same_topology(a: &Topology, b: &Topology) -> bool {
    a.paths.len() == b.paths.len()
        && a.modes.len() == b.modes.len()
        // SAFETY: Both topologies use the initialized non-virtual modeInfoIdx union
        // fields returned by QueryDisplayConfig (or initialized by the test fixtures).
        && a.paths.iter().zip(&b.paths).all(|(a, b)| unsafe {
            a.sourceInfo.adapterId == b.sourceInfo.adapterId
                && a.sourceInfo.id == b.sourceInfo.id
                && a.sourceInfo.Anonymous.modeInfoIdx == b.sourceInfo.Anonymous.modeInfoIdx
                && key(a) == key(b)
                && a.targetInfo.Anonymous.modeInfoIdx == b.targetInfo.Anonymous.modeInfoIdx
                && a.targetInfo.outputTechnology == b.targetInfo.outputTechnology
                && a.targetInfo.rotation == b.targetInfo.rotation
                && a.targetInfo.scaling == b.targetInfo.scaling
                && a.targetInfo.refreshRate == b.targetInfo.refreshRate
                && a.targetInfo.scanLineOrdering == b.targetInfo.scanLineOrdering
                && a.targetInfo.targetAvailable == b.targetInfo.targetAvailable
                && a.flags == b.flags
        })
        && a.modes.iter().zip(&b.modes).all(|(a, b)| {
            if a.infoType != b.infoType || a.id != b.id || a.adapterId != b.adapterId {
                return false;
            }
            // SAFETY: Both mode tags agree, and the match selects only their initialized
            // union member; a target signal's videoStandard is its initialized flag word.
            unsafe {
                match a.infoType {
                    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE => {
                        a.Anonymous.sourceMode == b.Anonymous.sourceMode
                    }
                    DISPLAYCONFIG_MODE_INFO_TYPE_DESKTOP_IMAGE => {
                        a.Anonymous.desktopImageInfo == b.Anonymous.desktopImageInfo
                    }
                    DISPLAYCONFIG_MODE_INFO_TYPE_TARGET => {
                        let a = a.Anonymous.targetMode.targetVideoSignalInfo;
                        let b = b.Anonymous.targetMode.targetVideoSignalInfo;
                        a.pixelRate == b.pixelRate
                            && a.hSyncFreq == b.hSyncFreq
                            && a.vSyncFreq == b.vSyncFreq
                            && a.activeSize == b.activeSize
                            && a.totalSize == b.totalSize
                            && a.Anonymous.videoStandard == b.Anonymous.videoStandard
                            && a.scanLineOrdering == b.scanLineOrdering
                    }
                    _ => false,
                }
            }
        })
}

fn is_settled(now: Instant, quiet_since: Instant, limit: Instant) -> Result<bool> {
    anyhow::ensure!(
        now < limit,
        "virtual display topology did not settle after hotplug protection"
    );
    Ok(now.duration_since(quiet_since) >= Duration::from_millis(500))
}

pub(super) struct Protection {
    dormant: BTreeMap<TargetKey, String>,
    deadline: Option<Instant>,
}

impl Protection {
    pub(super) fn capture() -> Result<Self> {
        let topology = Topology::query_all()?;
        let names = identities(&topology.paths)?;
        let dormant = dormant_targets(&topology.paths, &names);
        tracing::info!(
            count = dormant.len(),
            targets = ?dormant.values().collect::<Vec<_>>(),
            "preserving inactive targets across owned virtual display creation"
        );
        Ok(Self {
            dormant,
            deadline: None,
        })
    }

    pub(super) fn check(&self, owned: &Monitor, stage: &str) -> Result<bool> {
        if self.dormant.is_empty() {
            return Ok(false);
        }
        anyhow::ensure!(
            self.deadline.is_none_or(|limit| Instant::now() < limit),
            "virtual display startup protection expired"
        );
        let mut topology = Topology::query()?;
        let names = identities(&topology.paths)?;
        let owned_key = monitor_key(owned);
        anyhow::ensure!(
            names
                .get(&owned_key)
                .is_some_and(|id| id.eq_ignore_ascii_case(&owned.monitor_device_path)),
            "owned virtual display identity changed during hotplug protection"
        );
        let unwanted = unwanted_targets(&self.dormant, &names, owned_key);
        if unwanted.is_empty() {
            return Ok(false);
        }
        // Query again after reading target names: do not apply physical modes
        // from an earlier snapshot over a user's concurrent layout change.
        // CCD has no atomic compare-and-set, so a narrow final-call race remains.
        let fresh = Topology::query()?;
        anyhow::ensure!(
            same_topology(&topology, &fresh),
            "display layout changed during virtual hotplug protection"
        );
        topology.paths = protected_paths(&topology.paths, &unwanted, owned_key)?;
        (topology.paths, topology.modes) = referenced_modes(&topology.paths, &topology.modes)?;
        tracing::warn!(
            stage,
            targets = ?unwanted.iter().filter_map(|k| names.get(k)).collect::<Vec<_>>(),
            "Windows reactivated inactive displays during owned virtual display startup; restoring their inactive state"
        );
        // Queries can consume the remaining budget; check again immediately
        // before the only display-changing call.
        anyhow::ensure!(
            self.deadline.is_none_or(|limit| Instant::now() < limit),
            "virtual display startup protection expired before apply"
        );
        // This temporary, strict apply cannot import a saved topology, update
        // the database, or silently retime the displays that remain active.
        let flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG;
        // SAFETY: Both initialized slices remain live for the synchronous call, and
        // referenced_modes checked and remapped every retained mode index.
        let strict =
            unsafe { SetDisplayConfig(Some(&topology.paths), Some(&topology.modes), flags) };
        if strict != 0 {
            // Windows can refuse the exact layout while it is still placing the
            // new display (ERROR_INVALID_PARAMETER). Letting it adjust modes can
            // retime the remaining displays; the stream's layout restore puts
            // them back.
            // SAFETY: The same initialized path and mode slices, with checked mode
            // indices, remain live throughout this synchronous retry.
            unsafe {
                check(SetDisplayConfig(
                    Some(&topology.paths),
                    Some(&topology.modes),
                    flags | SDC_ALLOW_CHANGES,
                ))
            }
            .with_context(|| {
                format!(
                    "exact layout refused: {}",
                    std::io::Error::from_raw_os_error(strict)
                )
            })?;
        }
        let after = Topology::query()?;
        let after_names = identities(&after.paths)?;
        anyhow::ensure!(
            after_names
                .get(&owned_key)
                .is_some_and(|id| id.eq_ignore_ascii_case(&owned.monitor_device_path)),
            "owned virtual display disappeared after hotplug protection"
        );
        anyhow::ensure!(
            unwanted_targets(&self.dormant, &after_names, owned_key).is_empty(),
            "Windows kept an inactive display enabled after hotplug protection"
        );
        Ok(true)
    }

    pub(super) fn settle(&mut self, owned: &Monitor, stage: &str) -> Result<()> {
        if self.dormant.is_empty() {
            return Ok(());
        }
        // A recovery retry cannot start a fresh enforcement window. Once this
        // deadline expires, further calls fail without changing any displays.
        let limit = *self
            .deadline
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(1500));
        let mut quiet_since = Instant::now();
        loop {
            if self.check(owned, stage)? {
                quiet_since = Instant::now();
            }
            if is_settled(Instant::now(), quiet_since, limit)? {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(target: u32, source: u32, active: bool, available: bool) -> DISPLAYCONFIG_PATH_INFO {
        let mut path = DISPLAYCONFIG_PATH_INFO::default();
        path.targetInfo.id = target;
        path.targetInfo.targetAvailable = available.into();
        path.sourceInfo.id = source;
        path.sourceInfo.Anonymous.modeInfoIdx = 4 + source;
        path.targetInfo.Anonymous.modeInfoIdx = 20 + target;
        path.flags = if active { DISPLAYCONFIG_PATH_ACTIVE } else { 0 };
        path
    }

    fn names(values: &[(u32, &str)]) -> BTreeMap<TargetKey, String> {
        values
            .iter()
            .map(|(id, name)| ((0, 0, *id), (*name).into()))
            .collect()
    }

    #[test]
    fn alternate_inactive_routes_do_not_make_an_active_monitor_dormant() {
        let paths = [
            path(1, 0, true, true),
            path(1, 1, false, true),
            path(2, 0, false, true),
            path(3, 2, false, false),
        ];
        let result = dormant_targets(
            &paths,
            &names(&[(1, "desktop"), (2, "disabled-tv"), (3, "unplugged")]),
        );
        assert_eq!(result, names(&[(2, "disabled-tv")]));
    }

    #[test]
    fn restored_dormant_tv_is_pruned_without_retiming_or_changing_clones() {
        let paths = [
            path(1, 0, true, true),
            path(2, 1, true, true),
            path(3, 0, true, true),
            path(4, 2, true, true),
        ];
        let dormant = names(&[(2, "disabled-tv")]);
        let current = names(&[
            (1, "desktop"),
            (2, "disabled-tv"),
            (3, "desktop-clone"),
            (4, "owned-vdd"),
        ]);
        let removed = unwanted_targets(&dormant, &current, (0, 0, 4));
        let kept = protected_paths(&paths, &removed, (0, 0, 4)).unwrap();
        assert_eq!(
            kept.iter().map(key).collect::<Vec<_>>(),
            vec![(0, 0, 1), (0, 0, 3), (0, 0, 4)]
        );
        assert_eq!(kept[0].sourceInfo.id, kept[1].sourceInfo.id);
        for (actual, original) in kept.iter().zip([paths[0], paths[2], paths[3]]) {
            // SAFETY: path() initialized both source modeInfoIdx fields, and filtering
            // copied the paths without changing their union members.
            assert_eq!(unsafe { actual.sourceInfo.Anonymous.modeInfoIdx }, unsafe {
                original.sourceInfo.Anonymous.modeInfoIdx
            });
            // SAFETY: path() initialized both target modeInfoIdx fields, and filtering
            // copied the paths without changing their union members.
            assert_eq!(unsafe { actual.targetInfo.Anonymous.modeInfoIdx }, unsafe {
                original.targetInfo.Anonymous.modeInfoIdx
            });
        }
    }

    #[test]
    fn modes_of_a_pruned_display_are_left_out_and_the_rest_renumbered() {
        // path() points source s at mode 4 + s and target t at mode 20 + t.
        let modes: Vec<_> = (0..30)
            .map(|id| DISPLAYCONFIG_MODE_INFO {
                id,
                ..Default::default()
            })
            .collect();
        let kept = [
            path(1, 0, true, true),
            path(3, 0, true, true),
            path(4, 2, true, true),
        ];
        let (paths, used) = referenced_modes(&kept, &modes).unwrap();
        assert_eq!(
            used.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![4, 21, 23, 6, 24]
        );
        for path in &paths {
            // SAFETY: path() initialized both modeInfoIdx fields, and referenced_modes
            // rewrote those same union members with checked indices into used.
            let (source, target) = unsafe {
                (
                    path.sourceInfo.Anonymous.modeInfoIdx,
                    path.targetInfo.Anonymous.modeInfoIdx,
                )
            };
            assert_eq!(used[source as usize].id, 4 + path.sourceInfo.id);
            assert_eq!(used[target as usize].id, 20 + path.targetInfo.id);
        }
        assert!(referenced_modes(&kept, &modes[..10]).is_err());
    }

    #[test]
    fn new_or_replaced_monitors_and_owned_recovered_target_are_preserved() {
        let dormant = names(&[(2, "old-tv"), (4, "owned-vdd")]);
        let current = names(&[
            (1, "desktop"),
            (2, "replacement-monitor"),
            (3, "new-hotplug"),
            (4, "owned-vdd"),
        ]);
        assert!(unwanted_targets(&dormant, &current, (0, 0, 4)).is_empty());
    }

    #[test]
    fn missing_owned_target_refuses_to_apply_any_protection() {
        let paths = [path(2, 0, true, true)];
        assert!(protected_paths(&paths, &BTreeSet::from([(0, 0, 2)]), (0, 0, 4)).is_err());
    }

    #[test]
    fn physical_mode_or_clone_changes_abort_a_stale_prune() {
        let mut source = DISPLAYCONFIG_MODE_INFO {
            infoType: DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
            ..Default::default()
        };
        source.Anonymous.sourceMode.width = 1920;
        source.Anonymous.sourceMode.height = 1080;
        let before = Topology {
            paths: vec![path(1, 0, true, true)],
            modes: vec![source],
        };
        let mut after = Topology {
            paths: before.paths.clone(),
            modes: before.modes.clone(),
        };
        assert!(same_topology(&before, &after));
        after.modes[0].Anonymous.sourceMode.position.x = 100;
        assert!(!same_topology(&before, &after));
        after.modes = before.modes.clone();
        after.paths[0].sourceInfo.id = 2;
        assert!(!same_topology(&before, &after));
        after.paths = before.paths.clone();
        after.paths.push(path(2, 0, true, true));
        assert!(!same_topology(&before, &after));
    }

    #[test]
    fn elapsed_deadline_cannot_be_mistaken_for_a_quiet_startup() {
        let start = Instant::now();
        let limit = start + Duration::from_millis(1500);
        assert!(!is_settled(start + Duration::from_millis(499), start, limit).unwrap());
        assert!(is_settled(start + Duration::from_millis(500), start, limit).unwrap());
        assert!(is_settled(limit, start, limit).is_err());
        assert!(is_settled(limit + Duration::from_secs(1), start, limit).is_err());
    }
}
