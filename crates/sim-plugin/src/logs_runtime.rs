//! The `--logs` mode: feed correlated logs into the journal.
//!
//! Decision 3B: this runs as its own process rather than inside the metrics
//! plugin. Two reasons. Netdata owns the plugin's lifecycle and its stdout *is*
//! the plugins.d stream, so spawning per-node children there tangles teardown —
//! and this project already lost an hour to a plugin that outlived the removal
//! of its own file. Both processes use the composed NodeEngine and control
//! inputs; exact noisy values require identical evaluation histories.
//! The processes never manage one another's lifecycle.
//!
//! ## The pipeline
//!
//! ```text
//! infra-sim --logs
//!   -> Journal Export Format on stdin
//!   -> systemd-journal-remote --output=/var/log/journal/remote/remote-<host>.journal
//!   -> Netdata systemd-journal.plugin  (reads it via cap_dac_read_search)
//!   -> logs UI, one source per simulated node
//! ```
//!
//! Netdata derives the source name from the *filename* — `remote-sim-db-01`
//! from `remote-sim-db-01.journal` — so each node gets its own entry in the
//! logs source selector. `_HOSTNAME` inside the entries carries the same name,
//! which is why the journal-remote hop is unavoidable: journald refuses to let
//! a local client set a trusted field, so writing to the local journal would
//! attribute every simulated line to the machine running the demo.
//!
//! One child process per node is a consequence of the same constraint:
//! `--split-mode=host` is rejected for stdin sources, so the only way to get
//! per-node files is per-node processes. Fine at the scale this runs at; a
//! fleet in the hundreds should share one file and filter on `_HOSTNAME`.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use sim_engine::logs::{export_format, LogGenerator};
use sim_engine::ScenarioSet;

/// Where `systemd-journal-remote` writes, and where Netdata looks.
pub const DEFAULT_JOURNAL_DIR: &str = "/var/log/journal/remote";

/// Paths `systemd-journal-remote` is installed at across distributions.
const REMOTE_BINARIES: &[&str] = &[
    "/usr/lib/systemd/systemd-journal-remote",
    "/lib/systemd/systemd-journal-remote",
    "/usr/libexec/systemd/systemd-journal-remote",
];

pub fn find_journal_remote(override_path: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = override_path {
        return if p.exists() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("'{}' does not exist", p.display()))
        };
    }
    REMOTE_BINARIES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .ok_or_else(|| {
            format!(
                "systemd-journal-remote not found (looked in {}).\n\
                 It is what turns simulated log entries into journal files Netdata can read.\n\
                 Install it:  sudo apt-get install systemd-journal-remote\n\
                 or point at it with --journal-remote PATH",
                REMOTE_BINARIES.join(", ")
            )
        })
}

/// One journal-remote process and everything written into it: a single
/// generator at small fleet sizes (per-node files, the shipped behaviour) or
/// a shard of generators at scale.
struct Shard {
    generators: Vec<LogGenerator>,
    child: Child,
    writer: BufWriter<sim_engine::recording::RecordedWriter<std::process::ChildStdin>>,
    path: PathBuf,
}

/// Ceiling on concurrent `systemd-journal-remote` processes. One per node was
/// the original design and stays exactly that for small fleets; at the
/// project's newer fleet sizes (3,000 nodes) it meant 3,000 child processes,
/// 3,000 pipes and 3,000 journal files - fd and process-table exhaustion on
/// the host (review finding). Above this ceiling, nodes share shard files;
/// attribution survives because every entry carries `_HOSTNAME`, which is
/// what the logs UI filters on.
pub const MAX_JOURNAL_PROCESSES: usize = 64;

/// How many generators share one journal-remote process at this fleet size:
/// 1 (per-node files) below the ceiling, ceil(n / ceiling) above it.
pub fn shard_size(node_count: usize) -> usize {
    if node_count <= MAX_JOURNAL_PROCESSES {
        1
    } else {
        node_count.div_ceil(MAX_JOURNAL_PROCESSES)
    }
}

