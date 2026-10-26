//! Which object templates are Hidden Cubes.
//!
//! A cube is a `34 36` spawn whose template is in this book. The templates differ per zone and even within one
//! (Verteron has two), so the book starts from the ones confirmed in captures and learns more at run time: when
//! an object's opening finishes by taking the Hidden Cube key (`3a 36` with item 630900003, the item Questlog's
//! database lists as "Key: Hidden Cube (Bound)"), its template is a cube template. Learned templates are saved by
//! the terminal app so they are known from the first spawn next time.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Item taken by opening a Hidden Cube, presumably the consumed key: byte identical on every cube opening in the
/// dr89mh and Cube Watch captures, while gathering nodes end in other values.
pub const KEY_ITEM: u32 = 630_900_003;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// Seen in captures: opened with the key, at a known cube spot.
    Verified,
    /// From a single third party source, not confirmed in a capture.
    Hypothesis,
    /// Learned by this app from a key opening.
    Learned,
    /// Given on the command line.
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CubeTemplate {
    pub template: u32,
    /// Zone the template was seen in, when known.
    pub zone: Option<String>,
    pub evidence: Evidence,
}

/// Templates known before any capture. Sources in docs/PROTOCOL.md.
pub const KNOWN: &[(u32, &str, Evidence)] = &[
    (1_200_039, "Verteron", Evidence::Verified),
    (1_200_041, "Verteron", Evidence::Verified),
    (1_301_199, "Altgard", Evidence::Hypothesis),
];

#[derive(Debug, Clone)]
pub struct CubeBook {
    templates: BTreeMap<u32, CubeTemplate>,
}

impl Default for CubeBook {
    fn default() -> Self {
        let templates = KNOWN.iter().map(|&(template, zone, evidence)| (template, CubeTemplate { template, zone: Some(zone.into()), evidence })).collect();
        CubeBook { templates }
    }
}

impl CubeBook {
    pub fn is_cube(&self, template: u32) -> bool {
        self.templates.contains_key(&template)
    }

    pub fn get(&self, template: u32) -> Option<&CubeTemplate> {
        self.templates.get(&template)
    }

    /// Adds a template; a known one keeps its stronger evidence. Returns whether it was new.
    pub fn add(&mut self, entry: CubeTemplate) -> bool {
        if self.templates.contains_key(&entry.template) {
            return false;
        }
        self.templates.insert(entry.template, entry);
        true
    }

    /// Every template, lowest id first.
    pub fn all(&self) -> Vec<CubeTemplate> {
        self.templates.values().cloned().collect()
    }

    /// The templates this app learned, for saving.
    pub fn learned(&self) -> Vec<CubeTemplate> {
        self.templates.values().filter(|t| t.evidence == Evidence::Learned).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_the_known_templates_and_learns() {
        let mut book = CubeBook::default();
        assert!(book.is_cube(1_200_039));
        assert!(book.is_cube(1_301_199));
        assert!(!book.is_cube(1_500_041), "a gathering node");
        assert!(book.learned().is_empty());
        assert!(book.add(CubeTemplate { template: 1_400_001, zone: None, evidence: Evidence::Learned }));
        assert!(!book.add(CubeTemplate { template: 1_200_039, zone: None, evidence: Evidence::Learned }), "already known");
        assert_eq!(book.get(1_200_039).unwrap().evidence, Evidence::Verified);
        assert_eq!(book.learned().len(), 1);
        assert_eq!(book.all().len(), 4);
    }

    #[test]
    fn templates_serialise_for_the_saved_file() {
        let t = CubeTemplate { template: 7, zone: Some("Verteron".into()), evidence: Evidence::Learned };
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, r#"{"template":7,"zone":"Verteron","evidence":"learned"}"#);
        assert_eq!(serde_json::from_str::<CubeTemplate>(&json).unwrap(), t);
    }
}
