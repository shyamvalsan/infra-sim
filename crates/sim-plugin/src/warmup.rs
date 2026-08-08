//! Scheduled warm-up incidents.
//!
//! `spec.md` §3 asks for "2-3 minor, auto-resolving" incidents during warm-up
//! so the alert log and anomaly history have texture before a live session. A
//! fleet whose alert log is empty reads as a fleet nothing has ever happened
//! to, which is the opposite of what a prospect's estate looks like.
//!
//! ## Deterministic, not scheduled
//!
//! Nothing is stored and no timer runs. Which incident is active at time `t` is
//! a pure function of `(seed, t)`, computed fresh every tick. That keeps the
//! replay promise — an archived environment plus its seed reproduces the same
//! warm-up history — and means the plugin never writes `control.yaml`, which
//! the console owns. A plugin restart mid-incident resumes exactly where it
//! left off, because there is no state to lose.
//!
//! ## Minor, by construction
//!
//! Each incident runs only the opening stretch of a scenario's timeline, so it
//! reaches a first-order symptom and stops well short of the crisis the same
//! scenario produces when an SE triggers it deliberately. A warm-up incident
//! that looked like the hero demo would spoil the demo.

use std::collections::BTreeMap;

use sim_engine::{ActiveScenario, ScenarioSet};
use sim_spec::Scenario;

/// How often a warm-up slot opens. Roughly four incidents a day, so a 72-hour
/// warm-up leaves a dozen or so in the alert log — enough to read as lived-in,
/// not so many that the fleet looks broken.
const SLOT_SECONDS: i64 = 6 * 3600;

/// How long an incident runs before it stops.
const INCIDENT_SECONDS: i64 = 20 * 60;

/// Fraction of a slot in which an incident may start, so incidents do not all
/// land on the hour.
const JITTER_WINDOW: i64 = SLOT_SECONDS - INCIDENT_SECONDS - 600;

/// Scenarios currently running as warm-up incidents.
///
/// `roles` is the fleet's roles, used to skip scenarios this environment cannot
/// host — the same rule the console applies when deciding what to offer.
pub fn active(
    library: &BTreeMap<String, Scenario>,
    roles: &[&str],
    seed: u64,
    now: i64,
) -> ScenarioSet {
    let usable: Vec<&Scenario> = library
        .values()
        .filter(|s| s.applies_to(roles))
        .filter(|s| !s.timeline.is_empty())
        .collect();
    if usable.is_empty() {
        return ScenarioSet::default();
    }

    let slot = now.div_euclid(SLOT_SECONDS);
    let slot_start = slot * SLOT_SECONDS;

    // Everything below is derived from (seed, slot), so the same slot always
    // produces the same incident no matter when it is evaluated.
    let h = mix(seed, slot);
    let pick = (h % usable.len() as u64) as usize;
    let offset = if JITTER_WINDOW > 0 {
        ((h >> 32) % JITTER_WINDOW as u64) as i64
    } else {
        0
    };
    let started_at = slot_start + offset;

    if now < started_at || now >= started_at + INCIDENT_SECONDS {
        return ScenarioSet::default();
    }

    ScenarioSet::new(vec![ActiveScenario {
        scenario: minor(usable[pick]),
        started_at,
    }])
}

/// A description of what is running, for the plugin's log line.
pub fn describe_active(set: &ScenarioSet, now: i64) -> Option<String> {
    let a = set.active().first()?;
    let elapsed = now - a.started_at;
    Some(format!(
        "warm-up incident '{}' running ({}m of {}m)",
        a.scenario.name,
        elapsed / 60,
        INCIDENT_SECONDS / 60
    ))
}

/// Trim a scenario to its opening steps.
///
/// Hero scenarios are authored to build to a crisis. Keeping only the steps
/// that begin inside the incident window leaves the early, mild part of the
/// causal chain — enough for the health engine and ML to notice, not enough to
/// look like the scenario an SE will trigger on purpose later.
fn minor(scenario: &Scenario) -> Scenario {
    let mut out = scenario.clone();
    out.timeline
        .retain(|step| step.at.seconds() < INCIDENT_SECONDS / 2);
    if out.timeline.is_empty() {
        // Every step starts late; keep the first so the incident is not empty.
        out.timeline = vec![scenario.timeline[0].clone()];
    }
    out.name = format!("warmup-{}", scenario.name);
    out
}

