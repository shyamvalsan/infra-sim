//! Build an environment from a plain-text description.
//!
//! An SE types what the prospect runs — "3 web servers behind an nginx load
//! balancer, a postgres primary, 2 redis caches" — and gets an
//! `environment.yaml`. `spec.md` lists this as P1 "LLM-assisted environment
//! compose".
//!
//! There are two front ends and one back end. The keyword parser in this module
//! is the offline default: no API key, no network, reproducible. `--llm` swaps
//! in a real model (see [`crate::llm`]) for descriptions written in the
//! prospect's vocabulary rather than ours — "our checkout tier fronted by an
//! ALB, an Aurora writer, two ElastiCache nodes" — which keywords read badly.
//!
//! Both produce a [`Reading`], and a `Reading` is all [`render`] will accept.
//! That boundary is the point: the model chooses *which* known roles and
//! services to use and how many, and never writes the YAML. So a model that
//! misunderstands produces a wrong-but-valid fleet the SE can see and correct,
//! not an environment referencing a signal no generator defines — which fails
//! silently, in front of a prospect. It also keeps GUIDs derived, so
//! regenerating never orphans a running fleet's history.
//!
//! `spec.md` puts this on the right side of its own line: LLM-assisted compose
//! is P1, and the non-goal is per-datapoint LLM generation. Authoring an
//! environment file offline is not inference in the data path. The other
//! boundary it draws still holds — describing a stack in your own words is
//! fine, *ingesting customer documents* (RFIs, discovery notes) is an explicit
//! non-goal. Both paths take a sentence the SE typed, never a file the prospect
//! sent.

/// One recognised group of nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub count: usize,
    pub role: String,
    pub services: Vec<String>,
    /// Hostname element, e.g. `checkout` in `acme-checkout-01`. `None` uses the
    /// role's own slug. The LLM path sets this so hostnames carry the
    /// prospect's vocabulary instead of ours.
    pub slug: Option<String>,
    /// The phrase this came from, echoed back so the SE can check the reading.
    pub source: String,
}

impl Group {
    /// Hostname element actually used, after falling back to the role's slug.
    pub fn effective_slug(&self) -> &str {
        match &self.slug {
            Some(s) if !s.is_empty() => s.as_str(),
            _ => slug_for(&self.role),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub groups: Vec<Group>,
    /// Phrases that matched no known role or service.
    pub unrecognised: Vec<String>,
}

impl Reading {
    /// Fold groups that would generate colliding hostnames.
    ///
    /// Hostnames are `{prefix}{slug}-{n:02}`, so two groups sharing a slug
    /// would emit the same hostname twice. Netdata keys a vnode on its GUID and
    /// the GUID is derived from the hostname, so a collision is not a cosmetic
    /// duplicate — it is two nodes claiming one identity, interleaving their
    /// samples into a single corrupted series.
    pub fn dedupe_slugs(&mut self) {
        let mut folded: Vec<Group> = Vec::new();
        for group in std::mem::take(&mut self.groups) {
            match folded
                .iter_mut()
                .find(|g| g.effective_slug() == group.effective_slug())
            {
                Some(existing) => {
                    existing.count += group.count;
                    existing.source = format!("{}; {}", existing.source, group.source);
                }
                None => folded.push(group),
            }
        }
        self.groups = folded;
    }
}

/// A role the generator specs know how to model, with the words that select it.
struct RoleDef {
    role: &'static str,
    /// Service specs composed onto the base for this role.
    services: &'static [&'static str],
    /// Hostname element, e.g. `web` in `sim-web-01`.
    slug: &'static str,
    /// What this role models, for a reader that is not a keyword matcher.
    summary: &'static str,
    keywords: &'static [&'static str],
}

/// One role, as offered to an LLM.
pub struct RoleInfo {
    pub role: &'static str,
    pub slug: &'static str,
    pub services: &'static [&'static str],
    pub summary: &'static str,
}

