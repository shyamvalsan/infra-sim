//! The control-file format, shared by the plugin (reader) and console (writer).
//!
//! Defined once, in a crate both depend on. The plugin and the console are
//! separate processes agreeing on a file; if each carried its own copy of the
//! struct, a field added on one side would be silently dropped by the other and
//! surface mid-demo as a scenario that refuses to trigger.
//!
//! The file records **state, not commands** — which scenarios should be running
//! and since when — so writing it is idempotent and a plugin restart resumes at
//! the same offsets instead of rewinding a fault the presenter is describing.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlFile {
    #[serde(default)]
    pub active: Vec<ActiveEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveEntry {
    pub scenario: String,
    /// Unix seconds the scenario's timeline is measured from.
    ///
    /// Always written by the console. When absent the plugin assigns "now" on
    /// first read, which means a restart rewinds the scenario — indistinguishable
    /// on screen from the fault resolving itself.
    #[serde(default)]
    pub started_at: Option<i64>,
}

impl ControlFile {
    pub fn load(path: &Path) -> Result<Self, String> {
        // A missing file means "nothing running", the correct resting state for
        // an environment nobody has triggered yet.
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read control file '{}': {e}", path.display()))?;
        serde_yaml::from_str(&raw)
            .map_err(|e| format!("cannot parse control file '{}': {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let body = serde_yaml::to_string(self)
            .map_err(|e| format!("cannot serialise control file: {e}"))?;
        // Written in place rather than written-then-renamed: only the file is
        // user-owned, the directory around it belongs to root, so a rename would
        // demand a privilege prompt at the worst possible moment.
        std::fs::write(path, body)
            .map_err(|e| format!("cannot write control file '{}': {e}", path.display()))
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.active.iter().any(|e| e.scenario == name)
    }

    pub fn started_at(&self, name: &str) -> Option<i64> {
        self.active
            .iter()
            .find(|e| e.scenario == name)
            .and_then(|e| e.started_at)
    }

    /// Add a scenario, preserving the start time of anything already running.
    pub fn trigger(&mut self, name: &str, now: i64) {
        if self.is_active(name) {
            return;
        }
        self.active.push(ActiveEntry {
            scenario: name.to_string(),
            started_at: Some(now),
        });
    }

    pub fn resolve(&mut self, name: &str) {
        self.active.retain(|e| e.scenario != name);
    }

    pub fn resolve_all(&mut self) {
        self.active.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_is_idempotent_and_preserves_start_time() {
        let mut c = ControlFile::default();
        c.trigger("disk-fill", 1_000);
        c.trigger("disk-fill", 9_999);
        assert_eq!(c.active.len(), 1);
        assert_eq!(c.started_at("disk-fill"), Some(1_000));
    }

    #[test]
    fn triggering_a_second_scenario_leaves_the_first_running() {
        let mut c = ControlFile::default();
        c.trigger("disk-fill", 1_000);
        c.trigger("mem-leak", 2_000);
        assert_eq!(c.started_at("disk-fill"), Some(1_000));
        assert_eq!(c.started_at("mem-leak"), Some(2_000));
    }

    #[test]
    fn resolve_removes_only_the_named_scenario() {
        let mut c = ControlFile::default();
        c.trigger("disk-fill", 1_000);
        c.trigger("mem-leak", 2_000);
        c.resolve("disk-fill");
        assert!(!c.is_active("disk-fill"));
        assert!(c.is_active("mem-leak"));
    }

    #[test]
    fn round_trips_through_yaml() {
        let mut c = ControlFile::default();
        c.trigger("disk-fill", 1_700_000_000);
        let yaml = serde_yaml::to_string(&c).unwrap();
        let back: ControlFile = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.started_at("disk-fill"), Some(1_700_000_000));
    }

    #[test]
    fn a_missing_file_loads_as_empty() {
        let c = ControlFile::load(Path::new("/nonexistent/infra-sim/control.yaml")).unwrap();
        assert!(c.active.is_empty());
    }
}