/// SplitMix64 finalizer over the seed and slot. Cheap, and good enough that
/// consecutive slots do not correlate.
fn mix(seed: u64, slot: i64) -> u64 {
    let mut z = seed
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(slot as u64);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> BTreeMap<String, Scenario> {
        let mut m = BTreeMap::new();
        for (name, role) in [("alpha", "db"), ("beta", "web")] {
            let yaml = format!(
                "version: 1\nname: {name}\ndescription: t\nmanifest:\n  root_cause: t\n\
                 requires_roles: [{role}]\ntimeline:\n\
                 \x20 - at: 0s\n    description: early\n    target:\n      signal: cpu_busy\n\
                 \x20     role: {role}\n    effect: ramp\n    multiplier: 3.0\n    over: 10m\n\
                 \x20 - at: 40m\n    description: late crisis\n    target:\n      signal: cpu_busy\n\
                 \x20     role: {role}\n    effect: step\n    multiplier: 9.0\n"
            );
            m.insert(name.to_string(), Scenario::from_yaml(&yaml).unwrap());
        }
        m
    }

    #[test]
    fn an_incident_runs_for_a_bounded_window_each_slot() {
        let lib = library();
        let roles = ["db", "web"];
        // Walk a whole day a minute at a time and count active minutes.
        let mut active_minutes = 0;
        let mut slots_seen = std::collections::BTreeSet::new();
        // Start on a slot boundary so the day covers exactly four whole slots.
        let start = (1_700_000_000i64 / SLOT_SECONDS) * SLOT_SECONDS;
        for m in 0..(24 * 60) {
            let now = start + m * 60;
            let set = active(&lib, &roles, 42, now);
            if !set.is_empty() {
                active_minutes += 1;
                slots_seen.insert(now.div_euclid(SLOT_SECONDS));
            }
        }
        // Four slots a day, each ~20 minutes.
        assert_eq!(slots_seen.len(), 4, "expected one incident per 6h slot");
        assert!(
            (60..=100).contains(&active_minutes),
            "active for {active_minutes} minutes across the day"
        );
    }

    #[test]
    fn the_same_seed_and_time_always_give_the_same_incident() {
        // The replay promise depends on this: an archived environment plus its
        // seed must reproduce the same warm-up history.
        let lib = library();
        let roles = ["db", "web"];
        for m in 0..600 {
            let now = 1_700_000_000 + m * 37;
            let a = active(&lib, &roles, 7, now);
            let b = active(&lib, &roles, 7, now);
            assert_eq!(a.active().len(), b.active().len());
            if let (Some(x), Some(y)) = (a.active().first(), b.active().first()) {
                assert_eq!(x.scenario.name, y.scenario.name);
                assert_eq!(x.started_at, y.started_at);
            }
        }
    }

    #[test]
    fn a_different_seed_gives_a_different_history() {
        let lib = library();
        let roles = ["db", "web"];
        let names = |seed: u64| -> Vec<String> {
            (0..40)
                .filter_map(|s| {
                    let now = 1_700_000_000 + s * SLOT_SECONDS + 60;
                    active(&lib, &roles, seed, now)
                        .active()
                        .first()
                        .map(|a| a.scenario.name.clone())
                })
                .collect()
        };
        assert_ne!(names(1), names(2));
    }

    #[test]
    fn warm_up_incidents_stop_short_of_the_crisis() {
        // The late, severe steps must not run - a warm-up incident that looked
        // like the hero demo would spoil the demo.
        let lib = library();
        let trimmed = minor(&lib["alpha"]);
        assert_eq!(trimmed.timeline.len(), 1);
        assert!(trimmed.timeline[0].description.contains("early"));
        assert!(trimmed.name.starts_with("warmup-"));
    }

    #[test]
    fn a_fleet_without_the_required_roles_gets_no_incidents() {
        let lib = library();
        // Neither scenario applies to a cache-only fleet.
        for m in 0..(24 * 60) {
            let now = 1_700_000_000 + m * 60;
            assert!(active(&lib, &["cache"], 42, now).is_empty());
        }
    }

    #[test]
    fn an_empty_library_is_harmless() {
        assert!(active(&BTreeMap::new(), &["db"], 1, 1_700_000_000).is_empty());
    }
}
