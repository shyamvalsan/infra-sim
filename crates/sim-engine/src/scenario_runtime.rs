//! Scenario execution: turning armed scenarios into signal multipliers.
//!
//! Scenarios perturb generator inputs only. Nothing here touches an alert, an
//! anomaly score, or a dashboard — the real health engine and the real ML see
//! the perturbed data and reach their own conclusions. If a scenario runs and
//! Netdata stays quiet, the scenario was too weak; the answer is never to
//! fabricate the alert.

use std::collections::BTreeMap;

use sim_spec::Scenario;

/// A scenario that has been triggered, with the moment it started.
#[derive(Debug, Clone)]
pub struct ActiveScenario {
    pub scenario: Scenario,
    /// Unix seconds at which the timeline's `at: 0` falls.
    pub started_at: i64,
}

/// All scenarios known to the runtime and which of them are running.
///
/// Shared across every node's engine, because a scenario's whole point is that
/// it spans nodes: a database filling its disk should show up on the
/// application servers that depend on it.
#[derive(Debug, Clone, Default)]
pub struct ScenarioSet {
    active: Vec<ActiveScenario>,
}

impl ScenarioSet {
    pub fn new(active: Vec<ActiveScenario>) -> Self {
        Self { active }
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    pub fn active(&self) -> &[ActiveScenario] {
        &self.active
    }

    /// Combined multiplier for one signal on one node at `now`.
    ///
    /// Effects compound, so two scenarios hitting the same signal both apply —
    /// which is how a "noisy neighbour" plus a "slow disk" produce a worse
    /// outcome together than either alone, without either scenario knowing
    /// about the other.
    ///
    /// A `Recover` step is the exception: it scales whatever the earlier steps
    /// built back toward 1.0, so recovery unwinds the fault instead of becoming
    /// another multiplier on top of it.
    pub fn multiplier(
        &self,
        hostname: &str,
        role: Option<&str>,
        instance: &str,
        signal: &str,
        now: i64,
    ) -> f64 {
        let mut total = 1.0;
        for a in &self.active {
            let mut fault: f64 = 1.0;
            let mut recovery: f64 = 1.0;
            for step in &a.scenario.timeline {
                if !step.target.matches(hostname, role, instance, signal) {
                    continue;
                }
                let elapsed = (now - a.started_at - step.at.seconds()) as f64;
                if elapsed < 0.0 {
                    continue;
                }
                if step.effect.is_recovery() {
                    // Recover returns 1.0 -> 0.0 across its window; keep the
                    // smallest so the newest recovery dominates.
                    recovery = recovery.min(step.effect.multiplier_at(elapsed));
                } else {
                    fault *= step.effect.multiplier_at(elapsed);
                }
            }
            // Blend the fault back toward neutral as recovery progresses.
            total *= 1.0 + (fault - 1.0) * recovery;
        }
        total
    }

    /// Ground-truth summary for the console and the eval gym.
    pub fn manifests(&self) -> Vec<(&str, &sim_spec::Manifest, i64)> {
        self.active
            .iter()
            .map(|a| (a.scenario.name.as_str(), &a.scenario.manifest, a.started_at))
            .collect()
    }
}

/// Which scenarios the console wants running, read from the control file.
///
/// State rather than commands: the file says what *should* be active, so
/// re-reading it is idempotent and a plugin restart mid-demo resumes the same
/// scenarios at the same offsets instead of silently dropping them.
#[derive(Debug, Clone, Default)]
pub struct ControlState {
    pub active: BTreeMap<String, i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_spec::Scenario;

    fn scenario(yaml: &str) -> Scenario {
        Scenario::from_yaml(yaml).expect("scenario parses")
    }

    const RAMP: &str = r#"
version: 1
name: disk-fill
manifest:
  root_cause: sim-db-01 /var/lib/pgsql
timeline:
  - at: 0s
    target: { signal: disk_space_used_kb, hostname: sim-db-01 }
    effect: ramp
    multiplier: 3.0
    over: 100s
"#;

    #[test]
    fn an_untriggered_scenario_changes_nothing() {
        let set = ScenarioSet::default();
        assert_eq!(
            set.multiplier("sim-db-01", Some("db"), "", "disk_space_used_kb", 1_000),
            1.0
        );
    }

