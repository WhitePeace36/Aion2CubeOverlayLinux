//! What the overlay keeps between runs, in `$XDG_DATA_HOME/aion-cube-overlay` (usually
//! `~/.local/share/aion-cube-overlay`):
//!
//! - `templates.json`: cube templates learned from key openings, so the next run knows them from the first spawn.
//! - `spots.json`: every spot a cube was seen at, drawn faintly on the radar.
//!
//! Same files and format as the aion2-tui terminal app, so its files can be copied over.

use cube_core::cubes::{CubeBook, CubeTemplate};
use cube_core::spots::Spot;
use std::path::{Path, PathBuf};

/// `$XDG_DATA_HOME/aion-cube-overlay`, else `~/.local/share/aion-cube-overlay`, else `./aion-cube-overlay-data`.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("aion-cube-overlay");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(home).join(".local/share/aion-cube-overlay");
    }
    PathBuf::from("aion-cube-overlay-data")
}

/// Writes to a temporary file next to `path` first, so a failed write keeps the old file.
fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("ignoring {}: {e}", path.display());
            T::default()
        }),
        Err(_) => T::default(),
    }
}

pub struct SpotStore {
    path: PathBuf,
}

impl SpotStore {
    pub fn new(path: PathBuf) -> SpotStore {
        SpotStore { path }
    }

    pub fn load(&self) -> Vec<Spot> {
        read_json(&self.path)
    }

    pub fn save(&self, spots: &[Spot]) -> anyhow::Result<()> {
        write_atomic(&self.path, &serde_json::to_string_pretty(spots)?)
    }
}

pub struct TemplateStore {
    path: PathBuf,
    templates: Vec<CubeTemplate>,
}

impl TemplateStore {
    pub fn open(path: PathBuf) -> TemplateStore {
        let templates = read_json(&path);
        TemplateStore { path, templates }
    }

    pub fn len(&self) -> usize {
        self.templates.len()
    }

    pub fn load_into(&self, book: &mut CubeBook) {
        for t in &self.templates {
            book.add(t.clone());
        }
    }

    pub fn add(&mut self, template: CubeTemplate) -> anyhow::Result<()> {
        if self.templates.iter().any(|t| t.template == template.template) {
            return Ok(());
        }
        self.templates.push(template);
        write_atomic(&self.path, &serde_json::to_string_pretty(&self.templates)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cube_core::cubes::Evidence;
    use cube_core::geo::Vec3;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cube-overlay-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn templates_survive_a_restart() {
        let dir = temp_dir("templates");
        let path = dir.join("templates.json");
        let mut store = TemplateStore::open(path.clone());
        assert_eq!(store.len(), 0);
        store.add(CubeTemplate { template: 1_234_567, zone: Some("Eltnen".into()), evidence: Evidence::Learned }).unwrap();
        store.add(CubeTemplate { template: 1_234_567, zone: None, evidence: Evidence::Learned }).unwrap();
        let again = TemplateStore::open(path);
        assert_eq!(again.len(), 1);
        let mut book = CubeBook::default();
        assert!(!book.is_cube(1_234_567));
        again.load_into(&mut book);
        assert!(book.is_cube(1_234_567));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn spots_round_trip_and_a_broken_file_is_empty() {
        let dir = temp_dir("spots");
        let store = SpotStore::new(dir.join("spots.json"));
        assert!(store.load().is_empty());
        let spot = Spot { map_id: 1010, template: 1_200_039, pos: Vec3::new(1.0, 2.0, 3.0), first_seen_ms: 1, last_seen_ms: 2, sightings: 3, opened_by_you: 1 };
        store.save(std::slice::from_ref(&spot)).unwrap();
        assert_eq!(store.load(), vec![spot]);
        std::fs::write(dir.join("spots.json"), "not json").unwrap();
        assert!(store.load().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// `$XDG_CONFIG_HOME/aion-cube-overlay`, else `~/.config/aion-cube-overlay`.
pub fn default_config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir).join("aion-cube-overlay");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(home).join(".config/aion-cube-overlay");
    }
    PathBuf::from("aion-cube-overlay-config")
}

/// Where the meter window was left, its size and which mode it showed, in `meter.json`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MeterSettings {
    pub x: i32,
    pub y: i32,
    pub mode: String,
    /// Size, 1 = the base layout. Files from before it existed get the default.
    #[serde(default = "default_scale")]
    pub scale: f32,
    #[serde(default)]
    pub minimized: bool,
}

/// Both overlays start a quarter larger than their base layout, which reads small on most screens.
pub const DEFAULT_SCALE: f32 = 1.25;

/// The radar alone starts at its base size, unless --radar-scale or --scale says otherwise.
pub const DEFAULT_RADAR_SCALE: f32 = 1.0;

fn default_scale() -> f32 {
    DEFAULT_SCALE
}

impl Default for MeterSettings {
    fn default() -> Self {
        MeterSettings { x: 24, y: 560, mode: "5s".into(), scale: DEFAULT_SCALE, minimized: false }
    }
}

/// Reads the options file: command line options, one or more per line, `#` starts a comment, and quotes
/// ('...' or "...") keep spaces together, as in a shell. A missing file is no options.
pub fn read_options_file(path: &Path) -> anyhow::Result<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => split_words(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
    }
}

pub fn split_words(text: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    for line in text.lines() {
        let mut word: Option<String> = None;
        let mut quote: Option<char> = None;
        for ch in line.chars() {
            match (quote, ch) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), c) => word.get_or_insert_with(String::new).push(c),
                (None, '#') if word.is_none() => break,
                (None, '\'') | (None, '"') => {
                    quote = Some(ch);
                    word.get_or_insert_with(String::new);
                }
                (None, c) if c.is_whitespace() => words.extend(word.take()),
                (None, c) => word.get_or_insert_with(String::new).push(c),
            }
        }
        if quote.is_some() {
            anyhow::bail!("unclosed quote in: {line}");
        }
        words.extend(word);
    }
    Ok(words)
}

impl MeterSettings {
    pub fn load(path: &Path) -> MeterSettings {
        read_json(path)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        write_atomic(path, &serde_json::to_string_pretty(self)?)
    }
}

#[cfg(test)]
mod meter_settings_tests {
    use super::*;

    #[test]
    fn meter_settings_round_trip() {
        let path = std::env::temp_dir().join(format!("cube-overlay-meter-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert_eq!(MeterSettings::load(&path), MeterSettings::default());
        let s = MeterSettings { x: 300, y: 40, mode: "dps".into(), scale: 1.6, minimized: true };
        s.save(&path).unwrap();
        assert_eq!(MeterSettings::load(&path), s);
        std::fs::write(&path, r#"{"x": 1, "y": 2, "mode": "damage"}"#).unwrap();
        assert_eq!(MeterSettings::load(&path).scale, DEFAULT_SCALE, "an older file gets the default size");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn options_file_words() {
        let text = "--radar-scale 1.5   # bigger radar\n# a comment line\n--alert-cmd 'pw-play \"/usr/share/a b.oga\"' --corner top-right\n";
        assert_eq!(
            split_words(text).unwrap(),
            vec!["--radar-scale", "1.5", "--alert-cmd", "pw-play \"/usr/share/a b.oga\"", "--corner", "top-right"]
        );
        assert!(split_words("--alert-cmd 'oops").is_err());
        assert!(read_options_file(Path::new("/nonexistent/options")).unwrap().is_empty());
    }
}
