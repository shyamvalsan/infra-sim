//! Spec validation.
//!
//! Every check here exists to make a fidelity artifact unrepresentable rather
//! than merely unlikely. Failing loudly at load time is cheap; shipping a
//! partition whose shares sum past its total means a demo where an SRE sees
//! negative idle CPU.

use std::collections::BTreeSet;

use thiserror::Error;

use crate::{Context, GeneratorSpec, Shape, Signal, Total, SPEC_VERSION};

/// Floating-point slack for share sums. Shares are authored as decimal
/// fractions, so exact binary sums are not achievable.
const SHARE_EPSILON: f64 = 1e-9;

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("failed to parse spec: {0}")]
    Parse(String),

    #[error("unsupported spec version {found}, this build understands {SPEC_VERSION}")]
    Version { found: u32 },

    #[error("spec '{spec}' has no contexts")]
    NoContexts { spec: String },

    #[error("duplicate context id '{id}'")]
    DuplicateContext { id: String },

    #[error("context id '{id}' must be of the form '<type>.<id>', e.g. 'system.cpu'")]
    MalformedContextId { id: String },

    #[error("context '{context}' has no dimensions")]
    NoDimensions { context: String },

    #[error("context '{context}' has duplicate dimension id '{id}'")]
    DuplicateDimension { context: String, id: String },

    #[error("context '{context}' dimension '{dim}' has divisor 0")]
    ZeroDivisor { context: String, dim: String },

    #[error("context '{context}' references unknown signal '{signal}'")]
    UnknownSignal { context: String, signal: String },

    #[error("role '{role}' patches unknown signal '{signal}'")]
    UnknownRoleSignal { role: String, signal: String },

    #[error(
        "partition context '{context}' has {count} remainder dimensions, expected exactly 1 - \
         without one, conservation is enforced by clamping and the total drifts"
    )]
    RemainderCount { context: String, count: usize },

    #[error(
        "partition context '{context}' shares sum to {sum}, which exceeds 1.0 - \
         the remainder dimension would be negative"
    )]
    SharesExceedDriver { context: String, sum: f64 },

    #[error("partition context '{context}' dimension '{dim}' has negative share {share}")]
    NegativeShare {
        context: String,
        dim: String,
        share: f64,
    },

    #[error("signal '{signal}' has min {min} >= max {max}")]
    SignalRange { signal: String, min: f64, max: f64 },

    #[error(
        "signal '{signal}' base {base} lies outside [{min}, {max}] - \
         it would sit pinned to a bound, which is the 'free = 0' artifact class"
    )]
    SignalBaseOutOfRange {
        signal: String,
        base: f64,
        min: f64,
        max: f64,
    },

    #[error("signal '{signal}' has negative noise sigma {sigma}")]
    NegativeSigma { signal: String, sigma: f64 },

    #[error("partition context '{context}' total constant {value} must be > 0")]
    NonPositiveTotal { context: String, value: f64 },
}

impl GeneratorSpec {
    pub(crate) fn validate(&self) -> Result<(), SpecError> {
        if self.version != SPEC_VERSION {
            return Err(SpecError::Version {
                found: self.version,
            });
        }
        if self.contexts.is_empty() {
            return Err(SpecError::NoContexts {
                spec: self.name.clone(),
            });
        }

        for (name, signal) in &self.signals {
            validate_signal(name, signal)?;
        }

        let mut seen_contexts = BTreeSet::new();
        for context in &self.contexts {
            if !seen_contexts.insert(context.id.as_str()) {
                return Err(SpecError::DuplicateContext {
                    id: context.id.clone(),
                });
            }
            self.validate_context(context)?;
        }

        for (role_name, role) in &self.roles {
            for signal_name in role.signals.keys() {
                if !self.signals.contains_key(signal_name) {
                    return Err(SpecError::UnknownRoleSignal {
                        role: role_name.clone(),
                        signal: signal_name.clone(),
                    });
                }
            }
        }

        Ok(())
    }

