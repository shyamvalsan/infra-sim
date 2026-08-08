//! Live scenario control.
//!
//! The plugin watches a control file and re-reads it when its mtime changes.
//! Whoever writes that file — the console, an SE with an editor, a rehearsal
//! script — triggers and resolves scenarios mid-demo.
//!
//! The file records **state, not commands**: it says which scenarios should be
//! running and since when. That makes re-reading idempotent, and it means a
//! plugin restart mid-demo resumes the same scenarios at the same offsets
//! rather than silently dropping a fault the presenter is talking about.
//!
//! The plugin only ever reads it, so the console needs no privileged handshake
//! and nothing has to be writable by the Netdata user.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sim_engine::{ActiveScenario, ControlFile, ScenarioSet};
use sim_spec::Scenario;

/// Watches the control file and builds the active [`ScenarioSet`].
pub struct ControlChannel {
    path: PathBuf,
    /// Scenarios available to trigger, by name.
    library: BTreeMap<String, Scenario>,
    last_mtime: Option<SystemTime>,
    /// Start times already assigned, so a re-read does not restart a running
    /// scenario and yank the demo back to its opening state.
    started: BTreeMap<String, i64>,
    current: ScenarioSet,
    /// Set when the file is unreadable or invalid, so the problem is reported
    /// once rather than every tick.
    last_error: Option<String>,
}

impl ControlChannel {
    pub fn new(path: PathBuf, library: BTreeMap<String, Scenario>) -> Self {
        Self {
            path,
            library,
            last_mtime: None,
            started: BTreeMap::new(),
            current: ScenarioSet::default(),
            last_error: None,
        }
    }

    /// Every scenario this environment knows about, running or not.
    pub fn library(&self) -> &BTreeMap<String, Scenario> {
        &self.library
    }

    pub fn scenarios(&self) -> &ScenarioSet {
        &self.current
    }

