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
}
fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Mutex::default)
}
pub fn matches(nodes: &[Node], applied: &[Node]) -> bool {
    let active: std::collections::BTreeSet<_> = nodes
        .iter()
        .filter(|n| n.active)
        .map(|n| n.device_id.as_str())
        .collect();
    let expected: std::collections::BTreeSet<_> = applied
        .iter()
        .filter(|n| n.active)
        .map(|n| n.device_id.as_str())
        .collect();
    active == expected
        && nodes.iter().all(|n| {
            applied
                .iter()
                .any(|a| a.device_id == n.device_id && a.desired_position == n.desired_position)
        })
}
pub struct Lease {
    arrangement: Arrangement,
}
impl Lease {
    pub fn acquire(output: &str, arrangement: Arrangement, retained: &[String]) -> Result<Self> {
        let mut state = state().lock().unwrap();
        let monitors = crate::display::monitors()?;
        let target = monitors
            .iter()
            .find(|m| m.matches(output))
            .ok_or_else(|| anyhow::anyhow!("display arrangement target unavailable"))?;
        let key = format!("{:?}:{}", arrangement, target.device_id);
        if state.users != 0 {
            if state.key != key {
                bail!("another stream owns a different display arrangement");
            }
            state.users += 1;
            return Ok(Self { arrangement });
        }
        let before = Snapshot::capture()?;
        let retained: Vec<_> = monitors
            .iter()
            .filter(|m| retained.contains(&m.display_name) || retained.contains(&m.device_id))
            .map(|m| m.device_id.clone())
            .collect();
        let desired = arrangement.compose(&before.nodes, &target.device_id, &retained)?;
        crate::display_recovery::arrangement(Some((before.clone(), desired.clone())))?;
        let apply = (|| -> Result<()> {
            let ids = desired
                .iter()
                .filter(|n| n.active)
                .map(|n| n.device_id.clone())
                .collect::<Vec<_>>();
            let old = before
                .nodes
                .iter()
                .filter(|n| n.active)
                .map(|n| n.device_id.clone())
                .collect::<Vec<_>>();
            if ids != old {
                Topology::set_active(&ids)?;
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
            before.restore()?;
            crate::display_recovery::arrangement(None)?;
            return Err(error);
        }
        state.before = Some(before);
        state.applied = desired;
        state.users = 1;
        state.key = key;
        Ok(Self { arrangement })
    }
    /// Reapply the original stream policy after its owned VDD is recreated,
    /// retaining the original restoration snapshot and recovery journal.
    pub fn reapply(&self, output: &str, retained: &[String]) -> Result<()> {
        let mut state = state().lock().unwrap();
        let monitors = crate::display::monitors()?;
        let target = monitors
            .iter()
            .find(|m| m.matches(output))
            .ok_or_else(|| anyhow::anyhow!("recovered arrangement target unavailable"))?;
        let current = Snapshot::capture()?;
        let retained: Vec<_> = monitors
            .iter()
            .filter(|m| retained.contains(&m.display_name) || retained.contains(&m.device_id))
            .map(|m| m.device_id.clone())
            .collect();
        let desired = self
            .arrangement
            .compose(&current.nodes, &target.device_id, &retained)?;
        if let Some(before) = &state.before {
            crate::display_recovery::arrangement(Some((before.clone(), desired.clone())))?;
        }
        Topology::set_active(
            &desired
                .iter()
                .filter(|n| n.active)
                .map(|n| n.device_id.clone())
                .collect::<Vec<_>>(),
        )?;
        Topology::query()?.set_positions(
            &desired
                .iter()
                .filter(|n| n.active)
                .map(|n| (n.device_id.clone(), n.desired_position))
                .collect(),
        )?;
        state.applied = desired;
        state.key = format!("{:?}:{}", self.arrangement, target.device_id);
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = state().lock().unwrap();
        state.users -= 1;
        if state.users != 0 {
            return;
        }
        let restored = (|| -> Result<()> {
            let current = Topology::query()?.nodes()?;
            if matches(&current, &state.applied)
                && let Some(before) = &state.before
            {
                before.restore()?;
            }
            crate::display_recovery::arrangement(None)
        })();
        if let Err(error) = restored {
            tracing::warn!(%error,"display arrangement restoration remains pending");
        }
        state.before = None;
        state.applied.clear();
        state.key.clear();
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
            if matches(&Topology::query()?.nodes()?, &state.applied)
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
