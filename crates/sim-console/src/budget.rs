//! Host-tunable resource budgets for a shared console.
//!
//! The contract with whoever hosts the box (SOW-0021): every limit lives in
//! one file they own, refusals name the limit they hit and where to raise it,
//! and the defaults are safe for a small shared host. Code never hardcodes a
//! number a host might need to change.
//!
//! Defaults: 500 nodes/fleet (matches the per-group ceiling so one big group
//! stays expressible), 10 live simulations, 50 GB total payload disk, 7-day
//! TTL (user decisions, 2026-08-19).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The budget set. All fields host-tunable via the budgets file.
#[derive(Debug, Clone, Serialize)]
pub struct Budgets {
    /// The file these came from, for refusal messages. A host running with
    /// --budgets elsewhere must be told to edit *that* file.
    #[serde(skip)]
    pub source: std::path::PathBuf,
    /// Ceiling on the summed node counts of one create. The real guard is
    /// live-simulations + disk; this stops a single fat fleet on its own.
    pub max_nodes_per_fleet: usize,
    /// Live (including stopped-but-not-torn-down) simulations on the host.
    pub max_live_simulations: usize,
    /// Total bytes under the state dir (journals and agent DBs accumulate;
    /// this is the failure that actually fills disks).
    pub max_total_disk_bytes: u64,
    /// Simulations older than this are archived by the sweeper unless pinned.
    pub ttl_days: u64,
    /// Bind new simulations' agent ports to 0.0.0.0 instead of loopback, so
    /// remote users can open the dashboards directly. Off by default: a
    /// Netdata agent has no authentication of its own, and a public binding is
    /// a deliberate, warned choice for a firewalled host - see docs/hosting.md.
    pub public_dashboards: bool,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            source: std::path::PathBuf::new(),
            max_nodes_per_fleet: 500,
            max_live_simulations: 10,
            max_total_disk_bytes: 50 * 1024 * 1024 * 1024,
            ttl_days: 7,
            public_dashboards: false,
        }
    }
}

/// Where the file lives by default: `/etc/infra-sim/console.yaml`, owned by
/// the host's SRE, not by this repo.
pub const DEFAULT_BUDGETS_PATH: &str = "/etc/infra-sim/console.yaml";

/// The file as the SRE writes it: every key optional, unknown keys refused -
/// a typo'd limit silently ignored is worse than a refused start.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetsFile {
    #[serde(default)]
    max_nodes_per_fleet: Option<usize>,
    #[serde(default)]
    max_live_simulations: Option<usize>,
    #[serde(default)]
    max_total_disk_gb: Option<u64>,
    #[serde(default)]
    ttl_days: Option<u64>,
    #[serde(default)]
    public_dashboards: Option<bool>,
}

impl Budgets {
    /// Load budgets: defaults, overridden by the file when it exists.
    ///
    /// A malformed file is an error, not a silent default - an SRE who wrote
    /// a limit and got it ignored would be a bad failure mode.
    pub fn load(path: &Path) -> Result<Self, String> {
        let defaults = Self::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            let mut d = defaults;
            d.source = path.to_path_buf();
            return Ok(d);
        };
        let file: BudgetsFile =
            serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let source = path.to_path_buf();
        Ok(Self {
            source,
            max_nodes_per_fleet: file
                .max_nodes_per_fleet
                .filter(|v| *v > 0)
                .unwrap_or(defaults.max_nodes_per_fleet),
            max_live_simulations: file
                .max_live_simulations
                .filter(|v| *v > 0)
                .unwrap_or(defaults.max_live_simulations),
            max_total_disk_bytes: file
                .max_total_disk_gb
                .filter(|v| *v > 0)
                .map(|gb| gb * 1024 * 1024 * 1024)
                .unwrap_or(defaults.max_total_disk_bytes),
            ttl_days: file
                .ttl_days
                .filter(|v| *v > 0)
                .unwrap_or(defaults.ttl_days),
            public_dashboards: file.public_dashboards.unwrap_or(defaults.public_dashboards),
        })
    }

    /// Refuse a create that would exceed the host's budgets. Every message
    /// names the limit and the file that holds it.
    pub fn check_create(
        &self,
        nodes: usize,
        live_simulations: usize,
        disk_bytes: u64,
    ) -> Result<(), String> {
        if nodes > self.max_nodes_per_fleet {
            return Err(format!(
                "this fleet would have {nodes} nodes; the host caps a fleet at {} - raise \
                 max_nodes_per_fleet in {} if that is wrong",
                self.max_nodes_per_fleet,
                self.source.display()
            ));
        }
        if live_simulations >= self.max_live_simulations {
            return Err(format!(
                "the host is at its cap of {} live simulations - tear one down, or raise \
                 max_live_simulations in {}",
                self.max_live_simulations,
                self.source.display()
            ));
        }
        if disk_bytes >= self.max_total_disk_bytes {
            return Err(format!(
                "simulation storage is at its cap of {} GB - tear old fleets down, or raise \
                 max_total_disk_gb in {}",
                self.max_total_disk_bytes / 1024 / 1024 / 1024,
                self.source.display()
            ));
        }
        Ok(())
    }

    /// Whether a simulation of this age should be archived by the sweeper.
    pub fn expired(&self, age_secs: i64, pinned: bool) -> bool {
        !pinned && age_secs > (self.ttl_days as i64) * 86_400
    }
}

