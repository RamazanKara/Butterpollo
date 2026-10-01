//! Stable control tile identities used by existing Artemis/Moonlight clients.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    Resume,
    DisconnectMonitor,
    DisconnectInput,
    Terminate,
    Monitor,
    Input,
    RunningGame,
}
pub fn identify(id: u32, uuid: &str) -> Option<Control> {
    for (offset, control) in [
        (1, Control::Resume),
        (2, Control::DisconnectMonitor),
        (3, Control::DisconnectInput),
        (4, Control::Terminate),
        (5, Control::Monitor),
        (6, Control::Input),
        (7, Control::RunningGame),
    ] {
        if id == 2147483500 + offset
            || id == 2147483600 + offset
            || (matches!(offset, 1 | 4 | 5 | 6) && id == 2147483510 + offset)
            || uuid == format!("9a1c5a25-58fe-40e0-b9aa-7d3f0000000{offset}")
        {
            return Some(control);
        }
    }
    None
}

pub fn running_game_id(app: u32) -> u32 {
    let source = app & 0x7fff_ffff;
    let mut candidate = source ^ 0x4000_0000;
    while candidate == 0 || candidate == source || identify(candidate, "").is_some() {
        candidate = if candidate == 0x7fff_ffff {
            1
        } else {
            candidate + 1
        };
    }
    candidate
}

