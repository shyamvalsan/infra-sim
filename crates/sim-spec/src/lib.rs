//! Declarative generator spec format for Infra-Sim.
//!
//! A generator spec describes, per Netdata context, how to synthesise plausible
//! values: base levels, seasonality, noise, and — critically — the cross-metric
//! invariants that must hold *by construction*.
//!
//! The invariant point is the reason this is a data format and not code. The
//! throwaway probe in `prototypes/vnode-probe/` produced `system.ram free = 0`
//! within four minutes because it computed each dimension independently and
//! clamped the leftover. A [`Shape::Partition`] cannot express that bug: the
//! remainder dimension is defined as `total - sum(others)`, so conservation
//! holds for every sample regardless of what the signals do.
//!
//! Authoring cost is the project's dominant risk (a dashboard-complete Linux
//! node is 60–80 contexts), so the format is built around reuse: [`Signal`]s
//! are named and shared across contexts, and [`Role`] overrides retune a whole
//! spec for a node role without duplicating any context definition.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

mod validate;

pub mod scenario;

pub use scenario::{Effect, Manifest, Scenario, Step as ScenarioStep, Target};
pub use validate::SpecError;

/// The only spec version this build understands.
pub const SPEC_VERSION: u32 = 1;

/// A generator spec: one collector's worth of synthetic contexts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratorSpec {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,

    /// Named signals, referenced by contexts. Sharing one `cpu_busy` signal
    /// across every CPU-derived context is what makes those contexts correlate
    /// the way a real host's do.
    #[serde(default)]
    pub signals: BTreeMap<String, Signal>,

    pub contexts: Vec<Context>,

    /// Per-role retuning. A role only overrides signal parameters; it can never
    /// add or remove contexts, so every node of this spec has an identical
    /// chart set and differs solely in behaviour.
    #[serde(default)]
    pub roles: BTreeMap<String, Role>,
}

impl GeneratorSpec {
    /// Parse and validate. Validation is not optional: an unvalidated spec can
    /// express a partition whose shares exceed its total, which is exactly the
    /// artifact class this format exists to prevent.
    pub fn from_yaml(input: &str) -> Result<Self, SpecError> {
        let spec: GeneratorSpec =
            serde_yaml::from_str(input).map_err(|e| SpecError::Parse(e.to_string()))?;
        spec.validate()?;
        Ok(spec)
    }

    /// Signal parameters for a role, with role overrides applied over the
    /// defaults. Unknown role names are rejected at validation time, so callers
    /// that validated first can rely on a known role resolving.
    pub fn signals_for_role(&self, role: Option<&str>) -> BTreeMap<String, Signal> {
        let mut out = self.signals.clone();
        let Some(overrides) = role.and_then(|r| self.roles.get(r)) else {
            return out;
        };
        for (name, patch) in &overrides.signals {
            if let Some(base) = out.get_mut(name) {
                base.apply(patch);
            }
        }
        out
    }
}

/// Per-role signal retuning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub signals: BTreeMap<String, SignalPatch>,
}

/// A named time-varying scalar.
///
/// Evaluation is `clamp(base * seasonality + noise, min, max)`. The clamp is a
/// safety rail for pathological noise, not a modelling tool — `min`/`max` should
/// sit outside the range the signal actually reaches. A signal that spends real
/// time pinned to a bound is the `free = 0` artifact wearing a different hat,
/// and the engine's lint reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub base: f64,
    pub min: f64,
    pub max: f64,
    #[serde(default)]
    pub seasonality: Seasonality,
    #[serde(default)]
    pub noise: Noise,

    /// Declare a non-zero `min` as a real physical floor rather than a safety
    /// rail. Needed for quantities like "processes running", where 1 is a fact
    /// about the system and not a clamp.
    #[serde(default)]
    pub min_is_floor: bool,

    /// Declare `max` as a real physical ceiling, e.g. the kernel entropy pool
    /// size. Without this every `max` is treated as a rail.
    #[serde(default)]
    pub max_is_ceiling: bool,
}

impl Signal {
    /// Whether reaching `min` is physically meaningful rather than an artifact.
    ///
    /// Zero is always legitimate for the non-negative quantities these specs
    /// model — a quiet interface really does report zero errors — so it needs
    /// no annotation. Any other floor is a rail until the spec says otherwise.
    pub fn min_is_physical(&self) -> bool {
        self.min == 0.0 || self.min_is_floor
    }

    /// Whether reaching `max` is physically meaningful. Always false unless
    /// declared: an upper bound is a rail by default, and a metric flattened
    /// against one is the artifact this distinction exists to surface.
    pub fn max_is_physical(&self) -> bool {
        self.max_is_ceiling
    }
}

