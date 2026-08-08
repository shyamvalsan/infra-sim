//! Deterministic execution of Infra-Sim generator specs.
//!
//! Given a [`GeneratorSpec`], a [`NodeProfile`] and a master seed, this crate
//! produces the value stream for one simulated node. It holds the two
//! properties the product depends on:
//!
//! * **Reproducibility.** Same seed plus same config yields byte-identical
//!   output, so an archived `environment.yaml` replays a past demo exactly and
//!   the eval gym can score against a fixed ground truth.
//! * **Invariants by construction.** Conserved partitions and monotonic
//!   counters are structural properties of the evaluator, not assertions run
//!   afterwards. Code that could emit `free = 0` by clamping does not exist.

use std::collections::BTreeMap;
use std::sync::Arc;

use sim_spec::{Accumulate, GeneratorSpec, NoiseKind, Shape, Signal, Total};

pub mod control_file;
pub mod describe;
pub mod fidelity;
pub mod logs;
pub mod reskin;
pub mod rng;
pub mod scenario_runtime;

pub use control_file::{ActiveEntry, ControlFile};
use rng::Rng;
pub use scenario_runtime::{ActiveScenario, Perturbation, ScenarioSet};

/// Seconds in a day.
const DAY: i64 = 86_400;
/// Kernel USER_HZ: CPU time is reported in hundredths of a second per core.
const USER_HZ: f64 = 100.0;
/// Separator for signal-state keys. NUL cannot occur in a signal or instance
/// name, so scoped keys can never collide.
const KEY_SEP: char = '\0';

/// Node attribute name for core count, referenced by [`Accumulate::Jiffies`].
pub const ATTR_CORES: &str = "cores";

/// One instance of a per-device resource: a disk, an interface, a mount point.
#[derive(Debug, Clone)]
pub struct Instance {
    pub name: String,
    /// Scales every signal for this instance. A root disk carrying most of a
    /// host's IO and a near-idle secondary disk are the same spec at different
    /// weights.
    pub weight: f64,
    /// Attributes overriding the node's, e.g. per-mount `disk_total_kb`.
    pub attrs: BTreeMap<String, f64>,
}

/// Identity and hardware shape of one simulated node.
#[derive(Debug, Clone)]
pub struct NodeProfile {
    /// Stable UUID. This is the node's identity in Netdata: changing it orphans
    /// history, while changing `hostname` renames in place. The re-skin
    /// workflow in `spec.md` depends on that distinction.
    pub guid: String,
    pub hostname: String,
    pub role: Option<String>,
    /// Hardware attributes referenced by specs, e.g. `cores`, `ram_total_kb`.
    pub attrs: BTreeMap<String, f64>,
    /// Host labels emitted via `HOST_LABEL`.
    pub labels: BTreeMap<String, String>,
    /// Per-device resources keyed by group name (`disk`, `net`, `mount`).
    pub instances: BTreeMap<String, Vec<Instance>>,
    /// Offset from UTC. Lets nodes in different simulated regions peak at
    /// different wall-clock times instead of moving in lockstep, which is one
    /// of the cheaper tells that a fleet is synthetic.
    pub utc_offset_secs: i64,
}

impl NodeProfile {
    pub fn attr(&self, name: &str) -> Option<f64> {
        self.attrs.get(name).copied()
    }

    fn cores(&self) -> f64 {
        self.attr(ATTR_CORES).unwrap_or(1.0).max(1.0)
    }
}

/// A chart this node will emit, resolved from a context plus (optionally) one
/// instance.
///
/// Chart planning happens once, and both declaration and per-tick emission read
/// the same plan. If they were derived separately, a divergence would send
/// `SET` lines to a chart that was never declared — a failure that shows up as
/// silently missing data rather than an error.
#[derive(Debug, Clone)]
pub struct PlannedChart {
    pub chart_id: String,
    pub context_index: usize,
    pub context_id: String,
    pub family: String,
    /// Instance name, or empty for a node-level chart.
    pub scope: String,
    pub weight: f64,
    /// Chart labels emitted with the chart definition. Stock health templates
    /// filter on these, so a missing label means the alert never attaches.
    pub labels: BTreeMap<String, String>,
    instance_attrs: BTreeMap<String, f64>,
}

