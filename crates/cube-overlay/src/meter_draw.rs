//! Draws the DPS meter window and says what a click at a point means.
//!
//! ```text
//!  Awakened Alturadon             62%  auto  [—]     <- click: next enemy (auto, then each enemy); — minimizes
//!  ▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔▔        HP bar
//!  [ Damage ][  DPS  ][ 5 s DPS ]  [↺]               <- click: mode; ↺ resets the fight
//!  1 Mirro           ████████████████  31%  24.1k     <- class coloured bar, your row outlined
//!  2 Seraphel        ████████████      26%  20.3k
//!  ...
//!  Group 77.9k/s                          1:12        <- total in the mode, time under attack
//! ```
//!
//! Minimized, the meter is a single bar: the mode, your value and the group's, and the button to open it again.
//!
//! Everything else in the window drags it.

use crate::dps::{clock, short_number, Mode, Row, View};
use crate::radar::{AMBER, DIM, GREEN, RED, WHITE};
use crate::source::{Health, Status};
use crate::text::{Fonts, Rgba};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub const W: f32 = 300.0;
const PAD: f32 = 8.0;
const TABS_Y: f32 = 36.0;
const TABS_H: f32 = 22.0;
const RESET_W: f32 = 26.0;
const ROWS_Y: f32 = 64.0;
const ROW_H: f32 = 22.0;
const FOOTER_H: f32 = 30.0;
/// The minimize button, at the same place open and minimized.
const MIN_W: f32 = 22.0;
const MIN_X: f32 = W - PAD - MIN_W;
const MIN_Y: f32 = 6.0;
const MIN_H: f32 = 20.0;
/// Height of the minimized bar.
pub const MINI_H: f32 = 32.0;

/// Height of the window for `rows` rows (at least one line is always drawn).
pub fn height(rows: usize) -> f32 {
    ROWS_Y + rows.max(1) as f32 * ROW_H + FOOTER_H
}

/// Height of what is drawn: the bar when minimized, else the panel with `rows` rows.
pub fn content_height(rows: usize, minimized: bool) -> f32 {
    if minimized {
        MINI_H
    } else {
        height(rows)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Target,
    Mode(Mode),
    Reset,
    /// The minimize (or, minimized, the open again) button.
    Minimize,
    /// Anywhere else on the window: drags it.
    Drag,
    /// Outside the drawn panel.
    Outside,
}

/// What a click at logical `(x, y)` does, with `shown` rows on screen or the meter minimized.
pub fn hit(x: f32, y: f32, shown: usize, minimized: bool) -> Hit {
    if !(0.0..=W).contains(&x) || !(0.0..=content_height(shown, minimized)).contains(&y) {
        return Hit::Outside;
    }
    // A little more than the button's drawn square, so it is easy to hit.
    if x >= MIN_X - 3.0 && y <= MIN_Y + MIN_H + 3.0 {
        return Hit::Minimize;
    }
    if minimized {
        return Hit::Drag;
    }
    if y < 32.0 {
        return Hit::Target;
    }
    if (TABS_Y..TABS_Y + TABS_H).contains(&y) {
        let reset_x = W - PAD - RESET_W;
        if x >= reset_x && x <= W - PAD {
            return Hit::Reset;
        }
        let tab_w = (reset_x - 6.0 - PAD) / 3.0;
        if x >= PAD && x < PAD + 3.0 * tab_w {
            return Hit::Mode(Mode::ALL[(((x - PAD) / tab_w) as usize).min(2)]);
        }
    }
    Hit::Drag
}

/// Class colours by aion2-tui's class ids.
pub fn class_color(class_id: Option<u32>) -> Rgba {
    match class_id {
        Some(11) => Rgba(232, 122, 76, 255),  // Gladiator
        Some(12) => Rgba(226, 188, 84, 255),  // Templar
        Some(13) => Rgba(178, 110, 232, 255), // Assassin
        Some(14) => Rgba(112, 196, 104, 255), // Ranger
        Some(15) => Rgba(104, 146, 255, 255), // Sorcerer
        Some(16) => Rgba(70, 196, 196, 255),  // Elementalist
        Some(17) => Rgba(236, 226, 168, 255), // Cleric
        Some(18) => Rgba(255, 150, 196, 255), // Chanter
        Some(19) => Rgba(224, 84, 84, 255),   // Brawler
        _ => Rgba(150, 154, 166, 255),
    }
}

/// The rows to show: the best `max_rows`, with your own row kept in place of the last one if it falls below.
pub fn visible_rows(rows: &[Row], max_rows: usize) -> Vec<(usize, &Row)> {
    let max_rows = max_rows.max(1);
    let mut shown: Vec<(usize, &Row)> = rows.iter().enumerate().take(max_rows).collect();
    if let Some((i, me)) = rows.iter().enumerate().find(|(_, r)| r.is_self) {
        if i >= max_rows {
            shown.pop();
            shown.push((i, me));
        }
    }
    shown
}

struct Canvas<'a> {
    pix: &'a mut Pixmap,
    s: f32,
    fonts: &'a Fonts,
}