impl Signal {
    pub(crate) fn apply(&mut self, patch: &SignalPatch) {
        if let Some(v) = patch.base {
            self.base = v;
        }
        if let Some(v) = patch.min {
            self.min = v;
        }
        if let Some(v) = patch.max {
            self.max = v;
        }
        if let Some(v) = patch.noise_sigma {
            self.noise.sigma = v;
        }
        if let Some(v) = patch.daily_amplitude {
            self.seasonality.daily_amplitude = v;
        }
        if let Some(v) = patch.peak_hour {
            self.seasonality.peak_hour = v;
        }
    }
}

/// Sparse override of a [`Signal`]. Every field is optional; absent fields keep
/// the spec-level value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalPatch {
    pub base: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub noise_sigma: Option<f64>,
    pub daily_amplitude: Option<f64>,
    pub peak_hour: Option<f64>,
}

/// Daily and weekly shape, as multiplicative factors on a signal's base.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seasonality {
    /// Peak-to-base fraction. `0.4` means the daily peak sits 40% above base.
    #[serde(default)]
    pub daily_amplitude: f64,
    /// Local hour of the daily peak, `0.0..24.0`.
    #[serde(default = "default_peak_hour")]
    pub peak_hour: f64,
    /// Multiplier applied on Saturday and Sunday. `1.0` disables it.
    #[serde(default = "default_weekend_factor")]
    pub weekend_factor: f64,
}

impl Default for Seasonality {
    fn default() -> Self {
        Self {
            daily_amplitude: 0.0,
            peak_hour: default_peak_hour(),
            weekend_factor: default_weekend_factor(),
        }
    }
}

fn default_peak_hour() -> f64 {
    14.0
}

fn default_weekend_factor() -> f64 {
    1.0
}

/// Noise model applied on top of the seasonal shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Noise {
    #[serde(default)]
    pub kind: NoiseKind,
    /// Gaussian standard deviation, or per-step size for a random walk, as a
    /// fraction of the signal's base.
    #[serde(default)]
    pub sigma: f64,
    /// Random-walk mean reversion, `0.0..1.0`. Higher reverts to base faster.
    #[serde(default = "default_reversion")]
    pub reversion: f64,
}

fn default_reversion() -> f64 {
    0.05
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseKind {
    #[default]
    None,
    /// Independent per-sample jitter. Correct for genuinely uncorrelated
    /// quantities; wrong for anything an SRE would expect to look continuous.
    Gauss,
    /// Mean-reverting random walk. Produces the autocorrelation real system
    /// metrics have, which distribution-matching alone will not give you.
    Walk,
}

/// One Netdata context and how its dimensions are produced.
///
/// Note: no `deny_unknown_fields` here. Serde cannot combine it with
/// `#[serde(flatten)]`, and flattening `shape` is what keeps the YAML readable
/// (`shape: partition` at the context level rather than a nested block). The
/// inner shape structs stay strict, so typos inside a dimension are still
/// caught; only a stray key at context level would pass silently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    /// Netdata context id, e.g. `system.cpu`. Also used as the chart id.
    pub id: String,
    pub title: String,
    pub units: String,
    pub family: String,
    #[serde(default)]
    pub chart_type: ChartType,
    pub priority: u32,
    /// Expand this context into one chart per node instance (per disk, per
    /// interface, per mount). Absent means a single chart whose id is the
    /// context id.
    #[serde(default)]
    pub instances: Option<Instancing>,
    #[serde(flatten)]
    pub shape: Shape,
}

/// Per-instance expansion of a context.
///
/// Real collectors emit one chart *instance* per device sharing a context —
/// `disk.nvme0n1` and `disk.sda` both carry context `disk.io`. A simulated node
/// with a single unnamed disk chart reads as wrong immediately, so contexts
/// that are per-device in reality must be per-device here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instancing {
    /// Which of the node's instance lists to iterate, e.g. `disk`, `net`,
    /// `mount`. A node without this group simply produces no charts for this
    /// context, exactly as a host without that hardware would.
    pub group: String,

    /// Chart-id prefix. Netdata's convention is `<prefix>.<instance>`, where
    /// the prefix varies per context: `disk.io` yields `disk.nvme0n1` while
    /// `disk.ops` yields `disk_ops.nvme0n1`.
    pub chart_prefix: String,

    /// Family for each instance chart. `{instance}` is substituted with the
    /// instance name. Upstream is not uniform here — disk charts group by
    /// aspect (`io`, `ops`) while network charts group by interface — so the
    /// spec states it rather than inferring it.
    #[serde(default = "default_instance_family")]
    pub family: String,
}