impl PlannedChart {
    /// Instance-scoped attribute, if this chart has one. Used by the fidelity
    /// checks to know which total a per-instance partition should conserve.
    pub fn instance_attrs_get(&self, name: &str) -> Option<f64> {
        self.instance_attrs.get(name).copied()
    }
}

/// One chart's values for a single tick.
#[derive(Debug, Clone)]
pub struct Sample {
    pub chart_id: String,
    /// Dimension id and the integer value to emit, in declaration order.
    pub values: Vec<(String, i64)>,
}

/// Mutable per-signal state.
#[derive(Debug, Clone)]
struct SignalState {
    rng: Rng,
    /// Random-walk displacement, as a fraction of the current level.
    walk: f64,
}

/// Counts of samples that landed on a signal's configured bound.
///
/// A signal pinned to a bound is the artifact class that produced `free = 0` in
/// the throwaway probe: the value stopped being modelled and started being
/// clamped. Only rails are counted — see [`Signal::min_is_physical`].
#[derive(Debug, Clone, Default)]
pub struct LintStats {
    pub samples: u64,
    pub clamped_low: BTreeMap<String, u64>,
    pub clamped_high: BTreeMap<String, u64>,
}

impl LintStats {
    /// Signals whose clamp rate exceeds `threshold`, as
    /// `(signal, fraction_of_samples)`.
    pub fn pinned_signals(&self, threshold: f64) -> Vec<(String, f64)> {
        if self.samples == 0 {
            return Vec::new();
        }
        let total = self.samples as f64;
        let mut out = Vec::new();
        for (name, count) in &self.clamped_low {
            let rate = *count as f64 / total;
            if rate > threshold {
                out.push((format!("{name} (low)"), rate));
            }
        }
        for (name, count) in &self.clamped_high {
            let rate = *count as f64 / total;
            if rate > threshold {
                out.push((format!("{name} (high)"), rate));
            }
        }
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }
}

/// Executes one spec for one node.
pub struct NodeEngine {
    /// This node's composed spec: the base plus whatever services it runs.
    /// Shared rather than copied, since nodes of the same role compose the same
    /// set.
    spec: Arc<GeneratorSpec>,
    profile: NodeProfile,
    /// Signals with the node's role overrides already applied.
    signals: BTreeMap<String, Signal>,
    plan: Vec<PlannedChart>,
    /// Signal state, keyed `"<scope>\0<signal>"`.
    state: BTreeMap<String, SignalState>,
    /// Values resolved this tick, same key shape as `state`.
    resolved: BTreeMap<String, f64>,
    /// Monotonic accumulators, keyed `"<chart_id>/<dimension>"`.
    counters: BTreeMap<String, f64>,
    lint: LintStats,
    master_seed: u64,
}

impl NodeEngine {
    /// Build an engine for `profile`, seeding every signal stream from
    /// `master_seed` and the node's GUID.
    pub fn new(spec: Arc<GeneratorSpec>, profile: NodeProfile, master_seed: u64) -> Self {
        let signals = spec.signals_for_role(profile.role.as_deref());
        let plan = plan_charts(&spec, &profile);

        Self {
            spec,
            profile,
            signals,
            plan,
            state: BTreeMap::new(),
            resolved: BTreeMap::new(),
            counters: BTreeMap::new(),
            lint: LintStats::default(),
            master_seed,
        }
    }

    pub fn profile(&self) -> &NodeProfile {
        &self.profile
    }

    pub fn lint(&self) -> &LintStats {
        &self.lint
    }

    /// The charts this node emits. Declaration and emission share this plan.
    pub fn charts(&self) -> &[PlannedChart] {
        &self.plan
    }

    /// Advance the node by one interval and produce a sample per planned chart.
    ///
    /// `now` is unix seconds; `interval` is the collection period in seconds.
    /// The composed spec this node runs.
    pub fn spec(&self) -> &GeneratorSpec {
        &self.spec
    }

    pub fn tick(&mut self, scenarios: &ScenarioSet, now: i64, interval: f64) -> Vec<Sample> {
        // Values are memoised per tick, so contexts sharing a signal within the
        // same scope see the same value. That shared value is what makes a
        // node's charts correlate; resolving per context would destroy it.
        self.resolved.clear();
        self.lint.samples += 1;

        // Taken and restored so the plan can be iterated while signal state is
        // mutated. Cheaper than cloning the plan every tick.
        let plan = std::mem::take(&mut self.plan);
        let spec = Arc::clone(&self.spec);
        let samples = plan
            .iter()
            .map(|chart| {
                let ctx = &spec.contexts[chart.context_index];
                let values = self.sample_chart(chart, &ctx.shape, scenarios, now, interval);
                Sample {
                    chart_id: chart.chart_id.clone(),
                    values,
                }
            })
            .collect();
        self.plan = plan;
        samples
    }

