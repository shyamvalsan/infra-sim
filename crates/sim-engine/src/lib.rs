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

use sim_spec::{Accumulate, Context, GeneratorSpec, NoiseKind, Shape, Signal, Total};

pub mod rng;

use rng::Rng;

/// Seconds in a day.
const DAY: i64 = 86_400;
/// Kernel USER_HZ: CPU time is reported in hundredths of a second per core.
const USER_HZ: f64 = 100.0;

/// Node attribute name for core count, referenced by [`Accumulate::Jiffies`].
pub const ATTR_CORES: &str = "cores";

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

/// One context's values for a single tick.
#[derive(Debug, Clone)]
pub struct Sample {
    pub context_id: String,
    /// Dimension id and the integer value to emit, in declaration order.
    pub values: Vec<(String, i64)>,
}

/// Mutable per-signal state.
#[derive(Debug, Clone)]
struct SignalState {
    rng: Rng,
    /// Random-walk displacement, as a fraction of base.
    walk: f64,
}

/// Counts of samples that landed exactly on a signal's configured bound.
///
/// A signal pinned to a bound is the artifact class that produced `free = 0` in
/// the throwaway probe: the value stopped being modelled and started being
/// clamped. Bounds are a safety rail, so any sustained contact is a spec bug.
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
    profile: NodeProfile,
    /// Signals with the node's role overrides already applied.
    signals: BTreeMap<String, Signal>,
    state: BTreeMap<String, SignalState>,
    /// Monotonic accumulators, keyed `"<context>/<dimension>"`.
    counters: BTreeMap<String, f64>,
    lint: LintStats,
}

impl NodeEngine {
    /// Build an engine for `profile`, seeding every signal stream from
    /// `master_seed` and the node's GUID.
    pub fn new(spec: &GeneratorSpec, profile: NodeProfile, master_seed: u64) -> Self {
        let signals = spec.signals_for_role(profile.role.as_deref());
        let state = signals
            .keys()
            .map(|name| {
                let stream = format!("{}/{}", profile.guid, name);
                (
                    name.clone(),
                    SignalState {
                        rng: Rng::from_stream(master_seed, &stream),
                        walk: 0.0,
                    },
                )
            })
            .collect();

        Self {
            profile,
            signals,
            state,
            counters: BTreeMap::new(),
            lint: LintStats::default(),
        }
    }

    pub fn profile(&self) -> &NodeProfile {
        &self.profile
    }

    pub fn lint(&self) -> &LintStats {
        &self.lint
    }

    /// Advance the node by one interval and produce a sample per context.
    ///
    /// `now` is unix seconds; `interval` is the collection period in seconds.
    pub fn tick(&mut self, spec: &GeneratorSpec, now: i64, interval: f64) -> Vec<Sample> {
        // Evaluate every signal once per tick so that contexts sharing a signal
        // see the same value. Correlation across contexts is the whole point of
        // named signals; re-evaluating per context would destroy it.
        let mut resolved: BTreeMap<String, f64> = BTreeMap::new();
        let names: Vec<String> = self.signals.keys().cloned().collect();
        for name in names {
            let value = self.eval_signal(&name, now);
            resolved.insert(name, value);
        }
        self.lint.samples += 1;

        spec.contexts
            .iter()
            .map(|ctx| self.sample_context(ctx, &resolved, interval))
            .collect()
    }

    fn eval_signal(&mut self, name: &str, now: i64) -> f64 {
        let Some(signal) = self.signals.get(name) else {
            return 0.0;
        };
        let seasonal = seasonal_factor(signal, now, self.profile.utc_offset_secs);
        let mut value = signal.base * seasonal;

        // Noise is proportional to the *current* seasonal level, not to base.
        // Scaling off base makes a quiet 3 a.m. trough carry the same absolute
        // jitter as a busy afternoon, which drives troughs into the floor and
        // reads as obviously wrong when zoomed in.
        let level = value.abs();

        let Some(state) = self.state.get_mut(name) else {
            return value;
        };
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

        // Clamping is recorded only where the bound is a safety rail. Sitting
        // at a physical floor (zero errors on a quiet link) is realistic; being
        // held against an arbitrary rail is the artifact worth reporting.
        if value < signal.min {
            if !signal.min_is_physical() {
                *self.lint.clamped_low.entry(name.to_string()).or_default() += 1;
            }
            signal.min
        } else if value > signal.max {
            if !signal.max_is_physical() {
                *self.lint.clamped_high.entry(name.to_string()).or_default() += 1;
            }
            signal.max
        } else {
            value
        }
    }

    fn sample_context(
        &mut self,
        ctx: &Context,
        resolved: &BTreeMap<String, f64>,
        interval: f64,
    ) -> Sample {
        let values = match &ctx.shape {
            Shape::Independent { dimensions } => dimensions
                .iter()
                .map(|d| {
                    let v = resolved.get(&d.signal).copied().unwrap_or(0.0);
                    (d.id.clone(), v.round() as i64)
                })
                .collect(),

            Shape::Counters { dimensions } => dimensions
                .iter()
                .map(|d| {
                    let rate = resolved
                        .get(&d.rate_signal)
                        .copied()
                        .unwrap_or(0.0)
                        .max(0.0);
                    let key = format!("{}/{}", ctx.id, d.id);
                    let acc = self.counters.entry(key).or_insert(0.0);
                    // max(0.0) above keeps this addition non-negative, so the
                    // counter cannot go backwards even if a signal is misconfigured.
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
                    Total::NodeAttr { name } => self.profile.attr(name).unwrap_or(0.0),
                };
                let driver = resolved
                    .get(driver)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, total);

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
                                let key = format!("{}/{}", ctx.id, id);
                                let acc = self.counters.entry(key).or_insert(0.0);
                                *acc += per_interval * (v / total);
                                (id, *acc as i64)
                            })
                            .collect()
                    }
                }
            }
        };

        Sample {
            context_id: ctx.id.clone(),
            values,
        }
    }
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
      - { id: iowait, share: 0.10, algorithm: percentage-of-incremental-row }
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
      - { id: used,    share: 1.0, divisor: 1024 }
      - { id: free,    remainder: true, divisor: 1024 }
  - id: system.io
    title: Disk IO
    units: KiB/s
    family: disk
    chart_type: area
    priority: 150
    shape: counters
    dimensions:
      - { id: in, rate_signal: disk_read_rate }
