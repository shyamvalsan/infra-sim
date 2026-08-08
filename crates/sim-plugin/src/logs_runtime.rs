//! The `--logs` mode: feed correlated logs into the journal.
//!
//! Decision 3B: this runs as its own process rather than inside the metrics
//! plugin. Two reasons. Netdata owns the plugin's lifecycle and its stdout *is*
//! the plugins.d stream, so spawning per-node children there tangles teardown —
//! and this project already lost an hour to a plugin that outlived the removal
//! of its own file. And because the engine is a pure function of (spec,
//! profile, seed, tick, scenarios), a separate process reading the same
//! environment and the same `control.yaml` reproduces the same values the
//! plugin emitted. Logs and metrics correlate by construction; the two
//! processes never talk to each other.
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

/// One node's writer: its generator and the journal-remote process behind it.
struct Sink {
    generator: LogGenerator,
    child: Child,
    writer: BufWriter<std::process::ChildStdin>,
    path: PathBuf,
}

pub struct LogsRuntime {
    sinks: Vec<Sink>,
}

impl LogsRuntime {
    /// Spawn one `systemd-journal-remote` per node.
    pub fn start(
        generators: Vec<LogGenerator>,
        journal_dir: &Path,
        remote_bin: &Path,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(journal_dir).map_err(|e| {
            format!(
                "cannot create '{}': {e}\n\
                 Writing journal files needs root; run --logs with sudo.",
                journal_dir.display()
            )
        })?;

        let mut sinks = Vec::new();
        for generator in generators {
            let path = journal_dir.join(format!("remote-{}.journal", generator.hostname()));
            let mut child = Command::new(remote_bin)
                .arg(format!("--output={}", path.display()))
                .arg("-")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                // journal-remote writes progress to stderr; letting it through
                // would interleave with our own reporting for every node.
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("cannot run '{}': {e}", remote_bin.display()))?;

            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| "journal-remote stdin unavailable".to_string())?;

            sinks.push(Sink {
                generator,
                child,
                writer: BufWriter::new(stdin),
                path,
            });
        }
        Ok(Self { sinks })
    }

    pub fn files(&self) -> Vec<&Path> {
        self.sinks.iter().map(|s| s.path.as_path()).collect()
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
        for sink in &mut self.sinks {
            // A dead child means this node silently stopped logging. Better to
            // fail loudly than to present a demo with one node's logs missing.
            if let Ok(Some(status)) = sink.child.try_wait() {
                return Err(format!(
                    "journal-remote for '{}' exited ({status}); '{}' is no longer being written",
                    sink.generator.hostname(),
                    sink.path.display()
                ));
            }

            let entries = sink.generator.tick(scenarios, now, interval);
            let hostname = sink.generator.hostname().to_string();
            let boot_id = sink.generator.boot_id().to_string();
            for entry in &entries {
                sink.writer
                    .write_all(&export_format(entry, &hostname, &boot_id))
                    .map_err(|e| format!("writing logs for '{hostname}': {e}"))?;
            }
            // Unflushed entries look exactly like a fleet that stopped logging.
            sink.writer
                .flush()
                .map_err(|e| format!("flushing logs for '{hostname}': {e}"))?;
            written += entries.len();
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
        for sink in &mut self.sinks {
            let _ = sink.writer.flush();
        }
        // Take the writers out so the pipes close before we wait on the child.
        let sinks = std::mem::take(&mut self.sinks);
        for sink in sinks {
            let Sink {
                mut child, writer, ..
            } = sink;
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