#[derive(Clone, Default)]
pub struct Entry {
    pub id: u32,
    pub uuid: String,
    pub title: String,
    pub index: usize,
    pub art_version: String,
}
#[derive(Clone)]
pub struct Game {
    pub app: Entry,
    pub owner: String,
    pub generation: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    None,
    Monitor,
    Input,
}
pub struct Projection {
    pub current_game: u32,
    pub entries: Vec<Entry>,
}
fn synthetic(control: Control, secondary: bool) -> Entry {
    let offset = match control {
        Control::Resume => 1,
        Control::DisconnectMonitor => 2,
        Control::DisconnectInput => 3,
        Control::Terminate => 4,
        Control::Monitor => 5,
        Control::Input => 6,
        Control::RunningGame => 7,
    };
    let title = match control {
        Control::Resume => "Resume",
        Control::DisconnectMonitor => "Disconnect Monitor",
        Control::DisconnectInput => "Disconnect Input",
        Control::Terminate => "Terminate",
        Control::Monitor => "Remote Monitor",
        Control::Input => "Remote Input",
        Control::RunningGame => "",
    };
    let rank = if secondary {
        match control {
            Control::Monitor => "    ",
            Control::Input => "   ",
            Control::Resume => "  ",
            Control::Terminate => " ",
            _ => "",
        }
    } else {
        ""
    };
    Entry {
        id: 2147483500 + offset + if secondary { 10 } else { 0 },
        uuid: format!("9a1c5a25-58fe-40e0-b9aa-7d3f0000000{offset}"),
        title: format!("{rank}{title}"),
        art_version: "remote-session-v6".into(),
        ..Default::default()
    }
}
pub fn project(
    client: &str,
    owner: Owner,
    game: Option<&Game>,
    input_enabled: bool,
    configured: Vec<Entry>,
) -> Projection {
    let mut entries = configured
        .into_iter()
        .filter(|entry| {
            !matches!(
                identify(entry.id, &entry.uuid),
                Some(Control::Monitor | Control::Input)
            ) && ![
                "Remote Input",
                "Remote Monitor",
                "Virtual Display",
                "Terminate",
            ]
            .iter()
            .any(|name| entry.title.trim().eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    if owner == Owner::Monitor {
        return Projection {
            current_game: 0,
            entries: vec![
                synthetic(Control::Resume, false),
                synthetic(Control::DisconnectMonitor, false),
            ],
        };
    }
    let owns_game = game.is_some_and(|game| game.owner == client);
    let secondary = game.is_some() && !owns_game;
    if let Some(game) = game.filter(|_| secondary) {
        let mut active = game.app.clone();
        active.id = running_game_id(game.app.id);
        active.uuid = "9a1c5a25-58fe-40e0-b9aa-7d3f00000007".into();
        active.title = format!("     {}", game.app.title);
        let mut prefix = vec![
            active,
            synthetic(Control::Resume, true),
            synthetic(Control::Terminate, true),
        ];
        prefix.append(&mut entries);
        entries = prefix;
    }
    if input_enabled && owner != Owner::Input {
        entries.push(synthetic(Control::Input, secondary));
    }
    entries.push(synthetic(Control::Monitor, secondary));
    Projection {
        current_game: game.filter(|_| owns_game).map_or(0, |game| game.app.id),
        entries,
    }
}
pub fn ordered_title(title: &str, count: usize, index: usize) -> String {
    let bits = (usize::BITS - count.saturating_sub(1).leading_zeros()).max(1);
    let prefix = (0..bits)
        .rev()
        .map(|bit| {
            if index & (1usize << bit) == 0 {
                '\u{200b}'
            } else {
                '\u{200c}'
            }
        })
        .collect::<String>();
    format!("{prefix}{title}")
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confirmation {
    Terminate,
    Replace,
}
#[derive(Default)]
pub struct Confirmations {
    pending: std::collections::BTreeMap<(String, Confirmation), (String, u32, std::time::Instant)>,
}
impl Confirmations {
    pub fn confirm(
        &mut self,
        client: &str,
        action: Confirmation,
        generation: &str,
        app: u32,
        now: std::time::Instant,
    ) -> bool {
        self.pending.retain(|_, (_, _, until)| *until > now);
        let key = (client.into(), action);
        if let Some((saved_generation, saved_app, _)) = self.pending.remove(&key)
            && saved_generation == generation
            && saved_app == app
        {
            return true;
        }
        if self.pending.len() < 1024 {
            self.pending.insert(
                key,
                (
                    generation.into(),
                    app,
                    now + std::time::Duration::from_secs(60),
                ),
            );
        }
        false
    }
    pub fn clear(&mut self, client: &str, action: Confirmation) {
        self.pending.remove(&(client.into(), action));
    }
    pub fn active(&self, client: &str, action: Confirmation, generation: &str) -> bool {
        self.pending
            .get(&(client.into(), action))
            .is_some_and(|(saved, _, until)| {
                saved == generation && *until > std::time::Instant::now()
            })
    }
}

pub fn tiles() -> [(u32, &'static str); 6] {
    [
        (2147483501, "Resume"),
        (2147483502, "Disconnect Monitor"),
        (2147483503, "Disconnect Input"),
        (2147483504, "Terminate"),
        (2147483505, "Remote Monitor"),
        (2147483506, "Remote Input"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previous_catalogue_transitions_preserve_game_monitor_and_secondary_controls() {
        let entry = Entry {
            id: 73,
            title: "Game".into(),
            ..Default::default()
        };
        let game = Game {
            app: entry.clone(),
            owner: "owner".into(),
            generation: "1".into(),
        };
        let entries = vec![entry];
        let primary = project("owner", Owner::None, Some(&game), true, entries.clone());
        assert_eq!(primary.current_game, 73);
        assert_eq!(
            primary.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            [73, 2147483506, 2147483505]
        );
        let secondary = project("other", Owner::None, Some(&game), true, entries.clone());
        assert_eq!(secondary.current_game, 0);
        assert_eq!(
            secondary.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            [
                running_game_id(73),
                2147483511,
                2147483514,
                73,
                2147483516,
                2147483515
            ]
        );
        let monitor = project("other", Owner::Monitor, Some(&game), true, entries.clone());
        assert_eq!(
            monitor.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            [2147483501, 2147483502]
        );
        let input = project("other", Owner::Input, None, true, entries);
        assert_eq!(
            input.entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            [73, 2147483505]
        );
        let titles: Vec<_> = ["Z", "A", "M"]
            .iter()
            .enumerate()
            .map(|(i, t)| ordered_title(t, 3, i))
            .collect();
        let mut sorted = titles.clone();
        sorted.sort();
        assert_eq!(sorted, titles);
    }
    #[test]
    fn confirmations_are_scoped_to_client_action_generation_target_and_expiry() {
        use std::time::{Duration, Instant};
        let mut book = Confirmations::default();
        let now = Instant::now();
        assert!(!book.confirm("secondary", Confirmation::Replace, "game-1", 2, now));
        assert!(!book.confirm("other", Confirmation::Replace, "game-1", 2, now));
        assert!(!book.confirm("secondary", Confirmation::Terminate, "game-1", 2, now));
        assert!(book.confirm(
            "secondary",
            Confirmation::Replace,
            "game-1",
            2,
            now + Duration::from_secs(59)
        ));
        assert!(!book.confirm("secondary", Confirmation::Replace, "game-2", 2, now));
        assert!(!book.confirm("secondary", Confirmation::Replace, "game-3", 2, now));
        assert!(!book.confirm("secondary", Confirmation::Replace, "game-3", 3, now));
        assert!(!book.confirm(
            "secondary",
            Confirmation::Replace,
            "game-3",
            3,
            now + Duration::from_secs(60)
        ));
        book.clear("secondary", Confirmation::Replace);
        assert!(!book.confirm(
            "secondary",
            Confirmation::Replace,
            "game-3",
            3,
            now + Duration::from_secs(61)
        ));
    }
    #[test]
    fn active_game_projection_has_a_distinct_stable_resumable_identity() {
        for app in [1, 73, 2147483501, 0x4000_0000, 0x7fff_ffff] {
            let projected = running_game_id(app);
            assert_ne!(projected, app);
            assert_ne!(projected, 0);
            assert!(identify(projected, "").is_none());
        }
        assert_eq!(identify(2147483507, ""), Some(Control::RunningGame));
    }
}