"#;

    fn profile(guid: &str) -> NodeProfile {
        NodeProfile {
            guid: guid.to_string(),
            hostname: "sim-test-01".into(),
            role: None,
            attrs: BTreeMap::from([
                (ATTR_CORES.to_string(), 8.0),
                ("ram_total_kb".to_string(), 16_777_216.0),
            ]),
            labels: BTreeMap::new(),
            utc_offset_secs: 0,
        }
    }

    fn run(seed: u64, guid: &str, ticks: i64) -> Vec<Vec<Sample>> {
        let spec = GeneratorSpec::from_yaml(SPEC).expect("spec parses");
        let mut engine = NodeEngine::new(&spec, profile(guid), seed);
        (0..ticks)
            .map(|i| engine.tick(&spec, 1_700_000_000 + i, 1.0))
            .collect()
    }

    #[test]
    fn identical_seed_and_config_reproduce_identical_output() {
        let a = run(0xC0FFEE, "guid-a", 500);
        let b = run(0xC0FFEE, "guid-a", 500);
        for (ta, tb) in a.iter().zip(b.iter()) {
            for (sa, sb) in ta.iter().zip(tb.iter()) {
                assert_eq!(sa.context_id, sb.context_id);
                assert_eq!(sa.values, sb.values);
            }
        }
    }

    #[test]
    fn different_seeds_produce_different_worlds() {
        let a = run(1, "guid-a", 200);
        let b = run(2, "guid-a", 200);
        assert_ne!(a[199][0].values, b[199][0].values);
    }

    #[test]
    fn different_nodes_diverge_under_the_same_seed() {
        // Two nodes in one environment must not be carbon copies of each other.
        let a = run(5, "guid-a", 200);
        let b = run(5, "guid-b", 200);
        assert_ne!(a[199][0].values, b[199][0].values);
    }

    #[test]
    fn ram_partition_conserves_its_total_every_sample() {
        // The regression this format exists to prevent: the probe emitted
        // free = 0 because it clamped the leftover instead of deriving it.
        let ticks = run(11, "guid-a", 2_000);
        let total_kb = 16_777_216_i64;
        for t in &ticks {
            let ram = t.iter().find(|s| s.context_id == "system.ram").unwrap();
            let sum: i64 = ram.values.iter().map(|(_, v)| *v).sum();
            assert!(
                (sum - total_kb).abs() <= 1,
                "RAM not conserved: {sum} vs {total_kb}"
            );
            let free = ram.values.iter().find(|(id, _)| id == "free").unwrap().1;
            assert!(free > 0, "free memory hit zero - clamping artifact");
        }
    }

    #[test]
    fn counters_never_decrease() {
        let ticks = run(13, "guid-a", 3_000);
        let mut last = i64::MIN;
        for t in &ticks {
            let io = t.iter().find(|s| s.context_id == "system.io").unwrap();
            let v = io.values[0].1;
            assert!(v >= last, "counter went backwards: {v} < {last}");
            last = v;
        }
    }

    #[test]
    fn cpu_jiffies_accumulate_monotonically_across_all_states() {
        let ticks = run(17, "guid-a", 1_000);
        let mut last = [i64::MIN; 4];
        for t in &ticks {
            let cpu = t.iter().find(|s| s.context_id == "system.cpu").unwrap();
            for (i, (_, v)) in cpu.values.iter().enumerate() {
                assert!(*v >= last[i], "cpu jiffies went backwards");
                last[i] = *v;
            }
        }
    }

    #[test]
    fn seasonality_moves_the_daily_shape() {
        let spec = GeneratorSpec::from_yaml(SPEC).unwrap();
        let mut engine = NodeEngine::new(&spec, profile("guid-a"), 23);
        // Sample the driver at the configured peak and twelve hours away.
        let midnight = 1_700_000_000 - (1_700_000_000 % DAY);
        let at_peak = engine.eval_signal("cpu_busy", midnight + 14 * 3600);
        let at_trough = engine.eval_signal("cpu_busy", midnight + 2 * 3600);
        assert!(
            at_peak > at_trough,
            "no diurnal shape: peak {at_peak} <= trough {at_trough}"
        );
    }

    #[test]
    fn healthy_signals_do_not_sit_pinned_to_their_bounds() {
        let spec = GeneratorSpec::from_yaml(SPEC).unwrap();
        let mut engine = NodeEngine::new(&spec, profile("guid-a"), 29);
        for i in 0..5_000 {
            engine.tick(&spec, 1_700_000_000 + i, 1.0);
        }
        let pinned = engine.lint().pinned_signals(0.01);
        assert!(pinned.is_empty(), "signals pinned to bounds: {pinned:?}");
    }
}
