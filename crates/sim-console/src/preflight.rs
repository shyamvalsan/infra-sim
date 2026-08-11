//! The preflight green board.
//!
//! `spec.md` §6.3: "No demo starts off a red board." The board's value is
//! entirely in its honesty — a check that goes green because it is easy to make
//! green is worse than no check, because it converts an unknown into false
//! confidence.
//!
//! So every check here is answered by querying the live agent, never by the
//! console's own bookkeeping, and anything the console cannot actually verify
//! is reported as [`Status::Manual`] rather than quietly passing.

use serde::Serialize;

use crate::agent::NodeState;

/// Hours of warm-up before the 18-model ML ensemble is complete.
///
/// 18 models at `train every = 3h` is 54h; `spec.md` rounds to 72h for margin.
/// Verified against `netdata/netdata src/ml/ml_config.cc:131-133` — note the
/// spec text quotes config keys that v2.10 marks obsolete, but the values are
/// unchanged.
pub const WARMUP_HOURS: f64 = 72.0;

/// Fraction of dimensions that must have trained models to call ML ready.
///
/// Not 100%: dimensions appear and disappear as instances come and go, so a
/// live fleet essentially never reports every dimension trained at once.
/// Demanding perfection would make the board permanently red and train SEs to
/// ignore it.
const ML_READY_FRACTION: f64 = 0.90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Fail,
    /// The console cannot verify this; a human must confirm it.
    Manual,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    /// What to do about a non-passing check.
    pub remedy: String,
}