/// The roles a plan may use.
///
/// This is the same table the keyword parser matches against, exposed rather
/// than duplicated: a role that drifts out of one path would otherwise stay
/// silently available in the other.
pub fn roles() -> Vec<RoleInfo> {
    ROLES
        .iter()
        .map(|r| RoleInfo {
            role: r.role,
            slug: r.slug,
            services: r.services,
            summary: r.summary,
        })
        .collect()
}

/// Service specs present on disk, which are the only ones a node may name.
///
/// Read from the directory rather than hardcoded, so a spec the SE authors
/// becomes available without touching this file — and one that is missing is
/// never offered, because `run()` fails at load time on a service with no spec.
pub fn available_services(specs_dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(specs_dir) else {
        // No directory to read: offer only what the role table already names,
        // which is what the deterministic path would have produced anyway.
        let mut fallback: Vec<String> = ROLES
            .iter()
            .flat_map(|r| r.services.iter().map(|s| s.to_string()))
            .collect();
        fallback.sort();
        fallback.dedup();
        return fallback;
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("yaml") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            // The base spec is composed onto every node already; naming it as a
            // service would merge it into itself.
            (stem != "linux-system").then_some(stem)
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Only roles the specs can actually model appear here.
///
/// Recognising a word we cannot generate would produce a node with a Linux
/// baseline and nothing else — an empty dashboard where the prospect expects
/// their database. Better to report the phrase as unrecognised.
const ROLES: &[RoleDef] = &[
    RoleDef {
        role: "lb",
        services: &["nginx"],
        slug: "lb",
        summary: "edge load balancer or ingress: modest CPU and RAM, one disk, \
                  high connection counts",
        keywords: &[
            "load balancer",
            "loadbalancer",
            "load-balancer",
            "lb",
            "haproxy",
            "ingress",
            "edge",
            "reverse proxy",
            "proxy",
        ],
    },
    RoleDef {
        role: "web",
        services: &["nginx"],
        slug: "web",
        summary: "application or API tier serving requests: the general-purpose \
                  compute node, and the tier scenarios treat as downstream of a \
                  database",
        keywords: &[
            "web server",
            "web servers",
            "webserver",
            "webservers",
            "web",
            "app server",
            "app servers",
            "application server",
            "application servers",
            "frontend",
            "api server",
            "api servers",
        ],
    },
    RoleDef {
        role: "db",
        services: &["postgres"],
        slug: "db",
        summary: "relational database node: large RAM, a dedicated data volume \
                  at /var/lib/pgsql and a second interface for replication. Use \
                  for any relational engine - the modelled metrics are \
                  Postgres-shaped",
        keywords: &[
            "postgres",
            "postgresql",
            "database",
            "databases",
            "db",
            "primary",
            "sql server",
            "mysql",
            "mariadb",
        ],
    },
    RoleDef {
        role: "cache",
        services: &["redis"],
        slug: "cache",
        summary: "in-memory cache node: memory-bound, low disk activity",
        keywords: &["redis", "cache", "caches", "memcached", "valkey"],
    },
    RoleDef {
        role: "k8s-control-plane",
        services: &["kubernetes", "containers"],
        slug: "k8s-cp",
        summary: "Kubernetes control-plane node: apiserver, etcd, scheduler and \
                  controller-manager containers",
        keywords: &[
            "control plane",
            "control-plane",
            "master node",
            "master nodes",
            "kubernetes control plane",
        ],
    },
    RoleDef {
        role: "k8s-worker",
        services: &["kubernetes", "containers"],
        slug: "k8s-worker",
        summary: "Kubernetes worker node running containerised workloads: large \
                  core count, a containerd volume, per-container metrics",
        keywords: &[
            "worker node",
            "worker nodes",
            "kubernetes node",
            "kubernetes nodes",
            "k8s node",
            "k8s nodes",
            "worker",
            "workers",
            "kubernetes",
            "k8s",
        ],
    },
    RoleDef {
        role: "edge-gateway",
        services: &["containers"],
        slug: "edge-gw",
        summary: "small remote device - robot, kiosk, IoT or retail gateway: few \
                  cores, little RAM, eMMC storage and a flaky cellular \
                  interface alongside ethernet",
        keywords: &[
            "edge gateway",
            "edge gateways",
            "gateway",
            "gateways",
            "robot",
            "robots",
            "kiosk",
            "kiosks",
            "iot device",
            "iot devices",
            "edge device",
            "edge devices",
        ],
    },
];

/// Number words, so "three web servers" reads as naturally as "3 web servers".
const NUMBERS: &[(&str, usize)] = &[
    ("a", 1),
    ("an", 1),
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("a dozen", 12),
    ("twelve", 12),
    ("twenty", 20),
    ("fifty", 50),
];

/// Parse a description into node groups.
pub fn parse(text: &str) -> Reading {
    let mut reading = Reading::default();
    let lower = text.to_lowercase();

    // Split on separators an SE would naturally use. Each clause is expected to
    // describe one group.
    let clauses: Vec<&str> = lower
        .split([',', ';', '\n', '.'])
        .flat_map(|c| c.split(" and "))
        .flat_map(|c| c.split(" plus "))
        .flat_map(|c| c.split(" with "))
        .flat_map(|c| c.split(" behind "))
        .flat_map(|c| c.split(" fronted by "))
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();

    for clause in clauses {
        match match_clause(clause) {
            Some(g) => merge(&mut reading.groups, g),
            None => {
                // Only report clauses that look like they meant something; a
                // stray "running" is noise, not a missed requirement.
                if clause.split_whitespace().count() >= 2 {
                    reading.unrecognised.push(clause.to_string());
                }
            }
        }
    }

    reading
}

fn merge(groups: &mut Vec<Group>, incoming: Group) {
    if let Some(existing) = groups.iter_mut().find(|g| g.role == incoming.role) {
        existing.count += incoming.count;
        existing.source = format!("{}; {}", existing.source, incoming.source);
    } else {
        groups.push(incoming);
    }
}

fn match_clause(clause: &str) -> Option<Group> {
    // Longest keyword first, so "web server" is not shadowed by "web" and
    // "control plane" is not swallowed by a later "worker" match.
    let mut best: Option<(&RoleDef, usize)> = None;
    for def in ROLES {
        for kw in def.keywords {
            if clause.contains(kw) {
                let better = best.map(|(_, len)| kw.len() > len).unwrap_or(true);
                if better {
                    best = Some((def, kw.len()));
                }
            }
        }
    }
    let (def, _) = best?;

    Some(Group {
        count: extract_count(clause),
        role: def.role.to_string(),
        services: def.services.iter().map(|s| s.to_string()).collect(),
        // Keyword matching has no evidence for a better name than the role's.
        slug: None,
        source: clause.to_string(),
    })
}

fn extract_count(clause: &str) -> usize {
    for token in clause.split_whitespace() {
        let cleaned: String = token.chars().filter(|c| c.is_ascii_digit()).collect();
        if !cleaned.is_empty() {
            if let Ok(n) = cleaned.parse::<usize>() {
                if n > 0 && n <= 500 {
                    return n;
                }
            }
        }
    }
    for (word, n) in NUMBERS {
        // Word-boundary match so "an" does not fire inside "management".
        if clause
            .split(|c: char| !c.is_alphabetic())
            .any(|t| t == *word)
        {
            return *n;
        }
    }
    1
}

/// Hardware profile for a role. Sized so the generated fleet is plausible and,
/// critically, so no partition driver exceeds its total — a mismatch there
/// reports a permanently full disk or a pinned memory chart.
fn hardware(role: &str) -> (u32, u64, &'static str, &'static str) {
    match role {
        "lb" => (4, 4_194_304, "sda", "c5.xlarge"),
        "web" => (8, 16_777_216, "nvme0n1", "m5.2xlarge"),
        "db" => (16, 67_108_864, "nvme0n1", "r5.4xlarge"),
        "cache" => (8, 16_777_216, "sda", "r5.large"),
        "k8s-control-plane" => (8, 16_777_216, "nvme0n1", "m5.2xlarge"),
        "k8s-worker" => (16, 33_554_432, "nvme0n1", "m5.4xlarge"),
        "edge-gateway" => (4, 4_194_304, "mmcblk0", "edge-arm64"),
        _ => (4, 8_388_608, "sda", "m5.xlarge"),
    }
}

/// Mounts beyond `/` that a role should have, sized so each conserves its own
/// total rather than clamping.
fn extra_mounts(role: &str) -> &'static [(&'static str, u64)] {
    match role {
        "db" => &[("/var/lib/pgsql", 1_572_864_000), ("/var/log", 52_428_800)],
        "k8s-control-plane" | "k8s-worker" => &[("/var/lib/containerd", 209_715_200)],
        "edge-gateway" => &[("/var/log", 4_194_304)],
        _ => &[("/boot", 1_048_576)],
    }
}

