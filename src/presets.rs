//! Live/logger variable presets + last-session state, persisted as JSON.
//!
//! The file holds:
//! - **presets** — named sets of ADF ids, shared by the Live tab and the
//!   Value logger (a preset is just a variable selection);
//! - **last-session state** — the per-tab selections and the Live tab's view
//!   mode, so a fresh launch restores the previous session.
//!
//! Storage: `$DDE3_CONFIG/presets.json`, else
//! `$XDG_CONFIG_HOME/dde3-toolkit/presets.json`, else
//! `~/.config/dde3-toolkit/presets.json` (same convention as the log dir in
//! `app.rs`). Ids are the ADF ids from `LIVE_VARS` — stable across
//! regenerations of `src/live.rs`, unlike table indices.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::live::LIVE_VARS;

/// Live tab display mode (persisted with the last session).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveView {
    /// Line graph of one selected variable (the old "Graph" toggle).
    Line,
    /// Plain labels: name + value + unit, no charts.
    Labels,
    /// In-car style arc gauges.
    Gauges,
    /// Value cards with sparklines (the original card grid).
    Cards,
}

impl LiveView {
    pub const ALL: [LiveView; 4] = [
        LiveView::Line,
        LiveView::Labels,
        LiveView::Gauges,
        LiveView::Cards,
    ];

    pub fn name(self) -> &'static str {
        match self {
            LiveView::Line => "Line",
            LiveView::Labels => "Labels",
            LiveView::Gauges => "Gauges",
            LiveView::Cards => "Cards",
        }
    }

    #[cfg(test)]
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|v| v.name().eq_ignore_ascii_case(s))
            .copied()
    }
}

/// A named variable selection. `ids` are ADF ids (e.g. `0x0F83`), not
/// `LIVE_VARS` indices — the file survives table regeneration.
#[derive(Clone, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub ids: Vec<u16>,
}

/// Everything persisted in `presets.json`.
#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub presets: Vec<Preset>,
    /// Value-logger tab selection (last session).
    pub logger_ids: Vec<u16>,
    /// Live data tab selection (last session).
    pub live_ids: Vec<u16>,
    /// Live data tab view mode (last session).
    pub live_view: LiveView,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            presets: Vec::new(),
            logger_ids: default_on_ids(),
            live_ids: default_on_ids(),
            live_view: LiveView::Cards,
        }
    }
}

fn default_on_ids() -> Vec<u16> {
    LIVE_VARS
        .iter()
        .filter(|v| v.default_on)
        .map(|v| v.id)
        .collect()
}

/// The config dir: `$DDE3_CONFIG`, else `$XDG_CONFIG_HOME/dde3-toolkit`,
/// else `~/.config/dde3-toolkit` (or `./dde3-toolkit-config` with no HOME).
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DDE3_CONFIG").filter(|s| !s.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME");
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.as_ref().map(|h| PathBuf::from(h).join(".config")));
    match base {
        Some(b) => b.join("dde3-toolkit"),
        None => PathBuf::from("dde3-toolkit-config"),
    }
}

pub fn settings_path() -> PathBuf {
    config_dir().join("presets.json")
}

/// Load the settings; defaults when the file is missing or unreadable.
/// Unknown ADF ids (table regenerated) are dropped silently.
pub fn load() -> Settings {
    let path = settings_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return Settings::default(),
    };
    match serde_json::from_str::<Settings>(&raw) {
        Ok(mut s) => {
            s.presets = s
                .presets
                .into_iter()
                .map(|p| Preset {
                    name: p.name,
                    ids: keep_known_ids(p.ids),
                })
                .collect();
            s.logger_ids = keep_known_ids(s.logger_ids);
            s.live_ids = keep_known_ids(s.live_ids);
            s
        }
        Err(_) => Settings::default(),
    }
}

/// Save the settings (creates the config dir). Failures are reported but
/// never fatal — presets are a convenience, not a dependency.
pub fn save(s: &Settings) -> std::io::Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(s).unwrap_or_default();
    std::fs::write(&path, json)
}

fn keep_known_ids(ids: Vec<u16>) -> Vec<u16> {
    let known: Vec<u16> = LIVE_VARS.iter().map(|v| v.id).collect();
    ids.into_iter().filter(|id| known.contains(id)).collect()
}

/// `LIVE_VARS`-indexed checkbox state from a persisted id list.
pub fn ids_to_enabled(ids: &[u16]) -> Vec<bool> {
    LIVE_VARS.iter().map(|v| ids.contains(&v.id)).collect()
}

/// The inverse: checkbox state → id list (in `LIVE_VARS` order).
pub fn enabled_to_ids(enabled: &[bool]) -> Vec<u16> {
    LIVE_VARS
        .iter()
        .zip(enabled.iter())
        .filter(|(_, on)| **on)
        .map(|(v, _)| v.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_carry_the_default_on_vars() {
        let s = Settings::default();
        assert!(s.presets.is_empty());
        let expected: Vec<u16> = LIVE_VARS.iter().filter(|v| v.default_on).map(|v| v.id).collect();
        assert_eq!(s.live_ids, expected);
        assert_eq!(s.logger_ids, expected);
        assert_eq!(s.live_view, LiveView::Cards);
    }

    #[test]
    fn ids_round_trip_through_enabled() {
        let ids = vec![0x0F83u16, 0x0F10, 0x0E11];
        let enabled = ids_to_enabled(&ids);
        assert_eq!(enabled.iter().filter(|&&e| e).count(), 3);
        // Round-trip preserves the set (returned in LIVE_VARS order).
        let back = enabled_to_ids(&enabled);
        let mut sorted = ids;
        sorted.sort_unstable();
        let mut back_sorted = back;
        back_sorted.sort_unstable();
        assert_eq!(back_sorted, sorted);
    }

    #[test]
    fn unknown_ids_are_dropped() {
        assert_eq!(keep_known_ids(vec![0x0F83, 0xBEEF]), vec![0x0F83]);
    }

    #[test]
    fn live_view_names_round_trip() {
        for v in LiveView::ALL {
            assert_eq!(LiveView::from_name(v.name()), Some(v));
        }
        assert_eq!(LiveView::from_name("nope"), None);
    }

    #[test]
    fn save_load_round_trip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("dde3-presets-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("DDE3_CONFIG", &dir);
        let s = Settings {
            presets: vec![Preset {
                name: "smoke".into(),
                ids: vec![0x0F10, 0x0F83],
            }],
            logger_ids: vec![0x0F10],
            live_ids: vec![0x0F10, 0x0F83],
            live_view: LiveView::Gauges,
        };
        save(&s).unwrap();
        let l = load();
        assert_eq!(l.presets.len(), 1);
        assert_eq!(l.presets[0].name, "smoke");
        assert_eq!(l.presets[0].ids, vec![0x0F10, 0x0F83]);
        assert_eq!(l.logger_ids, vec![0x0F10]);
        assert_eq!(l.live_ids, vec![0x0F10, 0x0F83]);
        assert_eq!(l.live_view, LiveView::Gauges);
        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("DDE3_CONFIG");
    }
}
