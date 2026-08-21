//! Upload limits, and keeping them current without shipping a release.
//!
//! Discord's free cap has been 25 MB, then 8, then 25, then 10, and since
//! 13 August 2026 it is 20. Any of those numbers compiled into the app would
//! have been wrong within a year, so the list is data, layered:
//!
//! 1. a built-in copy, compiled in, which always exists and cannot fail to load;
//! 2. a user copy in the config directory, which wins when present.
//!
//! The user copy is what a remote refresh writes to, and what the Settings
//! screen edits. Corruption at that layer falls back to the built-in list
//! rather than leaving the app with no targets at all.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The shipped list. Compiled in so a missing or unreadable resource file can
/// never leave the app without any targets to offer.
const BUILT_IN: &str = include_str!("../resources/presets.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub id: String,
    pub group: String,
    pub label: String,
    pub bytes: u64,
    #[serde(default)]
    pub default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetFile {
    pub version: u32,
    #[serde(default)]
    pub updated: String,
    pub presets: Vec<Preset>,
    /// Where to re-fetch this list from. `None` — the default — means the app
    /// makes no network requests for presets at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    /// Unix seconds of the last successful refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_refreshed: Option<u64>,
}

impl PresetFile {
    /// The compiled-in list. Panics only if the file we ship is malformed,
    /// which a test catches at build time.
    pub fn built_in() -> Self {
        serde_json::from_str(BUILT_IN).expect("the bundled presets.json must parse")
    }

    pub fn find(&self, id: &str) -> Option<&Preset> {
        self.presets.iter().find(|preset| preset.id == id)
    }

    /// The preset marked `default`, or the first one.
    pub fn default_preset(&self) -> Option<&Preset> {
        self.presets.iter().find(|preset| preset.default).or_else(|| self.presets.first())
    }

    /// Presets in display order, grouped, with group order preserved from the
    /// file rather than sorted — "Discord" belongs at the top because that is
    /// what this app is for, not because D sorts early.
    pub fn grouped(&self) -> Vec<(String, Vec<&Preset>)> {
        let mut groups: Vec<(String, Vec<&Preset>)> = Vec::new();
        for preset in &self.presets {
            match groups.iter_mut().find(|(name, _)| name == &preset.group) {
                Some((_, members)) => members.push(preset),
                None => groups.push((preset.group.clone(), vec![preset])),
            }
        }
        groups
    }
}

pub fn user_presets_path(config_dir: &Path) -> PathBuf {
    config_dir.join("presets.json")
}

/// Load the effective preset list.
///
/// A user file that fails to parse is ignored rather than fatal: a bad remote
/// refresh or a hand-edit gone wrong should cost you your customisations, not
/// the ability to compress anything.
pub fn load(config_dir: &Path) -> PresetFile {
    let path = user_presets_path(config_dir);

    match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<PresetFile>(&raw) {
            Ok(parsed) if !parsed.presets.is_empty() => parsed,
            _ => PresetFile::built_in(),
        },
        Err(_) => PresetFile::built_in(),
    }
}

/// Write the user-level list.
pub fn save(config_dir: &Path, presets: &PresetFile) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let json = serde_json::to_string_pretty(presets)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(user_presets_path(config_dir), json)
}

/// Don't re-fetch more than once a day. Upload limits change about twice a
/// decade; polling harder would be pure noise.
const REFRESH_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// A hard cap on the download. The real file is under 2 KB; anything remotely
/// near this is not a preset list.
const MAX_REFRESH_BYTES: u64 = 256 * 1024;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Is a refresh due?
pub fn refresh_due(presets: &PresetFile, now: u64) -> bool {
    if presets.source_url.is_none() {
        return false;
    }
    match presets.last_refreshed {
        None => true,
        Some(last) => now.saturating_sub(last) >= REFRESH_INTERVAL_SECS,
    }
}

