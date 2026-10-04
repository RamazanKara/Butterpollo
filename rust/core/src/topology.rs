//! Checked client display placements independent of Windows display handles.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Physical,
    Client,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Left,
    Right,
    Above,
    Below,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Start,
    Center,
    End,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Placement {
    pub anchor_kind: Kind,
    pub anchor_id: String,
    pub edge: Edge,
    pub alignment: Alignment,
    pub gap_px: u32,
    #[serde(default)]
    pub primary: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Layout {
    pub version: u32,
    pub placements: BTreeMap<String, Placement>,
}
impl Default for Layout {
    fn default() -> Self {
        Self {
            version: 1,
            placements: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub kind: Kind,
    pub active: bool,
    pub primary: bool,
    pub desired_position: Position,
    pub mode: Mode,
    pub device_id: String,
}
impl Layout {
    pub fn validate(&self, clients: &[String], physical: &[String]) -> Result<()> {
        if self.version != 1 || self.placements.len() > 256 {
            bail!("unsupported display layout version or size");
        }
        let mut primary = 0;
        for (id, p) in &self.placements {
            if !clients.contains(id) || p.gap_px > 16384 || p.anchor_id.is_empty() {
                bail!("invalid display placement");
            }
            let known = if p.anchor_kind == Kind::Client {
                clients
            } else {
                physical
            };
            if !known.contains(&p.anchor_id) || id == &p.anchor_id {
                bail!("unknown or self-referencing display anchor");
            }
            if p.primary {
                primary += 1;
            }
            let mut visited = BTreeSet::new();
            let mut current = id;
            while let Some(p) = self
                .placements
                .get(current)
                .filter(|p| p.anchor_kind == Kind::Client)
            {
                if !visited.insert(current.clone()) {
                    bail!("display anchor cycle");
                }
                current = &p.anchor_id;
            }
        }
        if primary > 1 {
            bail!("only one client display may be primary");
        }
        Ok(())
    }
    pub fn compose(&self, nodes: &[Node]) -> Result<Vec<Node>> {
        if nodes.len() > 256 {
            bail!("too many display nodes");
        }
        let all: BTreeMap<_, _> = nodes
            .iter()
            .filter(|n| n.active)
            .map(|n| (n.id.clone(), n.clone()))
            .collect();
        let mut placed: BTreeMap<_, _> = all
            .iter()
            .filter(|(_, n)| n.kind == Kind::Physical)
            .map(|(id, n)| (id.clone(), n.clone()))
            .collect();
        let mut visiting = BTreeSet::new();
        for (id, node) in &all {
            if node.kind == Kind::Client {
                self.place(id, &all, &mut placed, &mut visiting)?;
            }
        }
        let primary = placed
            .values()
            .find(|n| {
                n.kind == Kind::Client && self.placements.get(&n.id).is_some_and(|p| p.primary)
            })
            .or_else(|| placed.values().find(|n| n.primary))
            .or_else(|| placed.values().next())
            .map(|n| (n.id.clone(), n.desired_position));
        if let Some((id, origin)) = primary {
            for node in placed.values_mut() {
                node.primary = node.id == id;
                node.desired_position = checked(
                    i64::from(node.desired_position.x) - i64::from(origin.x),
                    i64::from(node.desired_position.y) - i64::from(origin.y),
                )?;
            }
        }
        Ok(placed.into_values().collect())
    }
    fn place(
        &self,
        id: &str,
        all: &BTreeMap<String, Node>,
        placed: &mut BTreeMap<String, Node>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<()> {
        if placed.contains_key(id) {
            return Ok(());
        }
        if !visiting.insert(id.into()) {
            bail!("display anchor cycle");
        }
        let mut node = all
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown display node"))?
            .clone();
        let p = self.placements.get(id);
        if let Some(p) = p
            && all.contains_key(&p.anchor_id)
        {
            self.place(&p.anchor_id, all, placed, visiting)?;
        }
        let anchor = p.and_then(|p| placed.get(&p.anchor_id));
        node.desired_position = if let (Some(p), Some(a)) = (p, anchor) {
            let x = i64::from(a.desired_position.x);
            let y = i64::from(a.desired_position.y);
            let w = i64::from(a.mode.width);
            let h = i64::from(a.mode.height);
            let nw = i64::from(node.mode.width);
            let nh = i64::from(node.mode.height);
            let gap = i64::from(p.gap_px);
            let align = |size: i64, new: i64| match p.alignment {
                Alignment::Start => 0,
                Alignment::Center => (size - new) / 2,
                Alignment::End => size - new,
            };
            match p.edge {
                Edge::Left => checked(x - nw - gap, y + align(h, nh))?,
                Edge::Right => checked(x + w + gap, y + align(h, nh))?,
                Edge::Above => checked(x + align(w, nw), y - nh - gap)?,
                Edge::Below => checked(x + align(w, nw), y + h + gap)?,
            }
        } else {
            checked(
                placed
                    .values()
                    .map(|n| i64::from(n.desired_position.x) + i64::from(n.mode.width))
                    .max()
                    .unwrap_or(0),
                0,
            )?
        };
        visiting.remove(id);
        placed.insert(id.into(), node);
        Ok(())
    }
}
fn checked(x: i64, y: i64) -> Result<Position> {
    if x.abs() > 1_000_000 || y.abs() > 1_000_000 {
        bail!("display position exceeds the limit");
    }
    Ok(Position {
        x: x as i32,
        y: y as i32,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str, kind: Kind, w: u32, h: u32) -> Node {
        Node {
            id: id.into(),
            label: id.into(),
            kind,
            active: true,
            primary: kind == Kind::Physical,
            desired_position: Position { x: 0, y: 0 },
            mode: Mode {
                width: w,
                height: h,
                refresh_hz: 60.,
            },
            device_id: id.into(),
        }
    }
    fn p(anchor: &str, kind: Kind, edge: Edge) -> Placement {
        Placement {
            anchor_id: anchor.into(),
            anchor_kind: kind,
            edge,
            alignment: Alignment::Center,
            gap_px: 0,
            primary: false,
        }
    }
    #[test]
    fn dependencies_and_primary_origin_preserve_relative_geometry() {
        let mut layout = Layout::default();
        layout
            .placements
            .insert("a".into(), p("physical", Kind::Physical, Edge::Left));
        layout
            .placements
            .insert("b".into(), p("a", Kind::Client, Edge::Below));
        layout.placements.get_mut("b").unwrap().primary = true;
        layout
            .validate(&["a".into(), "b".into()], &["physical".into()])
            .unwrap();
        let nodes = layout
            .compose(&[
                node("physical", Kind::Physical, 1920, 1080),
                node("a", Kind::Client, 1280, 720),
                node("b", Kind::Client, 640, 480),
            ])
            .unwrap();
        let b = nodes.iter().find(|n| n.id == "b").unwrap();
        assert!(b.primary);
        assert_eq!(b.desired_position, Position { x: 0, y: 0 });
        let a = nodes.iter().find(|n| n.id == "a").unwrap();
        assert_eq!(a.desired_position, Position { x: -320, y: -720 });
        let physical = nodes.iter().find(|n| n.id == "physical").unwrap();
        assert_eq!(physical.desired_position, Position { x: 960, y: -900 });
        layout.placements.get_mut("a").unwrap().anchor_id = "b".into();
        layout.placements.get_mut("a").unwrap().anchor_kind = Kind::Client;
        assert!(
            layout
                .validate(&["a".into(), "b".into()], &["physical".into()])
                .is_err()
        );
        assert!(
            layout
                .compose(&[node("a", Kind::Client, 1, 1), node("b", Kind::Client, 1, 1)])
                .is_err()
        );
    }
}
