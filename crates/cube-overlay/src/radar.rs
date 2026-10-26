//! Draws one frame of the overlay into a tiny-skia pixmap: a north-up radar with you in the middle, a height
//! gauge beside it, and two or three lines of text below.
//!
//! Height is shown three ways, so it reads at a glance:
//! - the marker's shape: ▲ the cube is more than 2 m above you, ▼ more than 2 m below, ● about level;
//! - the gauge on the right: you are the line in the middle, every cube a mark at its height;
//! - the text: "23 m above you" in blue, "9 m below you" in pink.

use crate::source::{Health, Status};
use crate::text::{Fonts, Rgba};
use cube_core::geo::{compass, Vec3, UNITS_PER_METRE};
use cube_core::snapshot::{CubeState, CubeView, RadarSnapshot};
use tiny_skia::{FillRule, LineCap, Mask, Paint, Path, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

/// Overlay size in logical pixels at scale 1.
pub const BASE_W: f32 = 300.0;
pub const BASE_H: f32 = 318.0;
/// A cube within this many metres up or down is about level with you (as in aion2-tui).
pub const LEVEL_M: f32 = 2.0;
/// The fix behind the distances is called stale after this long.
const FIX_STALE_MS: u64 = 10_000;
/// Opened and gone cubes fade out over this long (the tracker keeps them a minute).
const FADE_MS: f32 = 60_000.0;

const PAD: f32 = 12.0;
const R: f32 = 108.0;
const GAUGE_X: f32 = 249.0;
const GAUGE_W: f32 = 7.0;

pub const WHITE: Rgba = Rgba(242, 244, 248, 255);
pub const DIM: Rgba = Rgba(166, 172, 184, 255);
pub const AMBER: Rgba = Rgba(255, 196, 51, 255);
pub const ORANGE: Rgba = Rgba(255, 128, 40, 255);
pub const GREEN: Rgba = Rgba(84, 214, 112, 255);
pub const RED: Rgba = Rgba(255, 92, 92, 255);
pub const ABOVE: Rgba = Rgba(102, 210, 255, 255);
pub const BELOW: Rgba = Rgba(255, 138, 214, 255);

#[derive(Debug, Clone, PartialEq)]
pub struct Look {
    /// Radius of the radar in metres. The game sends cubes within about 100 m.
    pub range_m: f32,
    /// Multiplies the background opacity (0 to 1).
    pub opacity: f32,
    /// Turns the map by this many degrees, in case the game's north is not world -Y.
    pub north_offset_deg: f32,
    pub show_spots: bool,
    pub show_trail: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look { range_m: 100.0, opacity: 1.0, north_offset_deg: 0.0, show_spots: true, show_trail: true }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub text: String,
    pub color: Rgba,
}

/// What the overlay adds to the snapshot: the capture status, a banner and the new cube flash.
#[derive(Debug, Clone, PartialEq)]
pub struct Hud {
    /// Wall clock, for animations.
    pub anim_ms: u64,
    pub status: Status,
    /// Why no cube may show yet, when the source knows.
    pub hint: Option<String>,
    pub banner: Option<Banner>,
    /// 0 to 1: how strongly the rim flashes for a new cube.
    pub flash: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    Above,
    Below,
    Level,
    Unknown,
}

pub fn elevation(height_m: Option<f32>) -> Elevation {
    match height_m {
        Some(h) if h > LEVEL_M => Elevation::Above,
        Some(h) if h < -LEVEL_M => Elevation::Below,
        Some(_) => Elevation::Level,
        None => Elevation::Unknown,
    }
}

/// "23 m above you", "9 m below you", "about your height".
pub fn height_text(height_m: Option<f32>) -> Option<String> {
    let h = height_m?;
    Some(match elevation(Some(h)) {
        Elevation::Above => format!("{:.0} m above you", h),
        Elevation::Below => format!("{:.0} m below you", -h),
        _ => "about your height".into(),
    })
}

/// The cube the radar points at: the nearest one still there with a distance, else the first still there.
pub fn tracked(snap: &RadarSnapshot) -> Option<&CubeView> {
    let open = || snap.cubes.iter().filter(|c| c.state.is_open_to_take());
    open().filter(|c| c.distance_m.is_some()).min_by(|a, b| a.distance_m.partial_cmp(&b.distance_m).unwrap_or(std::cmp::Ordering::Equal)).or_else(|| open().next())
}

/// The gauge's half range in metres: the smallest step that holds every cube still there.
pub fn gauge_scale(snap: &RadarSnapshot) -> f32 {
    let most = snap.open_cubes().filter_map(|c| c.height_m).map(f32::abs).fold(0.0, f32::max);
    [10.0, 25.0, 50.0, 100.0, 250.0, 500.0].into_iter().find(|&s| most <= s * 0.95).unwrap_or(1000.0)
}

struct Frame<'a> {
    pix: &'a mut Pixmap,
    s: f32,
    fonts: &'a Fonts,
}