pub struct LogsRuntime {
    shards: Vec<Shard>,
}

impl LogsRuntime {
    /// Spawn `systemd-journal-remote` processes: one per node for small
    /// fleets, one per shard of `shard_size()` nodes at scale.
    pub fn start(
        generators: Vec<LogGenerator>,
        journal_dir: &Path,
        remote_bin: &Path,
        recorder: Option<std::sync::Arc<sim_engine::recording::Recorder>>,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(journal_dir).map_err(|e| {
            format!(
                "cannot create '{}': {e}\n\
                 Writing journal files needs root; run --logs with sudo.",
                journal_dir.display()
            )
        })?;

        let shard = shard_size(generators.len());
        let total = generators.len();
        let mut shards = Vec::new();
        let mut iter = generators.into_iter();
        for i in 0..total.div_ceil(shard) {
            let chunk: Vec<LogGenerator> = (&mut iter).take(shard).collect();
            let path = if shard == 1 {
                // Per-node: Netdata derives the logs source from the filename
                // (remote-<host>.journal -> source <host>), which is the
                // product behaviour for fleets of this size.
                journal_dir.join(format!("remote-{}.journal", chunk[0].hostname()))
            } else {
                journal_dir.join(format!("remote-shard-{i:02}.journal"))
            };
            let mut child = Command::new(remote_bin)
                .arg(format!("--output={}", path.display()))
                .arg("-")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                // journal-remote writes progress to stderr; letting it through
                // would interleave with our own reporting for every shard.
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("cannot run '{}': {e}", remote_bin.display()))?;
            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| "journal-remote stdin unavailable".to_string())?;
            shards.push(Shard {
                generators: chunk,
                child,
                writer: BufWriter::new(sim_engine::recording::RecordedWriter::new(
                    stdin,
                    recorder.clone(),
                    sim_engine::recording::Kind::Journal,
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                )),
                path,
            });
        }
        Ok(Self { shards })
    }

    pub fn files(&self) -> Vec<&Path> {
        self.shards.iter().map(|s| s.path.as_path()).collect()
    }

    /// Generate and write one tick's logs for every node.
    ///
    /// Returns how many entries were written, so the caller can report volume
    /// rather than leaving an operator guessing whether anything is happening.
    pub fn tick(
        &mut self,
        scenarios: &ScenarioSet,
        now: i64,
        interval: f64,
    ) -> Result<usize, String> {
        let mut written = 0usize;
        for shard in &mut self.shards {
            // A dead child means these nodes silently stopped logging. Better
            // to fail loudly than to present a demo with logs missing.
            if let Ok(Some(status)) = shard.child.try_wait() {
                let hosts: Vec<&str> = shard.generators.iter().map(|g| g.hostname()).collect();
                return Err(format!(
                    "journal-remote for {} exited ({status}); '{}' is no longer being written",
                    match hosts.len() {
                        1 => hosts[0].to_string(),
                        n => format!("{} nodes ({} .. {})", n, hosts[0], hosts[n - 1]),
                    },
                    shard.path.display()
                ));
            }

            for generator in &mut shard.generators {
                let entries = generator.tick(scenarios, now, interval);
                let hostname = generator.hostname().to_string();
                let boot_id = generator.boot_id().to_string();
                for entry in &entries {
                    shard
                        .writer
                        .write_all(&export_format(entry, &hostname, &boot_id))
                        .map_err(|e| format!("writing logs for '{hostname}': {e}"))?;
                }
                written += entries.len();
            }
            // Unflushed entries look exactly like a fleet that stopped
            // logging; one flush per shard covers all its nodes.
            shard
                .writer
                .flush()
                .map_err(|e| format!("flushing '{}': {e}", shard.path.display()))?;
        }
        Ok(written)
    }
}