impl Check {
    fn new(name: &str, status: Status, detail: String, remedy: &str) -> Self {
        Self {
            name: name.to_string(),
            status,
            detail,
            remedy: remedy.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Board {
    pub checks: Vec<Check>,
    /// True only when nothing failed. Warnings do not block, manual items do
    /// not auto-pass.
    pub demo_ready: bool,
}

/// Everything the board is evaluated against.
pub struct Inputs<'a> {
    /// Hostnames the environment file defines.
    pub expected_nodes: &'a [String],
    /// Live state as the agent reports it.
    pub states: &'a [NodeState],
    pub scenario_count: usize,
    pub active_scenarios: usize,
    pub seed: u64,
    /// `None` means the lint was not run this session, reported as manual
    /// rather than passing.
    pub lint_clean: Option<bool>,
    pub uptime_hours: Option<f64>,
    /// Simulated hostnames the agent knows that this environment does not
    /// define - leftovers from an earlier environment on the same agent.
    pub orphans: &'a [String],
}

/// Evaluate the board from live agent state.
pub fn evaluate(input: &Inputs<'_>) -> Board {
    let Inputs {
        expected_nodes,
        states,
        scenario_count,
        active_scenarios,
        seed,
        lint_clean,
        uptime_hours,
        orphans,
    } = *input;
    let mut checks = Vec::new();

    // --- All vnodes online -------------------------------------------------
    let online: Vec<&NodeState> = states.iter().filter(|s| s.reachable).collect();
    let missing: Vec<&String> = expected_nodes
        .iter()
        .filter(|h| !states.iter().any(|s| &&s.hostname == h && s.reachable))
        .collect();
    checks.push(if missing.is_empty() && !expected_nodes.is_empty() {
        Check::new(
            "All simulated nodes online",
            Status::Pass,
            format!("{}/{} reachable", online.len(), expected_nodes.len()),
            "",
        )
    } else {
        Check::new(
            "All simulated nodes online",
            Status::Fail,
            format!(
                "{}/{} reachable; missing: {}",
                online.len(),
                expected_nodes.len(),
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "Check the plugin is running: pgrep -f infra-sim.plugin",
        )
    });

    // --- Charts present ----------------------------------------------------
    // A vnode carries only what the plugin emits, so "online" is not the same
    // as "has a dashboard worth showing".
    let thin: Vec<&NodeState> = online.iter().copied().filter(|s| s.contexts < 40).collect();
    checks.push(if online.is_empty() {
        Check::new(
            "Nodes carry a full chart set",
            Status::Fail,
            "no nodes online".into(),
            "Nothing to measure until nodes are online",
        )
    } else if thin.is_empty() {
        let min = online.iter().map(|s| s.contexts).min().unwrap_or(0);
        let max = online.iter().map(|s| s.contexts).max().unwrap_or(0);
        Check::new(
            "Nodes carry a full chart set",
            Status::Pass,
            format!("{min}-{max} contexts per node"),
            "",
        )
    } else {
        Check::new(
            "Nodes carry a full chart set",
            Status::Fail,
            format!(
                "thin nodes: {}",
                thin.iter()
                    .map(|s| format!("{} ({} contexts)", s.hostname, s.contexts))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "A vnode shows only the contexts its generator emits; check the spec loaded",
        )
    });

    // --- ML trained --------------------------------------------------------
    let worst = online
        .iter()
        .map(|s| s.ml_fraction())
        .fold(f64::INFINITY, f64::min);
    checks.push(if online.is_empty() {
        Check::new(
            "ML models trained",
            Status::Fail,
            "no nodes online".into(),
            "Nothing to measure until nodes are online",
        )
    } else if worst >= ML_READY_FRACTION {
        Check::new(
            "ML models trained",
            Status::Pass,
            format!("worst node {:.0}% of dimensions trained", worst * 100.0),
            "",
        )
    } else {
        Check::new(
            "ML models trained",
            Status::Warn,
            format!("worst node {:.0}% of dimensions trained", worst * 100.0),
            "ML needs ~15 min for first models; leave the environment running",
        )
    });

    // --- Warm-up duration --------------------------------------------------
    checks.push(match uptime_hours {
        Some(h) if h >= WARMUP_HOURS => Check::new(
            "Warm-up >= 72h",
            Status::Pass,
            format!("{h:.1}h since environment start"),
            "",
        ),
        Some(h) => Check::new(
            "Warm-up >= 72h",
            Status::Warn,
            format!(
                "{h:.1}h of {WARMUP_HOURS:.0}h - the 18-model ensemble is not complete, \
                 so anomaly detection will be noisier than a real fleet's"
            ),
            "Start the environment at least 72h before the demo",
        ),
        None => Check::new(
            "Warm-up >= 72h",
            Status::Manual,
            "environment start time unknown".into(),
            "Record when this environment was first started",
        ),
    });

    // --- Fidelity lint -----------------------------------------------------
    checks.push(match lint_clean {
        Some(true) => Check::new(
            "Fidelity lint clean",
            Status::Pass,
            "no signals pinned to their bounds".into(),
            "",
        ),
        Some(false) => Check::new(
            "Fidelity lint clean",
            Status::Fail,
            "signals are clamped against their bounds".into(),
            "Run: infra-sim --environment <env> --lint 2",
        ),
        None => Check::new(
            "Fidelity lint clean",
            Status::Manual,
            "not run in this session".into(),
            "Run: infra-sim --environment <env> --lint 2",
        ),
    });

    // --- Scenarios armed ---------------------------------------------------
    checks.push(if scenario_count > 0 {
        Check::new(
            "Scenarios armed",
            Status::Pass,
            format!("{scenario_count} available, {active_scenarios} running"),
            "",
        )
    } else {
        Check::new(
            "Scenarios armed",
            Status::Warn,
            "no scenarios loaded".into(),
            "Place scenario YAML alongside the environment, in scenarios/",
        )
    });

    // --- Reproducibility ---------------------------------------------------
    checks.push(Check::new(
        "Seed recorded",
        Status::Pass,
        format!("seed {seed}"),
        "",
    ));

    // --- Orphaned nodes ----------------------------------------------------
    // A vnode GUID is a durable identity, so nodes from a previous environment
    // persist in the agent's database after their plugin stops. On a demo
    // dashboard they appear as stale, unreachable hosts alongside the live
    // fleet, which reads as a broken estate rather than a simulated one.
    checks.push(if orphans.is_empty() {
        Check::new(
            "No orphaned simulated nodes",
            Status::Pass,
            "agent knows only this environment's nodes".into(),
            "",
        )
    } else {
        Check::new(
            "No orphaned simulated nodes",
            Status::Warn,
            format!(
                "{} node(s) from a previous environment: {}",
                orphans.len(),
                orphans.join(", ")
            ),
            "Remove them from Cloud, or restart the agent to clear stale hosts",
        )
    });

    // --- Things the console genuinely cannot verify ------------------------
    // Reported as manual rather than passed. `spec.md` leaves Cloud API
    // coverage for these as an open engineering question, and a check that
    // cannot fail is not a check.
    checks.push(Check::new(
        "Claimed to a per-prospect Space",
        Status::Manual,
        "console cannot read Cloud Space membership".into(),
        "Confirm the Space is named '<Prospect> (Simulated Demo)' and contains only this fleet",
    ));
    checks.push(Check::new(
        "Warm-up incidents visible in the alert log",
        Status::Manual,
        "requires judging whether the alert history reads plausibly".into(),
        "Open the alert log and confirm 2-3 resolved incidents are present",
    ));

    let demo_ready = !checks.iter().any(|c| c.status == Status::Fail);
    Board { checks, demo_ready }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        host: &str,
        reachable: bool,
        contexts: usize,
        trained: f64,
        untrained: f64,
    ) -> NodeState {
        NodeState {
            hostname: host.into(),
            reachable,
            contexts,
            charts: contexts,
            ml_trained: trained,
            ml_untrained: untrained,
            ..Default::default()
        }
    }

    fn find<'a>(b: &'a Board, name: &str) -> &'a Check {
        b.checks.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn a_healthy_fleet_is_demo_ready() {
        let expected = vec!["a".to_string(), "b".to_string()];
        let states = vec![
            node("a", true, 78, 95.0, 5.0),
            node("b", true, 78, 95.0, 5.0),
        ];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: Some(true),
            uptime_hours: Some(80.0),
            orphans: &[],
        });
        assert!(b.demo_ready);
        assert_eq!(find(&b, "All simulated nodes online").status, Status::Pass);
    }