    #[test]
    fn a_ramp_moves_the_targeted_signal_only() {
        let set = ScenarioSet::new(vec![ActiveScenario {
            scenario: scenario(RAMP),
            started_at: 1_000,
        }]);
        // Halfway through the ramp.
        let m = set.multiplier("sim-db-01", Some("db"), "", "disk_space_used_kb", 1_050);
        assert!((m - 2.0).abs() < 1e-9, "got {m}");
        // A different signal on the same host is untouched.
        assert_eq!(
            set.multiplier("sim-db-01", Some("db"), "", "cpu_busy", 1_050),
            1.0
        );
        // A different host is untouched.
        assert_eq!(
            set.multiplier("sim-web-01", Some("web"), "", "disk_space_used_kb", 1_050),
            1.0
        );
    }

    #[test]
    fn effects_are_inert_before_their_offset() {
        let yaml = RAMP.replace("at: 0s", "at: 500s");
        let set = ScenarioSet::new(vec![ActiveScenario {
            scenario: scenario(&yaml),
            started_at: 1_000,
        }]);
        assert_eq!(
            set.multiplier("sim-db-01", Some("db"), "", "disk_space_used_kb", 1_100),
            1.0
        );
        assert!(set.multiplier("sim-db-01", Some("db"), "", "disk_space_used_kb", 1_600) > 1.0);
    }

    #[test]
    fn two_scenarios_on_one_signal_compound() {
        let a = ActiveScenario {
            scenario: scenario(RAMP),
            started_at: 1_000,
        };
        let b = ActiveScenario {
            scenario: scenario(&RAMP.replace("name: disk-fill", "name: other")),
            started_at: 1_000,
        };
        let set = ScenarioSet::new(vec![a, b]);
        // Each contributes 2.0 at the halfway point.
        let m = set.multiplier("sim-db-01", Some("db"), "", "disk_space_used_kb", 1_050);
        assert!((m - 4.0).abs() < 1e-9, "got {m}");
    }

    #[test]
    fn recovery_unwinds_the_fault_rather_than_scaling_it() {
        let yaml = r#"
version: 1
name: spike-then-heal
manifest:
  root_cause: sim-db-01
timeline:
  - at: 0s
    target: { signal: cpu_busy, hostname: sim-db-01 }
    effect: step
    multiplier: 4.0
  - at: 100s
    target: { signal: cpu_busy, hostname: sim-db-01 }
    effect: recover
    over: 100s
"#;
        let set = ScenarioSet::new(vec![ActiveScenario {
            scenario: scenario(yaml),
            started_at: 0,
        }]);
        let at = |t| set.multiplier("sim-db-01", Some("db"), "", "cpu_busy", t);
        assert!((at(50) - 4.0).abs() < 1e-9, "fault not applied: {}", at(50));
        // Halfway through recovery: 1 + (4-1)*0.5
        assert!((at(150) - 2.5).abs() < 1e-9, "mid-recovery: {}", at(150));
        // Fully recovered, back to baseline rather than stuck high.
        assert!((at(200) - 1.0).abs() < 1e-9, "not healed: {}", at(200));
        assert!((at(10_000) - 1.0).abs() < 1e-9, "did not stay healed");
    }

    #[test]
    fn instance_targeting_hits_only_the_named_device() {
        let yaml = RAMP.replace(
            "target: { signal: disk_space_used_kb, hostname: sim-db-01 }",
            "target: { signal: disk_space_used_kb, hostname: sim-db-01, instance: \"/var/lib/pgsql\" }",
        );
        let set = ScenarioSet::new(vec![ActiveScenario {
            scenario: scenario(&yaml),
            started_at: 1_000,
        }]);
        assert!(
            set.multiplier(
                "sim-db-01",
                Some("db"),
                "/var/lib/pgsql",
                "disk_space_used_kb",
                1_100
            ) > 1.0
        );
        assert_eq!(
            set.multiplier(
                "sim-db-01",
                Some("db"),
                "/var/log",
                "disk_space_used_kb",
                1_100
            ),
            1.0
        );
    }

    #[test]
    fn role_targeting_hits_every_node_of_that_role() {
        let yaml = RAMP.replace(
            "target: { signal: disk_space_used_kb, hostname: sim-db-01 }",
            "target: { signal: cpu_busy, role: web }",
        );
        let set = ScenarioSet::new(vec![ActiveScenario {
            scenario: scenario(&yaml),
            started_at: 1_000,
        }]);
        assert!(set.multiplier("sim-web-01", Some("web"), "", "cpu_busy", 1_100) > 1.0);
        assert!(set.multiplier("sim-web-02", Some("web"), "", "cpu_busy", 1_100) > 1.0);
        assert_eq!(
            set.multiplier("sim-db-01", Some("db"), "", "cpu_busy", 1_100),
            1.0
        );
    }
}
