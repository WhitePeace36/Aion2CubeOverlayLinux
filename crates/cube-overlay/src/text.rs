//! Text on the overlay: a system font found through fontconfig (or `--font`), rasterised with ab_glyph and
//! blended straight into the tiny-skia pixmap, with a soft shadow so it stays readable over the game.

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::path::{Path, PathBuf};
use tiny_skia::{Pixmap, PremultipliedColorU8};

/// Common places of a sans serif font when `fc-match` is missing.
const FALLBACK_REGULAR: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
];
const FALLBACK_BOLD: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/noto/NotoSans-Bold.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    "/usr/share/fonts/google-noto/NotoSans-Bold.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
];

/// Straight (not premultiplied) RGBA.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub fn with_alpha(self, a: f32) -> Rgba {
        Rgba(self.0, self.1, self.2, (self.3 as f32 * a.clamp(0.0, 1.0)).round() as u8)
    }

    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.0, self.1, self.2, self.3)
    }
}

pub struct Fonts {
    regular: Option<FontVec>,
    bold: Option<FontVec>,
}

fn fc_match(pattern: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output().ok()?;
    let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    // ab_glyph reads TrueType and OpenType, not the Type 1 or bitmap fonts fontconfig may pick.
    (path.is_file() && matches!(ext.as_str(), "ttf" | "otf")).then_some(path)
}

fn load(path: &Path) -> Option<FontVec> {
    let bytes = std::fs::read(path).ok()?;
    match FontVec::try_from_vec(bytes) {
        Ok(f) => Some(f),
        Err(e) => {
            log::warn!("could not read the font {}: {e}", path.display());
            None
        }
    }
}

fn first(candidates: impl IntoIterator<Item = PathBuf>) -> Option<(PathBuf, FontVec)> {
    candidates.into_iter().find_map(|p| load(&p).map(|f| (p, f)))
}

impl Fonts {
    /// `custom` is used for every text when given; otherwise fontconfig's sans serif, then the usual paths.
    pub fn load(custom: Option<&Path>) -> Fonts {
        if let Some(path) = custom {
            if let Some(font) = load(path) {
                log::info!("font {}", path.display());
                let bold = load(path);
                return Fonts { regular: Some(font), bold };
            }
            log::warn!("could not load --font {}, looking for a system font", path.display());
        }
        let regular = first(fc_match("sans-serif:style=Regular").into_iter().chain(FALLBACK_REGULAR.iter().map(PathBuf::from)));
        let bold = first(fc_match("sans-serif:style=Bold").into_iter().chain(FALLBACK_BOLD.iter().map(PathBuf::from)));
        match (&regular, &bold) {
            (None, None) => log::warn!("no usable font found (install DejaVu or Noto, or pass --font); the overlay draws without text"),
            _ => log::info!(
                "fonts: {} / {}",
                regular.as_ref().map(|r| r.0.display().to_string()).unwrap_or_else(|| "-".into()),
                bold.as_ref().map(|b| b.0.display().to_string()).unwrap_or_else(|| "-".into())
            ),
        }
        let regular = regular.map(|r| r.1);
        let bold = bold.map(|b| b.1);
        Fonts { regular, bold }
    }

    #[cfg(test)]
    pub fn none() -> Fonts {
        Fonts { regular: None, bold: None }
    }

    #[cfg(test)]
    pub fn available(&self) -> bool {
        self.regular.is_some() || self.bold.is_some()
    }

    fn pick(&self, bold: bool) -> Option<&FontVec> {
        if bold {
            self.bold.as_ref().or(self.regular.as_ref())
        } else {
            self.regular.as_ref().or(self.bold.as_ref())
        }
    }

