//! Semantic fidelity checks over emitted samples.
//!
//! `spec.md` §8 calls these "semantic lints: units/ranges sane, invariants hold,
//! rates and their integrals consistent". They exist because the pinned-signal
//! check cannot see everything: a scenario once pushed disk utilisation to
//! 101.5%, which is impossible for one device, and the pinned-signal lint passed
//! it cleanly because the signal was not against a bound — the *bound itself*
//! was wrong. The health engine caught it before we did.
//!
//! These checks run over generated samples with no agent involved, so they are
//! cheap enough for CI and scale to collectors nobody reviews by hand.

use std::collections::BTreeMap;

use sim_spec::{GeneratorSpec, Shape, Total};

use crate::{NodeEngine, Sample, ScenarioSet};

/// A violation found in generated data.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub node: String,
    pub chart: String,
    pub kind: Kind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A percentage above 100 or below 0, or another unit out of its range.
    UnitOutOfRange,
    /// A counter that went backwards.
    CounterWentBackwards,
    /// A conserved partition whose dimensions do not sum to its total.
    ConservationBroken,
    /// A dimension that never changed across the whole run.
    PerfectlyFlat,
    /// A value that is not finite.
    NotFinite,
    /// A partition whose total references a node attribute the node lacks, so
    /// the conserved quantity collapses to zero.
    MissingTotal,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::UnitOutOfRange => "unit out of range",
            Kind::CounterWentBackwards => "counter went backwards",
            Kind::ConservationBroken => "conservation broken",
            Kind::PerfectlyFlat => "perfectly flat",
            Kind::NotFinite => "not finite",
            Kind::MissingTotal => "partition total missing",
        }
    }
}

/// Upper bound for a unit string, where one is physically meaningful.
///
/// Deliberately conservative: only units whose ceiling is a fact rather than a
/// convention. Netdata renders many percentages that legitimately exceed 100
/// (CPU across multiple cores, for instance), so this matches only the exact
/// unit strings used for single-resource ratios.
fn unit_ceiling(units: &str) -> Option<f64> {
    match units {
        "percentage" | "%" => Some(100.0),
        "% of time working" => Some(100.0),
        _ => None,
    }
}

/// Run the semantic checks over `ticks` generated samples per node.
///
/// Nodes are checked concurrently and their violations concatenated in node
/// order, so the report is identical to a sequential run.
pub fn check(engines: &mut [NodeEngine], ticks: i64, start: i64, interval: i64) -> Vec<Violation> {
    crate::parallel::map_engines(engines, |engine| check_node(engine, ticks, start, interval))
        .into_iter()
        .flatten()
        .collect()
}

/// The semantic checks for one node. Owns its own violation list so nodes share
/// nothing while they run.
fn check_node(engine: &mut NodeEngine, ticks: i64, start: i64, interval: i64) -> Vec<Violation> {
    let mut out = Vec::new();

    {
        let node = engine.profile().hostname.clone();
        let spec: GeneratorSpec = engine.spec().clone();
        let plan: Vec<(String, usize)> = engine
            .charts()
            .iter()
            .map(|c| (c.chart_id.clone(), c.context_index))
            .collect();

        // chart -> dimension -> last value, for monotonicity and flatness.
        let mut last: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        let mut changed: BTreeMap<String, BTreeMap<String, bool>> = BTreeMap::new();

        for i in 0..ticks {
            let samples = engine.tick(
                &ScenarioSet::default(),
                start + i * interval,
                interval as f64,
            );
            for sample in &samples {
                check_sample(&node, &spec, engine, sample, &mut out);
                let counters = matches!(
                    spec.contexts[chart_index(engine, &sample.chart_id)].shape,
                    Shape::Counters { .. }
                ) || matches!(
                    spec.contexts[chart_index(engine, &sample.chart_id)].shape,
                    Shape::Partition {
                        accumulate: sim_spec::Accumulate::Jiffies,
                        ..
                    }
                );
                track(sample, counters, &mut last, &mut changed, &node, &mut out);
            }
        }

        // A dimension that never moved is only suspicious when it *should*
        // have moved. Two cases are legitimate and must not be reported, or the
        // real findings drown:
        //
        //   * a flat zero - a healthy host genuinely reports zero OOM kills and
        //     zero interface errors for as long as it stays healthy;
        //   * a declared constant - a signal whose min equals its max, or one
        //     sourced from an attribute, was authored as a fixed value: a
        //     configured max_connections, a link's negotiated duplex, a port's
        //     rated speed;
        //   * a value resting on a declared physical floor - `min_is_floor` is
        //     the spec author stating that sitting there is a fact, which is why
        //     the pinned-signal check already ignores it too. A 0.01-weight app
        //     group really does run exactly one process, for as long as it runs.
        //
        // What is left is a dimension pinned at a non-zero value it was not
        // declared to hold, which is how every "/" mount came to report 100%
        // full: the driver's base exceeded the mount's size, so it clamped.
        for (chart, dims) in &changed {
            let idx = chart_index_by_id(&plan, chart);
            for (dim, moved) in dims {
                if *moved {
                    continue;
                }
                let value = last
                    .get(chart)
                    .and_then(|d| d.get(dim))
                    .copied()
                    .unwrap_or(0);
                if value == 0 {
                    continue;
                }
                if idx.is_some_and(|i| dimension_is_constant(&spec, i, dim, value)) {
                    continue;
                }
                out.push(Violation {
                    node: node.clone(),
                    chart: chart.clone(),
                    kind: Kind::PerfectlyFlat,
                    detail: format!(
                        "dimension '{dim}' held {value} for all {ticks} samples without being \
                         declared constant"
                    ),
                });
            }
        }
    }

    out
}

