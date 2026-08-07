//! Declarative scenarios: timed effects applied to generator signals.
//!
//! A scenario never fabricates an alert or an anomaly. It perturbs the *inputs*
//! — the same signals a healthy node uses — and lets the real health engine and
//! the real ML decide whether that constitutes a problem. That is the hard rule
//! applied to incidents: if a scenario runs and Netdata raises nothing, the
//! honest conclusion is that the scenario was too weak, not that the alert
//! should be faked.
//!
//! Every scenario carries a [`Manifest`] recording root cause, causal chain and
//! expected blast radius. The eval gym scores time-to-detect and root-cause
//! accuracy against it, so the manifest is written when the scenario is
//! authored, not reconstructed afterwards from what the product happened to do.

use serde::{Deserialize, Serialize};

use crate::SpecError;

pub const SCENARIO_VERSION: u32 = 1;

/// A scenario: what breaks, where, over what timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub version: u32,
    /// Stable id used to trigger and resolve this scenario.
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub manifest: Manifest,
    pub timeline: Vec<Step>,
}

impl Scenario {
    pub fn from_yaml(input: &str) -> Result<Self, SpecError> {
        let s: Scenario =
            serde_yaml::from_str(input).map_err(|e| SpecError::Parse(e.to_string()))?;
        s.validate()?;
        Ok(s)
    }