fn paint(color: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color.to_skia());
    p.anti_alias = true;
    p
}

fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, r.max(0.1))
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
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

impl Frame<'_> {
    fn fill(&mut self, path: Option<Path>, color: Rgba, mask: Option<&Mask>) {
        if let Some(p) = path {
            self.pix.fill_path(&p, &paint(color), FillRule::Winding, Transform::identity(), mask);
        }
    }

    fn stroke(&mut self, path: Option<Path>, color: Rgba, width: f32, dash: Option<(f32, f32)>, mask: Option<&Mask>) {
        let Some(p) = path else { return };
        let mut stroke = Stroke { width: width * self.s, line_cap: LineCap::Round, ..Stroke::default() };
        if let Some((on, off)) = dash {
            stroke.dash = StrokeDash::new(vec![on * self.s, off * self.s], 0.0);
        }
        self.pix.stroke_path(&p, &paint(color), &stroke, Transform::identity(), mask);
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Rgba, width: f32, dash: Option<(f32, f32)>, mask: Option<&Mask>) {
        let mut pb = PathBuilder::new();
        pb.move_to(x0, y0);
        pb.line_to(x1, y1);
        self.stroke(pb.finish(), color, width, dash, mask);
    }

    /// Text in logical units; returns the logical width drawn.
    fn text(&mut self, bold: bool, size: f32, x: f32, y: f32, text: &str, color: Rgba) -> f32 {
        let s = self.s;
        self.fonts.draw(self.pix, bold, size * s, x * s, y * s, text, color) / s
    }

    fn text_width(&self, bold: bool, size: f32, text: &str) -> f32 {
        self.fonts.width(bold, size * self.s, text) / self.s
    }

    /// The largest size up to `size` at which `text` fits in `max_w`.
    fn fit(&self, bold: bool, size: f32, text: &str, max_w: f32) -> f32 {
        let w = self.text_width(bold, size, text);
        if w <= max_w || w <= 0.0 {
            size
        } else {
            (size * max_w / w).max(size * 0.6)
        }
    }

    /// The elevation shape at logical `(x, y)`: ▲, ▼, ● or ◆ when the height is unknown.
    fn marker(&mut self, x: f32, y: f32, size: f32, el: Elevation, fill: Option<Rgba>, outline: Rgba, outline_w: f32) {
        let s = self.s;
        let (x, y, k) = (x * s, y * s, size * s);
        let path = match el {
            Elevation::Above | Elevation::Below => {
                let dir = if el == Elevation::Above { 1.0 } else { -1.0 };
                let mut pb = PathBuilder::new();
                pb.move_to(x, y - dir * k * 1.15);
                pb.line_to(x + k * 1.05, y + dir * k * 0.75);
                pb.line_to(x - k * 1.05, y + dir * k * 0.75);
                pb.close();
                pb.finish()
            }
            Elevation::Level => circle(x, y, k * 0.85),
            Elevation::Unknown => {
                let mut pb = PathBuilder::new();
                pb.move_to(x, y - k);
                pb.line_to(x + k * 0.8, y);
                pb.line_to(x, y + k);
                pb.line_to(x - k * 0.8, y);
                pb.close();
                pb.finish()
            }
        };
        if let Some(c) = fill {
            self.fill(path.clone(), c, None);
        }
        self.stroke(path, outline, outline_w, None, None);
    }

    fn cross(&mut self, x: f32, y: f32, size: f32, color: Rgba) {
        let s = self.s;
        let (x, y, k) = (x * s, y * s, size * s * 0.7);
        self.line(x - k, y - k, x + k, y + k, color, 2.0, None, None);
        self.line(x - k, y + k, x + k, y - k, color, 2.0, None, None);
    }
}