/// Validate a fetched list before it is allowed to replace anything.
///
/// The response is data from the network, so it is checked rather than
/// trusted: sizes must be sane, ids unique, and text short enough that a
/// hostile file cannot turn the picker into a wall of text.
pub fn validate(candidate: &PresetFile) -> Result<(), String> {
    if candidate.presets.is_empty() {
        return Err("the list contains no presets".to_string());
    }
    if candidate.presets.len() > 200 {
        return Err("the list contains an implausible number of presets".to_string());
    }

    let mut seen: Vec<&str> = Vec::with_capacity(candidate.presets.len());
    for preset in &candidate.presets {
        if preset.bytes == 0 || preset.bytes > 100_000_000_000 {
            return Err(format!("preset '{}' has an implausible size", preset.id));
        }
        if preset.id.is_empty() || preset.id.len() > 64 {
            return Err("a preset has a missing or overlong id".to_string());
        }
        if preset.label.len() > 80 || preset.group.len() > 40 {
            return Err(format!("preset '{}' has overlong text", preset.id));
        }
        if preset.note.as_ref().is_some_and(|note| note.len() > 240) {
            return Err(format!("preset '{}' has an overlong note", preset.id));
        }
        if seen.contains(&preset.id.as_str()) {
            return Err(format!("duplicate preset id '{}'", preset.id));
        }
        seen.push(&preset.id);
    }

    Ok(())
}