    #[test]
    fn a_missing_node_fails_the_board() {
        let expected = vec!["a".to_string(), "b".to_string()];
        let states = vec![node("a", true, 78, 95.0, 5.0)];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: Some(true),
            uptime_hours: Some(80.0),
            orphans: &[],
        });
        assert!(!b.demo_ready);
        assert_eq!(find(&b, "All simulated nodes online").status, Status::Fail);
    }

    #[test]
    fn a_thin_node_fails_even_though_it_is_online() {
        // Online is not the same as having a dashboard worth showing.
        let expected = vec!["a".to_string()];
        let states = vec![node("a", true, 6, 95.0, 5.0)];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: Some(true),
            uptime_hours: Some(80.0),
            orphans: &[],
        });
        assert!(!b.demo_ready);
        assert_eq!(
            find(&b, "Nodes carry a full chart set").status,
            Status::Fail
        );
    }

    #[test]
    fn short_warmup_warns_but_does_not_block() {
        let expected = vec!["a".to_string()];
        let states = vec![node("a", true, 78, 95.0, 5.0)];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: Some(true),
            uptime_hours: Some(2.0),
            orphans: &[],
        });
        assert_eq!(find(&b, "Warm-up >= 72h").status, Status::Warn);
        assert!(b.demo_ready, "a warning should not block the demo");
    }

    #[test]
    fn a_failing_lint_blocks_the_demo() {
        let expected = vec!["a".to_string()];
        let states = vec![node("a", true, 78, 95.0, 5.0)];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: Some(false),
            uptime_hours: Some(80.0),
            orphans: &[],
        });
        assert!(!b.demo_ready);
    }

    #[test]
    fn unverifiable_items_are_manual_and_never_silently_pass() {
        let expected = vec!["a".to_string()];
        let states = vec![node("a", true, 78, 95.0, 5.0)];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: None,
            uptime_hours: None,
            orphans: &[],
        });
        assert_eq!(
            find(&b, "Claimed to a per-prospect Space").status,
            Status::Manual
        );
        assert_eq!(find(&b, "Fidelity lint clean").status, Status::Manual);
        assert_eq!(find(&b, "Warm-up >= 72h").status, Status::Manual);
        // Manual items do not fail the board, but they are visibly not passes.
        assert!(b.demo_ready);
        assert!(
            b.checks
                .iter()
                .filter(|c| c.status == Status::Manual)
                .count()
                >= 3
        );
    }

    #[test]
    fn orphaned_nodes_from_a_previous_environment_are_flagged() {
        let expected = vec!["a".to_string()];
        let states = vec![node("a", true, 78, 95.0, 5.0)];
        let orphans = vec!["sim-old-01".to_string()];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: Some(true),
            uptime_hours: Some(80.0),
            orphans: &orphans,
        });
        let c = find(&b, "No orphaned simulated nodes");
        assert_eq!(c.status, Status::Warn);
        assert!(c.detail.contains("sim-old-01"), "{}", c.detail);
        // A leftover is untidy, not disqualifying.
        assert!(b.demo_ready);
    }

    #[test]
    fn untrained_ml_warns_rather_than_blocking() {
        let expected = vec!["a".to_string()];
        let states = vec![node("a", true, 78, 10.0, 90.0)];
        let b = evaluate(&Inputs {
            expected_nodes: &expected,
            states: &states,
            scenario_count: 5,
            active_scenarios: 0,
            seed: 42,
            lint_clean: Some(true),
            uptime_hours: Some(80.0),
            orphans: &[],
        });
        assert_eq!(find(&b, "ML models trained").status, Status::Warn);
    }
}
