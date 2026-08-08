//! Scenario execution: turning armed scenarios into signal multipliers.
//!
//! Scenarios perturb generator inputs only. Nothing here touches an alert, an
//! anomaly score, or a dashboard — the real health engine and the real ML see
//! the perturbed data and reach their own conclusions. If a scenario runs and
//! Netdata stays quiet, the scenario was too weak; the answer is never to
//! fabricate the alert.

use sim_spec::Scenario;

/// How active scenarios alter one signal.
///
/// Two channels rather than one because a multiplier cannot lift a signal whose
/// baseline is zero, and the signals that matter most in a fault - OOM kills,
/// interface errors, TCP resets - are exactly those.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Perturbation {
    pub multiplier: f64,
    pub additive: f64,
}

impl Perturbation {
    pub const NONE: Self = Self {
        multiplier: 1.0,
        additive: 0.0,
    };

    pub fn is_none(&self) -> bool {
        self.multiplier == 1.0 && self.additive == 0.0
    }
}

/// A scenario that has been triggered, with the moment it started.
#[derive(Debug, Clone)]
pub struct ActiveScenario {
    pub scenario: Scenario,
    /// Unix seconds at which the timeline's `at: 0` falls.
    pub started_at: i64,
    /// Unix seconds at which resolve was pressed, if it has been.
    pub recovering_since: Option<i64>,
}

impl ActiveScenario {
    /// How much of this scenario's fault still applies, 1.0 down to 0.0.
    ///
    /// Eased rather than linear: a fault that unwinds fastest at the start and
    /// tails off is what a system draining a backlog actually does, and it
    /// keeps the last of the recovery visible instead of clipping it.
    pub fn strength(&self, now: i64) -> f64 {
        let Some(since) = self.recovering_since else {
            return 1.0;
        };
        let t = ((now - since) as f64 / crate::RECOVERY_SECONDS as f64).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }
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

