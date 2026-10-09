//! Temporary exclusive/primary/isolated layouts with shared and crash restoration.
use crate::display::{Snapshot, Topology};
use anyhow::{Result, bail};
use butterpollo_core::{display_policy::Arrangement, topology::Node};
use std::sync::{Mutex, OnceLock};
#[derive(Default)]
struct State {
    users: usize,
    key: String,
    before: Option<Snapshot>,
    applied: Vec<Node>,
    /// Streamed displays in the order their streams arrived, with their lease
    /// counts. The first is laid out as for a single stream.
    owners: Vec<(String, usize)>,
    arrangement: Option<Arrangement>,
    retained: Vec<String>,
    /// The layout is saved to the Windows display database (see `persist`).
    persisted: bool,
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}
pub fn matches(nodes: &[Node], applied: &[Node]) -> bool {
    matches_connected(nodes, applied, None)
}
/// Like `matches`, but a display that is no longer connected (a TV switched
/// off at the wall, an unplugged monitor) does not count as a change by the
/// user: the rest of the layout is still the one the stream applied.
fn matches_connected(
    nodes: &[Node],
    applied: &[Node],
    connected: Option<&std::collections::BTreeSet<String>>,
) -> bool {
    let present = |id: &str| connected.is_none_or(|c| c.contains(id));
    let active: std::collections::BTreeSet<_> = nodes
        .iter()
        .filter(|n| n.active)
        .map(|n| n.device_id.as_str())
        .collect();
    let expected: std::collections::BTreeSet<_> = applied
        .iter()
        .filter(|n| n.active && present(&n.device_id))
        .map(|n| n.device_id.as_str())
        .collect();
    active == expected
        && nodes.iter().all(|n| {
            applied
                .iter()
                .any(|a| a.device_id == n.device_id && a.desired_position == n.desired_position)
        })
}
/// Whether the displays still have the layout a stream applied.
pub fn unchanged(applied: &[Node]) -> Result<bool> {
    let current = Topology::query()?.nodes()?;
    let connected = Topology::query_all()?
        .monitors()
        .into_iter()
        .map(|m| m.device_id)
        .collect();
    Ok(matches_connected(&current, applied, Some(&connected)))
}
/// Tell the layouts streams hold that a remote monitor's display was
/// switched on (`on`) or is about to be removed. Otherwise a stream's end
/// finds a layout it did not apply, takes it for the user's own and leaves
/// the physical displays off, and a restore would switch the remote monitor
/// off. `output`: its GDI name or device id.
pub fn retain(output: &str, on: bool) -> Result<()> {
    let arranged = state().lock().unwrap().users != 0;
    let activated = activation_state().lock().unwrap().users != 0;
    if !arranged && !activated {
        return Ok(());
    }
    let id = crate::display::monitors()
        .ok()
        .and_then(|all| all.into_iter().find(|m| m.matches(output)))
        .map_or_else(|| output.to_owned(), |m| m.device_id);
    let current = if on {
        Topology::query()?.nodes()?
    } else {
        Vec::new()
    };
    let mut result = Ok(());
    {
        let mut state = state().lock().unwrap();
        if retain_in(&mut state, &current, &id, on)
            && let Some(before) = &state.before
        {
            result =
                crate::display_recovery::arrangement(Some((before.clone(), state.applied.clone())));
        }
    }
    let mut state = activation_state().lock().unwrap();
    if retain_in(&mut state, &current, &id, on)
        && let Some(before) = &state.before
    {
        result = result.and(crate::display_recovery::activation(Some((
            before.clone(),
            state.applied.clone(),
        ))));
    }
    result
}
/// Record `id` in a held layout. Switched on, it is expected active where it
/// is now and restored active with the layout from before the stream; going
/// away, it is only no longer kept on. Its node stays: a display that is gone
/// does not count as a change, and one still there until its lease ends
/// must not either. True when the recorded layout changed.
fn retain_in(state: &mut State, current: &[Node], id: &str, on: bool) -> bool {
    if state.users == 0 {
        return false;
    }
    if !on {
        state.retained.retain(|r| r != id);
        return false;
    }
    let Some(node) = current.iter().find(|n| n.device_id == id) else {
        return false;
    };
    match state.applied.iter_mut().find(|n| n.device_id == id) {
        Some(applied) => *applied = node.clone(),
        None => state.applied.push(node.clone()),
    }
    if let Some(before) = &mut state.before {
        match before.nodes.iter_mut().find(|n| n.device_id == id) {
            Some(original) => original.active = true,
            None => before.nodes.push(node.clone()),
        }
    }
    if !state.retained.iter().any(|r| r == id) {
        state.retained.push(id.to_owned());
    }
    true
}
/// Whether a display on in `before` is connected but not on in `current`
/// (a snapshot lists only the displays that are on).
fn switched_off(before: &[Node], current: &[Node], connected: &[String]) -> bool {
    before.iter().filter(|b| b.active).any(|b| {
        connected.contains(&b.device_id)
            && !current
                .iter()
                .any(|c| c.device_id == b.device_id && c.active)
    })
}
fn connected() -> Result<Vec<String>> {
    Ok(Topology::query_all()?
        .monitors()
        .into_iter()
        .map(|m| m.device_id)
        .collect())
}
fn active_ids(nodes: &[Node]) -> std::collections::BTreeSet<String> {
    nodes
        .iter()
        .filter(|n| n.active)
        .map(|n| n.device_id.clone())
        .collect()
}
/// Lay out the current owners' displays and record the result for recovery.
/// `leaving` is switched off unless it was on before streaming started.
fn apply(state: &mut State, arrangement: Arrangement, leaving: Option<&str>) -> Result<()> {
    let current = Snapshot::capture()?;
    let targets: Vec<_> = state.owners.iter().map(|(t, _)| t.clone()).collect();
    let mut desired = arrangement.compose_all(&current.nodes, &targets, &state.retained)?;
    if let Some(leaving) = leaving {
        let was_active = state.before.as_ref().is_some_and(|before| {
            before
                .nodes
                .iter()
                .any(|n| n.device_id == leaving && n.active)
        });
        if !was_active {
            for node in desired.iter_mut().filter(|n| n.device_id == leaving) {
                node.active = false;
                node.primary = false;
            }
        }
    }
    if let Some(before) = &state.before {
        crate::display_recovery::arrangement(Some((before.clone(), desired.clone())))?;
    }
    let ids = active_ids(&desired);
    if ids != active_ids(&current.nodes) {
        Topology::set_active(&ids.into_iter().collect::<Vec<_>>())?;
    }
    Topology::query()?.set_positions(
        &desired
            .iter()
            .filter(|n| n.active)
            .map(|n| (n.device_id.clone(), n.desired_position))
            .collect(),
    )?;
    state.applied = desired;
    save_layout(state);
    Ok(())
}
/// Whether to save a stream's layout to the Windows display database. Windows
/// recalls the saved layout for the connected displays when an
/// exclusive-fullscreen game loses focus (the Win key, Alt+Tab), which would
/// switch the physical monitor back on. Only a layout with the stream's own
/// virtual display: the database entry is keyed by the connected displays, so
/// this changes only the entry for the user's displays plus that virtual
/// display, never the one for the user's displays alone. The entry is put
/// back when the last stream ends (`forget_saved_layout`).
fn persist(arrangement: Arrangement, virtual_target: bool) -> bool {
    virtual_target && arrangement == Arrangement::Exclusive
}
fn save_layout(state: &State) {
    if state.persisted
        && let Err(error) = Topology::save_current()
    {
        tracing::warn!(error = %format!("{error:#}"), "saving the stream layout for Windows to recall failed");
    }
}
/// Put back the database entry a stream saved: the user's displays on with
/// the virtual display beside them, as Windows lays out a new display.
/// Called before the user's layout is restored, while the virtual display is
/// still connected.
fn forget_saved_layout(before: &Snapshot, target: &str) -> Result<()> {
    let mut ids: Vec<_> = before
        .nodes
        .iter()
        .filter(|n| n.active && n.device_id != target)
        .map(|n| n.device_id.clone())
        .collect();
    ids.push(target.to_owned());
    Topology::set_active(&ids)?;
    Topology::save_current()
}
/// Retry while Windows is still applying another change, such as a virtual
/// display being removed: reading its mode fails until the change settles.
fn settled<T>(mut attempt: impl FnMut() -> Result<T>) -> Result<T> {
    for _ in 0..3 {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                tracing::debug!(error = %format!("{error:#}"), "display layout change pending; retrying");
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        }
    }
    attempt()
}
/// The layout to restore after streaming: Windows extends the desktop onto a
/// new virtual display before the arrangement is taken, but restoring that
/// would leave a retained display as an invisible monitor beside the user's
/// own, and its settings fail once it is removed (os error 31).
fn original_layout(mut snapshot: Snapshot, target: &str) -> Snapshot {
    let others_active = snapshot
        .nodes
        .iter()
        .any(|n| n.active && n.device_id != target);
    if others_active {
        for node in snapshot.nodes.iter_mut().filter(|n| n.device_id == target) {
            node.active = false;
            node.primary = false;
        }
        snapshot.hdr.remove(target);
        snapshot.scale.remove(target);
        snapshot.rotation.remove(target);
    }
    snapshot
}
pub struct Lease {
    arrangement: Arrangement,
    /// The streamed display; a recreated virtual display may change identity.
    target: Mutex<String>,
}
impl Lease {
    /// `virtual_target`: the streamed display is a virtual display made for
    /// streaming, so the user's own layout does not include it. `original`:
    /// the layout taken before that display was created; what Windows
    /// changed when it arrived is then undone with the rest.
    pub fn acquire(
        output: &str,
        arrangement: Arrangement,
        retained: &[String],
        virtual_target: bool,
        original: Option<Snapshot>,
    ) -> Result<Self> {
        let mut state = state().lock().unwrap();
        let monitors = crate::display::monitors()?;
        let target = monitors
            .iter()
            .find(|m| m.matches(output))
            .ok_or_else(|| anyhow::anyhow!("display arrangement target unavailable"))?;
        let target_id = target.device_id.clone();
        let retained: Vec<_> = monitors
            .iter()
            .filter(|m| retained.contains(&m.display_name) || retained.contains(&m.device_id))
            .map(|m| m.device_id.clone())
            .collect();
        if state.users != 0 {
            // Another stream owns the layout: join it rather than refuse.
            let owned = state.arrangement.unwrap_or(arrangement);
            if owned != arrangement {
                tracing::info!(requested = ?arrangement, active = ?owned, "joining the display arrangement of the running streams");
            }
            if let Some(owner) = state.owners.iter_mut().find(|(t, _)| *t == target_id) {
                owner.1 += 1;
                state.users += 1;
                return Ok(Self {
                    arrangement: owned,
                    target: Mutex::new(target_id),
                });
            }
            state.owners.push((target_id.clone(), 1));
            let added: Vec<_> = retained
                .into_iter()
                .filter(|id| !state.retained.contains(id))
                .collect();
            state.retained.extend(added.iter().cloned());
            if let Err(error) = apply(&mut state, owned, None) {
                state.owners.pop();
                state.retained.retain(|id| !added.contains(id));
                return Err(error);
            }
            state.users += 1;
            return Ok(Self {
                arrangement: owned,
                target: Mutex::new(target_id),
            });
        }
        let mut current = Snapshot::capture()?;
        let before = if virtual_target {
            original_layout(original.unwrap_or_else(|| current.clone()), &target_id)
        } else {
            current.clone()
        };
        // A host that stopped without putting back the layout it saved for
        // Windows to recall (a crash, a power cut) left the user's displays
        // off whenever this virtual display arrives. Put it back, unless this
        // stream saves its own.
        if virtual_target
            && !persist(arrangement, virtual_target)
            && switched_off(&before.nodes, &current.nodes, &connected()?)
        {
            tracing::info!(
                "the saved layout for the virtual display left the user's displays off; putting it back"
            );
            match forget_saved_layout(&before, &target_id).and_then(|()| Snapshot::capture()) {
                Ok(recaptured) => current = recaptured,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "putting back the saved display layout failed");
                }
            }
        }
        let desired = arrangement.compose(&current.nodes, &target_id, &retained)?;
        crate::display_recovery::arrangement(Some((before.clone(), desired.clone())))?;
        let apply = (|| -> Result<()> {
            let ids = active_ids(&desired);
            if ids != active_ids(&current.nodes) {
                Topology::set_active(&ids.into_iter().collect::<Vec<_>>())?;
            }
            Topology::query()?.set_positions(
                &desired
                    .iter()
                    .filter(|n| n.active)
                    .map(|n| (n.device_id.clone(), n.desired_position))
                    .collect(),
            )
        })();
        if let Err(error) = apply {
            current.restore()?;
            crate::display_recovery::arrangement(None)?;
            return Err(error);
        }
        state.before = Some(before);
        state.applied = desired;
        state.users = 1;
        state.key = format!("{arrangement:?}:{target_id}");
        state.owners = vec![(target_id.clone(), 1)];
        state.arrangement = Some(arrangement);
        state.retained = retained;
        state.persisted = persist(arrangement, virtual_target);
        save_layout(&state);
        Ok(Self {
            arrangement,
            target: Mutex::new(target_id),
        })
    }
    /// Displays this stream's layout switched off that Windows has switched
    /// back on, such as when an exclusive-fullscreen game loses focus and
    /// Windows recalls its saved layout for the connected displays.
    pub fn switched_back_on(&self) -> Result<Vec<String>> {
        let active: Vec<_> = crate::display::monitors()?
            .into_iter()
            .map(|m| m.device_id)
            .collect();
        let state = state().lock().unwrap();
        let Some(before) = &state.before else {
            return Ok(Vec::new());
        };
        Ok(butterpollo_core::display_policy::switched_back_on(
            &before.nodes,
            &state.applied,
            &active,
        ))
    }
    /// Reapply the stream layout after an owned VDD is recreated, retaining the
    /// original restoration snapshot and recovery journal.
    pub fn reapply(&self, output: &str, retained: &[String]) -> Result<()> {
        let mut state = state().lock().unwrap();
        let monitors = crate::display::monitors()?;
        let target = monitors
            .iter()
            .find(|m| m.matches(output))
            .ok_or_else(|| anyhow::anyhow!("recovered arrangement target unavailable"))?;
        let mut owned = self.target.lock().unwrap();
        if *owned != target.device_id {
            for owner in state.owners.iter_mut().filter(|(t, _)| *t == *owned) {
                owner.0 = target.device_id.clone();
            }
            *owned = target.device_id.clone();
        }
        for id in monitors
            .iter()
            .filter(|m| retained.contains(&m.display_name) || retained.contains(&m.device_id))
        {
            if !state.retained.contains(&id.device_id) {
                state.retained.push(id.device_id.clone());
            }
        }
        let arrangement = state.arrangement.unwrap_or(self.arrangement);
        apply(&mut state, arrangement, None)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = state().lock().unwrap();
        let target = self.target.lock().unwrap().clone();
        state.users -= 1;
        let mut left = false;
        if let Some(index) = state.owners.iter().position(|(t, _)| *t == target) {
            state.owners[index].1 -= 1;
            if state.owners[index].1 == 0 {
                state.owners.remove(index);
                left = true;
            }
        }
        if state.users != 0 {
            // Other streams continue: lay out their displays without this one.
            if left {
                let arrangement = state.arrangement.unwrap_or(self.arrangement);
                let result = if unchanged(&state.applied).unwrap_or(false) {
                    apply(&mut state, arrangement, Some(&target))
                } else {
                    // The user changed the layout; only stop expecting this display.
                    for node in state.applied.iter_mut().filter(|n| n.device_id == target) {
                        node.active = false;
                    }
                    Ok(())
                };
                if let Err(error) = result {
                    tracing::warn!(%error, "display arrangement update for the remaining streams failed");
                }
            }
            return;
        }
        let restored = (|| -> Result<()> {
            // Put the saved entry back even when the user changed the layout:
            // Windows recalls the entry for the remaining displays anyway once
            // the virtual display is removed.
            let unchanged = settled(|| unchanged(&state.applied))?;
            if state.persisted
                && let Some(before) = &state.before
                && let Err(error) = settled(|| forget_saved_layout(before, &target))
            {
                tracing::warn!(error = %format!("{error:#}"), "putting back the saved display layout failed");
            }
            if unchanged && let Some(before) = &state.before {
                settled(|| before.restore())?;
            }
            crate::display_recovery::arrangement(None)
        })();
        if let Err(error) = restored {
            tracing::warn!(error = %format!("{error:#}"), "display arrangement restoration remains pending");
        }
        state.persisted = false;
        state.before = None;
        state.applied.clear();
        state.key.clear();
        state.owners.clear();
        state.arrangement = None;
        state.retained.clear();
    }
}