    /// Re-read the control file if it changed. Cheap enough to call every tick:
    /// the common case is a single `stat`.
    ///
    /// Returns a human-readable description when the active set changed, for
    /// the agent log.
    pub fn poll(&mut self, now: i64) -> Option<String> {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();

        // A missing control file means "no scenarios", which is the correct
        // resting state for an environment nobody has triggered yet.
        if mtime.is_none() {
            if self.last_mtime.is_some() || !self.current.is_empty() {
                self.last_mtime = None;
                self.started.clear();
                self.current = ScenarioSet::default();
                return Some("control file removed; all scenarios resolved".into());
            }
            return None;
        }

        if mtime == self.last_mtime {
            return None;
        }
        self.last_mtime = mtime;

        let parsed = match ControlFile::load(&self.path) {
            Ok(p) => p,
            Err(e) => return self.report_error(e),
        };
        self.last_error = None;

        let mut active = Vec::new();
        let mut names = Vec::new();
        let mut unknown = Vec::new();
        let mut still_running = BTreeMap::new();

        for entry in &parsed.active {
            let Some(scenario) = self.library.get(&entry.scenario) else {
                unknown.push(entry.scenario.clone());
                continue;
            };
            // Keep the original start time for an already-running scenario;
            // an unrelated edit must not restart it mid-demo.
            let started_at = entry
                .started_at
                .or_else(|| self.started.get(&entry.scenario).copied())
                .unwrap_or(now);
            still_running.insert(entry.scenario.clone(), started_at);
            names.push(entry.scenario.clone());
            active.push(ActiveScenario {
                scenario: scenario.clone(),
                started_at,
            });
        }

        let previous: Vec<String> = self.started.keys().cloned().collect();
        self.started = still_running;
        self.current = ScenarioSet::new(active);

        let mut msg = if names.is_empty() {
            "no scenarios active".to_string()
        } else {
            format!("scenarios active: {}", names.join(", "))
        };
        let resolved: Vec<&String> = previous.iter().filter(|p| !names.contains(p)).collect();
        if !resolved.is_empty() {
            msg.push_str(&format!(
                " (resolved: {})",
                resolved
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !unknown.is_empty() {
            msg.push_str(&format!(" (unknown, ignored: {})", unknown.join(", ")));
        }
        Some(msg)
    }

    fn report_error(&mut self, msg: String) -> Option<String> {
        // A malformed control file leaves the running scenarios untouched.
        // Dropping a fault mid-demo because of a stray keystroke would be far
        // worse than carrying on with the last good state.
        if self.last_error.as_ref() == Some(&msg) {
            return None;
        }
        self.last_error = Some(msg.clone());
        Some(format!("{msg} (keeping previous scenario state)"))
    }
}

/// Load every `*.yaml` scenario in a directory.
pub fn load_library(dir: &Path) -> Result<BTreeMap<String, Scenario>, String> {
    let mut out = BTreeMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // No scenario directory is a valid configuration: an environment can
        // run as a pure baseline with nothing to trigger.
        Err(_) => return Ok(out),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read scenario '{}': {e}", path.display()))?;
        let scenario = Scenario::from_yaml(&raw)
            .map_err(|e| format!("invalid scenario '{}': {e}", path.display()))?;
        out.insert(scenario.name.clone(), scenario);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENARIO: &str = r#"
version: 1
name: disk-fill
manifest:
  root_cause: sim-db-01
timeline:
  - at: 0s
    target: { signal: disk_space_used_kb, hostname: sim-db-01 }
    effect: ramp
    multiplier: 3.0
    over: 100s
"#;

    fn library() -> BTreeMap<String, Scenario> {
        let s = Scenario::from_yaml(SCENARIO).unwrap();
        BTreeMap::from([(s.name.clone(), s)])
    }

    fn tempfile(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("infra-sim-test-{name}-{}.yaml", std::process::id()));
        p
    }

    #[test]
    fn a_missing_control_file_means_no_scenarios() {
        let path = tempfile("missing");
        let _ = std::fs::remove_file(&path);
        let mut c = ControlChannel::new(path, library());
        assert!(c.poll(1_000).is_none());
        assert!(c.scenarios().is_empty());
    }

    #[test]
    fn triggering_and_resolving_via_the_file() {
        let path = tempfile("trigger");
        std::fs::write(
            &path,
            "active:\n  - { scenario: disk-fill, started_at: 1000 }\n",
        )
        .unwrap();
        let mut c = ControlChannel::new(path.clone(), library());

        let msg = c.poll(1_000).expect("change detected");
        assert!(msg.contains("disk-fill"), "{msg}");
        assert_eq!(c.scenarios().active().len(), 1);
        let m = c
            .scenarios()
            .perturbation("sim-db-01", Some("db"), "", "disk_space_used_kb", 1_050)
            .multiplier;
        assert!((m - 2.0).abs() < 1e-9, "scenario not applied: {m}");

        // Resolve.
        std::fs::write(&path, "active: []\n").unwrap();
        // mtime granularity can be coarse; force a distinct value.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, "active: []\n#\n").unwrap();
        let msg = c.poll(2_000).expect("resolve detected");
        assert!(msg.contains("resolved: disk-fill"), "{msg}");
        assert!(c.scenarios().is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_scenario_name_is_ignored_not_fatal() {
        let path = tempfile("unknown");
        std::fs::write(&path, "active:\n  - { scenario: nope }\n").unwrap();
        let mut c = ControlChannel::new(path.clone(), library());
        let msg = c.poll(1_000).expect("change detected");
        assert!(msg.contains("unknown, ignored: nope"), "{msg}");
        assert!(c.scenarios().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_malformed_file_keeps_the_running_scenario() {
        // Losing a fault mid-demo over a stray keystroke would be worse than
        // continuing with the last good state.
        let path = tempfile("malformed");
        std::fs::write(
            &path,
            "active:\n  - { scenario: disk-fill, started_at: 1000 }\n",
        )
        .unwrap();
        let mut c = ControlChannel::new(path.clone(), library());
        c.poll(1_000);
        assert_eq!(c.scenarios().active().len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, "active: [ this is not: valid: yaml\n").unwrap();
        let msg = c.poll(2_000).expect("error reported");
        assert!(msg.contains("keeping previous scenario state"), "{msg}");
        assert_eq!(c.scenarios().active().len(), 1, "scenario was dropped");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unrelated_edit_does_not_restart_a_running_scenario() {
        let path = tempfile("restart");
        std::fs::write(&path, "active:\n  - { scenario: disk-fill }\n").unwrap();
        let mut c = ControlChannel::new(path.clone(), library());
        c.poll(1_000);
        let first = c.scenarios().active()[0].started_at;
        assert_eq!(first, 1_000);

        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&path, "active:\n  - { scenario: disk-fill }\n# a comment\n").unwrap();
        c.poll(5_000);
        assert_eq!(
            c.scenarios().active()[0].started_at,
            first,
            "scenario restarted, yanking the demo back to its opening state"
        );
        let _ = std::fs::remove_file(&path);
    }
}