/// Logical radar offset of a point `distance_m` away on map `bearing`, and whether it was pulled in to the rim.
fn place(distance_m: f32, bearing: f32, look: &Look) -> (f32, f32, bool) {
    let inner = R - 10.0;
    let b = (bearing - look.north_offset_deg).to_radians();
    let mut r = distance_m / look.range_m.max(1.0) * inner;
    let clamped = r > inner;
    if clamped {
        r = inner + 3.0;
    }
    (r * b.sin(), -r * b.cos(), clamped)
}

/// Distance and bearing of a world point from you, in metres and degrees.
fn polar(me: &Vec3, p: &Vec3) -> (f32, f32) {
    (me.planar_distance(p) / UNITS_PER_METRE, me.bearing_to(p))
}

fn state_color(c: &CubeView) -> Rgba {
    match c.state {
        CubeState::Visible | CubeState::OutOfView => AMBER,
        CubeState::Opening => ORANGE,
        CubeState::Opened if c.opened_by_you => GREEN,
        CubeState::Opened | CubeState::Gone => RED,
    }
}

fn fade(c: &CubeView, now_ms: u64) -> f32 {
    if c.state.is_open_to_take() {
        1.0
    } else {
        (1.0 - now_ms.saturating_sub(c.state_ms) as f32 / FADE_MS).clamp(0.25, 1.0)
    }
}

fn signed_m(h: f32) -> String {
    if h >= 0.0 {
        format!("+{h:.0}")
    } else {
        format!("\u{2212}{:.0}", -h)
    }
}

