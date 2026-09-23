//! Correlated logs.
//!
//! `spec.md` P0 asks for logs that line up with the metrics and with whatever
//! fault is running. The demo this serves is an SE clicking from a disk alert
//! into the logs and finding the database complaining about the actual root
//! cause, at the right moment, on the right node.
//!
//! ## Faults are matched on signals, not scenario names
//!
//! Rules inspect actual scoped values from the same composed NodeEngine used
//! by metrics. Additive faults, role tuning and node-index targets therefore
//! follow the same model. Scenario names are not part of the matching contract.
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
//! Reproduction requires the same evaluation history, not only a seed and time.
//! Independently started processes can have different noise histories.

use crate::rng::Rng;
use crate::{NodeEngine, ScenarioSet};

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
    /// Actual value (or fraction of a declared capacity) at or above this.
    Above(f64),
    /// Actual value or capacity fraction at or below this.
    Below(f64),
    Positive,
}

impl Trigger {
    /// Severity in 0.0..=1.0, or `None` when the rule is not firing.
    ///
    /// Returning a magnitude rather than a boolean is what lets log volume and
    /// wording escalate as a fault deepens, instead of switching on at full
    /// intensity the moment a threshold is crossed.
    fn severity(self, multiplier: f64) -> Option<f64> {
        match self {
            Trigger::Positive => {
                (multiplier > 0.0).then(|| (multiplier / (1.0 + multiplier) + 0.15).min(1.0))
            }
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
    value: f64,
    capacity: Option<f64>,
    rng: &'a mut Rng,
}

impl Ctx<'_> {
    fn percent(&self) -> f64 {
        100.0 * self.value / self.capacity.expect("capacity-based rule was validated")
    }

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

#[derive(Clone, Copy)]
enum Capacity {
    Attribute(&'static str),
    Signal(&'static str),
}

struct FaultRule {
    signal: &'static str,
    /// Instance group to iterate (`mount`, `disk`, `net`), or `None` for a
    /// node-level signal.
    group: Option<&'static str>,
    trigger: Trigger,
    capacity: Option<Capacity>,
    /// Service that must be present for this rule to apply, if any. A node
    /// without Postgres should not log Postgres errors.
    requires_service: Option<&'static str>,
    /// Expected lines per minute at full severity.
    rate_per_min: f64,
    write: fn(&mut Ctx) -> Line,
}

/// Fault rules, keyed on the signals the hero scenarios actually move.
const FAULT_RULES: &[FaultRule] = &[
    FaultRule {
        signal: "disk_space_used_kb",
        group: Some("mount"),
        trigger: Trigger::Above(0.9),
        capacity: Some(Capacity::Attribute("disk_total_kb")),
        requires_service: Some("postgres"),
        rate_per_min: 6.0,
        write: |c| {
            (4, "postgres", format!("WARNING: storage on {} is {:.1}% used\nHINT: Check free disk space before extending database files", c.instance, c.percent()))
        },
    },
    FaultRule {
        signal: "disk_space_used_kb",
        group: Some("mount"),
        trigger: Trigger::Above(0.9),
        capacity: Some(Capacity::Attribute("disk_total_kb")),
        requires_service: None,
        rate_per_min: 2.0,
        write: |c| {
            (
                4,
                "kernel",
                format!(
                    "Filesystem on {} is running low on free blocks: {:.1}% used",
                    c.instance,
                    c.percent()
                ),
            )
        },
    },
    FaultRule {
        signal: "disk_await_write_ms",
        group: Some("disk"),
        trigger: Trigger::Above(20.0),
        capacity: None,
        requires_service: None,
        rate_per_min: 1.5,
        write: |c| {
            (
                4,
                "kernel",
                format!(
                    "{}: elevated disk write latency, average {:.2} ms",
                    c.instance, c.value
                ),
            )
        },
    },
    FaultRule {
        signal: "oom_kill_rate",
        group: None,
        trigger: Trigger::Positive,
        capacity: None,
        requires_service: None,
        rate_per_min: 3.0,
        write: |c| {
            (
                2,
                "kernel",
                format!(
                    "Out of memory: OOM kills observed at {:.2} events/s",
                    c.value
                ),
            )
        },
    },
    FaultRule {
        signal: "mem_available_kb",
        group: None,
        trigger: Trigger::Below(0.1),
        capacity: Some(Capacity::Attribute("ram_total_kb")),
        requires_service: None,
        rate_per_min: 1.5,
        write: |c| {
            (
                4,
                "kernel",
                format!(
                    "Memory pressure: {:.0} KiB available ({:.1}% of RAM)",
                    c.value,
                    c.percent()
                ),
            )
        },
    },
    FaultRule {
        signal: "swapio_out_rate",
        group: None,
        trigger: Trigger::Above(100.0),
        capacity: None,
        requires_service: None,
        rate_per_min: 1.0,
        write: |c| {
            (
                4,
                "systemd",
                format!(
                    "Memory pressure on {}: swap out {:.2} KiB/s",
                    c.hostname, c.value
                ),
            )
        },
    },
    FaultRule {
        signal: "nginx_conn_active",
        group: None,
        trigger: Trigger::Above(0.9),
        capacity: Some(Capacity::Signal("nginx_connections_capacity")),
        requires_service: Some("nginx"),
        rate_per_min: 5.0,
        write: |c| {
            (
                4,
                "nginx",
                format!(
                    "[warn] connection usage high: {:.0} active of {:.0} configured slots ({:.1}%)",
                    c.value,
                    c.capacity.unwrap(),
                    c.percent()
                ),
            )
        },
    },
    FaultRule {
        signal: "pg_connections_used",
        group: None,
        trigger: Trigger::Above(0.9),
        capacity: Some(Capacity::Signal("pg_connections_max")),
        requires_service: Some("postgres"),
        rate_per_min: 3.0,
        write: |c| {
            (
                4,
                "postgres",
                format!(
                    "WARNING: connection usage high: {:.0} used of {:.0} configured slots ({:.1}%)",
                    c.value,
                    c.capacity.unwrap(),
                    c.percent()
                ),
            )
        },
    },
    FaultRule {
        signal: "net_err_rate",
        group: Some("net"),
        trigger: Trigger::Above(10.0),
        capacity: None,
        requires_service: None,
        rate_per_min: 4.0,
        write: |c| {
            (
                4,
                "kernel",
                format!("{}: interface errors {:.2}/s", c.instance, c.value),
            )
        },
    },
    FaultRule {
        signal: "net_drop_rate",
        group: Some("net"),
        trigger: Trigger::Above(30.0),
        capacity: None,
        requires_service: None,
        rate_per_min: 3.0,
        write: |c| {
            (
                4,
                "kernel",
                format!("{}: dropped packets {:.2}/s", c.instance, c.value),
            )
        },
    },
    FaultRule {
        signal: "tcp_retrans_rate",
        group: None,
        trigger: Trigger::Above(20.0),
        capacity: None,
        requires_service: None,
        rate_per_min: 2.0,
        write: |c| {
            (
                4,
                "kernel",
                format!("TCP retransmissions {:.2}/s on {}", c.value, c.hostname),
            )
        },
    },
    FaultRule {
        signal: "cpu_busy",
        group: None,
        trigger: Trigger::Above(80.0),
        capacity: None,
        requires_service: None,
        rate_per_min: 1.0,
        write: |c| {
            (
                4,
                "systemd",
                format!("CPU pressure on {}: {:.1}% busy", c.hostname, c.value),
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
    model: NodeEngine,
    services: Vec<String>,
    /// Instance names by group, snapshotted so rules can iterate them.
    instances: Vec<(String, Vec<String>)>,
    boot_id: String,
    rng: Rng,
    pids: Vec<(String, u32)>,
}

impl LogGenerator {
    pub fn new(model: NodeEngine, services: &[String], master_seed: u64) -> Self {
        let profile = model.profile();
        let mut rng = Rng::from_stream(master_seed, &format!("logs:{}", profile.hostname));
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
            model,
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
        if identifier == "kernel" {
            return 0;
        }
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
        self.model.tick(scenarios, now, interval);
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
                value: 0.0,
                capacity: None,
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
                let Some(mut value) = self.model.observed_signal(&instance, rule.signal) else {
                    continue;
                };
                if !value.is_finite() {
                    continue;
                }
                let capacity = match rule.capacity {
                    None => None,
                    Some(Capacity::Signal(name)) => self.model.observed_signal(&instance, name),
                    Some(Capacity::Attribute(name)) => rule
                        .group
                        .and_then(|group| self.model.profile().instances.get(group))
                        .and_then(|instances| instances.iter().find(|i| i.name == instance))
                        .and_then(|i| i.attrs.get(name).copied())
                        .or_else(|| self.model.profile().attr(name)),
                };
                if rule.capacity.is_some() && !capacity.is_some_and(|v| v.is_finite() && v > 0.0) {
                    continue;
                }
                // Filesystem partitions clamp their driver to the real mount capacity.
                if rule.signal == "disk_space_used_kb" {
                    value = value.clamp(0.0, capacity.unwrap());
                }
                let measured = capacity.map_or(value, |limit| value / limit);
                let Some(severity) = rule.trigger.severity(measured) else {
                    continue;
                };
                if self.rng.next_f64() >= rule.rate_per_min * minutes * severity {
                    continue;
                }
                let mut rng = self.rng.clone();
                let mut ctx = Ctx {
                    hostname: &self.hostname,
                    instance: &instance,
                    value,
                    capacity,
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
    use crate::{Instance, NodeProfile};
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
            attrs: BTreeMap::from([
                ("disk_total_kb".into(), 800.0),
                ("ram_total_kb".into(), 1000.0),
            ]),
            labels: BTreeMap::new(),
            instances,
            utc_offset_secs: 0,
        }
    }

    fn model(profile: NodeProfile, seed: u64) -> NodeEngine {
        let spec =
            sim_spec::GeneratorSpec::from_yaml(include_str!("fixtures/log-model.yaml")).unwrap();
        NodeEngine::with_rank(std::sync::Arc::new(spec), profile, seed, Some(0))
    }

    fn generator() -> LogGenerator {
        LogGenerator::new(model(profile(), 12345), &["postgres".to_string()], 12345)
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

    fn additive_on(signal: &str, amount: f64) -> ScenarioSet {
        let mut active = scenario_on(signal, None, 1.0).active().to_vec();
        active[0].scenario.timeline[0].effect = sim_spec::Effect::Add { amount };
        ScenarioSet::new(active)
    }

    #[test]
    fn additive_network_errors_report_values_without_inventing_link_state() {
        let entries = collect(&mut generator(), &additive_on("net_err_rate", 40.0), 600);
        assert!(entries
            .iter()
            .any(|e| e.message.contains("eth0: interface errors 40.00/s")));
        assert!(entries.iter().all(|e| !e.message.contains("NIC Link")));
    }

    #[test]
    fn node_index_targets_use_the_same_rank_as_metrics() {
        let mut active = additive_on("oom_kill_rate", 1.0).active().to_vec();
        active[0].scenario.timeline[0].target.node_index = Some(1);
        let set = ScenarioSet::new(active);
        let mut selected = generator();
        let spec = std::sync::Arc::new(
            sim_spec::GeneratorSpec::from_yaml(include_str!("fixtures/log-model.yaml")).unwrap(),
        );
        let other = NodeEngine::with_rank(spec, profile(), 12345, Some(1));
        let mut unselected = LogGenerator::new(other, &[], 12345);
        assert!(collect(&mut selected, &set, 600)
            .iter()
            .any(|e| e.message.contains("OOM kills")));
        assert!(collect(&mut unselected, &set, 600)
            .iter()
            .all(|e| !e.message.contains("OOM kills")));
    }

    #[test]
    fn disk_rules_use_instance_weight_and_capacity() {
        let mut p = profile();
        let mounts = p.instances.get_mut("mount").unwrap();
        mounts[0].weight = 0.1;
        mounts[1].attrs.insert("disk_total_kb".into(), 2000.0);
        let mut g = LogGenerator::new(model(p, 12345), &["postgres".into()], 12345);
        let entries = collect(&mut g, &scenario_on("disk_space_used_kb", None, 8.0), 600);
        assert!(
            entries
                .iter()
                .all(|e| !e.message.contains("WARNING: storage")
                    && !e.message.contains("free blocks"))
        );
    }

    #[test]
    fn missing_capacity_does_not_invent_storage_pressure() {
        let mut p = profile();
        p.attrs.remove("disk_total_kb");
        let mut g = LogGenerator::new(model(p, 12345), &["postgres".into()], 12345);
        let entries = collect(&mut g, &scenario_on("disk_space_used_kb", None, 8.0), 600);
        assert!(
            entries
                .iter()
                .all(|e| !e.message.contains("WARNING: storage")
                    && !e.message.contains("free blocks"))
        );
    }

    #[test]
    fn service_connection_warnings_require_actual_service_occupancy() {
        let mut g = generator();
        let entries = collect(&mut g, &additive_on("pg_connections_used", 100.0), 900);
        assert!(entries.iter().any(|e| e
            .message
            .contains("100 used of 100 configured slots (100.0%)")));
        let mut g = LogGenerator::new(model(profile(), 12345), &["nginx".into()], 12345);
        let entries = collect(&mut g, &additive_on("nginx_conn_active", 100.0), 900);
        assert!(entries.iter().any(|e| e
            .message
            .contains("100 active of 100 configured slots (100.0%)")));
        assert!(entries
            .iter()
            .all(|e| !e.message.contains("upstream timed out")));
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
            .filter(|e| e.message.contains("WARNING: storage"))
            .collect::<Vec<_>>();
        assert!(!space.is_empty(), "no disk-full logs: {:?}", entries.len());
        assert!(
            space
                .iter()
                .all(|e| e.identifier == "postgres" && e.priority <= 4),
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
        let mut g = LogGenerator::new(model(profile(), 12345), &[], 99);
        let set = additive_on("oom_kill_rate", 1.0);
        let entries = collect(&mut g, &set, 600);
        assert!(
            entries
                .iter()
                .any(|e| e.message.contains("Out of memory: OOM kills observed")),
            "no OOM killer logs"
        );
    }

    #[test]
    fn a_headroom_signal_triggers_when_driven_down() {
        // mem_available_kb falls under pressure, so the rule must fire on a
        // multiplier below 1, not above it.
        let mut g = LogGenerator::new(model(profile(), 12345), &[], 7);
        let set = scenario_on("mem_available_kb", None, 0.2);
        let entries = collect(&mut g, &set, 900);
        assert!(
            entries
                .iter()
                .any(|e| e.message.contains("Memory pressure:")),
            "falling headroom produced no memory-pressure logs"
        );
    }

    #[test]
    fn a_node_without_the_service_never_logs_its_errors() {
        // A cache node logging Postgres errors is the kind of detail that ends
        // a demo's credibility.
        let mut g = LogGenerator::new(model(profile(), 12345), &["redis".to_string()], 5);
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
            .any(|e| e.message.contains("WARNING: storage")));
    }

    #[test]
    fn output_is_reproducible_for_a_seed() {
        // Identical models, seeds and complete tick histories reproduce output.
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