    /// Width of `text` in pixels.
    pub fn width(&self, bold: bool, size: f32, text: &str) -> f32 {
        let Some(font) = self.pick(bold) else { return 0.0 };
        let scaled = font.as_scaled(PxScale::from(size));
        let mut w = 0.0;
        let mut prev = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(p) = prev {
                w += scaled.kern(p, id);
            }
            w += scaled.h_advance(id);
            prev = Some(id);
        }
        w
    }

    /// Draws `text` with its baseline at `y`, starting at `x`. Returns the width drawn.
    pub fn draw(&self, pix: &mut Pixmap, bold: bool, size: f32, x: f32, y: f32, text: &str, color: Rgba) -> f32 {
        let shadow = Rgba(0, 0, 0, (color.3 as f32 * 0.85) as u8);
        let off = (size / 14.0).clamp(1.0, 3.0);
        self.draw_plain(pix, bold, size, x + off, y + off, text, shadow);
        self.draw_plain(pix, bold, size, x, y, text, color)
    }

    fn draw_plain(&self, pix: &mut Pixmap, bold: bool, size: f32, x: f32, y: f32, text: &str, color: Rgba) -> f32 {
        let Some(font) = self.pick(bold) else { return 0.0 };
        let scale = PxScale::from(size);
        let scaled = font.as_scaled(scale);
        let mut caret = x;
        let mut prev = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(p) = prev {
                caret += scaled.kern(p, id);
            }
            let glyph = id.with_scale_and_position(scale, ab_glyph::point(caret, y));
            caret += scaled.h_advance(id);
            prev = Some(id);
            let Some(outlined) = font.outline_glyph(glyph) else { continue };
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, coverage| {
                blend(pix, bounds.min.x as i32 + gx as i32, bounds.min.y as i32 + gy as i32, color, coverage);
            });
        }
        caret - x
    }
}

/// Source-over blend of one pixel.
pub fn blend(pix: &mut Pixmap, x: i32, y: i32, color: Rgba, coverage: f32) {
    if x < 0 || y < 0 || x >= pix.width() as i32 || y >= pix.height() as i32 {
        return;
    }
    let a = (color.3 as f32 / 255.0) * coverage.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let idx = y as usize * pix.width() as usize + x as usize;
    let dst = pix.pixels_mut()[idx];
    let inv = 1.0 - a;
    let mix = |src: u8, dst: u8| ((src as f32 * a) + dst as f32 * inv).round().min(255.0) as u8;
    let out_a = (a * 255.0 + dst.alpha() as f32 * inv).round().min(255.0) as u8;
    let (r, g, b) = (mix(color.0, dst.red()), mix(color.1, dst.green()), mix(color.2, dst.blue()));
    // Premultiplied channels never exceed alpha; rounding could push them one over.
    let px = PremultipliedColorU8::from_rgba(r.min(out_a), g.min(out_a), b.min(out_a), out_a);
    if let Some(px) = px {
        pix.pixels_mut()[idx] = px;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending_stays_premultiplied() {
        let mut pix = Pixmap::new(2, 1).unwrap();
        blend(&mut pix, 0, 0, Rgba(255, 128, 0, 255), 0.5);
        let p = pix.pixels()[0];
        assert_eq!(p.alpha(), 128);
        assert!(p.red() <= p.alpha() && p.green() <= p.alpha());
        blend(&mut pix, 5, 5, Rgba(255, 255, 255, 255), 1.0);
        blend(&mut pix, 1, 0, Rgba(255, 255, 255, 255), 1.0);
        assert_eq!(pix.pixels()[1].alpha(), 255);
    }

    #[test]
    fn a_system_font_is_found_and_measures_text() {
        let fonts = Fonts::load(None);
        if !fonts.available() {
            eprintln!("no font on this machine, skipping");
            return;
        }
        let narrow = fonts.width(false, 16.0, "i");
        let wide = fonts.width(false, 16.0, "Cube 38 m NE");
        assert!(wide > narrow * 5.0, "{narrow} {wide}");
        let mut pix = Pixmap::new(200, 40).unwrap();
        let drawn = fonts.draw(&mut pix, true, 18.0, 4.0, 28.0, "Cube", Rgba(255, 255, 255, 255));
        assert!(drawn > 20.0);
        assert!(pix.pixels().iter().any(|p| p.alpha() > 200), "something was drawn");
    }
}