/// Fetch the configured list, if one is configured and a refresh is due.
///
/// Returns `Ok(None)` when there was nothing to do. Never panics and never
/// blocks startup — the caller runs it off the UI thread and ignores failures.
pub fn refresh(config_dir: &Path, force: bool) -> Result<Option<PresetFile>, String> {
    let current = load(config_dir);

    let Some(url) = current.source_url.clone() else {
        return Ok(None);
    };
    if !force && !refresh_due(&current, unix_now()) {
        return Ok(None);
    }
    // Plain HTTP would let anyone on the path rewrite the user's upload limits.
    if !url.starts_with("https://") {
        return Err("the preset URL must be https".to_string());
    }

    let body = ureq::get(&url)
        .call()
        .map_err(|e| format!("could not fetch presets: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("could not read the response: {e}"))?;

    if body.len() as u64 > MAX_REFRESH_BYTES {
        return Err("the response is far too large to be a preset list".to_string());
    }

    let mut fetched: PresetFile =
        serde_json::from_str(&body).map_err(|e| format!("the response is not valid JSON: {e}"))?;
    validate(&fetched)?;

    // The remote file does not get to redirect us somewhere else next time.
    fetched.source_url = Some(url);
    fetched.last_refreshed = Some(unix_now());

    save(config_dir, &fetched).map_err(|e| format!("could not save presets: {e}"))?;
    Ok(Some(fetched))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("media-compressor-tests")
            .join(format!("presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_bundled_list_parses_and_leads_with_discord() {
        let presets = PresetFile::built_in();
        assert!(!presets.presets.is_empty());

        let groups = presets.grouped();
        assert_eq!(groups[0].0, "Discord", "Discord should lead the list");
    }

    #[test]
    fn the_discord_free_tier_reflects_the_august_2026_increase() {
        let presets = PresetFile::built_in();
        let free = presets.find("discord-free").expect("discord-free must exist");

        assert_eq!(free.bytes, 20_000_000, "free tier was raised to 20 MB on 13 Aug 2026");
        assert!(free.default, "the free tier is the sensible default");
    }

    #[test]
    fn every_preset_has_a_unique_id_and_a_positive_size() {
        let presets = PresetFile::built_in();
        let mut seen = Vec::new();

        for preset in &presets.presets {
            assert!(preset.bytes > 0, "{} has no size", preset.id);
            assert!(!preset.label.is_empty(), "{} has no label", preset.id);
            assert!(!seen.contains(&preset.id), "duplicate preset id {}", preset.id);
            seen.push(preset.id.clone());
        }
    }

    #[test]
    fn exactly_one_preset_is_marked_default() {
        let count = PresetFile::built_in().presets.iter().filter(|p| p.default).count();
        assert_eq!(count, 1, "ambiguous defaults make the startup target arbitrary");
    }

    fn one_preset(id: &str, bytes: u64) -> Preset {
        Preset {
            id: id.to_string(),
            group: "Custom".to_string(),
            label: "My cap".to_string(),
            bytes,
            default: true,
            note: None,
        }
    }

    fn file_with(presets: Vec<Preset>) -> PresetFile {
        PresetFile {
            version: 1,
            updated: "2026-08-21".to_string(),
            presets,
            source_url: None,
            last_refreshed: None,
        }
    }

    #[test]
    fn no_source_url_means_no_network_activity_is_ever_due() {
        let presets = file_with(vec![one_preset("mine", 1000)]);
        assert!(!refresh_due(&presets, unix_now()), "an unconfigured app must not phone home");
    }

    #[test]
    fn a_configured_url_refreshes_once_and_then_waits_a_day() {
        let mut presets = file_with(vec![one_preset("mine", 1000)]);
        presets.source_url = Some("https://example.invalid/presets.json".to_string());

        let now = 1_800_000_000u64;
        assert!(refresh_due(&presets, now), "never refreshed, so it is due");

        presets.last_refreshed = Some(now);
        assert!(!refresh_due(&presets, now + 60), "a minute later is not due");
        assert!(!refresh_due(&presets, now + REFRESH_INTERVAL_SECS - 1), "just under a day");
        assert!(refresh_due(&presets, now + REFRESH_INTERVAL_SECS), "a day later is due");
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_cause_a_refresh_storm() {
        let mut presets = file_with(vec![one_preset("mine", 1000)]);
        presets.source_url = Some("https://example.invalid/presets.json".to_string());
        presets.last_refreshed = Some(2_000_000_000);

        // saturating_sub keeps this from underflowing into "wildly overdue".
        assert!(!refresh_due(&presets, 1_000_000_000));
    }

    #[test]
    fn a_fetched_list_is_validated_rather_than_trusted() {
        assert!(validate(&file_with(vec![one_preset("ok", 20_000_000)])).is_ok());

        assert!(validate(&file_with(vec![])).is_err(), "an empty list is not usable");
        assert!(validate(&file_with(vec![one_preset("zero", 0)])).is_err(), "zero size");
        assert!(
            validate(&file_with(vec![one_preset("huge", 999_000_000_000)])).is_err(),
            "a 999 GB cap is not a real limit"
        );
        assert!(
            validate(&file_with(vec![one_preset("dup", 100), one_preset("dup", 200)])).is_err(),
            "duplicate ids"
        );

        let mut overlong = one_preset("long", 100);
        overlong.label = "x".repeat(200);
        assert!(validate(&file_with(vec![overlong])).is_err(), "overlong label");
    }

    #[test]
    fn refreshing_without_a_configured_url_does_nothing_at_all() {
        let dir = temp_dir("no-url");
        assert_eq!(refresh(&dir, true).unwrap(), None);
    }

    #[test]
    fn a_plain_http_url_is_refused() {
        let dir = temp_dir("http");
        let mut presets = file_with(vec![one_preset("mine", 1000)]);
        presets.source_url = Some("http://example.invalid/presets.json".to_string());
        save(&dir, &presets).unwrap();

        let error = refresh(&dir, true).unwrap_err();
        assert!(error.contains("https"), "got {error}");
    }

    #[test]
    fn a_user_file_overrides_the_bundled_list() {
        let dir = temp_dir("override");
        let custom = file_with(vec![one_preset("mine", 12_345)]);

        save(&dir, &custom).unwrap();
        let loaded = load(&dir);

        assert_eq!(loaded.presets.len(), 1);
        assert_eq!(loaded.default_preset().unwrap().id, "mine");
    }

    #[test]
    fn a_corrupt_user_file_falls_back_instead_of_leaving_no_targets() {
        let dir = temp_dir("corrupt");
        std::fs::write(user_presets_path(&dir), "{ this is not json").unwrap();

        let loaded = load(&dir);
        assert!(loaded.find("discord-free").is_some(), "should fall back to the bundled list");
    }

    #[test]
    fn an_empty_user_file_falls_back_too() {
        let dir = temp_dir("empty");
        std::fs::write(
            user_presets_path(&dir),
            r#"{"version":1,"updated":"","presets":[]}"#,
        )
        .unwrap();

        assert!(!load(&dir).presets.is_empty(), "an empty list is not a usable configuration");
    }
}