fn default_instance_family() -> String {
    "{instance}".to_string()
}

impl Instancing {
    /// Chart id for an instance, following Netdata's `<prefix>.<instance>`.
    pub fn chart_id(&self, instance: &str) -> String {
        format!("{}.{}", self.chart_prefix, instance)
    }

    pub fn family_for(&self, instance: &str) -> String {
        self.family.replace("{instance}", instance)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartType {
    #[default]
    Line,
    Area,
    Stacked,
}

impl ChartType {
    pub fn as_str(self) -> &'static str {
        match self {
            ChartType::Line => "line",
            ChartType::Area => "area",
            ChartType::Stacked => "stacked",
        }
    }
}

/// How a context's dimensions relate to each other.
///
/// This is the invariant-carrying part of the format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum Shape {
    /// Dimensions vary independently, each driven by its own signal. Use only
    /// when the dimensions genuinely have no arithmetic relationship.
    Independent { dimensions: Vec<GaugeDimension> },

    /// Dimensions partition a conserved total. Exactly one dimension carries
    /// `remainder: true` and absorbs `total - sum(others)`, so conservation is
    /// structural rather than checked after the fact.
    Partition {
        total: Total,
        /// Signal in `0.0..=total` giving the non-remainder mass. Shares are
        /// fractions *of this driver*, not of the total, which is what lets
        /// `system.cpu` move `user`/`system`/`iowait` together while `idle`
        /// takes up the slack.
        driver: String,
        #[serde(default)]
        accumulate: Accumulate,
        dimensions: Vec<ShareDimension>,
    },

    /// Monotonic counters. Each dimension accumulates `rate * interval`, so the
    /// emitted value never decreases and Netdata's `incremental` algorithm sees
    /// a well-formed counter.
    Counters { dimensions: Vec<CounterDimension> },
}

/// Where a partition's conserved total comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Total {
    /// A literal, e.g. `100.0` for a percentage breakdown.
    Constant { value: f64 },
    /// A node attribute resolved at runtime, e.g. total RAM. Keeps one spec
    /// correct across nodes with different hardware.
    NodeAttr { name: String },
}

/// Counter accumulation for a partition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accumulate {
    /// Emit the instantaneous share.
    #[default]
    None,
    /// Accumulate into per-core jiffies, matching how the kernel reports CPU
    /// time. Netdata then derives percentages via
    /// `percentage-of-incremental-row`.
    Jiffies,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GaugeDimension {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Named signal supplying this dimension's value.
    pub signal: String,
    #[serde(default = "default_multiplier")]
    pub multiplier: i64,
    #[serde(default = "default_divisor")]
    pub divisor: i64,
    #[serde(default)]
    pub algorithm: Algorithm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareDimension {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Fraction of the driver, `0.0..=1.0`. Ignored when `remainder` is set.
    #[serde(default)]
    pub share: f64,
    /// Absorbs whatever the other dimensions leave. Exactly one per partition.
    #[serde(default)]
    pub remainder: bool,
    #[serde(default = "default_multiplier")]
    pub multiplier: i64,
    #[serde(default = "default_divisor")]
    pub divisor: i64,
    #[serde(default)]
    pub algorithm: Algorithm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CounterDimension {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Named signal giving the *rate* in units per second. The engine
    /// integrates it; the spec never states a cumulative value.
    pub rate_signal: String,
    #[serde(default = "default_multiplier")]
    pub multiplier: i64,
    #[serde(default = "default_divisor")]
    pub divisor: i64,
    #[serde(default = "default_incremental")]
    pub algorithm: Algorithm,
}

fn default_multiplier() -> i64 {
    1
}

fn default_divisor() -> i64 {
    1
}

fn default_incremental() -> Algorithm {
    Algorithm::Incremental
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Algorithm {
    #[default]
    Absolute,
    Incremental,
    PercentageOfAbsoluteRow,
    PercentageOfIncrementalRow,
}

impl Algorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Absolute => "absolute",
            Algorithm::Incremental => "incremental",
            Algorithm::PercentageOfAbsoluteRow => "percentage-of-absolute-row",
            Algorithm::PercentageOfIncrementalRow => "percentage-of-incremental-row",
        }
    }
}

impl Shape {
    /// Dimension ids in emission order.
    pub fn dimension_ids(&self) -> Vec<&str> {
        match self {
            Shape::Independent { dimensions } => dimensions.iter().map(|d| d.id.as_str()).collect(),
            Shape::Partition { dimensions, .. } => {
                dimensions.iter().map(|d| d.id.as_str()).collect()
            }
            Shape::Counters { dimensions } => dimensions.iter().map(|d| d.id.as_str()).collect(),
        }
    }
}