/// Activate a selected, connected physical target before capture or mode changes.
/// This lease is independent of the later primary/exclusive arrangement.
pub struct Activation;
fn activation_state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}
impl Activation {
    pub fn acquire(output: &str) -> Result<Option<Self>> {
        if output.is_empty() {
            return Ok(None);
        }
        let mut state = activation_state().lock().unwrap();
        let available = Topology::query_all()?.monitors();
        let target = available
            .iter()
            .find(|monitor| monitor.matches(output))
            .ok_or_else(|| anyhow::anyhow!("selected display is not connected"))?;
        if state.users != 0 && state.key == target.device_id {
            state.users += 1;
            return Ok(Some(Self));
        }
        let before = Snapshot::capture()?;
        if before
            .nodes
            .iter()
            .any(|node| node.device_id == target.device_id && node.active)
        {
            return Ok(None);
        }
        if state.users != 0 {
            bail!("another stream owns a different display activation");
        }
        let mut ids: Vec<_> = before
            .nodes
            .iter()
            .filter(|node| node.active)
            .map(|node| node.device_id.clone())
            .collect();
        ids.push(target.device_id.clone());
        // Record the original layout before allowing Windows to activate a target.
        crate::display_recovery::activation(Some((before.clone(), vec![])))?;
        let applied = (|| -> Result<Vec<Node>> {
            Topology::set_active(&ids)?;
            Topology::query()?.nodes()
        })();
        let applied = match applied {
            Ok(applied) => applied,
            Err(error) => {
                before.restore()?;
                crate::display_recovery::activation(None)?;
                return Err(error);
            }
        };
        if let Err(error) =
            crate::display_recovery::activation(Some((before.clone(), applied.clone())))
        {
            before.restore()?;
            crate::display_recovery::activation(None)?;
            return Err(error);
        }
        state.before = Some(before);
        state.applied = applied;
        state.key = target.device_id.clone();
        state.users = 1;
        Ok(Some(Self))
    }
}
impl Drop for Activation {
    fn drop(&mut self) {
        let mut state = activation_state().lock().unwrap();
        state.users -= 1;
        if state.users != 0 {
            return;
        }
        let restored = (|| -> Result<()> {
            if unchanged(&state.applied)?
                && let Some(before) = &state.before
            {
                before.restore()?;
            }
            crate::display_recovery::activation(None)
        })();
        if let Err(error) = restored {
            tracing::warn!(%error, "display activation restoration remains pending");
        }
        state.before = None;
        state.applied.clear();
        state.key.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use butterpollo_core::topology::{Kind, Mode, Position};
    fn node(id: &str, active: bool, primary: bool) -> Node {
        Node {
            id: id.into(),
            label: id.into(),
            kind: Kind::Physical,
            active,
            primary,
            desired_position: Position { x: 0, y: 0 },
            mode: Mode {
                width: 1920,
                height: 1080,
                refresh_hz: 60.,
            },
            device_id: id.into(),
        }
    }
    fn snapshot(nodes: Vec<Node>) -> Snapshot {
        Snapshot {
            version: 1,
            hdr: nodes.iter().map(|n| (n.device_id.clone(), true)).collect(),
            scale: nodes.iter().map(|n| (n.device_id.clone(), 125)).collect(),
            rotation: Default::default(),
            clone_groups: vec![],
            nodes,
        }
    }
    #[test]
    fn an_unplugged_display_does_not_keep_the_layout_from_being_restored() {
        let mut tv = node("tv", true, false);
        tv.desired_position = Position { x: 1920, y: 0 };
        let applied = vec![node("monitor", true, true), tv];
        let current = vec![node("monitor", true, true)];
        let without_tv: std::collections::BTreeSet<String> = ["monitor".to_string()].into();
        assert!(matches_connected(&current, &applied, Some(&without_tv)));
        // Still connected but switched off: the user changed the layout.
        let with_tv: std::collections::BTreeSet<String> =
            ["monitor".to_string(), "tv".to_string()].into();
        assert!(!matches_connected(&current, &applied, Some(&with_tv)));
        assert!(!matches(&current, &applied));
        assert!(matches(&applied, &applied));
    }
    #[test]
    fn a_remote_monitor_switched_on_mid_stream_keeps_the_streams_layout() {
        let mut state = State {
            users: 1,
            before: Some(snapshot(vec![node("phys", true, true)])),
            applied: vec![node("vdd", true, true), node("phys", false, false)],
            ..Default::default()
        };
        let mut remote = node("remote", true, false);
        remote.desired_position = Position { x: 1920, y: 0 };
        let current = vec![node("vdd", true, true), remote];
        let connected: std::collections::BTreeSet<String> =
            ["vdd", "phys", "remote"].map(String::from).into();
        assert!(!matches_connected(
            &current,
            &state.applied,
            Some(&connected)
        ));
        assert!(retain_in(&mut state, &current, "remote", true));
        assert!(matches_connected(
            &current,
            &state.applied,
            Some(&connected)
        ));
        // The restore keeps it on beside the user's own displays.
        let before = state.before.as_ref().unwrap();
        assert!(
            before
                .nodes
                .iter()
                .any(|n| n.device_id == "remote" && n.active)
        );
        assert!(
            before
                .nodes
                .iter()
                .any(|n| n.device_id == "phys" && n.active)
        );
        assert_eq!(state.retained, ["remote"]);
        // Disconnected: no longer kept on, and its removal is no change.
        assert!(!retain_in(&mut state, &[], "remote", false));
        assert!(state.retained.is_empty());
        let current = vec![node("vdd", true, true)];
        let connected: std::collections::BTreeSet<String> =
            ["vdd", "phys"].map(String::from).into();
        assert!(matches_connected(
            &current,
            &state.applied,
            Some(&connected)
        ));
        // Without a stream holding a layout there is nothing to record.
        let mut idle = State::default();
        assert!(!retain_in(
            &mut idle,
            &[node("remote", true, false)],
            "remote",
            true
        ));
        assert!(idle.applied.is_empty() && idle.retained.is_empty());
    }
    #[test]
    fn a_saved_layout_left_behind_by_a_crash_is_noticed_when_the_virtual_display_arrives() {
        let before = vec![node("phys", true, true), node("tv", false, false)];
        let connected = ["phys", "tv", "vdd"].map(String::from);
        // Windows recalled a stream's saved layout as the display arrived.
        assert!(switched_off(
            &before,
            &[node("vdd", true, true)],
            &connected
        ));
        // As Windows lays out a new display: everything stays on.
        let extended = [node("phys", true, true), node("vdd", true, false)];
        assert!(!switched_off(&before, &extended, &connected));
        // A display unplugged since, or off before the stream, is no sign.
        let unplugged = ["tv", "vdd"].map(String::from);
        assert!(!switched_off(
            &before,
            &[node("vdd", true, true)],
            &unplugged
        ));
        assert!(persist(Arrangement::Exclusive, true));
        assert!(!persist(Arrangement::Exclusive, false));
        assert!(!persist(Arrangement::Extended, true));
    }
    #[test]
    fn the_original_layout_leaves_out_the_streams_virtual_display() {
        let layout = original_layout(
            snapshot(vec![node("monitor", true, true), node("vdd", true, false)]),
            "vdd",
        );
        let vdd = layout.nodes.iter().find(|n| n.device_id == "vdd").unwrap();
        assert!(!vdd.active && !vdd.primary);
        assert!(
            layout
                .nodes
                .iter()
                .any(|n| n.device_id == "monitor" && n.active)
        );
        assert!(!layout.hdr.contains_key("vdd") && !layout.scale.contains_key("vdd"));
        assert!(layout.hdr.contains_key("monitor"));
        // With no other active display the layout cannot exclude it.
        let only = original_layout(
            snapshot(vec![node("monitor", false, false), node("vdd", true, true)]),
            "vdd",
        );
        assert!(only.nodes.iter().any(|n| n.device_id == "vdd" && n.active));
    }
}