fn chart_index_by_id(plan: &[(String, usize)], chart_id: &str) -> Option<usize> {
    plan.iter().find(|(id, _)| id == chart_id).map(|(_, i)| *i)
}

/// Whether a dimension's driving signal was authored as a fixed value, or is
/// resting on a value the spec declared it may legitimately rest on.
///
/// `flat_value` is the emitted integer the dimension held for the whole run.
fn dimension_is_constant(
    spec: &GeneratorSpec,
    context_index: usize,
    dim: &str,
    flat_value: i64,
) -> bool {
    let Some(ctx) = spec.contexts.get(context_index) else {
        return false;
    };
    let signal_name = match &ctx.shape {
        Shape::Independent { dimensions } => dimensions
            .iter()
            .find(|d| d.id == dim)
            .map(|d| d.signal.clone()),
        Shape::Counters { dimensions } => dimensions
            .iter()
            .find(|d| d.id == dim)
            .map(|d| d.rate_signal.clone()),
        // A partition dimension is driven by the shared driver, so a flat
        // partition means the driver is stuck - never a declared constant.
        Shape::Partition { .. } => None,
    };
    signal_name
        .and_then(|n| spec.signals.get(&n))
        // `from_attr` is a declared constant by construction: the value comes
        // from the node or instance attribute verbatim, with no seasonality,
        // noise or bounds. A port's rated speed does not vary and must not be
        // reported as a stuck signal.
        //
        // A declared physical floor counts only while the dimension is actually
        // sitting *on* it. Anywhere else, `min_is_floor` grants no exemption -
        // otherwise one annotation would silence every stuck value on the signal.
        .is_some_and(|s| {
            s.min == s.max
                || s.from_attr.is_some()
                || (s.min_is_floor && flat_value == s.min.round() as i64)
        })
}

/// Index of the context behind a chart id.
fn chart_index(engine: &NodeEngine, chart_id: &str) -> usize {
    engine
        .charts()
        .iter()
        .find(|c| c.chart_id == chart_id)
        .map(|c| c.context_index)
        .unwrap_or(0)
}

fn track(
    sample: &Sample,
    is_counter: bool,
    last: &mut BTreeMap<String, BTreeMap<String, i64>>,
    changed: &mut BTreeMap<String, BTreeMap<String, bool>>,
    node: &str,
    out: &mut Vec<Violation>,
) {
    let seen = last.entry(sample.chart_id.clone()).or_default();
    let moved = changed.entry(sample.chart_id.clone()).or_default();
    for (dim, value) in &sample.values {
        if let Some(prev) = seen.get(dim) {
            if prev != value {
                moved.insert(dim.clone(), true);
            }
            // Netdata's incremental algorithm treats a decrease as a counter
            // reset and emits a huge spike, so a non-monotonic counter shows up
            // on the dashboard as an impossible burst rather than as an error.
            if is_counter && value < prev {
                out.push(Violation {
                    node: node.to_string(),
                    chart: sample.chart_id.clone(),
                    kind: Kind::CounterWentBackwards,
                    detail: format!("dimension '{dim}' fell from {prev} to {value}"),
                });
            }
        } else {
            moved.entry(dim.clone()).or_insert(false);
        }
        seen.insert(dim.clone(), *value);
    }
}