/// Draws the whole overlay. `scale` is logical to physical pixels (user scale times output scale).
pub fn render(pix: &mut Pixmap, scale: f32, snap: &RadarSnapshot, look: &Look, hud: &Hud, fonts: &Fonts) {
    pix.fill(tiny_skia::Color::TRANSPARENT);
    let mut f = Frame { pix, s: scale, fonts };
    let s = scale;
    let bg = look.opacity.clamp(0.0, 1.0);
    let now = snap.now_ms;
    let me = snap.me;
    let fix_age = me.map(|m| now.saturating_sub(m.t_ms));
    let stale = fix_age.is_some_and(|a| a > FIX_STALE_MS);
    let focus = tracked(snap);

    // Panel.
    f.fill(rounded_rect(0.5 * s, 0.5 * s, (BASE_W - 1.0) * s, (BASE_H - 1.0) * s, 14.0 * s), Rgba(10, 12, 18, 255).with_alpha(0.62 * bg), None);

    // Radar disc, rings and cross hair, clipped to the disc.
    let (cx, cy) = (PAD + R, PAD + R);
    let mut mask = Mask::new(f.pix.width(), f.pix.height());
    if let (Some(m), Some(disc)) = (mask.as_mut(), circle(cx * s, cy * s, R * s)) {
        m.fill_path(&disc, FillRule::Winding, true, Transform::identity());
    }
    let mask = mask.as_ref();
    f.fill(circle(cx * s, cy * s, R * s), Rgba(18, 22, 30, 255).with_alpha(0.55 * bg + 0.15), None);
    let inner = R - 10.0;
    for frac in [0.25, 0.5, 1.0] {
        f.stroke(circle(cx * s, cy * s, inner * frac * s), Rgba(255, 255, 255, 46), 1.0, if frac < 1.0 { Some((3.0, 4.0)) } else { None }, mask);
    }
    let rot = look.north_offset_deg.to_radians();
    for k in 0..4 {
        let a = -rot + k as f32 * std::f32::consts::FRAC_PI_2;
        let (dx, dy) = (a.sin() * R, -a.cos() * R);
        f.line((cx - dx) * s, (cy - dy) * s, (cx + dx) * s, (cy + dy) * s, Rgba(255, 255, 255, 22), 1.0, None, mask);
    }
    // Range labels on the half and full rings, toward the north east.
    let diag = std::f32::consts::FRAC_1_SQRT_2;
    for (frac, label) in [(0.5, format!("{:.0}", look.range_m * 0.5)), (1.0, format!("{:.0} m", look.range_m))] {
        let (lx, ly) = (cx + inner * frac * diag + 3.0, cy - inner * frac * diag - 2.0);
        f.text(false, 9.5, lx, ly, &label, DIM.with_alpha(0.75));
    }
    // North.
    let (nx, ny) = (cx - look.north_offset_deg.to_radians().sin() * (R - 9.0), cy - look.north_offset_deg.to_radians().cos() * (R - 9.0));
    let nw = f.text_width(true, 12.0, "N");
    f.text(true, 12.0, nx - nw / 2.0, ny + 4.5, "N", WHITE.with_alpha(0.9));

    // Flash of the rim for a new cube.
    if hud.flash > 0.01 {
        f.stroke(circle(cx * s, cy * s, (R - 1.5) * s), AMBER.with_alpha(hud.flash), 2.0 + 4.0 * hud.flash, None, None);
    } else {
        f.stroke(circle(cx * s, cy * s, (R - 0.5) * s), Rgba(255, 255, 255, 60), 1.0, None, None);
    }

    if let Some(fix) = me {
        // Your path, faint.
        if look.show_trail && snap.trail.len() > 1 {
            let mut pb = PathBuilder::new();
            for (i, t) in snap.trail.iter().enumerate() {
                let (d, b) = polar(&fix.pos, &t.pos);
                let (x, y, _) = place(d, b, look);
                if i == 0 {
                    pb.move_to((cx + x) * s, (cy + y) * s);
                } else {
                    pb.line_to((cx + x) * s, (cy + y) * s);
                }
            }
            f.stroke(pb.finish(), Rgba(255, 255, 255, 50), 1.5, None, mask);
        }
        // Spots where cubes were seen in earlier runs.
        if look.show_spots {
            for spot in &snap.spots {
                let (d, b) = polar(&fix.pos, &spot.pos);
                if d > look.range_m * 1.05 {
                    continue;
                }
                let (x, y, _) = place(d, b, look);
                f.stroke(circle((cx + x) * s, (cy + y) * s, 3.0 * s), AMBER.with_alpha(0.35), 1.0, None, mask);
            }
        }
    }

    // Cubes: opened and gone first (faded), then the ones still there, the tracked one last.
    let mut order: Vec<&CubeView> = snap.cubes.iter().collect();
    order.sort_by_key(|c| (c.state.is_open_to_take(), focus.is_some_and(|t| t.id == c.id)));
    let mut labels: Vec<(f32, f32, String, Rgba)> = Vec::new();
    for c in order {
        let (Some(d), Some(b)) = (c.distance_m, c.bearing) else { continue };
        let (x, y, clamped) = place(d, b, look);
        let (mx, my) = (cx + x, cy + y);
        let alpha = fade(c, now) * if stale { 0.8 } else { 1.0 };
        let color = state_color(c).with_alpha(alpha);
        let is_focus = focus.is_some_and(|t| t.id == c.id);
        let el = elevation(c.height_m);
        match c.state {
            CubeState::Opened | CubeState::Gone => {
                if c.opened_by_you {
                    f.marker(mx, my, 4.0, Elevation::Level, None, color, 2.0);
                } else {
                    f.cross(mx, my, 6.0, color);
                }
                continue;
            }
            _ => {}
        }
        if is_focus {
            f.line(cx * s, cy * s, mx * s, my * s, AMBER.with_alpha(0.55 * alpha), 2.0, if stale { Some((5.0, 5.0)) } else { None }, mask);
        }
        if c.state == CubeState::Opening {
            let pulse = ((hud.anim_ms % 1000) as f32 / 1000.0 * std::f32::consts::TAU).sin() * 0.5 + 0.5;
            f.stroke(circle(mx * s, my * s, (10.0 + 4.0 * pulse) * s), ORANGE.with_alpha(0.7 - 0.4 * pulse), 2.0, None, None);
        }
        let size = if is_focus { 8.0 } else { 6.0 };
        let fill = if c.state == CubeState::OutOfView { None } else { Some(color) };
        if is_focus {
            f.fill(circle(mx * s, my * s, (size + 6.0) * s), AMBER.with_alpha(0.18 * alpha), None);
        }
        f.marker(mx, my, size, el, fill, if fill.is_some() { Rgba(20, 16, 4, 220) } else { color }, if fill.is_some() { 1.2 } else { 2.0 });
        if clamped {
            // Beyond the range: a small arrow outward on the rim.
            let b = (b - look.north_offset_deg).to_radians();
            let (ux, uy) = (b.sin(), -b.cos());
            let tip = (mx + ux * 9.0, my + uy * 9.0);
            f.line(mx * s, my * s, tip.0 * s, tip.1 * s, color, 2.0, None, None);
        }
        // Labels: distance for the tracked cube, height for every cube off your level.
        let mut text = String::new();
        if is_focus {
            text = format!("{d:.0} m");
        }
        let label_color = match el {
            Elevation::Above => ABOVE,
            Elevation::Below => BELOW,
            _ => WHITE,
        };
        if let (Some(h), Elevation::Above | Elevation::Below) = (c.height_m, el) {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&signed_m(h));
            if !is_focus {
                text.push_str(" m");
            }
        }
        if !text.is_empty() {
            labels.push((mx, my, text, label_color.with_alpha(alpha)));
        }
    }
    for (mx, my, text, color) in labels {
        let size = 11.5;
        let w = f.text_width(true, size, &text);
        let x = if mx + 15.0 + w > PAD + 2.0 * R - 2.0 { mx - 15.0 - w } else { mx + 15.0 };
        f.text(true, size, x, my + 4.0, &text, color);
    }

    // You.
    match me {
        Some(_) => {
            let alpha = if stale { 0.55 } else { 1.0 };
            f.fill(circle(cx * s, cy * s, 6.5 * s), Rgba(0, 0, 0, 160).with_alpha(alpha), None);
            f.fill(circle(cx * s, cy * s, 4.5 * s), WHITE.with_alpha(alpha), None);
            if stale {
                f.stroke(circle(cx * s, cy * s, 11.0 * s), WHITE.with_alpha(0.5), 1.2, Some((3.0, 3.0)), None);
            }
        }
        None => {
            let w = f.text_width(true, 18.0, "?");
            f.text(true, 18.0, cx - w / 2.0, cy + 6.0, "?", DIM);
        }
    }

    draw_gauge(&mut f, snap, focus);
    draw_text(&mut f, snap, look, hud, focus, fix_age, stale);

    if let Some(banner) = &hud.banner {
        let size = 13.0;
        let w = f.text_width(true, size, &banner.text) + 20.0;
        let (bx, by) = (cx - w / 2.0, PAD + 20.0);
        f.fill(rounded_rect(bx * s, by * s, w * s, 22.0 * s, 11.0 * s), Rgba(10, 12, 18, 225), None);
        f.stroke(rounded_rect(bx * s, by * s, w * s, 22.0 * s, 11.0 * s), banner.color.with_alpha(0.9), 1.5, None, None);
        f.text(true, size, bx + 10.0, by + 15.5, &banner.text, banner.color);
    }
}

