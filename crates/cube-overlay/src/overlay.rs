//! The overlay's behaviour, independent of Wayland: when to show, banners for alerts, the new cube flash and the
//! optional alert command.

use crate::radar::{self, Banner, Hud, Look, AMBER, GREEN, RED};
use crate::source::{Health, Source};
use crate::text::Fonts;
use cube_core::geo::compass;
use cube_core::snapshot::{Alert, AlertKind, RadarSnapshot};
use tiny_skia::Pixmap;

/// The radar stays up this long after the last cube is gone (as AionCore does).
const HIDE_AFTER_MS: u64 = 3_000;
const FLASH_MS: u64 = 2_000;
/// Shown this long after starting and after the capture status changes, so you can see it runs.
const SHOW_STATUS_MS: u64 = 6_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Shown while a cube is around, a banner is up or the capture has a problem.
    Auto,
    Always,
    Hidden,
}

pub struct Overlay {
    source: Box<dyn Source>,
    pub look: Look,
    fonts: Fonts,
    alert_cmd: Option<String>,
    pub visibility: Visibility,
    snap: RadarSnapshot,
    banner: Option<(Banner, u64)>,
    flash_from: Option<u64>,
    last_wanted_ms: Option<u64>,
    wall_ms: u64,
    show_until_ms: u64,
    last_health: Option<Health>,
    was_visible: bool,
}

impl Overlay {
    pub fn new(source: Box<dyn Source>, look: Look, fonts: Fonts, alert_cmd: Option<String>, visibility: Visibility) -> Overlay {
        Overlay { source, look, fonts, alert_cmd, visibility, snap: RadarSnapshot::default(), banner: None, flash_from: None, last_wanted_ms: None, wall_ms: 0, show_until_ms: 0, last_health: None, was_visible: false }
    }

    pub fn step(&mut self, wall_ms: u64) {
        self.wall_ms = wall_ms;
        self.source.step(wall_ms);
        self.snap = self.source.snapshot();
        for alert in self.source.drain_alerts() {
            self.on_alert(&alert, wall_ms);
        }
        if self.banner.as_ref().is_some_and(|(_, until)| wall_ms >= *until) {
            self.banner = None;
        }
        let status = self.source.status();
        if self.last_health != Some(status.health) {
            // At start and whenever the capture changes state (waiting, reading the game), show it a moment.
            self.show_until_ms = wall_ms + SHOW_STATUS_MS;
            self.last_health = Some(status.health);
        }
        let wanted = self.snap.open_cubes().next().is_some() || self.banner.is_some() || status.health == Health::Error || wall_ms < self.show_until_ms;
        if wanted {
            self.last_wanted_ms = Some(wall_ms);
        }
        let visible = self.visible();
        if visible != self.was_visible {
            self.was_visible = visible;
            log::info!("radar {}", if visible { "shown" } else { "hidden until a cube is near (--always keeps it up)" });
        }
    }

    fn on_alert(&mut self, alert: &Alert, wall_ms: u64) {
        let (text, color, ms, flash) = match alert.kind {
            AlertKind::Appeared => ("New cube nearby", AMBER, 3_500, true),
            AlertKind::StillThere => ("Cube still there", AMBER, 3_500, true),
            AlertKind::OpenedByOther => ("Opened by another player", RED, 4_000, false),
            AlertKind::OpenedByYou => ("Cube opened", GREEN, 2_500, false),
            AlertKind::Learned => ("New cube type learned", GREEN, 3_000, false),
        };
        self.banner = Some((Banner { text: text.into(), color }, wall_ms + ms));
        if flash {
            self.flash_from = Some(wall_ms);
            self.run_alert_cmd(alert);
        }
    }

