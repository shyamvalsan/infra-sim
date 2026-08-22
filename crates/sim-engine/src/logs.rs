//! Correlated logs.
//!
//! `spec.md` P0 asks for logs that line up with the metrics and with whatever
//! fault is running. The demo this serves is an SE clicking from a disk alert
//! into the logs and finding the database complaining about the actual root
//! cause, at the right moment, on the right node.
//!
//! ## Faults are matched on signals, not scenario names
//!
//! A rule fires when a *signal* is perturbed past a threshold — the generator
//! asks [`ScenarioSet::perturbation`] exactly the question the metrics engine
//! asks. Nothing here knows that `disk-fill` exists. Any scenario that pushes
//! `disk_space_used_kb` up gets disk-full logs, including ones written later,
//! and a scenario renamed or retuned cannot drift away from its own logs.
//!
//! ## Why there are no access logs
//!
//! A web node reporting 1,200 req/s on its chart while its logs show three
//! lines a second is exactly the contradiction an SRE notices, and emitting the
//! real volume would be absurd for a demo. Real deployments split this the same
//! way: nginx access logs go to a file, and only errors and notable events
//! reach journald. So this emits what journald would actually hold — errors,
//! state changes, and the periodic housekeeping every daemon logs.
//!
//! Everything is a pure function of (profile, seed, tick, scenarios), so the
//! separate logs process reproduces the same values the metrics plugin emitted
//! without the two having to coordinate.

use crate::rng::Rng;
use crate::{NodeProfile, ScenarioSet};
use std::collections::BTreeMap;

/// One journal entry, before it is framed as Journal Export Format.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// Wall-clock microseconds, which becomes `__REALTIME_TIMESTAMP`.
    pub realtime_us: i64,
    /// syslog priority, 0 (emerg) to 7 (debug).
    pub priority: u8,
    pub identifier: String,
    pub comm: String,
    pub pid: u32,
    pub message: String,
    /// Extra journal fields, e.g. `ERRNO`.
    pub extra: Vec<(String, String)>,
}

/// How a rule decides a signal is faulted.
#[derive(Debug, Clone, Copy)]
enum Trigger {
    /// Multiplier at or above this — the signal is being driven up.
    Above(f64),
    /// Multiplier at or below this — the signal is being driven down, which is
    /// how headroom signals like `mem_available_kb` express pressure.
    Below(f64),
}

impl Trigger {
    /// Severity in 0.0..=1.0, or `None` when the rule is not firing.
    ///
    /// Returning a magnitude rather than a boolean is what lets log volume and
    /// wording escalate as a fault deepens, instead of switching on at full
    /// intensity the moment a threshold is crossed.
    fn severity(self, multiplier: f64) -> Option<f64> {
        match self {
            Trigger::Above(t) => (multiplier >= t).then(|| {
                // Full severity at three times the trigger.
                (((multiplier - t) / (t * 2.0)).clamp(0.0, 1.0) + 0.15).min(1.0)
            }),
            Trigger::Below(t) => {
                (multiplier <= t).then(|| (((t - multiplier) / t).clamp(0.0, 1.0) + 0.15).min(1.0))
            }
        }
    }
}

/// Context handed to a rule's message writer.
struct Ctx<'a> {
    hostname: &'a str,
    /// Device or mount the rule matched, empty for node-level signals.
    instance: &'a str,
    severity: f64,
    rng: &'a mut Rng,
}

impl Ctx<'_> {
    fn pick(&mut self, options: &[&str]) -> String {
        let i = (self.rng.next_f64() * options.len() as f64) as usize;
        options[i.min(options.len() - 1)].to_string()
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.rng.next_f64() * (hi - lo)
    }
}