fn draw_gauge(f: &mut Frame, snap: &RadarSnapshot, focus: Option<&CubeView>) {
    let s = f.s;
    let top = PAD + 16.0;
    let bottom = PAD + 2.0 * R - 16.0;
    let mid = (top + bottom) / 2.0;
    let half = (bottom - top) / 2.0;
    let scale = gauge_scale(snap);
    let x = GAUGE_X;
    let y_of = |h: f32| mid - (h / scale).clamp(-1.08, 1.08) * half;

    f.fill(rounded_rect(x * s, top * s, GAUGE_W * s, (mid - top) * s, 3.0 * s), ABOVE.with_alpha(0.22), None);
    f.fill(rounded_rect(x * s, mid * s, GAUGE_W * s, (bottom - mid) * s, 3.0 * s), BELOW.with_alpha(0.22), None);
    for frac in [-1.0f32, -0.5, 0.5, 1.0] {
        let y = mid - frac * half;
        f.line((x - 2.0) * s, y * s, (x + GAUGE_W + 2.0) * s, y * s, Rgba(255, 255, 255, 70), 1.0, None, None);
    }
    let lx = x + GAUGE_W + 5.0;
    // The tracked cube's height goes right of the bar; a scale label it would cover is left out.
    let focus_y = focus.filter(|c| c.state.is_open_to_take()).and_then(|c| c.height_m).filter(|h| h.abs() > LEVEL_M).map(y_of);
    let clear = |y: f32| focus_y.is_none_or(|fy| (fy - y).abs() > 11.0);
    if clear(top) {
        f.text(false, 9.5, lx, top + 3.5, &format!("+{scale:.0}"), ABOVE.with_alpha(0.9));
    }
    if clear(bottom) {
        f.text(false, 9.5, lx, bottom + 3.5, &format!("\u{2212}{scale:.0}"), BELOW.with_alpha(0.9));
    }
    f.text(false, 9.5, x - 4.0, bottom + 15.0, "height", DIM.with_alpha(0.8));
    // You: the line in the middle.
    f.line((x - 5.0) * s, mid * s, (x + GAUGE_W + 5.0) * s, mid * s, WHITE, 2.5, None, None);
    if clear(mid) {
        f.text(false, 9.5, lx, mid + 3.5, "you", WHITE.with_alpha(0.85));
    }

    let mut focus_mark = None;
    for c in snap.open_cubes() {
        let Some(h) = c.height_m else { continue };
        let y = y_of(h);
        let is_focus = focus.is_some_and(|t| t.id == c.id);
        if is_focus {
            focus_mark = Some((y, h));
            continue;
        }
        let fill = if c.state == CubeState::OutOfView { None } else { Some(state_color(c)) };
        f.marker(x + GAUGE_W / 2.0, y, 4.5, elevation(Some(h)), fill, if fill.is_some() { Rgba(20, 16, 4, 220) } else { AMBER }, 1.2);
    }
    if let Some((y, h)) = focus_mark {
        let el = elevation(Some(h));
        f.marker(x + GAUGE_W / 2.0, y, 6.5, el, Some(AMBER), Rgba(20, 16, 4, 230), 1.2);
        if matches!(el, Elevation::Above | Elevation::Below) {
            let color = if el == Elevation::Above { ABOVE } else { BELOW };
            f.text(true, 11.0, x + GAUGE_W + 5.0, y + 4.0, &signed_m(h), color);
        }
    }
}