fn slug_for(role: &str) -> &'static str {
    ROLES
        .iter()
        .find(|r| r.role == role)
        .map(|r| r.slug)
        .unwrap_or("node")
}

/// Render an environment file from a reading.
///
/// GUIDs are derived deterministically from the environment name and hostname,
/// so re-running the same description reproduces the same identities. That
/// matters: regenerating an environment must not orphan the history of a fleet
/// already running.
pub fn render(reading: &Reading, name: &str, seed: u64, prefix: &str) -> String {
    // Built as explicit lines rather than one large format string: YAML is
    // indentation-sensitive and Rust's `\` line continuation silently strips
    // leading whitespace, which produced a file whose keys sat at the wrong
    // nesting level and failed to parse.
    let mut lines: Vec<String> = vec![
        "version: 1".into(),
        format!("name: {name}"),
        "description: >-".into(),
        "  Generated from a text description.".into(),
        String::new(),
        format!("seed: {seed}"),
        "update_every: 1".into(),
        "generator: ../specs/linux-system.yaml".into(),
        "specs: ../specs".into(),
        "scenarios: ../scenarios".into(),
        String::new(),
        "# GUIDs are derived from the environment name and hostname, so".into(),
        "# regenerating this file reproduces the same node identities rather".into(),
        "# than orphaning the history of a fleet that is already running.".into(),
        "nodes:".into(),
    ];

    for group in &reading.groups {
        let (cores, ram_kb, disk, itype) = hardware(&group.role);
        let slug = group.effective_slug();
        for i in 1..=group.count {
            let hostname = format!("{prefix}{slug}-{i:02}");
            let guid = derive_guid(name, &hostname);
            let root_kb: u64 = 104_857_600;
            // Weight scales the shared disk-usage driver into this mount so it
            // lands near 38% rather than clamping at 100% full.
            let root_weight = (root_kb as f64 * 0.38 / 180_000_000.0 * 10_000.0).round() / 10_000.0;
            let services = if group.services.is_empty() {
                "[]".to_string()
            } else {
                format!("[{}]", group.services.join(", "))
            };

            lines.push(String::new());
            lines.push(format!("  - hostname: {hostname}"));
            lines.push(format!("    guid: {guid}"));
            lines.push(format!("    role: {}", group.role));
            lines.push(format!("    services: {services}"));
            lines.push("    utc_offset_secs: 0".into());
            lines.push("    attrs:".into());
            lines.push(format!("      cores: {cores}"));
            lines.push(format!("      ram_total_kb: {ram_kb}"));
            lines.push("      swap_total_kb: 4194304".into());
            lines.push("    instances:".into());
            lines.push("      disk:".into());
            lines.push(format!("        - {{ name: {disk}, weight: 1.0 }}"));
            // A database keeps its data on a second device, as a real primary
            // would - and as the hero scenarios expect to find.
            if group.role == "db" {
                lines.push("        - { name: nvme1n1, weight: 0.35 }".into());
            }
            lines.push("      mount:".into());
            lines.push(format!(
                "        - {{ name: \"/\", weight: {root_weight}, attrs: {{ disk_total_kb: {root_kb}, inodes_total: 6553600 }} }}"
            ));
            for &(path, total) in extra_mounts(&group.role) {
                let w = (total as f64 * 0.38 / 180_000_000.0 * 10_000.0).round() / 10_000.0;
                lines.push(format!(
                    "        - {{ name: \"{path}\", weight: {w}, attrs: {{ disk_total_kb: {total}, inodes_total: 6553600 }} }}"
                ));
            }
            lines.push("      net:".into());
            lines.push("        - { name: eth0, weight: 1.0 }".into());
            if group.role == "db" {
                // Replication link, which db-replication-lag targets directly.
                lines.push("        - { name: eth1, weight: 0.45 }".into());
            }
            if group.role == "edge-gateway" {
                lines.push("        - { name: wwan0, weight: 0.12 }".into());
            }
            if matches!(group.role.as_str(), "k8s-control-plane" | "k8s-worker") {
                lines.push("      container:".into());
                for c in ["app-api", "sidecar-proxy", "log-shipper"] {
                    lines.push(format!("        - {{ name: {c}, weight: 1.0 }}"));
                }
            }
            lines.push("    labels:".into());
            lines.push("      _os_name: Ubuntu".into());
            lines.push("      _os_version: 24.04.1 LTS".into());
            lines.push("      _kernel_version: 6.8.0-51-generic".into());
            lines.push("      _architecture: x86_64".into());
            lines.push(format!("      _system_cores: \"{cores}\""));
            lines.push(format!("      _system_ram_total: \"{ram_kb}\""));
            lines.push("      _virtualization: kvm".into());
            lines.push("      _virt_detection: systemd-detect-virt".into());
            lines.push("      _container: none".into());
            lines.push("      _container_detection: none".into());
            lines.push("      _cloud_provider_type: aws".into());
            lines.push(format!("      _cloud_instance_type: {itype}"));
            lines.push("      _cloud_instance_region: us-east-1".into());
            lines.push("      _install_type: infra-sim".into());
            lines.push(format!("      infra_sim_role: {}", group.role));
            lines.push("      infra_sim_env: production".into());
        }
    }

    lines.join("\n") + "\n"
}