    fn validate(&self) -> Result<(), SpecError> {
        if self.version != SCENARIO_VERSION {
            return Err(SpecError::Version {
                found: self.version,
            });
        }
        if self.timeline.is_empty() {
            return Err(SpecError::EmptyTimeline {
                scenario: self.name.clone(),
            });
        }
        for step in &self.timeline {
            if step.target.signal.is_empty() {
                return Err(SpecError::EmptyTargetSignal {
                    scenario: self.name.clone(),
                });
            }
            if let Effect::Ramp { over, .. } = &step.effect {
                if over.seconds() == 0 {
                    return Err(SpecError::ZeroRamp {
                        scenario: self.name.clone(),
                    });
                }
            }
            if let Effect::Oscillate { period, .. } = &step.effect {
                if period.seconds() == 0 {
                    return Err(SpecError::ZeroPeriod {
                        scenario: self.name.clone(),
                    });
                }
            }
            if let Effect::AddRamp { over, .. } = &step.effect {
                if over.seconds() == 0 {
                    return Err(SpecError::ZeroRamp {
                        scenario: self.name.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Total timeline length, used by the console to show progress.
    pub fn duration(&self) -> i64 {
        self.timeline
            .iter()
            .map(|s| s.at.seconds() + s.effect.settle_seconds())
            .max()
            .unwrap_or(0)
    }
}

/// Ground truth for the eval gym and for demo rehearsal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The component an investigator should land on.
    pub root_cause: String,
    /// Ordered causal chain, most upstream first.
    #[serde(default)]
    pub causal_chain: Vec<String>,
    /// Hostnames expected to show symptoms, including the root cause.
    #[serde(default)]
    pub blast_radius: Vec<String>,
    /// What a correct investigation should conclude, in one sentence.
    #[serde(default)]
    pub expected_finding: String,
}

/// One timed effect.
///
/// No `deny_unknown_fields`: serde cannot combine it with the `flatten` that
/// keeps `effect:` at step level rather than nested. `Target` stays strict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Offset from scenario trigger.
    #[serde(default)]
    pub at: Duration,
    #[serde(default)]
    pub description: String,
    pub target: Target,
    #[serde(flatten)]
    pub effect: Effect,
}

/// Which signals a step perturbs.
///
/// An empty selector matches everything, so `{ signal: x }` hits the whole
/// fleet. Selectors combine with AND.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Generator signal to perturb. Scenarios move existing signals rather than
    /// introducing new ones, so a fault stays coupled to every context that
    /// signal feeds — which is what produces a coherent blast radius instead of
    /// one anomalous chart.
    pub signal: String,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// Restrict to one instance (a disk, a mount, an interface). Absent means
    /// node-level signals and every instance.
    #[serde(default)]
    pub instance: Option<String>,
}

impl Target {
    pub fn matches(
        &self,
        hostname: &str,
        role: Option<&str>,
        instance: &str,
        signal: &str,
    ) -> bool {
        if self.signal != signal {
            return false;
        }
        if let Some(want) = &self.hostname {
            if want != hostname {
                return false;
            }
        }
        if let Some(want) = &self.role {
            if role != Some(want.as_str()) {
                return false;
            }
        }
        if let Some(want) = &self.instance {
            if want != instance {
                return false;
            }
        }
        true
    }
}

/// How a signal is perturbed. All effects are multipliers on the signal's
/// value, so a scenario composes with seasonality and noise rather than
/// replacing them — a fault during the nightly trough looks different from the
/// same fault at peak, exactly as it would in reality.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    /// Jump to `multiplier` and hold.
    Step { multiplier: f64 },

    /// Move linearly from 1.0 to `multiplier` across `over`, then hold.
    Ramp { multiplier: f64, over: Duration },

    /// Compound `rate_per_hour` indefinitely. Models unbounded growth such as a
    /// leak, where the interesting property is that it never levels off.
    Drift { rate_per_hour: f64 },

    /// Oscillate between `1.0 ± amplitude` with the given period. Models
    /// flapping.
    Oscillate { amplitude: f64, period: Duration },

    /// Add an absolute amount, in the signal's own units.
    ///
    /// Required because a multiplier cannot lift a signal whose baseline is
    /// zero, and the signals that matter most in a fault are exactly those:
    /// OOM kills, interface errors, packet drops and TCP resets are all zero on
    /// a healthy host by design. Giving them a small non-zero base instead
    /// would mean a healthy fleet permanently reporting errors.
    Add { amount: f64 },

    /// Ramp an absolute amount from zero to `amount` across `over`.
    AddRamp { amount: f64, over: Duration },

    /// Return to normal over `over`. Recovery matters: showing a system heal is
    /// as persuasive as showing it break.
    Recover { over: Duration },
}

impl Effect {
    /// Seconds after a step's start before its value stops changing. `Drift`
    /// and `Oscillate` never settle, hence zero.
    fn settle_seconds(&self) -> i64 {
        match self {
            Effect::Step { .. } | Effect::Add { .. } => 0,
            Effect::Ramp { over, .. } => over.seconds(),
            Effect::AddRamp { over, .. } => over.seconds(),
            Effect::Recover { over } => over.seconds(),
            Effect::Drift { .. } | Effect::Oscillate { .. } => 0,
        }
    }

    /// Whether this effect contributes an absolute amount rather than a
    /// multiplier.
    pub fn is_additive(&self) -> bool {
        matches!(self, Effect::Add { .. } | Effect::AddRamp { .. })
    }

    /// Absolute amount contributed `elapsed` seconds after this step began.
    pub fn additive_at(&self, elapsed: f64) -> f64 {
        if elapsed < 0.0 {
            return 0.0;
        }
        match self {
            Effect::Add { amount } => *amount,
            Effect::AddRamp { amount, over } => {
                let span = over.seconds() as f64;
                amount * (elapsed / span).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    /// Multiplier `elapsed` seconds after this step became active.
    pub fn multiplier_at(&self, elapsed: f64) -> f64 {
        if elapsed < 0.0 {
            return 1.0;
        }
        match self {
            Effect::Add { .. } | Effect::AddRamp { .. } => 1.0,
            Effect::Step { multiplier } => *multiplier,

            Effect::Ramp { multiplier, over } => {
                let span = over.seconds() as f64;
                let t = (elapsed / span).clamp(0.0, 1.0);
                1.0 + (multiplier - 1.0) * t
            }

            Effect::Drift { rate_per_hour } => {
                let hours = elapsed / 3600.0;
                // Compounding, so a leak accelerates in absolute terms the way
                // a real one does.
                (1.0 + rate_per_hour).powf(hours)
            }

            Effect::Oscillate { amplitude, period } => {
                let p = period.seconds() as f64;
                let phase = std::f64::consts::TAU * elapsed / p;
                1.0 + amplitude * phase.sin()
            }

            Effect::Recover { over } => {
                let span = over.seconds() as f64;
                let t = (elapsed / span).clamp(0.0, 1.0);
                // Approaches 1.0; the runtime multiplies this against whatever
                // the earlier steps produced, so recovery unwinds them.
                1.0 - t
            }
        }
    }

    /// Whether this effect replaces prior multipliers rather than compounding.
    pub fn is_recovery(&self) -> bool {
        matches!(self, Effect::Recover { .. })
    }
}

/// A duration written as `45s`, `30m`, `2h` or `3d`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Duration(i64);

impl Duration {
    pub fn seconds(self) -> i64 {
        self.0
    }

    fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty duration".into());
        }
        let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
        let n: i64 = digits
            .parse()
            .map_err(|_| format!("'{s}' does not start with a number"))?;
        let mult = match unit {
            "s" | "" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86_400,
            other => return Err(format!("unknown duration unit '{other}' in '{s}'")),
        };
        Ok(Duration(n * mult))
    }
}

impl Serialize for Duration {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&format!("{}s", self.0))
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Accept both `30m` and a bare integer count of seconds.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Secs(i64),
        }
        match Raw::deserialize(de)? {
            Raw::Text(s) => Duration::parse(&s).map_err(serde::de::Error::custom),
            Raw::Secs(n) => Ok(Duration(n)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENARIO: &str = r#"
version: 1
name: disk-fill
description: WAL retention misconfigured on the primary
manifest:
  root_cause: sim-db-01 /var/lib/pgsql
  causal_chain:
    - WAL retention raised without resizing the volume
    - free space falls
  blast_radius: [sim-db-01]
  expected_finding: Disk fill on the database data volume
timeline:
  - at: 0s
    target: { signal: disk_space_used_kb, hostname: sim-db-01, instance: "/var/lib/pgsql" }
    effect: ramp
    multiplier: 2.4
    over: 45m
  - at: 30m
    target: { signal: disk_await_write_ms, role: db }
    effect: step
    multiplier: 3.0
"#;

    #[test]
    fn parses_a_scenario_with_a_manifest() {
        let s = Scenario::from_yaml(SCENARIO).expect("parses");
        assert_eq!(s.name, "disk-fill");
        assert_eq!(s.timeline.len(), 2);
        assert_eq!(s.manifest.blast_radius, vec!["sim-db-01"]);
        // 30m offset + step settles immediately; 0m + 45m ramp is the longest.
        assert_eq!(s.duration(), 45 * 60);
    }

    #[test]
    fn durations_accept_units_and_bare_seconds() {
        assert_eq!(Duration::parse("45s").unwrap().seconds(), 45);
        assert_eq!(Duration::parse("30m").unwrap().seconds(), 1800);
        assert_eq!(Duration::parse("2h").unwrap().seconds(), 7200);
        assert_eq!(Duration::parse("3d").unwrap().seconds(), 259_200);
        assert!(Duration::parse("5y").is_err());
        assert!(Duration::parse("").is_err());
    }

    #[test]
    fn ramp_interpolates_then_holds() {
        let e = Effect::Ramp {
            multiplier: 3.0,
            over: Duration(100),
        };
        assert!((e.multiplier_at(0.0) - 1.0).abs() < 1e-9);
        assert!((e.multiplier_at(50.0) - 2.0).abs() < 1e-9);
        assert!((e.multiplier_at(100.0) - 3.0).abs() < 1e-9);
        // Holds past the end rather than resetting.
        assert!((e.multiplier_at(10_000.0) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn drift_compounds_and_never_levels_off() {
        let e = Effect::Drift { rate_per_hour: 0.5 };
        let one = e.multiplier_at(3600.0);
        let two = e.multiplier_at(7200.0);
        assert!((one - 1.5).abs() < 1e-9);
        assert!((two - 2.25).abs() < 1e-9);
        assert!(two > one);
    }

    #[test]
    fn effects_are_inert_before_they_start() {
        let e = Effect::Step { multiplier: 9.0 };
        assert_eq!(e.multiplier_at(-1.0), 1.0);
    }

    #[test]
    fn oscillate_stays_within_its_amplitude() {
        let e = Effect::Oscillate {
            amplitude: 0.4,
            period: Duration(60),
        };
        for i in 0..600 {
            let v = e.multiplier_at(i as f64 * 0.1);
            assert!((0.6 - 1e-9..=1.4 + 1e-9).contains(&v), "out of band: {v}");
        }
    }

    #[test]
    fn target_selectors_combine_with_and() {
        let t = Target {
            signal: "disk_space_used_kb".into(),
            hostname: Some("sim-db-01".into()),
            role: None,
            instance: Some("/var/lib/pgsql".into()),
        };
        assert!(t.matches(
            "sim-db-01",
            Some("db"),
            "/var/lib/pgsql",
            "disk_space_used_kb"
        ));
        assert!(!t.matches("sim-db-01", Some("db"), "/var/log", "disk_space_used_kb"));
        assert!(!t.matches(
            "sim-web-01",
            Some("web"),
            "/var/lib/pgsql",
            "disk_space_used_kb"
        ));
        assert!(!t.matches("sim-db-01", Some("db"), "/var/lib/pgsql", "cpu_busy"));
    }

    #[test]
    fn an_empty_selector_matches_the_whole_fleet() {
        let t = Target {
            signal: "cpu_busy".into(),
            hostname: None,
            role: None,
            instance: None,
        };
        assert!(t.matches("anything", None, "", "cpu_busy"));
        assert!(t.matches("other", Some("db"), "eth0", "cpu_busy"));
    }

    #[test]
    fn additive_effects_can_lift_a_zero_baseline_signal() {
        // A multiplier cannot: OOM kills and interface errors are zero at rest
        // by design, and giving them a non-zero base would mean a healthy fleet
        // permanently reporting errors.
        let e = Effect::Add { amount: 3.0 };
        assert_eq!(e.multiplier_at(10.0), 1.0);
        assert_eq!(e.additive_at(10.0), 3.0);
        assert_eq!(e.additive_at(-1.0), 0.0);
        assert!(e.is_additive());
    }

    #[test]
    fn add_ramp_interpolates_the_absolute_amount() {
        let e = Effect::AddRamp {
            amount: 10.0,
            over: Duration(100),
        };
        assert!((e.additive_at(0.0) - 0.0).abs() < 1e-9);
        assert!((e.additive_at(50.0) - 5.0).abs() < 1e-9);
        assert!((e.additive_at(100.0) - 10.0).abs() < 1e-9);
        assert!((e.additive_at(9_999.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn multiplicative_effects_contribute_nothing_additive() {
        let e = Effect::Step { multiplier: 4.0 };
        assert_eq!(e.additive_at(10.0), 0.0);
        assert!(!e.is_additive());
    }

    #[test]
    fn rejects_a_zero_length_ramp() {
        let yaml = SCENARIO.replace("over: 45m", "over: 0s");
        let err = Scenario::from_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("zero-length ramp"), "{err}");
    }

    #[test]
    fn rejects_an_empty_timeline() {
        let yaml = SCENARIO.split("timeline:").next().unwrap().to_string() + "timeline: []\n";
        let err = Scenario::from_yaml(&yaml).unwrap_err();
        assert!(err.to_string().contains("empty timeline"), "{err}");
    }
}