    fn validate_context(&self, context: &Context) -> Result<(), SpecError> {
        // Netdata splits chart ids on the first '.' into type and id; a bare id
        // silently lands in a nameless family on the dashboard.
        let (chart_type, chart_id) =
            context
                .id
                .split_once('.')
                .ok_or_else(|| SpecError::MalformedContextId {
                    id: context.id.clone(),
                })?;
        if chart_type.is_empty() || chart_id.is_empty() {
            return Err(SpecError::MalformedContextId {
                id: context.id.clone(),
            });
        }

        let ids = context.shape.dimension_ids();
        if ids.is_empty() {
            return Err(SpecError::NoDimensions {
                context: context.id.clone(),
            });
        }
        let mut seen = BTreeSet::new();
        for id in &ids {
            if !seen.insert(*id) {
                return Err(SpecError::DuplicateDimension {
                    context: context.id.clone(),
                    id: (*id).to_string(),
                });
            }
        }

        match &context.shape {
            Shape::Independent { dimensions } => {
                for d in dimensions {
                    self.require_signal(&context.id, &d.signal)?;
                    require_divisor(&context.id, &d.id, d.divisor)?;
                }
            }

            Shape::Counters { dimensions } => {
                for d in dimensions {
                    self.require_signal(&context.id, &d.rate_signal)?;
                    require_divisor(&context.id, &d.id, d.divisor)?;
                }
            }

            Shape::Partition {
                total,
                driver,
                dimensions,
                ..
            } => {
                self.require_signal(&context.id, driver)?;

                if let Total::Constant { value } = total {
                    if *value <= 0.0 {
                        return Err(SpecError::NonPositiveTotal {
                            context: context.id.clone(),
                            value: *value,
                        });
                    }
                }

                let remainders = dimensions.iter().filter(|d| d.remainder).count();
                if remainders != 1 {
                    return Err(SpecError::RemainderCount {
                        context: context.id.clone(),
                        count: remainders,
                    });
                }

                let mut sum = 0.0;
                for d in dimensions {
                    require_divisor(&context.id, &d.id, d.divisor)?;
                    if d.remainder {
                        continue;
                    }
                    if d.share < 0.0 {
                        return Err(SpecError::NegativeShare {
                            context: context.id.clone(),
                            dim: d.id.clone(),
                            share: d.share,
                        });
                    }
                    sum += d.share;
                }
                if sum > 1.0 + SHARE_EPSILON {
                    return Err(SpecError::SharesExceedDriver {
                        context: context.id.clone(),
                        sum,
                    });
                }
            }
        }

        Ok(())
    }

    fn require_signal(&self, context: &str, signal: &str) -> Result<(), SpecError> {
        if self.signals.contains_key(signal) {
            Ok(())
        } else {
            Err(SpecError::UnknownSignal {
                context: context.to_string(),
                signal: signal.to_string(),
            })
        }
    }
}

fn require_divisor(context: &str, dim: &str, divisor: i64) -> Result<(), SpecError> {
    if divisor == 0 {
        Err(SpecError::ZeroDivisor {
            context: context.to_string(),
            dim: dim.to_string(),
        })
    } else {
        Ok(())
    }
}

fn validate_signal(name: &str, signal: &Signal) -> Result<(), SpecError> {
    if signal.min >= signal.max {
        return Err(SpecError::SignalRange {
            signal: name.to_string(),
            min: signal.min,
            max: signal.max,
        });
    }
    if signal.base < signal.min || signal.base > signal.max {
        return Err(SpecError::SignalBaseOutOfRange {
            signal: name.to_string(),
            base: signal.base,
            min: signal.min,
            max: signal.max,
        });
    }
    if signal.noise.sigma < 0.0 {
        return Err(SpecError::NegativeSigma {
            signal: name.to_string(),
            sigma: signal.noise.sigma,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::GeneratorSpec;

    const BASE: &str = r#"
version: 1
name: test
signals:
  busy:
    base: 30.0
    min: 1.0
    max: 95.0
contexts:
  - id: system.cpu
    title: CPU
    units: percentage
    family: cpu
    chart_type: stacked
    priority: 100
    shape: partition
    total: { from: constant, value: 100.0 }
    driver: busy
    dimensions:
      - { id: user, share: 0.7 }
      - { id: idle, remainder: true }
"#;

    #[test]
    fn accepts_a_wellformed_spec() {
        let spec = GeneratorSpec::from_yaml(BASE).expect("should parse");
        assert_eq!(spec.contexts.len(), 1);
    }

    #[test]
    fn rejects_partition_without_remainder() {
        let yaml = BASE.replace("      - { id: idle, remainder: true }\n", "");
        let err = GeneratorSpec::from_yaml(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("remainder dimensions"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_shares_exceeding_the_driver() {
        let yaml = BASE.replace("share: 0.7", "share: 1.4");
        let err = GeneratorSpec::from_yaml(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("exceeds 1.0"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_unknown_signal_reference() {
        let yaml = BASE.replace("driver: busy", "driver: nonexistent");
        let err = GeneratorSpec::from_yaml(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("unknown signal"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn rejects_signal_base_outside_its_range() {
        let yaml = BASE.replace("base: 30.0", "base: 200.0");
        let err = GeneratorSpec::from_yaml(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("lies outside"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn role_overrides_patch_only_named_fields() {
        let yaml = format!("{BASE}roles:\n  db:\n    signals:\n      busy: {{ base: 65.0 }}\n");
        let spec = GeneratorSpec::from_yaml(&yaml).expect("should parse");
        let defaults = spec.signals_for_role(None);
        let db = spec.signals_for_role(Some("db"));
        assert_eq!(defaults["busy"].base, 30.0);
        assert_eq!(db["busy"].base, 65.0);
        // Untouched fields survive the patch.
        assert_eq!(db["busy"].max, 95.0);
    }

    #[test]
    fn rejects_role_patching_unknown_signal() {
        let yaml = format!("{BASE}roles:\n  db:\n    signals:\n      ghost: {{ base: 1.0 }}\n");
        let err = GeneratorSpec::from_yaml(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("unknown signal"),
            "unexpected error: {err}"
        );
    }
}