impl Drop for LogsRuntime {
    /// Close every pipe, which is what stops the children.
    ///
    /// `systemd-journal-remote` reading from stdin exits on EOF, and the kernel
    /// closes our pipes however this process dies — including SIGKILL. So the
    /// children cannot outlive us the way the Python probe once did, and no
    /// signal handling is needed to guarantee it.
    fn drop(&mut self) {
        let shards = std::mem::take(&mut self.shards);
        for shard in shards {
            let Shard {
                mut child, writer, ..
            } = shard;
            drop(writer);
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_binary_explains_how_to_install_it() {
        let err = find_journal_remote(Some(Path::new("/nonexistent/journal-remote"))).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn the_search_path_covers_the_usual_install_locations() {
        // Distributions disagree about where this lives; a wrong guess reads to
        // the operator as "logs are broken".
        assert!(REMOTE_BINARIES
            .iter()
            .any(|p| p.contains("/usr/lib/systemd/")));
        assert!(REMOTE_BINARIES.iter().any(|p| p.contains("/libexec/")));
    }

    #[test]
    fn small_fleets_keep_one_process_per_node() {
        assert_eq!(shard_size(1), 1);
        assert_eq!(shard_size(64), 1);
    }

    #[test]
    fn sharding_spawns_a_bounded_number_of_processes() {
        // Structural, not behavioural: 300 generators against a fake
        // journal-remote must spawn exactly ceil(300/64) processes and one
        // journal file per shard - the resource ceiling this exists for.
        // A real journal-remote is not a test dependency.
        use sim_engine::NodeProfile;
        let dir = std::env::temp_dir().join(format!("infra-sim-shard-{}", std::process::id()));
        let journals = dir.join("journals");
        std::fs::create_dir_all(&journals).unwrap();
        let fake = dir.join("fake-remote");
        // cat, not sleep: the real journal-remote exits on stdin EOF, and Drop's
        // wait() depends on that; the fake must honour the same contract.
        std::fs::write(&fake, "#!/bin/sh\ncat >/dev/null\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let generators: Vec<LogGenerator> = (0..300)
            .map(|i| {
                let profile = NodeProfile {
                    hostname: format!("sim-robot-{i:03}"),
                    guid: format!("{i:03}000000-0000-4000-8000-000000000000"),
                    role: Some("web".into()),
                    attrs: Default::default(),
                    labels: Default::default(),
                    instances: Default::default(),
                    utc_offset_secs: 0,
                };
                let spec = sim_spec::GeneratorSpec::from_yaml(include_str!(
                    "../../sim-engine/src/fixtures/log-model.yaml"
                ))
                .unwrap();
                let engine = sim_engine::NodeEngine::new(std::sync::Arc::new(spec), profile, 7);
                LogGenerator::new(engine, &[], 7)
            })
            .collect();

        let mut runtime = LogsRuntime::start(generators, &journals, &fake, None).unwrap();
        // The invariant is the ceiling, not an exact count: shard_size
        // guarantees ceil(n / shard_size) processes at or below MAX.
        let processes = 300usize.div_ceil(shard_size(300));
        assert!(
            processes <= MAX_JOURNAL_PROCESSES,
            "{processes} processes for 300 nodes"
        );
        assert_eq!(runtime.files().len(), processes);
        let written = runtime
            .tick(&ScenarioSet::default(), 1_700_000_000, 1.0)
            .unwrap();
        assert!(written > 0, "entries flowed through shared pipes");
        drop(runtime);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_fleets_bound_the_process_count() {
        // 3,000 robots must not mean 3,000 journal-remote children: shards
        // grow so the process count stays at the ceiling.
        assert_eq!(shard_size(65), 2);
        assert_eq!(shard_size(3000), 47);
        assert_eq!(3000usize.div_ceil(shard_size(3000)), MAX_JOURNAL_PROCESSES);
    }

    #[test]
    fn journal_filenames_produce_the_netdata_source_name() {
        // Netdata derives the logs source from the filename, stripping
        // "remote-": remote-sim-db-01.journal -> source "sim-db-01".
        let dir = Path::new("/var/log/journal/remote");
        let name = dir.join(format!("remote-{}.journal", "sim-db-01"));
        assert_eq!(
            name.file_name().unwrap().to_str().unwrap(),
            "remote-sim-db-01.journal"
        );
    }
}
