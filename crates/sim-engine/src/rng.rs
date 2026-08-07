//! Deterministic random number generation.
//!
//! Reproducibility is a product requirement, not a convenience: `spec.md`
//! promises that an `environment.yaml` plus a seed replays an identical world,
//! and the eval gym scores runs against that guarantee. So the PRNG is
//! implemented here rather than pulled from a crate — a dependency bump must
//! never silently change what a recorded seed replays.
//!
//! SplitMix64, from Steele, Lea & Flood, "Fast splittable pseudorandom number
//! generators" (OOPSLA 2014). Chosen for having no state beyond a single `u64`,
//! which makes per-stream seeding trivial.

/// FNV-1a over a byte string. Used only to turn stream names into seeds, never
/// for anything security-relevant.
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Derive an independent stream seed from a master seed and a name.
///
/// Streams are addressed by name rather than by draw order, so adding a node or
/// reordering contexts leaves every other stream's output untouched. Without
/// this, inserting one context would reshuffle the entire environment and break
/// replay of an archived demo.
pub fn derive_seed(master: u64, name: &str) -> u64 {
    master ^ fnv1a(name.as_bytes()).rotate_left(17)
}

/// SplitMix64.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
    /// Cached second variate from Box-Muller, which produces two at a time.
    spare_normal: Option<f64>,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed,
            spare_normal: None,
        }
    }

    pub fn from_stream(master: u64, name: &str) -> Self {
        Self::new(derive_seed(master, name))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`. Uses the top 53 bits, matching f64's mantissa.
    pub fn next_f64(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64) * (1.0 / (1u64 << 53) as f64)
    }

    /// Standard normal via Box-Muller.
    pub fn next_normal(&mut self) -> f64 {
        if let Some(spare) = self.spare_normal.take() {
            return spare;
        }
        // u1 must be non-zero for ln().
        let mut u1 = self.next_f64();
        while u1 <= f64::EPSILON {
            u1 = self.next_f64();
        }
        let u2 = self.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = std::f64::consts::TAU * u2;
        self.spare_normal = Some(r * theta.sin());
        r * theta.cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_streams_diverge() {
        let mut a = Rng::from_stream(7, "node-a/system.cpu/user");
        let mut b = Rng::from_stream(7, "node-b/system.cpu/user");
        let a: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
        let b: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
        assert_ne!(a, b);
    }

    #[test]
    fn stream_seeds_are_order_independent() {
        // Deriving a stream must not depend on how many were derived before it,
        // otherwise inserting a context reshuffles unrelated nodes.
        let first = derive_seed(99, "beta");
        let _ = derive_seed(99, "alpha");
        let again = derive_seed(99, "beta");
        assert_eq!(first, again);
    }

    #[test]
    fn uniforms_stay_in_range() {
        let mut rng = Rng::new(1234);
        for _ in 0..100_000 {
            let v = rng.next_f64();
            assert!((0.0..1.0).contains(&v), "out of range: {v}");
        }
    }

    #[test]
    fn normals_have_roughly_unit_variance() {
        let mut rng = Rng::new(2024);
        let n = 200_000;
        let samples: Vec<f64> = (0..n).map(|_| rng.next_normal()).collect();
        let mean = samples.iter().sum::<f64>() / n as f64;
        let var = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
        assert!(mean.abs() < 0.02, "mean drifted: {mean}");
        assert!((var - 1.0).abs() < 0.05, "variance off: {var}");
    }
}