/// Total bytes under the state dir, recursively. Journals and agent databases
/// are plain files; at tens of simulations this walk is milliseconds.
pub fn state_dir_bytes(state_dir: &Path) -> u64 {
    fn walk(state_dir: &Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(state_dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|e| {
                let Ok(meta) = e.metadata() else {
                    return 0;
                };
                if meta.is_dir() {
                    walk(&e.path())
                } else {
                    meta.len()
                }
            })
            .sum()
    }
    walk(state_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_agreed_budget() {
        let b = Budgets::default();
        assert_eq!(b.max_nodes_per_fleet, 500);
        assert_eq!(b.max_live_simulations, 10);
        assert_eq!(b.max_total_disk_bytes, 50 * 1024 * 1024 * 1024);
        assert_eq!(b.ttl_days, 7);
        assert!(!b.public_dashboards);
    }

    #[test]
    fn the_file_overrides_only_what_it_names() {
        let dir = std::env::temp_dir().join(format!("infra-sim-budget-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("console.yaml");
        std::fs::write(&f, "max_live_simulations: 3\nttl_days: 30\n").unwrap();
        let b = Budgets::load(&f).unwrap();
        assert_eq!(b.max_live_simulations, 3);
        assert_eq!(b.ttl_days, 30);
        // Untouched keys keep their defaults.
        assert_eq!(b.max_nodes_per_fleet, 500);
        assert_eq!(b.max_total_disk_bytes, 50 * 1024 * 1024 * 1024);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_a_broken_one_is_an_error() {
        assert_eq!(
            Budgets::load(Path::new("/nonexistent/console.yaml"))
                .unwrap()
                .max_live_simulations,
            10
        );
        let dir = std::env::temp_dir().join(format!("infra-sim-budget2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("console.yaml");
        std::fs::write(&f, "max_live_simulations: not-a-number\n").unwrap();
        let err = Budgets::load(&f).unwrap_err();
        assert!(err.contains("not-a-number"), "{err}");
        // Unknown keys are refused rather than typo-silently-ignored.
        std::fs::write(&f, "live_sims: 3\n").unwrap();
        assert!(Budgets::load(&f).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refusals_name_the_limit_and_the_file() {
        let b = Budgets::default();
        let err = b.check_create(600, 0, 0).unwrap_err();
        assert!(err.contains("600") && err.contains("500"), "{err}");
        assert!(err.contains("max_nodes_per_fleet"), "{err}");

        let err = b.check_create(10, 10, 0).unwrap_err();
        assert!(
            err.contains("10 live simulations") && err.contains("max_live_simulations"),
            "{err}"
        );

        let err = b.check_create(10, 0, 50 * 1024 * 1024 * 1024).unwrap_err();
        assert!(
            err.contains("50 GB") && err.contains("max_total_disk_gb"),
            "{err}"
        );

        assert!(b.check_create(500, 9, 49 * 1024 * 1024 * 1024).is_ok());
    }

    #[test]
    fn expiry_needs_age_and_the_absence_of_a_pin() {
        let b = Budgets::default();
        let seven_days = 7 * 86_400;
        assert!(!b.expired(seven_days - 1, false));
        assert!(b.expired(seven_days + 1, false));
        // Pinned survives any age: pin-to-keep is absolute until unpinned.
        assert!(!b.expired(seven_days + 86_400, true));
    }

    #[test]
    fn the_disk_walk_sums_files_recursively() {
        let dir = std::env::temp_dir().join(format!("infra-sim-disk-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("one"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("a/two"), vec![0u8; 200]).unwrap();
        std::fs::write(dir.join("a/b/three"), vec![0u8; 300]).unwrap();
        assert_eq!(state_dir_bytes(&dir), 600);
        assert_eq!(state_dir_bytes(Path::new("/nonexistent")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
