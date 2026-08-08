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

    /// Unix seconds at which the operator pressed resolve.
    ///
    /// Set rather than deleting the entry, so the fault unwinds over
    /// `RECOVERY_SECONDS` instead of vanishing between two samples. A fault
    /// that disappears in one collection interval looks like a rendering
    /// glitch, and `spec.md` calls showing recovery as persuasive as showing
    /// failure.
    #[serde(default)]
    pub recovering_since: Option<i64>,
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
        // Re-triggering something mid-recovery cancels the recovery and keeps
        // the original start time, so the fault resumes where it was rather
        // than restarting from the top of the timeline.
        if let Some(e) = self.active.iter_mut().find(|e| e.scenario == name) {
            e.recovering_since = None;
            return;
        }
        self.active.push(ActiveEntry {
            scenario: name.to_string(),
            started_at: Some(now),
            recovering_since: None,
        });
    }

    /// Begin unwinding a scenario. Idempotent: pressing resolve twice does not
    /// restart the recovery.
    pub fn resolve(&mut self, name: &str, now: i64) {
        for e in self.active.iter_mut().filter(|e| e.scenario == name) {
            e.recovering_since.get_or_insert(now);
        }
    }

    pub fn resolve_all(&mut self, now: i64) {
        for e in self.active.iter_mut() {
            e.recovering_since.get_or_insert(now);
        }
    }

    /// Drop entries whose recovery has finished.
    ///
    /// Called by the console, which owns this file. The plugin never writes it;
    /// a finished entry simply contributes nothing.
    pub fn prune_recovered(&mut self, now: i64) {
        self.active.retain(|e| match e.recovering_since {
            Some(t) => now - t < crate::RECOVERY_SECONDS,
            None => true,
        });
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
    fn resolve_unwinds_only_the_named_scenario_then_prunes_it() {
        // Resolve used to delete the entry, so the fault vanished between two
        // samples - on screen indistinguishable from a rendering glitch.
        let mut c = ControlFile::default();
        c.trigger("disk-fill", 1000);
        c.trigger("noisy-neighbour", 1000);

        c.resolve("disk-fill", 2000);
        assert!(c.is_active("disk-fill"), "still present while it unwinds");
        assert!(c.is_active("noisy-neighbour"));

        // Idempotent: pressing resolve again does not restart the unwind.
        c.resolve("disk-fill", 2100);
        let e = c.active.iter().find(|e| e.scenario == "disk-fill").unwrap();
        assert_eq!(e.recovering_since, Some(2000));

        c.prune_recovered(2000 + crate::RECOVERY_SECONDS - 1);
        assert!(c.is_active("disk-fill"), "not yet finished");

        c.prune_recovered(2000 + crate::RECOVERY_SECONDS);
        assert!(!c.is_active("disk-fill"), "gone once it has unwound");
        assert!(c.is_active("noisy-neighbour"), "untouched");
    }

    #[test]
    fn re_triggering_mid_recovery_resumes_rather_than_restarting() {
        let mut c = ControlFile::default();
        c.trigger("disk-fill", 1000);
        c.resolve("disk-fill", 2000);
        c.trigger("disk-fill", 2050);
        let e = c.active.iter().find(|e| e.scenario == "disk-fill").unwrap();
        assert_eq!(e.recovering_since, None, "recovery cancelled");
        assert_eq!(e.started_at, Some(1000), "timeline position kept");
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