    /// Resolve a signal within a scope, memoised for this tick.
    fn value(
        &mut self,
        chart: &PlannedChart,
        name: &str,
        scenarios: &ScenarioSet,
        now: i64,
    ) -> f64 {
        let key = format!("{}{KEY_SEP}{}", chart.scope, name);
        if let Some(v) = self.resolved.get(&key) {
            return *v;
        }
        let v = self.eval_signal(&key, name, &chart.scope, chart.weight, scenarios, now);
        self.resolved.insert(key, v);
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_signal(
        &mut self,
        key: &str,
        name: &str,
        scope: &str,
        weight: f64,
        scenarios: &ScenarioSet,
        now: i64,
    ) -> f64 {
        let Some(signal) = self.signals.get(name) else {
            return 0.0;
        };
        let seasonal = seasonal_factor(signal, now, self.profile.utc_offset_secs);
        // Scenarios perturb the signal level, so a fault propagates into every
        // context that signal feeds. That coupling is what produces a coherent
        // blast radius instead of one conspicuously anomalous chart.
        let perturb = scenarios.perturbation(
            &self.profile.hostname,
            self.profile.role.as_deref(),
            scope,
            name,
            now,
        );
        // Weight scales the whole signal, so a lightly-loaded disk is quieter
        // in every context that references it, not just one. The additive term
        // is applied after weighting, in the signal's own units, because it
        // represents an absolute event rate rather than a scaling of load.
        let scenario = perturb.multiplier;
        let mut value = signal.base * seasonal * weight * scenario + perturb.additive;

        // Noise is proportional to the *current* level, not to base. Scaling
        // off base makes a quiet 3 a.m. trough carry the same absolute jitter
        // as a busy afternoon, which drives troughs into the floor and reads as
        // obviously wrong when zoomed in.
        let level = value.abs();

        // Streams are addressed by name, so adding an instance or reordering
        // contexts leaves every other stream's sequence untouched.
        let state = self.state.entry(key.to_string()).or_insert_with(|| {
            let stream = format!("{}/{}/{}", self.profile.guid, scope, name);
            SignalState {
                rng: Rng::from_stream(self.master_seed, &stream),
                walk: 0.0,
            }
        });

        match signal.noise.kind {
            NoiseKind::None => {}
            NoiseKind::Gauss => {
                value += level * signal.noise.sigma * state.rng.next_normal();
            }
            NoiseKind::Walk => {
                // Ornstein-Uhlenbeck style: pull toward zero, then kick. This
                // yields the autocorrelation real metrics have; independent
                // per-sample jitter does not, and an SRE reads the difference
                // immediately when zoomed in.
                let kick = signal.noise.sigma * state.rng.next_normal();
                state.walk += -signal.noise.reversion * state.walk + kick;
                value += level * state.walk;
            }
        }

        // Bounds scale with weight, otherwise a 0.1-weight instance would be
        // clamped by bounds written for a full-weight one. They scale with the
        // scenario multiplier too: a fault is *meant* to push a signal past its
        // normal operating range, and clamping it back would silently defeat
        // the scenario while the lint reported a pinned signal.
        //
        // A declared physical ceiling is never widened. Reality does not grant
        // headroom because a scenario is running: disk utilisation above 100%
        // is impossible, and shipping it is precisely the "is this fake?"
        // artifact the fidelity work exists to prevent. This was caught live -
        // a scenario pushed 10min_disk_utilization to 101.5%.
        let max = if signal.max_is_physical() {
            signal.max * weight
        } else {
            // Headroom covers both channels, so an additive fault is not
            // clamped away by a bound written for the healthy baseline.
            signal.max * weight * scenario.max(1.0) + perturb.additive.max(0.0)
        };
        let min = signal.min * weight;

        // Clamping is recorded only where the bound is a safety rail. Sitting
        // at a physical floor (zero errors on a quiet link) is realistic; being
        // held against an arbitrary rail is the artifact worth reporting.
        let lint_name = if scope.is_empty() {
            name.to_string()
        } else {
            format!("{name}@{scope}")
        };
        if value < min {
            if !signal.min_is_physical() {
                *self.lint.clamped_low.entry(lint_name).or_default() += 1;
            }
            min
        } else if value > max {
            if !signal.max_is_physical() {
                *self.lint.clamped_high.entry(lint_name).or_default() += 1;
            }
            max
        } else {
            value
        }
    }

    /// Attribute lookup: instance attributes shadow the node's, so one
    /// `disk.space` context serves mounts of different sizes.
    fn resolve_attr(&self, chart: &PlannedChart, name: &str) -> f64 {
        chart
            .instance_attrs
            .get(name)
            .copied()
            .or_else(|| self.profile.attr(name))
            .unwrap_or(0.0)
    }

    fn sample_chart(
        &mut self,
        chart: &PlannedChart,
        shape: &Shape,
        scenarios: &ScenarioSet,
        now: i64,
        interval: f64,
    ) -> Vec<(String, i64)> {
        match shape {
            Shape::Independent { dimensions } => dimensions
                .iter()
                .map(|d| {
                    let v = self.value(chart, &d.signal, scenarios, now);
                    (d.id.clone(), v.round() as i64)
                })
                .collect(),

            Shape::Counters { dimensions } => dimensions
                .iter()
                .map(|d| {
                    let rate = self.value(chart, &d.rate_signal, scenarios, now).max(0.0);
                    let key = format!("{}/{}", chart.chart_id, d.id);
                    let acc = self.counters.entry(key).or_insert(0.0);
                    // max(0.0) above keeps this addition non-negative, so the
                    // counter cannot go backwards even if a signal is
                    // misconfigured.
                    *acc += rate * interval;
                    (d.id.clone(), *acc as i64)
                })
                .collect(),

            Shape::Partition {
                total,
                driver,
                accumulate,
                dimensions,
            } => {
                let total = match total {
                    Total::Constant { value } => *value,
                    Total::NodeAttr { name } => self.resolve_attr(chart, name),
                };
                let driver = self.value(chart, driver, scenarios, now).clamp(0.0, total);

                // Non-remainder dimensions take their share of the driver; the
                // remainder absorbs everything left of the total. Conservation
                // is therefore exact by construction.
                let mut allocated = 0.0;
                let mut parts: Vec<(String, f64)> = Vec::with_capacity(dimensions.len());
                for d in dimensions {
                    if d.remainder {
                        parts.push((d.id.clone(), f64::NAN));
                    } else {
                        let v = driver * d.share;
                        allocated += v;
                        parts.push((d.id.clone(), v));
                    }
                }
                let remainder = (total - allocated).max(0.0);
                for part in parts.iter_mut() {
                    if part.1.is_nan() {
                        part.1 = remainder;
                    }
                }

                match accumulate {
                    Accumulate::None => parts
                        .into_iter()
                        .map(|(id, v)| (id, v.round() as i64))
                        .collect(),
                    Accumulate::Jiffies => {
                        // Convert each share of the total into per-core CPU
                        // time, matching how /proc/stat reports jiffies.
                        let per_interval = USER_HZ * self.profile.cores() * interval;
                        parts
                            .into_iter()
                            .map(|(id, v)| {
                                let key = format!("{}/{}", chart.chart_id, id);
                                let acc = self.counters.entry(key).or_insert(0.0);
                                *acc += per_interval * (v / total);
                                (id, *acc as i64)
                            })
                            .collect()
                    }
                }
            }
        }
    }
}

/// Expand a spec's contexts into this node's concrete charts.
///
/// A context with instancing produces nothing when the node has no matching
/// instance group — which is correct: a host without a second disk should not
/// have a chart for one.
fn plan_charts(spec: &GeneratorSpec, profile: &NodeProfile) -> Vec<PlannedChart> {
    let mut plan = Vec::new();
    for (index, ctx) in spec.contexts.iter().enumerate() {
        match &ctx.instances {
            None => plan.push(PlannedChart {
                chart_id: ctx.id.clone(),
                context_index: index,
                context_id: ctx.id.clone(),
                family: ctx.family.clone(),
                scope: String::new(),
                weight: 1.0,
                labels: ctx.labels.clone(),
                instance_attrs: BTreeMap::new(),
            }),
            Some(inst) => {
                let Some(instances) = profile.instances.get(&inst.group) else {
                    continue;
                };
                for i in instances {
                    plan.push(PlannedChart {
                        chart_id: inst.chart_id(&i.name),
                        context_index: index,
                        context_id: ctx.id.clone(),
                        family: inst.family_for(&i.name),
                        scope: i.name.clone(),
                        weight: i.weight,
                        labels: inst.labels_for(&i.name),
                        instance_attrs: i.attrs.clone(),
                    });
                }
            }
        }
    }
    plan
}

/// Multiplicative daily/weekly shape at `now`.
fn seasonal_factor(signal: &Signal, now: i64, utc_offset_secs: i64) -> f64 {
    let s = &signal.seasonality;
    if s.daily_amplitude == 0.0 && s.weekend_factor == 1.0 {
        return 1.0;
    }

    let local = now + utc_offset_secs;
    let secs_of_day = local.rem_euclid(DAY);
    let hour = secs_of_day as f64 / 3600.0;

    // Cosine peaking at peak_hour.
    let phase = std::f64::consts::TAU * (hour - s.peak_hour) / 24.0;
    let mut factor = 1.0 + s.daily_amplitude * phase.cos();

    // Unix epoch day 0 was a Thursday; with Sunday as 0 that is index 4.
    let days = local.div_euclid(DAY);
    let dow = (days + 4).rem_euclid(7);
    if dow == 0 || dow == 6 {
        factor *= s.weekend_factor;
    }

    factor.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
version: 1
name: test
signals:
  cpu_busy:
    base: 30.0
    min: 1.0
    max: 95.0
    seasonality: { daily_amplitude: 0.3, peak_hour: 14.0 }
    noise: { kind: walk, sigma: 0.03 }
  mem_used_kb:
    base: 4000000.0
    min: 100000.0
    max: 7000000.0
    noise: { kind: gauss, sigma: 0.01 }
  disk_read_rate:
    base: 400.0
    min: 0.0
    max: 100000.0
    noise: { kind: gauss, sigma: 0.2 }
  disk_used_kb:
    base: 100000000.0
    min: 1000.0
    max: 400000000.0
    noise: { kind: walk, sigma: 0.002 }
contexts:
  - id: system.cpu
    title: CPU
    units: percentage
    family: cpu
    chart_type: stacked
    priority: 100
    shape: partition
    total: { from: constant, value: 100.0 }
    driver: cpu_busy
    accumulate: jiffies
    dimensions:
      - { id: user,   share: 0.60, algorithm: percentage-of-incremental-row }
      - { id: system, share: 0.25, algorithm: percentage-of-incremental-row }
      - { id: idle,   remainder: true, algorithm: percentage-of-incremental-row }
  - id: system.ram
    title: RAM
    units: MiB
    family: ram
    chart_type: stacked
    priority: 200
    shape: partition
    total: { from: node_attr, name: ram_total_kb }
    driver: mem_used_kb
    dimensions:
      - { id: used, share: 1.0, divisor: 1024 }
      - { id: free, remainder: true, divisor: 1024 }
  - id: disk.io
    title: Disk IO
    units: KiB/s
    family: io
    chart_type: area
    priority: 150
    instances: { group: disk, chart_prefix: disk, family: io }
    shape: counters
    dimensions:
      - { id: reads, rate_signal: disk_read_rate }
  - id: disk.space
    title: Disk Space
    units: GiB
    family: utilization
    chart_type: stacked
    priority: 160
    instances: { group: mount, chart_prefix: disk_space, family: "{instance}" }
    shape: partition
    total: { from: node_attr, name: disk_total_kb }
    driver: disk_used_kb
    dimensions:
      - { id: used,  share: 1.0, divisor: 1048576 }
      - { id: avail, remainder: true, divisor: 1048576 }
"#;

    fn profile(guid: &str) -> NodeProfile {
        NodeProfile {
            guid: guid.to_string(),
            hostname: "sim-test-01".into(),
            role: None,
            attrs: BTreeMap::from([
                (ATTR_CORES.to_string(), 8.0),
                ("ram_total_kb".to_string(), 16_777_216.0),
                ("disk_total_kb".to_string(), 524_288_000.0),
            ]),
            labels: BTreeMap::new(),
            instances: BTreeMap::from([
                (
                    "disk".to_string(),
                    vec![
                        Instance {
                            name: "nvme0n1".into(),
                            weight: 1.0,
                            attrs: BTreeMap::new(),
                        },
                        Instance {
                            name: "sdb".into(),
                            weight: 0.2,
                            attrs: BTreeMap::new(),
                        },
                    ],
                ),
                (
                    "mount".to_string(),
                    vec![
                        Instance {
                            name: "/".into(),
                            weight: 1.0,
                            attrs: BTreeMap::new(),
                        },
                        Instance {
                            name: "/boot".into(),
                            weight: 0.01,
                            // A small mount needs its own total, otherwise it
                            // would be sized like the root filesystem.
                            attrs: BTreeMap::from([("disk_total_kb".to_string(), 2_097_152.0)]),
                        },
                    ],
                ),
            ]),
            utc_offset_secs: 0,
        }
    }

    fn engine(seed: u64, guid: &str) -> NodeEngine {
        let spec = Arc::new(GeneratorSpec::from_yaml(SPEC).expect("spec parses"));
        NodeEngine::new(spec, profile(guid), seed)
    }

    fn run(seed: u64, guid: &str, ticks: i64) -> Vec<Vec<Sample>> {
        let mut e = engine(seed, guid);
        (0..ticks)
            .map(|i| e.tick(&ScenarioSet::default(), 1_700_000_000 + i, 1.0))
            .collect()
    }

    fn chart<'a>(t: &'a [Sample], id: &str) -> &'a Sample {
        t.iter()
            .find(|s| s.chart_id == id)
            .unwrap_or_else(|| panic!("no chart {id}"))
    }

    #[test]
    fn instanced_contexts_expand_to_one_chart_per_instance() {
        let e = engine(1, "guid-a");
        let ids: Vec<&str> = e.charts().iter().map(|c| c.chart_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "system.cpu",
                "system.ram",
                "disk.nvme0n1",
                "disk.sdb",
                "disk_space./",
                "disk_space./boot",
            ]
        );
    }

    #[test]
    fn instance_family_templating_matches_netdata_convention() {
        let e = engine(1, "guid-a");
        let fam = |id: &str| {
            e.charts()
                .iter()
                .find(|c| c.chart_id == id)
                .map(|c| c.family.clone())
                .unwrap()
        };
        // Disk IO groups by aspect upstream; disk space groups by mount.
        assert_eq!(fam("disk.nvme0n1"), "io");
        assert_eq!(fam("disk.sdb"), "io");
        assert_eq!(fam("disk_space./"), "/");
        assert_eq!(fam("disk_space./boot"), "/boot");
    }

    #[test]
    fn a_node_without_an_instance_group_emits_no_charts_for_it() {
        let spec = Arc::new(GeneratorSpec::from_yaml(SPEC).unwrap());
        let mut p = profile("guid-a");
        p.instances.remove("disk");
        let e = NodeEngine::new(spec, p, 1);
        assert!(!e.charts().iter().any(|c| c.context_id == "disk.io"));
        // Other contexts are unaffected.
        assert!(e.charts().iter().any(|c| c.chart_id == "system.cpu"));
    }

    #[test]
    fn instances_of_one_context_carry_different_values() {
        // Two disks that move identically are an obvious tell.
        let ticks = run(3, "guid-a", 300);
        let a = chart(&ticks[299], "disk.nvme0n1").values[0].1;
        let b = chart(&ticks[299], "disk.sdb").values[0].1;
        assert_ne!(a, b);
        // The weighted instance should be markedly quieter.
        assert!(b < a / 2, "weight not applied: nvme={a} sdb={b}");
    }

    #[test]
    fn instance_attrs_shadow_node_attrs() {
        let ticks = run(5, "guid-a", 50);
        let root: i64 = chart(&ticks[49], "disk_space./")
            .values
            .iter()
            .map(|(_, v)| v)
            .sum();
        let boot: i64 = chart(&ticks[49], "disk_space./boot")
            .values
            .iter()
            .map(|(_, v)| v)
            .sum();
        // Emitted values are raw KiB; the GiB divisor is a chart-dimension
        // attribute the agent applies on read, not something we pre-scale.
        // 500 GiB root vs a 2 GiB /boot, each conserving its own total.
        assert_eq!(root, 524_288_000);
        assert_eq!(boot, 2_097_152);
    }

    #[test]
    fn identical_seed_and_config_reproduce_identical_output() {
        let a = run(0xC0FFEE, "guid-a", 500);
        let b = run(0xC0FFEE, "guid-a", 500);
        for (ta, tb) in a.iter().zip(b.iter()) {
            for (sa, sb) in ta.iter().zip(tb.iter()) {
                assert_eq!(sa.chart_id, sb.chart_id);
                assert_eq!(sa.values, sb.values);
            }
        }
    }

    #[test]
    fn different_seeds_produce_different_worlds() {
        let a = run(1, "guid-a", 200);
        let b = run(2, "guid-a", 200);
        assert_ne!(
            chart(&a[199], "system.cpu").values,
            chart(&b[199], "system.cpu").values
        );
    }

    #[test]
    fn different_nodes_diverge_under_the_same_seed() {
        let a = run(5, "guid-a", 200);
        let b = run(5, "guid-b", 200);
        assert_ne!(
            chart(&a[199], "system.cpu").values,
            chart(&b[199], "system.cpu").values
        );
    }

    #[test]
    fn ram_partition_conserves_its_total_every_sample() {
        // The regression this format exists to prevent: the probe emitted
        // free = 0 because it clamped the leftover instead of deriving it.
        let ticks = run(11, "guid-a", 2_000);
        for t in &ticks {
            let ram = chart(t, "system.ram");
            let sum: i64 = ram.values.iter().map(|(_, v)| *v).sum();
            assert!((sum - 16_777_216).abs() <= 1, "RAM not conserved: {sum}");
            let free = ram.values.iter().find(|(id, _)| id == "free").unwrap().1;
            assert!(free > 0, "free memory hit zero - clamping artifact");
        }
    }

    #[test]
    fn counters_never_decrease_on_any_instance() {
        let ticks = run(13, "guid-a", 3_000);
        for id in ["disk.nvme0n1", "disk.sdb"] {
            let mut last = i64::MIN;
            for t in &ticks {
                let v = chart(t, id).values[0].1;
                assert!(v >= last, "{id} counter went backwards: {v} < {last}");
                last = v;
            }
        }
    }

    #[test]
    fn cpu_jiffies_accumulate_monotonically_across_all_states() {
        let ticks = run(17, "guid-a", 1_000);
        let mut last = [i64::MIN; 3];
        for t in &ticks {
            for (i, (_, v)) in chart(t, "system.cpu").values.iter().enumerate() {
                assert!(*v >= last[i], "cpu jiffies went backwards");
                last[i] = *v;
            }
        }
    }

    #[test]
    fn a_shared_signal_resolves_once_per_scope_per_tick() {
        // system.cpu and any other context driven by cpu_busy must see the same
        // value within a tick, or charts stop correlating.
        let mut e = engine(19, "guid-a");
        e.tick(&ScenarioSet::default(), 1_700_000_000, 1.0);
        let node_scope: Vec<&String> = e
            .resolved
            .keys()
            .filter(|k| k.starts_with(KEY_SEP))
            .collect();
        // Node-scope keys are unique per signal, so no signal resolved twice.
        let mut seen = node_scope.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), node_scope.len());
    }

    #[test]
    fn seasonality_moves_the_daily_shape() {
        let mut e = engine(23, "guid-a");
        let c = e.charts()[0].clone();
        let midnight = 1_700_000_000 - (1_700_000_000 % DAY);
        let sc = ScenarioSet::default();
        let at_peak = e.value(&c, "cpu_busy", &sc, midnight + 14 * 3600);
        e.resolved.clear();
        let at_trough = e.value(&c, "cpu_busy", &sc, midnight + 2 * 3600);
        assert!(
            at_peak > at_trough,
            "no diurnal shape: peak {at_peak} <= trough {at_trough}"
        );
    }

    #[test]
    fn healthy_signals_do_not_sit_pinned_to_their_bounds() {
        let mut e = engine(29, "guid-a");
        for i in 0..5_000 {
            e.tick(&ScenarioSet::default(), 1_700_000_000 + i, 1.0);
        }
        let pinned = e.lint().pinned_signals(0.01);
        assert!(pinned.is_empty(), "signals pinned to bounds: {pinned:?}");
    }
}
