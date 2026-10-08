//! What the notification-area icon says about the running app, and the
//! notifications Vibepollo shows when an app starts, pauses, resumes and
//! stops.

/// The icon's state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Icon {
    #[default]
    Idle,
    Streaming,
    Paused,
}
/// The running app and whether a client streams it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub app: Option<String>,
    pub streaming: bool,
}
/// A notification: title and text.
pub type Notice = (&'static str, String);
/// Follows the running app. An app counts as streaming from its launch
/// until its stream ends, so the client connecting after the launch is no
/// pause and no resume.
#[derive(Clone, Debug, Default)]
pub struct Tracker {
    status: Status,
    paused: bool,
}
impl Tracker {
    pub fn icon(&self) -> Icon {
        match (&self.status.app, self.paused) {
            (None, _) => Icon::Idle,
            (Some(_), false) => Icon::Streaming,
            (Some(_), true) => Icon::Paused,
        }
    }
    pub fn app(&self) -> Option<&str> {
        self.status.app.as_deref()
    }
    /// The tooltip line, while an app runs.
    pub fn summary(&self) -> Option<String> {
        let app = self.status.app.as_ref()?;
        Some(if self.paused {
            format!("{app} is paused")
        } else {
            format!("Streaming {app}")
        })
    }
    /// Take the current status. Returns None when nothing changed, else
    /// the notification for the change, if it has one.
    pub fn update(&mut self, current: Status) -> Option<Option<Notice>> {
        if current == self.status {
            return None;
        }
        let previous = std::mem::replace(&mut self.status, current);
        let current = &self.status;
        let notice = match (&previous.app, &current.app) {
            (_, Some(app)) if previous.app.as_ref() != Some(app) => {
                self.paused = false;
                Some(("App launched", format!("Streaming started for {app}")))
            }
            (Some(app), None) => {
                self.paused = false;
                Some(("App stopped", format!("Streaming stopped for {app}")))
            }
            (Some(_), Some(app)) if previous.streaming && !current.streaming => {
                self.paused = true;
                Some((
                    "Stream paused",
                    format!("{app} keeps running; reconnect from Moonlight to resume."),
                ))
            }
            (Some(_), Some(app)) if self.paused && current.streaming => {
                self.paused = false;
                Some(("Stream resumed", format!("Streaming {app} again")))
            }
            _ => None,
        };
        Some(notice)
    }
}

/// Draw the state's dot, ringed in white, in the bottom-right corner of a
/// `width`×`height` icon image: BGRA rows with straight alpha. False,
/// leaving the image as is, for Idle or an image without alpha.
pub fn badge(pixels: &mut [u8], width: usize, height: usize, icon: Icon) -> bool {
    let color: [u8; 3] = match icon {
        Icon::Idle => return false,
        Icon::Streaming => [0x43, 0xa0, 0x2e],
        Icon::Paused => [0x22, 0x99, 0xd2],
    };
    if width == 0
        || pixels.len() < width * height * 4
        || !pixels.as_chunks::<4>().0.iter().any(|p| p[3] != 0)
    {
        return false;
    }
    let size = width.min(height) as f32;
    let radius = size / 4.0;
    let ring = (size / 16.0).max(1.0);
    let (cx, cy) = (width as f32 - radius, height as f32 - radius);
    for y in 0..height {
        for x in 0..width {
            let distance = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
            if coverage == 0.0 {
                continue;
            }
            let fill = if distance > radius - ring {
                [0xff; 3]
            } else {
                color
            };
            let pixel = &mut pixels[(y * width + x) * 4..][..4];
            let below = f32::from(pixel[3]) / 255.0 * (1.0 - coverage);
            let alpha = coverage + below;
            for c in 0..3 {
                pixel[c] = ((f32::from(fill[c]) * coverage + f32::from(pixel[c]) * below) / alpha)
                    .round() as u8;
            }
            pixel[3] = (alpha * 255.0).round() as u8;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(app: Option<&str>, streaming: bool) -> Status {
        Status {
            app: app.map(str::to_owned),
            streaming,
        }
    }
    fn title(change: Option<Option<Notice>>) -> Option<&'static str> {
        change.expect("a change").map(|(title, _)| title)
    }
    #[test]
    fn launch_connect_pause_resume_and_stop() {
        let mut tray = Tracker::default();
        assert_eq!(tray.icon(), Icon::Idle);
        assert_eq!(tray.summary(), None);
        assert_eq!(
            tray.update(status(Some("Desktop"), false)),
            Some(Some((
                "App launched",
                "Streaming started for Desktop".to_owned()
            )))
        );
        assert_eq!(tray.icon(), Icon::Streaming);
        // The client connects after the launch: no notification.
        assert_eq!(tray.update(status(Some("Desktop"), true)), Some(None));
        assert_eq!(tray.update(status(Some("Desktop"), true)), None);
        assert_eq!(
            title(tray.update(status(Some("Desktop"), false))),
            Some("Stream paused")
        );
        assert_eq!(tray.icon(), Icon::Paused);
        assert_eq!(tray.summary().as_deref(), Some("Desktop is paused"));
        assert_eq!(
            title(tray.update(status(Some("Desktop"), true))),
            Some("Stream resumed")
        );
        assert_eq!(tray.icon(), Icon::Streaming);
        assert_eq!(tray.app(), Some("Desktop"));
        assert_eq!(
            tray.update(status(None, false)),
            Some(Some((
                "App stopped",
                "Streaming stopped for Desktop".to_owned()
            )))
        );
        assert_eq!(tray.icon(), Icon::Idle);
        assert_eq!(tray.app(), None);
    }
    #[test]
    fn switching_apps_announces_the_new_one_and_clears_the_pause() {
        let mut tray = Tracker::default();
        tray.update(status(Some("Desktop"), true));
        tray.update(status(Some("Desktop"), false));
        assert_eq!(tray.icon(), Icon::Paused);
        assert_eq!(
            title(tray.update(status(Some("Steam Big Picture"), false))),
            Some("App launched")
        );
        assert_eq!(tray.icon(), Icon::Streaming);
        assert_eq!(
            tray.summary().as_deref(),
            Some("Streaming Steam Big Picture")
        );
    }
    #[test]
    fn the_badge_marks_the_corner_of_icons_with_alpha() {
        let (width, height) = (16, 16);
        let red = [0x10, 0x4b, 0xad, 0xff];
        let mut pixels = red.repeat(width * height);
        assert!(!badge(&mut pixels, width, height, Icon::Idle));
        assert!(badge(&mut pixels, width, height, Icon::Streaming));
        let at = |pixels: &[u8], x: usize, y: usize| pixels[(y * width + x) * 4..][..4].to_vec();
        assert_eq!(at(&pixels, 0, 0), red);
        assert_eq!(at(&pixels, 12, 12), [0x43, 0xa0, 0x2e, 0xff]);
        // The ring's edge, nearly all white.
        assert!(at(&pixels, 12, 8).iter().all(|c| *c >= 0xf0));
        let mut paused = red.repeat(width * height);
        badge(&mut paused, width, height, Icon::Paused);
        assert_eq!(at(&paused, 12, 12), [0x22, 0x99, 0xd2, 0xff]);
        // A transparent corner gets the dot alone, opaque.
        let mut clear = [0u8; 4].repeat(width * height);
        clear[3] = 0xff;
        assert!(badge(&mut clear, width, height, Icon::Paused));
        assert_eq!(at(&clear, 12, 12), [0x22, 0x99, 0xd2, 0xff]);
        let mut flat = [0u8; 4].repeat(width * height);
        assert!(!badge(&mut flat, width, height, Icon::Streaming));
        assert!(flat.iter().all(|b| *b == 0));
    }
}