    /// Combined perturbation for one signal on one node at `now`.
    ///
    /// Effects compound, so two scenarios hitting the same signal both apply —
    /// which is how a "noisy neighbour" plus a "slow disk" produce a worse
    /// outcome together than either alone, without either scenario knowing
    /// about the other.
    ///
    /// A `Recover` step is the exception: it scales whatever the earlier steps
    /// built back toward 1.0, so recovery unwinds the fault instead of becoming
    /// another multiplier on top of it.
    pub fn perturbation(
        &self,
        hostname: &str,
        role: Option<&str>,
        instance: &str,
        signal: &str,
        now: i64,
    ) -> Perturbation {
        let mut out = Perturbation::NONE;
        for a in &self.active {
            let mut fault: f64 = 1.0;
            let mut added: f64 = 0.0;
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
                } else if step.effect.is_additive() {
                    added += step.effect.additive_at(elapsed);
                } else {
                    fault *= step.effect.multiplier_at(elapsed);
                }
            }
            // Blend both contributions back toward neutral as recovery runs -
            // whether that recovery came from the scenario's own timeline or
            // from an operator pressing resolve.
            let recovery = recovery * a.strength(now);
            out.multiplier *= 1.0 + (fault - 1.0) * recovery;
            out.additive += added * recovery;
        }
        out
    }

    /// Convenience for callers that only care about the multiplicative part.
    #[cfg(test)]
    fn multiplier(
        &self,
        hostname: &str,
        role: Option<&str>,
        instance: &str,
        signal: &str,
        now: i64,
    ) -> f64 {
        self.perturbation(hostname, role, instance, signal, now)
            .multiplier
    }

    /// Ground-truth summary for the console and the eval gym.
    pub fn manifests(&self) -> Vec<(&str, &sim_spec::Manifest, i64)> {
        self.active
            .iter()
            .map(|a| (a.scenario.name.as_str(), &a.scenario.manifest, a.started_at))
            .collect()
    }
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
            recovering_since: None,
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
            recovering_since: None,
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
            recovering_since: None,
            scenario: scenario(RAMP),
            started_at: 1_000,
        };
        let b = ActiveScenario {
            recovering_since: None,
            scenario: scenario(&RAMP.replace("name: disk-fill", "name: other")),
            started_at: 1_000,
        };
        let set = ScenarioSet::new(vec![a, b]);
        // Each contributes 2.0 at the halfway point.
        let m = set.multiplier("sim-db-01", Some("db"), "", "disk_space_used_kb", 1_050);
        assert!((m - 4.0).abs() < 1e-9, "got {m}");
    }

    #[test]
    fn pressing_resolve_unwinds_the_fault_over_the_recovery_window() {
        let scenario = Scenario::from_yaml(
            "version: 1\nname: t\ndescription: d\nmanifest:\n  root_cause: r\n\
             timeline:\n  - at: 0s\n    description: s\n    target:\n      signal: cpu_busy\n\
             \x20   effect: step\n    multiplier: 5.0\n",
        )
        .unwrap();
        let at = |recovering: Option<i64>, now: i64| {
            ScenarioSet::new(vec![ActiveScenario {
                scenario: scenario.clone(),
                started_at: 0,
                recovering_since: recovering,
            }])
            .perturbation("h", None, "", "cpu_busy", now)
            .multiplier
        };
        assert_eq!(at(None, 600), 5.0, "untouched while running");

        // Immediately after resolve the fault is still at full strength, and it
        // is gone by the end of the window - the point being that it passes
        // through every value in between instead of snapping.
        assert!((at(Some(600), 600) - 5.0).abs() < 1e-6);
        let mid = at(Some(600), 600 + crate::RECOVERY_SECONDS / 2);
        assert!(mid > 1.0 && mid < 5.0, "mid-recovery was {mid}");
        assert!((at(Some(600), 600 + crate::RECOVERY_SECONDS) - 1.0).abs() < 1e-6);

        // Monotonic: recovery never briefly makes the fault worse.
        let mut prev = f64::MAX;
        for step in 0..=crate::RECOVERY_SECONDS {
            let v = at(Some(600), 600 + step);
            assert!(v <= prev + 1e-9, "recovery went backwards at {step}s");
            prev = v;
        }
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
            recovering_since: None,
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
    fn an_additive_effect_lifts_a_zero_baseline_signal() {
        let yaml = r#"
version: 1
name: oom
manifest:
  root_cause: sim-web-01
timeline:
  - at: 0s
    target: { signal: oom_kill_rate, hostname: sim-web-01 }
    effect: add
    amount: 2.0
"#;
        let set = ScenarioSet::new(vec![ActiveScenario {
            recovering_since: None,
            scenario: scenario(yaml),
            started_at: 0,
        }]);
        let p = set.perturbation("sim-web-01", Some("web"), "", "oom_kill_rate", 10);
        assert_eq!(p.multiplier, 1.0);
        assert_eq!(p.additive, 2.0);
        // Untargeted hosts are unaffected.
        assert!(set
            .perturbation("sim-web-02", Some("web"), "", "oom_kill_rate", 10)
            .is_none());
    }

    #[test]
    fn recovery_unwinds_an_additive_effect_too() {
        let yaml = r#"
version: 1
name: errors-then-heal
manifest:
  root_cause: sim-lb-01
timeline:
  - at: 0s
    target: { signal: net_err_rate, hostname: sim-lb-01 }
    effect: add
    amount: 40.0
  - at: 100s
    target: { signal: net_err_rate, hostname: sim-lb-01 }
    effect: recover
    over: 100s
"#;
        let set = ScenarioSet::new(vec![ActiveScenario {
            recovering_since: None,
            scenario: scenario(yaml),
            started_at: 0,
        }]);
        let at = |t| {
            set.perturbation("sim-lb-01", Some("lb"), "", "net_err_rate", t)
                .additive
        };
        assert!((at(50) - 40.0).abs() < 1e-9);
        assert!((at(150) - 20.0).abs() < 1e-9, "mid-recovery: {}", at(150));
        assert!((at(200) - 0.0).abs() < 1e-9, "not healed: {}", at(200));
    }

    #[test]
    fn instance_targeting_hits_only_the_named_device() {
        let yaml = RAMP.replace(
            "target: { signal: disk_space_used_kb, hostname: sim-db-01 }",
            "target: { signal: disk_space_used_kb, hostname: sim-db-01, instance: \"/var/lib/pgsql\" }",
        );
        let set = ScenarioSet::new(vec![ActiveScenario {
            recovering_since: None,
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
            recovering_since: None,
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