/// Deterministic UUID from a name pair.
///
/// Not random: the same description must yield the same node identities, or
/// regenerating an environment silently orphans a running fleet's history.
fn derive_guid(env: &str, hostname: &str) -> String {
    let h = |salt: u64, s: &str| -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ salt;
        for b in s.bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    };
    let a = h(1, env) ^ h(2, hostname).rotate_left(21);
    let b = h(3, hostname) ^ h(4, env).rotate_left(37);
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16,
        (a & 0x0fff) as u16,
        // RFC 4122 variant bits, so the value reads as a real UUID.
        ((b >> 48) as u16 & 0x3fff) | 0x8000,
        b & 0xffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(text: &str) -> Vec<(String, usize)> {
        parse(text)
            .groups
            .into_iter()
            .map(|g| (g.role, g.count))
            .collect()
    }

    #[test]
    fn reads_a_typical_sentence() {
        let r = roles(
            "3 web servers behind an nginx load balancer, a postgres primary and 2 redis caches",
        );
        assert!(r.contains(&("web".to_string(), 3)), "{r:?}");
        assert!(r.contains(&("lb".to_string(), 1)), "{r:?}");
        assert!(r.contains(&("db".to_string(), 1)), "{r:?}");
        assert!(r.contains(&("cache".to_string(), 2)), "{r:?}");
    }

    #[test]
    fn reads_number_words() {
        let r = roles("three web servers and one database");
        assert!(r.contains(&("web".to_string(), 3)), "{r:?}");
        assert!(r.contains(&("db".to_string(), 1)), "{r:?}");
    }

    #[test]
    fn longest_keyword_wins() {
        // "control plane" must not be read as a generic kubernetes worker.
        let r = roles("a kubernetes control plane and 4 worker nodes");
        assert!(r.contains(&("k8s-control-plane".to_string(), 1)), "{r:?}");
        assert!(r.contains(&("k8s-worker".to_string(), 4)), "{r:?}");
    }

    #[test]
    fn merges_repeated_roles() {
        let r = roles("2 web servers, 3 more web servers");
        assert_eq!(r, vec![("web".to_string(), 5)]);
    }

    #[test]
    fn reports_what_it_did_not_understand() {
        // Silently inventing something plausible is the failure this project
        // can least afford, so unknown phrases are surfaced.
        let r = parse("5 web servers and 3 cassandra rings");
        assert!(r.groups.iter().any(|g| g.role == "web"));
        assert!(
            r.unrecognised.iter().any(|u| u.contains("cassandra")),
            "unrecognised: {:?}",
            r.unrecognised
        );
    }

    #[test]
    fn an_edge_fleet_reads_as_gateways() {
        let r = roles("50 edge gateways and a regional aggregator");
        assert!(r.contains(&("edge-gateway".to_string(), 50)), "{r:?}");
    }

    #[test]
    fn guids_are_stable_across_runs() {
        // Regenerating must not orphan a running fleet's history.
        let a = derive_guid("acme", "acme-web-01");
        let b = derive_guid("acme", "acme-web-01");
        assert_eq!(a, b);
        assert_ne!(a, derive_guid("acme", "acme-web-02"));
        assert_ne!(a, derive_guid("other", "acme-web-01"));
    }

    #[test]
    fn derived_guids_are_valid_uuids() {
        let g = derive_guid("acme", "acme-web-01");
        let parts: Vec<&str> = g.split('-').collect();
        assert_eq!(parts.len(), 5, "{g}");
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "{g}"
        );
        assert!(g.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "{g}");
    }

    #[test]
    fn renders_one_node_per_count() {
        let r = parse("2 web servers and a database");
        let yaml = render(&r, "acme", 42, "acme-");
        assert_eq!(yaml.matches("- hostname:").count(), 3);
        assert!(yaml.contains("hostname: acme-web-01"));
        assert!(yaml.contains("hostname: acme-web-02"));
        assert!(yaml.contains("hostname: acme-db-01"));
        assert!(yaml.contains("services: [postgres]"));
    }

    #[test]
    fn an_empty_description_produces_no_groups() {
        assert!(parse("").groups.is_empty());
        assert!(parse("   ").groups.is_empty());
    }

    #[test]
    fn unknown_roles_do_not_become_empty_nodes() {
        // A node with a Linux baseline and nothing else is an empty dashboard
        // where the prospect expects their service.
        let r = parse("4 elasticsearch data nodes");
        assert!(r.groups.is_empty(), "{:?}", r.groups);
        assert!(!r.unrecognised.is_empty());
    }

    #[test]
    fn generated_db_nodes_carry_what_the_scenarios_target() {
        // The hero scenarios target a data volume and a replication interface.
        // Without them the steps resolve to nothing and the scenario silently
        // does nothing - which the scenario checker catches, but only after a
        // generated environment has already been handed to someone.
        let yaml = render(&parse("a postgres primary"), "acme", 1, "acme-");
        assert!(
            yaml.contains("/var/lib/pgsql"),
            "db needs a data volume:\n{yaml}"
        );
        assert!(
            yaml.contains("name: eth1"),
            "db needs a replication link:\n{yaml}"
        );
    }

    #[test]
    fn every_role_gets_a_root_mount_and_an_interface() {
        for text in [
            "2 web servers",
            "a load balancer",
            "a redis cache",
            "3 worker nodes",
            "4 edge gateways",
        ] {
            let yaml = render(&parse(text), "acme", 1, "acme-");
            assert!(yaml.contains(r#"name: "/""#), "{text} has no root mount");
            assert!(yaml.contains("net:"), "{text} has no interface");
        }
    }
}