fn check_sample(
    node: &str,
    spec: &GeneratorSpec,
    engine: &NodeEngine,
    sample: &Sample,
    out: &mut Vec<Violation>,
) {
    let Some(chart) = engine
        .charts()
        .iter()
        .find(|c| c.chart_id == sample.chart_id)
    else {
        return;
    };
    let ctx = &spec.contexts[chart.context_index];

    for (dim, value) in &sample.values {
        let v = *value as f64;
        if !v.is_finite() {
            out.push(Violation {
                node: node.to_string(),
                chart: sample.chart_id.clone(),
                kind: Kind::NotFinite,
                detail: format!("dimension '{dim}' is {v}"),
            });
        }
    }

    // Units: only meaningful for gauges emitted directly. Counter dimensions
    // hold cumulative totals that Netdata converts to rates on read, so their
    // raw values carry no unit semantics at all.
    if let Shape::Independent { dimensions } = &ctx.shape {
        if let Some(ceiling) = unit_ceiling(&ctx.units) {
            for (d, (dim, value)) in dimensions.iter().zip(&sample.values) {
                // Divisors scale the emitted integer into display units.
                let shown = (*value as f64) * (d.multiplier as f64) / (d.divisor as f64);
                if shown > ceiling + 1e-9 || shown < -ceiling - 1e-9 {
                    out.push(Violation {
                        node: node.to_string(),
                        chart: sample.chart_id.clone(),
                        kind: Kind::UnitOutOfRange,
                        detail: format!(
                            "dimension '{dim}' displays {shown:.2} {} which exceeds {ceiling}",
                            ctx.units
                        ),
                    });
                }
            }
        }
    }

    // Conservation, checked against what was actually emitted rather than
    // against the evaluator's intent.
    if let Shape::Partition {
        total,
        accumulate: sim_spec::Accumulate::None,
        ..
    } = &ctx.shape
    {
        let expected = match total {
            Total::Constant { value } => Some(*value),
            Total::NodeAttr { name } => {
                let v = chart
                    .instance_attrs_get(name)
                    .or_else(|| engine.profile().attr(name));
                if v.is_none() {
                    // Without the attribute the total is zero, so every
                    // dimension collapses to zero and the chart is silently
                    // dead rather than wrong-looking.
                    out.push(Violation {
                        node: node.to_string(),
                        chart: sample.chart_id.clone(),
                        kind: Kind::MissingTotal,
                        detail: format!(
                            "total references node attribute '{name}', which this node does not define"
                        ),
                    });
                }
                v
            }
        };
        if let Some(expected) = expected {
            let sum: i64 = sample.values.iter().map(|(_, v)| *v).sum();
            if (sum as f64 - expected).abs() > 1.5 {
                out.push(Violation {
                    node: node.to_string(),
                    chart: sample.chart_id.clone(),
                    kind: Kind::ConservationBroken,
                    detail: format!("dimensions sum to {sum}, expected {expected}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Instance, NodeProfile};
    use std::sync::Arc;

    const GOOD: &str = r#"
version: 1
name: good
signals:
  busy:
    base: 30.0
    min: 1.0
    max: 95.0
    noise: { kind: walk, sigma: 0.04 }
  used_kb:
    base: 4000000.0
    min: 100000.0
    max: 7000000.0
    noise: { kind: gauss, sigma: 0.01 }
contexts:
  - id: system.ram
    title: RAM
    units: MiB
    family: ram
    chart_type: stacked
    priority: 200
    shape: partition
    total: { from: node_attr, name: ram_total_kb }
    driver: used_kb
    dimensions:
      - { id: used, share: 1.0 }
      - { id: free, remainder: true }
  - id: disk.util
    title: Utilization
    units: percentage
    family: utilization
    chart_type: line
    priority: 300
    shape: independent
    dimensions:
      - { id: utilization, signal: busy }
"#;

    fn profile() -> NodeProfile {
        NodeProfile {
            guid: "guid-a".into(),
            hostname: "sim-test-01".into(),
            role: None,
            attrs: BTreeMap::from([
                ("cores".to_string(), 8.0),
                ("ram_total_kb".to_string(), 16_777_216.0),
            ]),
            labels: BTreeMap::new(),
            instances: BTreeMap::new(),
            utc_offset_secs: 0,
        }
    }

    fn run(yaml: &str, ticks: i64) -> Vec<Violation> {
        let spec = Arc::new(GeneratorSpec::from_yaml(yaml).expect("parses"));
        let mut engines = vec![NodeEngine::new(spec, profile(), 7)];
        check(&mut engines, ticks, 1_700_000_000, 1)
    }

    #[test]
    fn a_healthy_spec_produces_no_violations() {
        let v = run(GOOD, 200);
        assert!(v.is_empty(), "unexpected violations: {v:?}");
    }

    #[test]
    fn catches_a_percentage_above_one_hundred() {
        // This is the 101.5% disk utilisation artifact, reproduced: a bound set
        // above what the unit permits. The pinned-signal lint passes it because
        // nothing is clamped - the bound itself is wrong.
        let yaml = GOOD
            .replace("    max: 95.0", "    max: 400.0")
            .replace("base: 30.0", "base: 260.0");
        let v = run(&yaml, 50);
        assert!(
            v.iter().any(|x| x.kind == Kind::UnitOutOfRange),
            "should catch >100%: {v:?}"
        );
    }

    #[test]
    fn catches_a_partition_whose_total_attribute_is_missing() {
        // Without the attribute the total is zero, so every dimension collapses
        // to zero: the chart is silently dead rather than visibly wrong, which
        // is exactly the failure mode worth a dedicated check.
        let yaml = GOOD.replace("name: ram_total_kb", "name: nonexistent_attr");
        let v = run(&yaml, 20);
        assert!(
            v.iter().any(|x| x.kind == Kind::MissingTotal),
            "should catch the missing total: {v:?}"
        );
    }

    #[test]
    fn catches_a_partition_driver_that_exceeds_its_total() {
        // The real bug this check found: a mount whose driver base is larger
        // than the mount itself clamps to the total, so the filesystem reports
        // 100% full forever. Nothing is out of range and no bound is violated -
        // the only visible symptom is that the numbers never move.
        let yaml = GOOD
            .replace("base: 4000000.0", "base: 90000000.0")
            .replace("max: 7000000.0", "max: 99000000.0");
        let v = run(&yaml, 60);
        assert!(
            v.iter().any(|x| x.kind == Kind::PerfectlyFlat),
            "should catch the stuck partition: {v:?}"
        );
    }

    #[test]
    fn a_declared_constant_is_not_reported_as_flat() {
        // An MTU or a negotiated link speed genuinely never moves. Reporting
        // those would bury the findings that matter.
        let yaml = GOOD.replace(
            "  busy:\n    base: 30.0\n    min: 1.0\n    max: 95.0\n    noise: { kind: walk, sigma: 0.04 }",
            "  busy:\n    base: 30.0\n    min: 30.0\n    max: 30.0",
        );
        let v = run(&yaml, 60);
        assert!(
            !v.iter().any(|x| x.kind == Kind::PerfectlyFlat),
            "declared constants must not be flagged: {v:?}"
        );
    }

    #[test]
    fn a_flat_zero_is_not_reported() {
        // A healthy host really does report zero OOM kills forever.
        let yaml = r#"
version: 1
name: quiet
signals:
  errors:
    base: 0.0
    min: 0.0
    max: 1000.0
contexts:
  - id: net.errors
    title: Errors
    units: errors/s
    family: errors
    chart_type: line
    priority: 100
    shape: counters
    dimensions:
      - { id: inbound, rate_signal: errors }
"#;
        let v = run(yaml, 60);
        assert!(v.is_empty(), "a healthy zero must not be flagged: {v:?}");
    }

    #[test]
    fn counters_are_checked_for_monotonicity() {
        // Guard on the guard: a counter context must be exercised so a future
        // regression in accumulation is caught here rather than on a dashboard,
        // where a decrease renders as an impossible spike.
        let yaml = r#"
version: 1
name: counters
signals:
  rate:
    base: 400.0
    min: 0.0
    max: 100000.0
    noise: { kind: gauss, sigma: 0.5 }
contexts:
  - id: system.io
    title: IO
    units: KiB/s
    family: disk
    chart_type: area
    priority: 150
    shape: counters
    dimensions:
      - { id: reads, rate_signal: rate }
"#;
        let v = run(yaml, 500);
        assert!(
            !v.iter().any(|x| x.kind == Kind::CounterWentBackwards),
            "counters must be monotonic: {v:?}"
        );
    }

    #[test]
    fn instanced_partitions_use_their_own_totals() {
        let yaml = r#"
version: 1
name: mounts
signals:
  used_kb:
    base: 100000000.0
    min: 1000.0
    max: 400000000.0
    noise: { kind: walk, sigma: 0.002 }
contexts:
  - id: disk.space
    title: Space
    units: GiB
    family: utilization
    chart_type: stacked
    priority: 300
    instances: { group: mount, chart_prefix: disk_space, family: "{instance}" }
    shape: partition
    total: { from: node_attr, name: disk_total_kb }
    driver: used_kb
    dimensions:
      - { id: used,  share: 1.0 }
      - { id: avail, remainder: true }
"#;
        let spec = Arc::new(GeneratorSpec::from_yaml(yaml).unwrap());
        let mut p = profile();
        p.instances.insert(
            "mount".into(),
            vec![
                Instance {
                    name: "/".into(),
                    weight: 1.0,
                    attrs: BTreeMap::from([("disk_total_kb".to_string(), 524_288_000.0)]),
                },
                Instance {
                    name: "/boot".into(),
                    weight: 0.004,
                    attrs: BTreeMap::from([("disk_total_kb".to_string(), 2_097_152.0)]),
                },
            ],
        );
        let mut engines = vec![NodeEngine::new(spec, p, 11)];
        let v = check(&mut engines, 100, 1_700_000_000, 1);
        assert!(
            !v.iter().any(|x| x.kind == Kind::ConservationBroken),
            "per-instance totals should each conserve: {v:?}"
        );
    }
}