impl Canvas<'_> {
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgba) {
        if let Some(p) = rounded(x * self.s, y * self.s, w * self.s, h * self.s, r * self.s) {
            let mut paint = Paint::default();
            paint.set_color(color.to_skia());
            paint.anti_alias = true;
            self.pix.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    fn outline(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgba, width: f32) {
        if let Some(p) = rounded(x * self.s, y * self.s, w * self.s, h * self.s, r * self.s) {
            let mut paint = Paint::default();
            paint.set_color(color.to_skia());
            paint.anti_alias = true;
            let stroke = Stroke { width: width * self.s, ..Stroke::default() };
            self.pix.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
        }
    }

    /// The meter's panel is opaque enough that text needs no shadow (it smeared the dark labels on the
    /// selected tab).
    fn text(&mut self, bold: bool, size: f32, x: f32, y: f32, text: &str, color: Rgba) -> f32 {
        self.fonts.draw_plain(self.pix, bold, size * self.s, x * self.s, y * self.s, text, color) / self.s
    }

    fn width(&self, bold: bool, size: f32, text: &str) -> f32 {
        self.fonts.width(bold, size * self.s, text) / self.s
    }

    fn text_right(&mut self, bold: bool, size: f32, right: f32, y: f32, text: &str, color: Rgba) -> f32 {
        let w = self.width(bold, size, text);
        self.text(bold, size, right - w, y, text, color);
        w
    }

    /// `text` cut with an ellipsis to fit `max_w`.
    fn fit(&self, bold: bool, size: f32, text: &str, max_w: f32) -> String {
        if self.width(bold, size, text) <= max_w {
            return text.to_string();
        }
        let mut cut: String = text.to_string();
        while !cut.is_empty() && self.width(bold, size, &format!("{cut}\u{2026}")) > max_w {
            cut.pop();
        }
        format!("{cut}\u{2026}")
    }

    /// A circular arrow: the reset button's icon.
    fn reset_icon(&mut self, cx: f32, cy: f32, r: f32, color: Rgba) {
        let s = self.s;
        let mut pb = PathBuilder::new();
        let start = 40f32.to_radians();
        let steps = 20;
        for i in 0..=steps {
            let a = start + (300f32.to_radians()) * i as f32 / steps as f32;
            let (x, y) = ((cx + r * a.cos()) * s, (cy - r * a.sin()) * s);
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        paint.anti_alias = true;
        if let Some(p) = pb.finish() {
            self.pix.stroke_path(&p, &paint, &Stroke { width: 1.8 * s, ..Stroke::default() }, Transform::identity(), None);
        }
        // Arrow head at the start of the arc.
        let (hx, hy) = (cx + r * start.cos(), cy - r * start.sin());
        let mut pb = PathBuilder::new();
        pb.move_to((hx + 3.5) * s, (hy - 1.0) * s);
        pb.line_to((hx - 1.5) * s, (hy - 3.5) * s);
        pb.line_to((hx - 0.5) * s, (hy + 2.5) * s);
        pb.close();
        if let Some(p) = pb.finish() {
            self.pix.fill_path(&p, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn unit(mode: Mode) -> &'static str {
    match mode {
        Mode::Damage => "",
        Mode::Average | Mode::Recent => "/s",
    }
}

impl Canvas<'_> {
    /// The minimize button: a dash while open, a small window to open it again while minimized.
    fn minimize_button(&mut self, minimized: bool) {
        self.rect(MIN_X, MIN_Y, MIN_W, MIN_H, 5.0, Rgba(255, 255, 255, 26));
        let (cx, cy) = (MIN_X + MIN_W / 2.0, MIN_Y + MIN_H / 2.0);
        if minimized {
            self.outline(cx - 5.0, cy - 4.0, 10.0, 8.0, 1.5, DIM, 1.5);
            self.rect(cx - 5.0, cy - 4.0, 10.0, 2.5, 1.0, DIM);
        } else {
            self.rect(cx - 5.0, cy - 0.9, 10.0, 1.8, 0.9, DIM);
        }
    }
}

/// The minimized bar: the mode, your value (or the leader's when you are not in the fight) and the group's.
fn render_mini(c: &mut Canvas, view: &View, status: &Status, opacity: f32) {
    c.rect(0.5, 0.5, W - 1.0, MINI_H - 1.0, 9.0, Rgba(10, 12, 18, 255).with_alpha(0.78 * opacity.clamp(0.0, 1.0)));
    c.outline(0.5, 0.5, W - 1.0, MINI_H - 1.0, 9.0, Rgba(255, 255, 255, 34), 1.0);
    c.minimize_button(true);
    let y = 21.0;
    let mut x = PAD + 2.0;
    x += c.text(true, 11.0, x, y, view.mode.label(), AMBER) + 10.0;
    let right_edge = MIN_X - 8.0;
    if view.target.is_none() || status.health != Health::Good {
        let text = match status.health {
            Health::Good => "no fight yet".to_string(),
            _ => status.text.clone(),
        };
        let color = if status.health == Health::Error { RED } else { DIM };
        let text = c.fit(false, 12.0, &text, right_edge - x);
        c.text(false, 12.0, x, y, &text, color);
        return;
    }
    let group = format!("group {}{}", short_number(view.total), unit(view.mode));
    let right = right_edge - c.text_right(false, 11.5, right_edge, y, &group, DIM) - 10.0;
    let lead = view.rows.iter().position(|r| r.is_self).or(if view.rows.is_empty() { None } else { Some(0) });
    if let Some(i) = lead {
        let row = &view.rows[i];
        let value = format!("{}{}", short_number(row.value), unit(view.mode));
        let w = c.width(true, 13.0, &value);
        let label = if row.is_self { format!("#{} you", i + 1) } else { format!("#1 {}", row.name) };
        let label = c.fit(false, 12.0, &label, (right - x - w - 8.0).max(0.0));
        let lw = c.text(false, 12.0, x, y, &label, if row.is_self { WHITE } else { DIM });
        c.text(true, 13.0, x + lw + 8.0, y, &value, class_color(row.class_id));
    }
}

/// Draws the meter. Returns the rows drawn (for hit testing) and the height drawn (for the input region).
pub fn render(pix: &mut Pixmap, scale: f32, view: &View, status: &Status, max_rows: usize, minimized: bool, opacity: f32, fonts: &Fonts) -> (usize, f32) {
    pix.fill(tiny_skia::Color::TRANSPARENT);
    let mut c = Canvas { pix, s: scale, fonts };
    if minimized {
        render_mini(&mut c, view, status, opacity);
        return (visible_rows(&view.rows, max_rows).len().max(1), MINI_H);
    }
    let rows = visible_rows(&view.rows, max_rows);
    let shown = rows.len().max(1);
    let h = height(shown);
    c.rect(0.5, 0.5, W - 1.0, h - 1.0, 10.0, Rgba(10, 12, 18, 255).with_alpha(0.78 * opacity.clamp(0.0, 1.0)));
    c.outline(0.5, 0.5, W - 1.0, h - 1.0, 10.0, Rgba(255, 255, 255, 34), 1.0);
    c.minimize_button(false);

    // Header: the enemy, its HP and whether it is followed or pinned.
    match &view.target {
        Some(t) => {
            let tag = if view.pinned { "pinned" } else { "auto" };
            let tag_color = if view.pinned { AMBER } else { DIM };
            let mut right = MIN_X - 8.0;
            right -= c.text_right(false, 10.5, right, 20.0, tag, tag_color) + 8.0;
            let hp = if t.dead {
                "dead".to_string()
            } else {
                t.hp_pct.map(|p| format!("{:.0}%", p * 100.0)).unwrap_or_default()
            };
            if !hp.is_empty() {
                right -= c.text_right(true, 12.5, right, 20.0, &hp, if t.dead { RED } else { WHITE }) + 8.0;
            }
            let name = if t.name.is_empty() { format!("#{}", t.id) } else { t.name.clone() };
            let name = c.fit(true, 14.0, &name, right - PAD);
            c.text(true, 14.0, PAD, 20.0, &name, if t.is_boss { AMBER } else { WHITE });
            let bar_w = MIN_X - 6.0 - PAD;
            c.rect(PAD, 26.0, bar_w, 3.0, 1.5, Rgba(255, 255, 255, 30));
            if let Some(p) = t.hp_pct.filter(|_| !t.dead) {
                let color = if p > 0.5 { GREEN } else if p > 0.2 { AMBER } else { RED };
                c.rect(PAD, 26.0, bar_w * p as f32, 3.0, 1.5, color);
            }
        }
        None => {
            c.text(true, 14.0, PAD, 20.0, "No fight yet", DIM);
        }
    }

    // Mode tabs and the reset button.
    let reset_x = W - PAD - RESET_W;
    let tab_w = (reset_x - 6.0 - PAD) / 3.0;
    for (i, mode) in Mode::ALL.iter().enumerate() {
        let x = PAD + i as f32 * tab_w;
        let on = *mode == view.mode;
        c.rect(x + 1.0, TABS_Y, tab_w - 2.0, TABS_H, 6.0, if on { Rgba(255, 196, 51, 225) } else { Rgba(255, 255, 255, 22) });
        let label = mode.label();
        let w = c.width(true, 11.5, label);
        c.text(true, 11.5, x + (tab_w - w) / 2.0, TABS_Y + 15.5, label, if on { Rgba(24, 18, 4, 255) } else { DIM });
    }
    c.rect(reset_x, TABS_Y, RESET_W, TABS_H, 6.0, Rgba(255, 255, 255, 22));
    c.reset_icon(reset_x + RESET_W / 2.0, TABS_Y + TABS_H / 2.0, 5.5, DIM);

    // Rows.
    if rows.is_empty() {
        let text = if view.target.is_some() { "no damage on it yet" } else { "Attack something to start" };
        c.text(false, 12.5, PAD + 4.0, ROWS_Y + 15.0, text, DIM);
    }
    let top = rows.first().map(|(_, r)| r.value).unwrap_or(0.0).max(1e-9);
    for (slot, (rank, row)) in rows.iter().enumerate() {
        let y = ROWS_Y + slot as f32 * ROW_H;
        let full = W - 2.0 * PAD;
        let color = class_color(row.class_id);
        c.rect(PAD, y + 1.0, full, ROW_H - 2.0, 4.0, Rgba(255, 255, 255, 14));
        let frac = (row.value / top).clamp(0.0, 1.0) as f32;
        c.rect(PAD, y + 1.0, full * frac, ROW_H - 2.0, 4.0, color.with_alpha(if row.is_self { 0.62 } else { 0.42 }));
        if row.is_self {
            c.outline(PAD, y + 1.0, full, ROW_H - 2.0, 4.0, WHITE.with_alpha(0.75), 1.2);
        }
        let value = format!("{}{}", short_number(row.value), unit(view.mode));
        let mut right = W - PAD - 6.0;
        right -= c.text_right(true, 12.0, right, y + 15.5, &value, WHITE) + 8.0;
        right -= c.text_right(false, 10.5, right, y + 15.0, &format!("{:.0}%", row.share * 100.0), DIM.with_alpha(0.95)) + 6.0;
        c.text(false, 10.5, PAD + 5.0, y + 15.0, &format!("{}", rank + 1), DIM);
        let name = c.fit(row.is_self, 12.5, &row.name, right - PAD - 22.0);
        c.text(row.is_self, 12.5, PAD + 20.0, y + 15.5, &name, WHITE);
    }

    // Footer: the total in the mode, how long the enemy has been fought, and the capture status if not fine.
    let fy = ROWS_Y + shown as f32 * ROW_H + 19.0;
    let (left, color) = match status.health {
        Health::Good if view.target.is_some() => {
            let label = match view.mode {
                Mode::Damage => "Total",
                Mode::Average => "Group",
                Mode::Recent => "Group, last 5 s",
            };
            (format!("{label}  {}{}", short_number(view.total), unit(view.mode)), WHITE)
        }
        Health::Good => ("reading the game".to_string(), DIM),
        Health::Waiting => (status.text.clone(), Rgba(230, 200, 120, 255)),
        Health::Error => (status.text.clone(), RED),
    };
    let mut right = W - PAD - 2.0;
    if view.target.is_some() {
        let time = if view.active { clock(view.attacked_ms) } else { format!("{}  ended", clock(view.attacked_ms)) };
        right -= c.text_right(false, 11.5, right, fy, &time, DIM) + 10.0;
    }
    let left = c.fit(view.target.is_some() && status.health == Health::Good, 11.5, &left, right - PAD - 2.0);
    c.text(status.health == Health::Good, 11.5, PAD + 2.0, fy, &left, color);
    (shown, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, value: f64, is_self: bool) -> Row {
        Row { id: 1, name: name.into(), class_id: Some(11), is_self, value, damage: value as u64, share: 0.0 }
    }

    #[test]
    fn clicks_land_on_the_controls() {
        assert_eq!(hit(40.0, 15.0, 5, false), Hit::Target);
        assert_eq!(hit(W - 18.0, 15.0, 5, false), Hit::Minimize);
        assert_eq!(hit(20.0, 45.0, 5, false), Hit::Mode(Mode::Damage));
        assert_eq!(hit(130.0, 45.0, 5, false), Hit::Mode(Mode::Average));
        assert_eq!(hit(220.0, 45.0, 5, false), Hit::Mode(Mode::Recent));
        assert_eq!(hit(W - 15.0, 45.0, 5, false), Hit::Reset);
        assert_eq!(hit(150.0, 100.0, 5, false), Hit::Drag);
        assert_eq!(hit(150.0, height(5) + 5.0, 5, false), Hit::Outside);
        assert_eq!(hit(-1.0, 10.0, 5, false), Hit::Outside);
    }

    #[test]
    fn minimized_is_a_bar_with_one_button() {
        assert_eq!(hit(W - 18.0, 15.0, 5, true), Hit::Minimize);
        assert_eq!(hit(40.0, 15.0, 5, true), Hit::Drag, "no enemy switching in the bar");
        assert_eq!(hit(40.0, 45.0, 5, true), Hit::Outside, "below the bar: through to the game");
        assert_eq!(content_height(5, true), MINI_H);
        assert_eq!(content_height(5, false), height(5));
    }

    #[test]
    fn your_row_stays_visible() {
        let rows: Vec<Row> = (0..10).map(|i| row(&format!("P{i}"), 100.0 - i as f64, i == 8)).collect();
        let shown = visible_rows(&rows, 5);
        assert_eq!(shown.len(), 5);
        assert_eq!(shown.last().unwrap().0, 8, "you, with your real rank");
        let shown = visible_rows(&rows, 10);
        assert_eq!(shown.len(), 10);
    }

    #[test]
    fn draws_every_state_without_panicking() {
        let fonts = Fonts::load(None);
        let status = Status { text: "ok".into(), health: Health::Good };
        let mut pix = Pixmap::new(W as u32 * 2, height(8) as u32 * 2).unwrap();
        let empty = View { mode: Mode::Damage, target: None, pinned: false, attacked_ms: 0, active: false, rows: vec![], total: 0.0 };
        assert_eq!(render(&mut pix, 2.0, &empty, &status, 8, false, 1.0, &fonts), (1, height(1)));
        let full = View { rows: (0..12).map(|i| row(&format!("Player {i}"), 1000.0 * (12 - i) as f64, i == 3)).collect(), ..empty };
        assert_eq!(render(&mut pix, 2.0, &full, &Status { text: "waiting".into(), health: Health::Waiting }, 8, false, 1.0, &fonts), (8, height(8)));
        assert!(pix.pixels().iter().any(|p| p.alpha() > 0));
        let full = View { target: Some(Default::default()), ..full };
        assert_eq!(render(&mut pix, 2.0, &full, &status, 8, true, 1.0, &fonts).1, MINI_H);
        // Nothing drawn below the bar.
        let below = (MINI_H as u32 * 2 + 4) as usize * pix.width() as usize;
        assert!(pix.pixels()[below..].iter().all(|p| p.alpha() == 0));
    }
}