fn draw_text(f: &mut Frame, snap: &RadarSnapshot, look: &Look, hud: &Hud, focus: Option<&CubeView>, fix_age: Option<u64>, stale: bool) {
    let x = PAD + 2.0;
    let max_w = BASE_W - 2.0 * PAD - 4.0;
    let y1 = PAD + 2.0 * R + 26.0;
    let y2 = y1 + 22.0;
    let y3 = y2 + 20.0;
    let open = snap.open_cubes().count();

    // Line 1: where to go.
    let (head, head_color) = match focus {
        Some(c) => match (c.distance_m, c.bearing) {
            (Some(d), Some(b)) => {
                let approx = if c.estimated || stale { "\u{2248} " } else { "" };
                let what = if c.state == CubeState::Opening { "Cube being opened" } else if c.state == CubeState::OutOfView { "Cube (out of view)" } else { "Cube" };
                (format!("{what} {approx}{d:.0} m {}", compass(b - look.north_offset_deg)), if c.state == CubeState::Opening { ORANGE } else { AMBER })
            }
            _ => ("Cube detected".to_string(), AMBER),
        },
        None => ("No cube nearby".to_string(), DIM),
    };
    let size = f.fit(true, 19.0, &head, max_w);
    f.text(true, size, x, y1, &head, head_color);

    // Line 2: how high, and how many more.
    let mut cursor = x;
    match focus {
        Some(c) if c.distance_m.is_some() => {
            if let Some(text) = height_text(c.height_m) {
                let el = elevation(c.height_m);
                let color = match el {
                    Elevation::Above => ABOVE,
                    Elevation::Below => BELOW,
                    _ => WHITE,
                };
                f.marker(cursor + 6.0, y2 - 5.0, 5.5, el, Some(color), Rgba(0, 0, 0, 200), 1.0);
                cursor += 16.0;
                cursor += f.text(true, 15.0, cursor, y2, &text, color);
            }
            if open > 1 {
                f.text(false, 13.0, cursor + 10.0, y2, &format!("+{} more", open - 1), DIM);
            }
        }
        Some(_) => {
            let text = "use a skill or dash to get your position";
            let size = f.fit(false, 13.0, text, max_w);
            f.text(false, size, x, y2, text, WHITE);
        }
        None => {
            // The map name, unless line 3 carries a hint (which names the map or its absence itself).
            let text = match (&snap.map, &hud.hint) {
                (_, Some(_)) => hud.status.text.clone(),
                (Some(m), None) => m.name.clone(),
                (None, None) => "map unknown until a loading screen".into(),
            };
            let size = f.fit(false, 13.0, &text, max_w);
            f.text(false, size, x, y2, &text, DIM);
        }
    }

    // Line 3: status.
    let (text, color) = match hud.status.health {
        Health::Error => (hud.status.text.clone(), RED),
        Health::Waiting => (hud.status.text.clone(), Rgba(230, 200, 120, 255)),
        Health::Good => match (focus, fix_age) {
            (Some(_), Some(age)) if stale => (format!("your position is {} s old, use a skill to update it", age / 1000), DIM),
            (None, _) if hud.hint.is_some() => (hud.hint.clone().unwrap_or_default(), Rgba(230, 200, 120, 255)),
            _ => {
                let map = snap.map.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| "map unknown".into());
                (format!("{map}  \u{b7}  {} seen, {} opened by you", snap.counts.cubes_seen, snap.counts.opened_by_you), DIM)
            }
        },
    };
    let size = f.fit(false, 11.5, &text, max_w);
    f.text(false, size, x, y3, &text, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::Demo;
    use crate::source::Source;
    use cube_core::cubes::CubeBook;

    fn hud() -> Hud {
        Hud { anim_ms: 0, status: Status { text: "ok".into(), health: Health::Good }, hint: None, banner: None, flash: 0.0 }
    }

    #[test]
    fn elevation_uses_the_level_band() {
        assert_eq!(elevation(Some(2.5)), Elevation::Above);
        assert_eq!(elevation(Some(-2.5)), Elevation::Below);
        assert_eq!(elevation(Some(1.9)), Elevation::Level);
        assert_eq!(elevation(None), Elevation::Unknown);
        assert_eq!(height_text(Some(23.4)).unwrap(), "23 m above you");
        assert_eq!(height_text(Some(-9.0)).unwrap(), "9 m below you");
        assert_eq!(height_text(Some(0.5)).unwrap(), "about your height");
    }

    #[test]
    fn places_north_up_and_clamps_to_the_rim() {
        let look = Look::default();
        let (x, y, clamped) = place(50.0, 0.0, &look);
        assert!(x.abs() < 0.01 && y < 0.0 && !clamped, "north is up");
        let (x, y, _) = place(50.0, 90.0, &look);
        assert!(x > 0.0 && y.abs() < 0.01, "east is right");
        let (x, y, clamped) = place(500.0, 180.0, &look);
        assert!(clamped && y > 0.0 && y < R && x.abs() < 0.01);
        let turned = Look { north_offset_deg: 90.0, ..Look::default() };
        let (x, y, _) = place(50.0, 90.0, &turned);
        assert!(x.abs() < 0.01 && y < 0.0, "with a 90 degree offset, map east is drawn up");
    }

    #[test]
    fn the_gauge_grows_with_the_highest_cube() {
        let mut demo = Demo::new(1_791_722_000_000, CubeBook::default());
        demo.step(1_791_722_000_000 + 15_000);
        let snap = demo.snapshot();
        assert_eq!(gauge_scale(&snap), 25.0, "cube B is 20 odd metres up");
        assert_eq!(gauge_scale(&RadarSnapshot::default()), 10.0);
    }

    #[test]
    fn renders_every_demo_moment_without_fonts() {
        let start = 1_791_722_000_000;
        let mut demo = Demo::new(start, CubeBook::default());
        let mut pix = Pixmap::new(BASE_W as u32 * 2, BASE_H as u32 * 2).unwrap();
        for t in (0..75_000).step_by(2_500) {
            demo.step(start + t);
            render(&mut pix, 2.0, &demo.snapshot(), &Look::default(), &hud(), &Fonts::none());
            assert!(pix.pixels().iter().any(|p| p.alpha() > 0));
        }
    }
}