/// A line a fault produces: priority, syslog identifier, message.
type Line = (u8, &'static str, String);

struct FaultRule {
    signal: &'static str,
    /// Instance group to iterate (`mount`, `disk`, `net`), or `None` for a
    /// node-level signal.
    group: Option<&'static str>,
    trigger: Trigger,
    /// Service that must be present for this rule to apply, if any. A node
    /// without Postgres should not log Postgres errors.
    requires_service: Option<&'static str>,
    /// Expected lines per minute at full severity.
    rate_per_min: f64,
    write: fn(&mut Ctx) -> Line,
}

/// Fault rules, keyed on the signals the hero scenarios actually move.
const FAULT_RULES: &[FaultRule] = &[
    // --- Filesystem filling up -------------------------------------------
    FaultRule {
        signal: "disk_space_used_kb",
        group: Some("mount"),
        trigger: Trigger::Above(2.5),
        requires_service: Some("postgres"),
        rate_per_min: 6.0,
        write: |c| {
            let relation = c.pick(&["orders", "order_items", "checkout_events", "audit_log"]);
            let block = (c.range(1.0, 9.0) * 100_000.0) as u64;
            (
                3,
                "postgres",
                format!(
                    "ERROR:  could not extend file \"base/16384/{block}\": No space left on device\n\
                     HINT:  Check free disk space; relation \"{relation}\" on {}",
                    c.instance
                ),
            )
        },
    },
    FaultRule {
        signal: "disk_space_used_kb",
        group: Some("mount"),
        trigger: Trigger::Above(2.5),
        requires_service: None,
        rate_per_min: 2.0,
        write: |c| {
            (
                4,
                "kernel",
                format!(
                    "EXT4-fs warning (device {}): ext4_has_free_clusters:379: Filesystem \
                     on {} is running low on free blocks",
                    c.instance.trim_start_matches('/').replace('/', "-"),
                    c.instance
                ),
            )
        },
    },
    // --- Storage getting slow --------------------------------------------
    FaultRule {
        signal: "disk_await_write_ms",
        group: Some("disk"),
        trigger: Trigger::Above(3.0),
        requires_service: None,
        rate_per_min: 1.5,
        write: |c| {
            let secs = (c.range(120.0, 360.0) / 60.0).round() as u64 * 60;
            let task = c.pick(&["jbd2/nvme0n1-8", "kworker/u16:2+flush", "postgres"]);
            (
                4,
                "kernel",
                format!(
                    "INFO: task {task}:{} blocked for more than {secs} seconds on {}",
                    (c.range(400.0, 9000.0)) as u64,
                    c.instance
                ),
            )
        },
    },
    FaultRule {
        signal: "disk_await_write_ms",
        group: Some("disk"),
        trigger: Trigger::Above(3.0),
        requires_service: Some("postgres"),
        rate_per_min: 2.0,
        write: |c| {
            let write = c.range(20.0, 240.0);
            let sync = c.range(5.0, 190.0);
            (
                5,
                "postgres",
                format!(
                    "LOG:  checkpoint complete: wrote {} buffers ({:.1}%); \
                     write={:.3} s, sync={:.3} s, total={:.3} s",
                    (c.range(9000.0, 90000.0)) as u64,
                    c.range(3.0, 40.0),
                    write,
                    sync,
                    write + sync + 0.4
                ),
            )
        },
    },
    // --- Memory exhaustion ------------------------------------------------
    FaultRule {
        signal: "oom_kill_rate",
        group: None,
        trigger: Trigger::Above(1.5),
        requires_service: None,
        rate_per_min: 3.0,
        write: |c| {
            let victim = c.pick(&["python3", "node", "java", "gunicorn", "ruby"]);
            let pid = c.range(2000.0, 60000.0) as u64;
            let rss = c.range(400_000.0, 3_000_000.0) as u64;
            (
                2,
                "kernel",
                format!(
                    "Out of memory: Killed process {pid} ({victim}) \
                     total-vm:{}kB, anon-rss:{rss}kB, file-rss:0kB, shmem-rss:0kB, UID:1000",
                    rss + c.range(100_000.0, 900_000.0) as u64
                ),
            )
        },
    },
    FaultRule {
        signal: "mem_available_kb",
        group: None,
        trigger: Trigger::Below(0.6),
        requires_service: None,
        rate_per_min: 1.5,
        write: |c| {
            (
                4,
                "kernel",
                format!(
                    "{}: page allocation stalls for {}ms, order:0",
                    c.pick(&["kswapd0", "kcompactd0"]),
                    c.range(2000.0, 60000.0) as u64
                ),
            )
        },
    },
    FaultRule {
        signal: "swapio_out_rate",
        group: None,
        trigger: Trigger::Above(3.0),
        requires_service: None,
        rate_per_min: 1.0,
        write: |c| {
            (
                4,
                "systemd",
                format!(
                    "system.slice: Consumed {} of memory, swap pressure sustained on {}",
                    c.pick(&["7.4G", "11.2G", "15.9G"]),
                    c.hostname
                ),
            )
        },
    },
    // --- Connection saturation -------------------------------------------
    FaultRule {
        signal: "tcp_sockets_inuse",
        group: None,
        trigger: Trigger::Above(2.0),
        requires_service: Some("nginx"),
        rate_per_min: 5.0,
        write: |c| {
            let upstream = c.pick(&["10.0.2.14:8080", "10.0.2.15:8080", "10.0.3.21:5432"]);
            (
                3,
                "nginx",
                format!(
                    "upstream timed out (110: Connection timed out) while reading \
                     response header from upstream, upstream: \"http://{upstream}/\""
                ),
            )
        },
    },
    FaultRule {
        signal: "tcp_sockets_inuse",
        group: None,
        trigger: Trigger::Above(3.0),
        requires_service: Some("postgres"),
        rate_per_min: 3.0,
        write: |c| {
            (
                3,
                "postgres",
                format!(
                    "FATAL:  remaining connection slots are reserved for \
                     non-replication superuser connections (host {})",
                    c.pick(&["10.0.2.14", "10.0.2.15", "10.0.2.16"])
                ),
            )
        },
    },
    // --- Link instability -------------------------------------------------
    FaultRule {
        signal: "net_err_rate",
        group: Some("net"),
        trigger: Trigger::Above(2.0),
        requires_service: None,
        rate_per_min: 4.0,
        write: |c| (4, "kernel", format!("{}: NIC Link is Down", c.instance)),
    },
    FaultRule {
        signal: "net_drop_rate",
        group: Some("net"),
        trigger: Trigger::Above(2.0),
        requires_service: None,
        rate_per_min: 3.0,
        write: |c| {
            let speed = c.pick(&["100", "1000", "10000"]);
            (
                4,
                "kernel",
                format!(
                    "{}: NIC Link is Up {speed} Mbps Full Duplex, Flow Control: RX/TX",
                    c.instance
                ),
            )
        },
    },
    FaultRule {
        signal: "tcp_retrans_rate",
        group: None,
        trigger: Trigger::Above(2.5),
        requires_service: Some("postgres"),
        rate_per_min: 2.0,
        write: |c| {
            (
                4,
                "postgres",
                format!(
                    "LOG:  streaming replication lag is {:.1}s; standby \"{}\" is behind primary",
                    c.range(5.0, 900.0) * c.severity.max(0.2),
                    c.pick(&["standby-01", "standby-02"])
                ),
            )
        },
    },
    // --- Saturated CPU ----------------------------------------------------
    FaultRule {
        signal: "cpu_busy",
        group: None,
        trigger: Trigger::Above(1.6),
        requires_service: None,
        rate_per_min: 1.0,
        write: |c| {
            let depth = c.range(8.0, 90.0) as u64;
            (
                4,
                "systemd",
                format!(
                    "Scheduling latency on {} exceeded threshold; run queue depth {depth}",
                    c.hostname
                ),
            )
        },
    },
];

/// Steady-state chatter, so a quiet node is not an empty node.
struct RoutineRule {
    requires_service: Option<&'static str>,
    /// Expected lines per minute.
    rate_per_min: f64,
    write: fn(&mut Ctx) -> Line,
}

const ROUTINE_RULES: &[RoutineRule] = &[
    RoutineRule {
        requires_service: None,
        rate_per_min: 0.5,
        write: |c| {
            (
                6,
                "systemd",
                format!(
                    "Started {}.",
                    c.pick(&[
                        "Daily apt upgrade and clean activities",
                        "Cleanup of Temporary Directories",
                        "Rotate log files",
                        "Refresh fwupd metadata"
                    ])
                ),
            )
        },
    },
    RoutineRule {
        requires_service: None,
        rate_per_min: 0.35,
        write: |c| {
            (
                6,
                "CRON",
                format!(
                    "(root) CMD ({})",
                    c.pick(&[
                        "cd / && run-parts --report /etc/cron.hourly",
                        "/usr/local/bin/metrics-flush.sh",
                        "test -x /usr/sbin/anacron || start -q anacron"
                    ])
                ),
            )
        },
    },
    RoutineRule {
        requires_service: Some("postgres"),
        rate_per_min: 0.8,
        write: |c| {
            let table = c.pick(&["orders", "customers", "sessions", "order_items"]);
            (
                6,
                "postgres",
                format!(
                    "LOG:  automatic vacuum of table \"app.public.{table}\": \
                     index scans: 1, pages: {} removed, tuples: {} removed",
                    c.range(0.0, 400.0) as u64,
                    c.range(100.0, 90000.0) as u64
                ),
            )
        },
    },
    RoutineRule {
        requires_service: Some("postgres"),
        rate_per_min: 0.4,
        write: |c| {
            (
                6,
                "postgres",
                format!(
                    "LOG:  checkpoint starting: {}",
                    c.pick(&["time", "wal", "immediate force wait"])
                ),
            )
        },
    },
    RoutineRule {
        requires_service: Some("nginx"),
        rate_per_min: 0.3,
        write: |c| {
            (
                5,
                "nginx",
                format!(
                    "{} client closed connection while waiting for request, client: {}",
                    c.pick(&["*12", "*847", "*9134"]),
                    c.pick(&["203.0.113.14", "198.51.100.7", "203.0.113.201"])
                ),
            )
        },
    },
    RoutineRule {
        requires_service: Some("redis"),
        rate_per_min: 0.6,
        write: |c| {
            (
                6,
                "redis",
                c.pick(&[
                    "Background saving started by pid 214",
                    "Background saving terminated with success",
                    "DB saved on disk",
                    "10 changes in 300 seconds. Saving...",
                ]),
            )
        },
    },
    RoutineRule {
        requires_service: Some("kubernetes"),
        rate_per_min: 0.9,
        write: |c| {
            let pod = c.pick(&[
                "checkout-api",
                "catalog-api",
                "payments-worker",
                "session-cache",
            ]);
            let suffix = c.range(100000.0, 999999.0) as u64;
            (
                6,
                "kubelet",
                format!(
                    "\"SyncLoop (PLEG)\" event pod=\"default/{pod}-{suffix}\" \
                     type=\"ContainerStarted\""
                ),
            )
        },
    },
];

/// Per-node log generator.
pub struct LogGenerator {
    hostname: String,
    role: Option<String>,
    /// Host labels snapshotted at construction, so fault rules - which ask the
    /// same perturbation query the metrics path asks - can be label-targeted
    /// exactly like the charts are.
    labels: BTreeMap<String, String>,
    services: Vec<String>,
    /// Instance names by group, snapshotted so rules can iterate them.
    instances: Vec<(String, Vec<String>)>,
    boot_id: String,
    rng: Rng,
    pids: Vec<(String, u32)>,
}

impl LogGenerator {
    pub fn new(profile: &NodeProfile, services: &[String], master_seed: u64) -> Self {
        let mut rng = Rng::from_stream(master_seed, &format!("logs:{}", profile.hostname));
        let labels = profile.labels.clone();
        let instances = profile
            .instances
            .iter()
            .map(|(group, list)| {
                (
                    group.clone(),
                    list.iter().map(|i| i.name.clone()).collect::<Vec<_>>(),
                )
            })
            .collect();

        // A boot id derived from the GUID, so it is stable across restarts of
        // the logs process. journald treats a new boot id as a reboot, and a
        // fleet that appears to reboot every restart is an obvious tell.
        let boot_id = derive_boot_id(&profile.guid);

        // Daemon PIDs are assigned once and reused, because a service whose PID
        // changes on every line reads as crash-looping.
        let mut pids = Vec::new();
        for name in [
            "systemd", "CRON", "kernel", "postgres", "nginx", "redis", "kubelet",
        ] {
            let pid = 300 + (rng.next_u64() % 30_000) as u32;
            pids.push((name.to_string(), pid));
        }

        Self {
            hostname: profile.hostname.clone(),
            role: profile.role.clone(),
            labels,
            services: services.to_vec(),
            instances,
            boot_id,
            rng,
            pids,
        }
    }

    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    fn pid_for(&self, identifier: &str) -> u32 {
        self.pids
            .iter()
            .find(|(n, _)| n == identifier)
            .map(|(_, p)| *p)
            // The kernel is always PID 0; anything unknown gets a stable-ish id.
            .unwrap_or(if identifier == "kernel" { 0 } else { 1 })
    }

    fn has_service(&self, service: Option<&str>) -> bool {
        match service {
            None => true,
            Some(s) => self.services.iter().any(|own| own == s),
        }
    }

    /// Instances a rule should be evaluated against.
    ///
    /// A node-level signal still needs one pass, so `None` yields a single
    /// empty instance rather than nothing.
    fn instances_for(&self, group: Option<&str>) -> Vec<String> {
        match group {
            None => vec![String::new()],
            Some(g) => self
                .instances
                .iter()
                .find(|(name, _)| name == g)
                .map(|(_, list)| list.clone())
                .unwrap_or_default(),
        }
    }

    /// Log entries for one tick.
    pub fn tick(&mut self, scenarios: &ScenarioSet, now: i64, interval: f64) -> Vec<LogEntry> {
        let mut out = Vec::new();
        let minutes = interval / 60.0;

        for rule in ROUTINE_RULES {
            if !self.has_service(rule.requires_service) {
                continue;
            }
            if self.rng.next_f64() >= rule.rate_per_min * minutes {
                continue;
            }
            let mut rng = self.rng.clone();
            let mut ctx = Ctx {
                hostname: &self.hostname,
                instance: "",
                severity: 0.0,
                rng: &mut rng,
            };
            let line = (rule.write)(&mut ctx);
            self.rng = rng;
            out.push(self.entry(line, now));
        }

        for rule in FAULT_RULES {
            if !self.has_service(rule.requires_service) {
                continue;
            }
            for instance in self.instances_for(rule.group) {
                let p = scenarios.perturbation(
                    &self.hostname,
                    self.role.as_deref(),
                    &instance,
                    rule.signal,
                    &self.labels,
                    now,
                );
                let Some(severity) = rule.trigger.severity(p.multiplier) else {
                    continue;
                };
                if self.rng.next_f64() >= rule.rate_per_min * minutes * severity {
                    continue;
                }
                let mut rng = self.rng.clone();
                let mut ctx = Ctx {
                    hostname: &self.hostname,
                    instance: &instance,
                    severity,
                    rng: &mut rng,
                };
                let line = (rule.write)(&mut ctx);
                self.rng = rng;
                out.push(self.entry(line, now));
            }
        }

        out
    }

    fn entry(&self, (priority, identifier, message): Line, now: i64) -> LogEntry {
        LogEntry {
            realtime_us: now * 1_000_000,
            priority,
            comm: identifier.to_string(),
            pid: self.pid_for(identifier),
            identifier: identifier.to_string(),
            message,
            extra: Vec::new(),
        }
    }
}

/// A 32-hex-character boot id derived from the node GUID.
fn derive_boot_id(guid: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for b in guid.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let second = hash.rotate_left(31) ^ 0x9e37_79b9_7f4a_7c15;
    format!("{hash:016x}{second:016x}")
}

/// Frame entries as Journal Export Format for `systemd-journal-remote`.
///
/// Underscore-prefixed fields are "trusted" — journald refuses to let a local
/// client set them, which is why logs cannot simply be written to the local
/// journal. `systemd-journal-remote` accepts them because its whole purpose is
/// ingesting entries that were formed on another host, and that is what makes
/// per-node `_HOSTNAME` attribution possible.
///
/// A value containing a newline has to be written in the binary form: the field
/// name alone, then a 64-bit little-endian length, then the raw bytes. Writing
/// it as `NAME=value` instead would end the entry at the first newline and
/// corrupt every entry after it.
pub fn export_format(entry: &LogEntry, hostname: &str, boot_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |name: &str, value: &str| {
        if value.contains('\n') {
            out.extend_from_slice(name.as_bytes());
            out.push(b'\n');
            out.extend_from_slice(&(value.len() as u64).to_le_bytes());
            out.extend_from_slice(value.as_bytes());
            out.push(b'\n');
        } else {
            out.extend_from_slice(name.as_bytes());
            out.push(b'=');
            out.extend_from_slice(value.as_bytes());
            out.push(b'\n');
        }
    };

    put("__REALTIME_TIMESTAMP", &entry.realtime_us.to_string());
    // Monotonic must accompany the boot id; derived from realtime so it rises
    // with it rather than being a second, unrelated clock.
    put(
        "__MONOTONIC_TIMESTAMP",
        &(entry.realtime_us % 1_000_000_000_000).to_string(),
    );
    put("_BOOT_ID", boot_id);
    put("_HOSTNAME", hostname);
    put("_TRANSPORT", "journal");
    put("_MACHINE_ID", &boot_id[..32.min(boot_id.len())]);
    put("PRIORITY", &entry.priority.to_string());
    put("SYSLOG_IDENTIFIER", &entry.identifier);
    put(
        "SYSLOG_FACILITY",
        if entry.identifier == "kernel" {
            "0"
        } else {
            "3"
        },
    );
    put("_COMM", &entry.comm);
    put("_PID", &entry.pid.to_string());
    put("_UID", "0");
    put("_GID", "0");
    // Marks every simulated line, so an operator can always tell them apart
    // from the host's own logs with a single facet.
    put("INFRA_SIM", "true");
    for (k, v) in &entry.extra {
        put(k, v);
    }
    put("MESSAGE", &entry.message);
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Instance;
    use sim_spec::Scenario;
    use std::collections::BTreeMap;

    fn profile() -> NodeProfile {
        let mut instances = BTreeMap::new();
        instances.insert(
            "mount".to_string(),
            vec![
                Instance {
                    name: "/".into(),
                    weight: 1.0,
                    attrs: BTreeMap::new(),
                },
                Instance {
                    name: "/var/lib/pgsql".into(),
                    weight: 1.0,
                    attrs: BTreeMap::new(),
                },
            ],
        );
        instances.insert(
            "disk".to_string(),
            vec![Instance {
                name: "nvme0n1".into(),
                weight: 1.0,
                attrs: BTreeMap::new(),
            }],
        );
        instances.insert(
            "net".to_string(),
            vec![Instance {
                name: "eth0".into(),
                weight: 1.0,
                attrs: BTreeMap::new(),
            }],
        );
        NodeProfile {
            guid: "6a5c93f8-2e71-4d09-a3b6-84f7c1e05d92".into(),
            hostname: "sim-db-01".into(),
            role: Some("db".into()),
            attrs: BTreeMap::new(),
            labels: BTreeMap::new(),
            instances,
            utc_offset_secs: 0,
        }
    }

    fn generator() -> LogGenerator {
        LogGenerator::new(&profile(), &["postgres".to_string()], 12345)
    }

    /// A scenario that drives one signal on one instance hard.
    ///
    /// Built from YAML rather than struct literals, so these tests exercise the
    /// same parse path the shipped scenarios take.
    fn scenario_named(
        name: &str,
        signal: &str,
        instance: Option<&str>,
        multiplier: f64,
    ) -> ScenarioSet {
        let instance_line = match instance {
            Some(i) => format!("\n      instance: \"{i}\""),
            None => String::new(),
        };
        let yaml = format!(
            "version: 1\n\
             name: {name}\n\
             description: test\n\
             manifest:\n\
             \x20 root_cause: test\n\
             timeline:\n\
             \x20 - at: 0s\n\
             \x20   description: test\n\
             \x20   target:\n\
             \x20     signal: {signal}\n\
             \x20     role: db{instance_line}\n\
             \x20   effect: step\n\
             \x20   multiplier: {multiplier}\n"
        );
        let sc = Scenario::from_yaml(&yaml).expect("test scenario parses");
        ScenarioSet::new(vec![crate::ActiveScenario {
            recovering_since: None,
            scenario: sc,
            started_at: 1_700_000_000,
        }])
    }

    fn scenario_on(signal: &str, instance: Option<&str>, multiplier: f64) -> ScenarioSet {
        scenario_named("test", signal, instance, multiplier)
    }

    /// Collect entries over a window, so a probabilistic rule is exercised.
    fn collect(g: &mut LogGenerator, set: &ScenarioSet, ticks: i64) -> Vec<LogEntry> {
        let mut all = Vec::new();
        for i in 0..ticks {
            all.extend(g.tick(set, 1_700_000_000 + i, 1.0));
        }
        all
    }

    #[test]
    fn a_quiet_node_still_logs_something() {
        // An empty log pane reads as broken, not as healthy.
        let mut g = generator();
        let entries = collect(&mut g, &ScenarioSet::default(), 1200);
        assert!(!entries.is_empty(), "20 minutes produced no routine logs");
    }

    #[test]
    fn a_quiet_node_logs_nothing_alarming() {
        let mut g = generator();
        let entries = collect(&mut g, &ScenarioSet::default(), 1800);
        // Priority <= 4 is warning or worse. A healthy fleet that cries wolf
        // teaches an SE to ignore the logs pane during a demo.
        assert!(
            entries.iter().all(|e| e.priority >= 5),
            "healthy node logged: {:?}",
            entries
                .iter()
                .filter(|e| e.priority < 5)
                .map(|e| &e.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn filling_a_disk_makes_postgres_complain_about_that_mount() {
        // The demo: a disk alert, then logs naming the same volume.
        let mut g = generator();
        let set = scenario_on("disk_space_used_kb", Some("/var/lib/pgsql"), 8.0);
        let entries = collect(&mut g, &set, 600);
        let space = entries
            .iter()
            .filter(|e| e.message.contains("No space left on device"))
            .collect::<Vec<_>>();
        assert!(!space.is_empty(), "no disk-full logs: {:?}", entries.len());
        assert!(
            space
                .iter()
                .all(|e| e.identifier == "postgres" && e.priority <= 3),
            "disk-full lines should be postgres errors"
        );
        assert!(
            entries.iter().any(|e| e.message.contains("/var/lib/pgsql")),
            "logs must name the mount the scenario targeted"
        );
    }

    #[test]
    fn an_untargeted_mount_stays_quiet() {
        // Instance scoping is the difference between a log pane that supports
        // the root cause and one that muddies it.
        let mut g = generator();
        let set = scenario_on("disk_space_used_kb", Some("/var/lib/pgsql"), 8.0);
        let entries = collect(&mut g, &set, 600);
        assert!(
            !entries
                .iter()
                .any(|e| e.message.contains("device -)") || e.message.contains("on / is running")),
            "the untargeted root mount produced disk-full logs"
        );
    }

    #[test]
    fn memory_exhaustion_reaches_the_oom_killer() {
        let mut g = LogGenerator::new(&profile(), &[], 99);
        let set = scenario_on("oom_kill_rate", None, 40.0);
        let entries = collect(&mut g, &set, 600);
        assert!(
            entries
                .iter()
                .any(|e| e.message.contains("Out of memory: Killed process")),
            "no OOM killer logs"
        );
    }

    #[test]
    fn a_headroom_signal_triggers_when_driven_down() {
        // mem_available_kb falls under pressure, so the rule must fire on a
        // multiplier below 1, not above it.
        let mut g = LogGenerator::new(&profile(), &[], 7);
        let set = scenario_on("mem_available_kb", None, 0.2);
        let entries = collect(&mut g, &set, 900);
        assert!(
            entries
                .iter()
                .any(|e| e.message.contains("page allocation stalls")),
            "falling headroom produced no memory-pressure logs"
        );
    }

    #[test]
    fn a_node_without_the_service_never_logs_its_errors() {
        // A cache node logging Postgres errors is the kind of detail that ends
        // a demo's credibility.
        let mut g = LogGenerator::new(&profile(), &["redis".to_string()], 5);
        let set = scenario_on("disk_space_used_kb", Some("/var/lib/pgsql"), 8.0);
        let entries = collect(&mut g, &set, 600);
        assert!(
            entries.iter().all(|e| e.identifier != "postgres"),
            "a redis node emitted postgres logs"
        );
    }

    #[test]
    fn rules_match_signals_not_scenario_names() {
        // Nothing in this module knows the hero scenarios by name, so a new
        // scenario moving the same signal gets the same logs.
        let mut g = generator();
        let renamed = scenario_named(
            "some-future-scenario",
            "disk_space_used_kb",
            Some("/var/lib/pgsql"),
            8.0,
        );
        let entries = collect(&mut g, &renamed, 600);
        assert!(entries
            .iter()
            .any(|e| e.message.contains("No space left on device")));
    }

    #[test]
    fn output_is_reproducible_for_a_seed() {
        // The logs process runs separately from the metrics plugin; they only
        // stay correlated because both are pure functions of the same inputs.
        let set = scenario_on("disk_space_used_kb", Some("/var/lib/pgsql"), 8.0);
        let a = collect(&mut generator(), &set, 300);
        let b = collect(&mut generator(), &set, 300);
        assert_eq!(a, b);
    }

    #[test]
    fn severity_scales_with_how_far_past_the_trigger() {
        let t = Trigger::Above(2.0);
        assert!(t.severity(1.9).is_none());
        let mild = t.severity(2.1).expect("fires at the trigger");
        let bad = t.severity(9.0).expect("fires when far past");
        assert!(bad > mild, "{bad} !> {mild}");
        assert!(bad <= 1.0);
    }

    #[test]
    fn boot_ids_are_stable_and_journald_shaped() {
        let a = derive_boot_id("6a5c93f8-2e71-4d09-a3b6-84f7c1e05d92");
        assert_eq!(a, derive_boot_id("6a5c93f8-2e71-4d09-a3b6-84f7c1e05d92"));
        assert_ne!(a, derive_boot_id("other-guid"));
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn export_format_carries_the_trusted_hostname() {
        // This is the whole reason for the journal-remote path: journald will
        // not let a local client set _HOSTNAME.
        let entry = LogEntry {
            realtime_us: 1_700_000_000_000_000,
            priority: 3,
            identifier: "postgres".into(),
            comm: "postgres".into(),
            pid: 1421,
            message: "ERROR: something".into(),
            extra: Vec::new(),
        };
        let out = export_format(&entry, "sim-db-01", "4a1f9d205e834c17b6a20d94e7fc3518");
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("_HOSTNAME=sim-db-01"));
        assert!(text.contains("__REALTIME_TIMESTAMP=1700000000000000"));
        assert!(text.contains("PRIORITY=3"));
        assert!(text.contains("MESSAGE=ERROR: something"));
        assert!(text.ends_with("\n\n"), "entries end with a blank line");
    }

    #[test]
    fn a_multiline_message_uses_the_binary_form() {
        // Written as NAME=value, the first newline would end the entry and
        // corrupt every entry after it in the stream.
        let entry = LogEntry {
            realtime_us: 1_700_000_000_000_000,
            priority: 3,
            identifier: "postgres".into(),
            comm: "postgres".into(),
            pid: 1,
            message: "ERROR: could not extend\nHINT: check disk".into(),
            extra: Vec::new(),
        };
        let out = export_format(&entry, "h", "b");
        assert!(
            !out.windows(8).any(|w| w == b"MESSAGE="),
            "multiline message must not use the plain form"
        );
        let idx = out
            .windows(7)
            .position(|w| w == b"MESSAGE")
            .expect("field present");
        assert_eq!(out[idx + 7], b'\n', "field name is followed by a newline");
        let len = u64::from_le_bytes(out[idx + 8..idx + 16].try_into().unwrap());
        assert_eq!(len as usize, entry.message.len());
    }

    #[test]
    fn the_postgres_disk_full_message_is_multiline() {
        // Guards the interaction between the rule above and the binary framing:
        // this rule emits a real two-line Postgres error.
        let mut g = generator();
        let set = scenario_on("disk_space_used_kb", Some("/var/lib/pgsql"), 8.0);
        let entries = collect(&mut g, &set, 600);
        let multi = entries.iter().find(|e| e.message.contains("HINT"));
        let entry = multi.expect("a HINT line was produced");
        assert!(entry.message.contains('\n'));
        let out = export_format(entry, "sim-db-01", "b");
        assert!(!out.windows(8).any(|w| w == b"MESSAGE="));
    }
}