    fn run_alert_cmd(&self, alert: &Alert) {
        let Some(cmd) = &self.alert_cmd else { return };
        let c = &alert.cube;
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", cmd])
            .env("CUBE_ALERT", format!("{:?}", alert.kind).to_lowercase())
            .env("CUBE_DISTANCE", c.distance_m.map(|d| format!("{d:.0}")).unwrap_or_default())
            .env("CUBE_HEIGHT", c.height_m.map(|h| format!("{h:.0}")).unwrap_or_default())
            .env("CUBE_DIRECTION", c.bearing.map(|b| compass(b - self.look.north_offset_deg).to_string()).unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null());
        match command.spawn() {
            // Reap it on a thread of its own so it never becomes a zombie.
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(e) => log::warn!("could not run the alert command: {e}"),
        }
    }

    /// Whether the radar should be drawn now.
    pub fn visible(&self) -> bool {
        match self.visibility {
            Visibility::Always => true,
            Visibility::Hidden => false,
            Visibility::Auto => self.last_wanted_ms.is_some_and(|t| self.wall_ms.saturating_sub(t) <= HIDE_AFTER_MS),
        }
    }

    pub fn hud(&self) -> Hud {
        let flash = self.flash_from.map_or(0.0, |t| {
            let p = self.wall_ms.saturating_sub(t) as f32 / FLASH_MS as f32;
            if p >= 1.0 {
                0.0
            } else {
                // Three pulses fading out.
                (1.0 - p) * ((p * 3.0 * std::f32::consts::TAU).cos() * 0.5 + 0.5)
            }
        });
        Hud { anim_ms: self.wall_ms, status: self.source.status(), hint: self.source.hint(), banner: self.banner.as_ref().map(|b| b.0.clone()), flash }
    }

    /// Draws the current frame; a hidden overlay is fully transparent.
    pub fn draw(&self, pix: &mut Pixmap, scale: f32) {
        if self.visible() {
            radar::render(pix, scale, &self.snap, &self.look, &self.hud(), &self.fonts);
        } else {
            pix.fill(tiny_skia::Color::TRANSPARENT);
        }
    }

    pub fn toggle_hidden(&mut self) {
        self.visibility = if self.visibility == Visibility::Hidden { Visibility::Auto } else { Visibility::Hidden };
        log::info!("visibility: {:?}", self.visibility);
    }

    pub fn toggle_always(&mut self) {
        self.visibility = if self.visibility == Visibility::Always { Visibility::Auto } else { Visibility::Always };
        log::info!("visibility: {:?}", self.visibility);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::Demo;
    use cube_core::cubes::CubeBook;

    #[test]
    fn shows_itself_for_cubes_and_hides_after_they_are_gone() {
        let start = 1_791_722_000_000;
        let demo = Demo::new(start, CubeBook::default());
        let mut o = Overlay::new(Box::new(demo), Look::default(), Fonts::load(None), None, Visibility::Auto);
        o.step(start);
        assert!(o.visible(), "shown a moment at start");
        o.step(start + 1_000);
        o.step(start + 10_000 - 4);
        let mut quiet = Overlay::new(Box::new(Demo::new(start, CubeBook::default())), Look::default(), Fonts::load(None), None, Visibility::Auto);
        quiet.step(start);
        quiet.step(start + 1_400);
        quiet.show_until_ms = 0;
        quiet.last_wanted_ms = None;
        quiet.step(start + 1_450);
        assert!(!quiet.visible(), "nothing to show before the first cube");
        o.step(start + 2_000);
        assert!(o.visible(), "cube C appeared");
        assert!(o.hud().banner.is_some());
        assert!(o.hud().flash > 0.0);
        // Cube C left view at 26 s but is still there to be opened, so the radar stays up until the dungeon at
        // 72 s clears the map; the next round brings cube C back at 76.5 s.
        o.step(start + 71_900);
        assert!(o.visible());
        o.step(start + 72_100);
        assert!(o.visible(), "a few seconds of grace");
        o.step(start + 75_300);
        assert!(!o.visible(), "hidden a few seconds after the last cube");
        o.step(start + 77_000);
        assert!(o.visible(), "and back for cube C");
        o.toggle_always();
        assert!(o.visible());
        o.toggle_hidden();
        assert!(!o.visible());
    }
}
