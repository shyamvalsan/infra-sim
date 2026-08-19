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

use std::collections::BTreeMap;

/// One recognised group of nodes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group {
    pub count: usize,
    pub role: String,
    pub services: Vec<String>,
    /// Where these nodes are. `None` means unplaced: no coordinates are written
    /// at all, rather than a default that would put a prospect's estate off the
    /// coast of Africa.
    pub site: Option<Site>,
    /// What this network device is, for its `device_vendor`, `device_model` and
    /// `device_type` labels. Set from the device catalogue when a model is
    /// picked; absent, the generic switch's own labels are used.
    ///
    /// A node loading Netdata's Cisco Catalyst profile while labelled as a
    /// made-up switch model is the kind of contradiction an SRE reads first.
    pub device: Option<DeviceIdentity>,
    /// Hostname element, e.g. `checkout` in `acme-checkout-01`. `None` uses the
    /// role's own slug. The LLM path sets this so hostnames carry the
    /// prospect's vocabulary instead of ours.
    pub slug: Option<String>,
    /// Host labels authored for this group, layered over the fleet's: same key
    /// here wins. Validated before it reaches the renderer - see
    /// [`crate::labels`].
    pub labels: BTreeMap<String, String>,
    /// The phrase this came from, echoed back so the SE can check the reading.
    pub source: String,
}

/// The fleet's home. Its own location when it has one; otherwise the site of the
/// largest placed group, so a fleet assembled without a fleet-level location
/// still places the simulation's agent somewhere true.
fn fleet_site(reading: &Reading) -> Option<Site> {
    reading.site.or_else(|| {
        reading
            .groups
            .iter()
            .filter(|g| g.site.is_some())
            .fold(None::<&Group>, |best, g| match best {
                Some(b) if b.count >= g.count => Some(b),
                _ => Some(g),
            })
            .and_then(|g| g.site)
    })
}

/// A node's user-authored label lines: fleet labels with the group's layered
/// on top (same key, group wins). Emitted between the generated identity
/// labels and the site labels, so a node reads generated-then-authored-then-
/// placed top to bottom.
fn user_label_lines(
    fleet: &BTreeMap<String, String>,
    group: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut merged = fleet.clone();
    for (k, v) in group {
        merged.insert(k.clone(), v.clone());
    }
    merged
        .iter()
        .map(|(k, v)| format!("      {k}: {}", crate::labels::yaml_scalar(v)))
        .collect()
}

/// The tier the generated `infra_sim_env` label reports: the user's own
/// `environment` label when one is set (group overrides fleet), else
/// production. Keeps our marker in agreement with the user's vocabulary
/// instead of asserting "production" about a staging fleet.
fn env_tier(fleet: &BTreeMap<String, String>, group: &BTreeMap<String, String>) -> String {
    group
        .get(crate::labels::ENVIRONMENT_LABEL)
        .or_else(|| fleet.get(crate::labels::ENVIRONMENT_LABEL))
        .cloned()
        .unwrap_or_else(|| "production".to_string())
}

/// The `latitude`/`longitude` label lines for one node, or nothing when the
/// group has no site. Emitted for every node class, because a switch is at a
/// site as much as a server is.
fn site_labels(site: Option<Site>, hostname: &str) -> Vec<String> {
    match site {
        None => Vec::new(),
        Some(s) => {
            let (lat, lon) = s.scattered(hostname);
            vec![
                format!("      latitude: {lat:.6}"),
                format!("      longitude: {lon:.6}"),
            ]
        }
    }
}

/// What a network device says it is.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceIdentity {
    pub vendor: String,
    pub model: String,
    /// `switch`, `router`, `firewall`, ... from the profile's own metadata.
    pub kind: String,
}

/// splitmix64's finalizer. Spreads a change in any input bit across all 64.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A geographic site, for the `latitude` and `longitude` host labels.
///
/// Netdata Cloud reads these to place a node on a map. The agent itself does not
/// interpret them - they are ordinary host labels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Site {
    pub lat: f64,
    pub lon: f64,
}

impl Site {
    /// Reject anything that is not a point on Earth. A silently wrong coordinate
    /// is worse than a refused one: it puts the prospect's fleet somewhere
    /// visibly absurd in front of them.
    pub fn new(lat: f64, lon: f64) -> Result<Self, String> {
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err(format!("latitude {lat} is not between -90 and 90"));
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return Err(format!("longitude {lon} is not between -180 and 180"));
        }
        Ok(Self { lat, lon })
    }

    /// This site, offset by up to roughly 500m, fixed by hostname.
    ///
    /// Machines in one rack really do share a coordinate, but 27 nodes on one
    /// pin is a map that hides 26 of them. The offset is derived from the
    /// hostname so a node never moves between runs, re-skins or reinstalls.
    pub fn scattered(&self, hostname: &str) -> (f64, f64) {
        // ~500m of latitude. Longitude degrees shrink towards the poles, so the
        // east-west offset is divided by cos(lat) to keep the scatter circular.
        const SPREAD_DEG: f64 = 0.0045;
        let h = hostname.bytes().fold(0xcbf2_9ce4_8422_2325u64, |acc, b| {
            (acc ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
        // FNV on its own is not enough here. `-web-01` and `-web-02` differ in
        // one trailing byte, which barely moves FNV's upper bits, so two windows
        // of the same hash gave every node at a site the same longitude and
        // latitudes 7m apart. Avalanche first, and derive each axis from its own
        // mixed stream.
        let unit = |bits: u64| (bits & 0xffff) as f64 / 32_767.5 - 1.0;
        let lat = (self.lat + unit(mix(h)) * SPREAD_DEG).clamp(-90.0, 90.0);
        let shrink = self.lat.to_radians().cos().abs().max(0.02);
        let mut lon = self.lon + unit(mix(h ^ 0x9e37_79b9_7f4a_7c15)) * SPREAD_DEG / shrink;
        // A site near the antimeridian must wrap, not clamp.
        if lon > 180.0 {
            lon -= 360.0;
        } else if lon < -180.0 {
            lon += 360.0;
        }
        (lat, lon)
    }
}

impl Group {
    /// Hostname element actually used, after falling back to the role's slug.
    /// The hostname element for this group's nodes.
    ///
    /// An explicit slug wins; the model sets it from the prospect's own word for
    /// the tier. Otherwise the group's distinctive software names it, and only
    /// then the role.
    ///
    /// Naming by role alone produced `ceph-web-01` for a Ceph storage node while
    /// `ceph-db-01` was the one running Postgres. That cost two sessions of
    /// looking in the wrong place, so a hostname now says what the node is.
    pub fn effective_slug(&self) -> &str {
        if let Some(s) = self.slug.as_deref().filter(|s| !s.is_empty()) {
            return s;
        }
        // A network device's service is its model, not software installed on it:
        // a Catalyst is still `sw-01` to whoever runs the network, and the hero
        // scenario targets that suffix.
        if self.role == "network-device" {
            return slug_for(&self.role);
        }
        match self.distinctive_service() {
            Some(svc) => svc,
            None => slug_for(&self.role),
        }
    }

    /// The one service that makes this group different from a bare node of its
    /// role, if there is exactly one.
    ///
    /// A web server running nginx is just a web server - `web` is the better
    /// name. A web-shaped node running Ceph is a Ceph node.
    fn distinctive_service(&self) -> Option<&str> {
        let defaults = default_services(&self.role);
        let mut extra = self
            .services
            .iter()
            .map(String::as_str)
            // `processes` is composed onto every node, so it never distinguishes
            // one group from another.
            .filter(|s| *s != "processes")
            .filter(|s| !defaults.contains(s));
        let first = extra.next()?;
        // Two or more and no single name is right; fall back to the role.
        extra.next().is_none().then_some(first)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub groups: Vec<Group>,
    /// Phrases that matched no known role or service.
    pub unrecognised: Vec<String>,
    /// The fleet's own location: the default every group inherits, and the place
    /// the simulation's own agent is labelled with.
    ///
    /// Held here rather than inferred from the groups. Inferring it picked the
    /// largest placed group, which on a tie between two groups labelled the
    /// simulation's agent with a *group override* instead of the fleet's own
    /// location - visible on a live agent as a parent node in the wrong city.
    pub site: Option<Site>,
    /// Fleet-wide user-authored host labels, inherited by every group and
    /// overridable per group. Validated upstream; see [`crate::labels`].
    pub labels: BTreeMap<String, String>,
}

impl Reading {
    /// Fold groups that would generate colliding hostnames.
    ///
    /// Hostnames are `{prefix}{slug}-{n:02}`, so two groups sharing a slug
    /// would emit the same hostname twice. Netdata keys a vnode on its GUID and
    /// the GUID is derived from the hostname, so a collision is not a cosmetic
    /// duplicate — it is two nodes claiming one identity, interleaving their
    /// samples into a single corrupted series.
    /// Make every group's hostname element unique.
    ///
    /// Two groups sharing an element emit the same hostnames, and since the GUID
    /// derives from the hostname that is two nodes claiming one identity.
    ///
    /// Groups that are genuinely the same thing are merged. Groups that merely
    /// collide - the same software on two different role shapes - are
    /// disambiguated instead, because merging them would give one of them the
    /// wrong hardware, mounts and scenario targets.
    pub fn dedupe_slugs(&mut self) {
        let mut folded: Vec<Group> = Vec::new();
        for group in std::mem::take(&mut self.groups) {
            match folded
                .iter_mut()
                .find(|g| g.role == group.role && g.services == group.services)
            {
                Some(existing) => {
                    existing.count += group.count;
                    existing.source = format!("{}; {}", existing.source, group.source);
                    // Same-role rows are legitimate console input; their labels
                    // merge rather than vanish (incoming wins same-key).
                    for (k, v) in group.labels {
                        existing.labels.insert(k, v);
                    }
                }
                None => folded.push(group),
            }
        }

        // Now break any remaining ties on the hostname element.
        let mut used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for group in folded.iter_mut() {
            let base = group.effective_slug().to_string();
            if used.insert(base.clone()) {
                continue;
            }
            // The role is the most informative qualifier; an index only if even
            // that is taken.
            let mut candidate = format!("{base}-{}", slug_for(&group.role));
            let mut n = 2;
            while !used.insert(candidate.clone()) {
                candidate = format!("{base}-{n}");
                n += 1;
            }
            group.slug = Some(candidate);
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
    RoleDef {
        role: "network-device",
        // Not a service: a switch is a different node class, and its spec is
        // the node's base rather than something composed onto Linux.
        services: &[],
        slug: "sw",
        summary: "switch, router or firewall polled over SNMP: ports rather \
                  than disks, and no operating system underneath",
        keywords: &[
            "network device",
            "network devices",
            "switch",
            "switches",
            "router",
            "routers",
            "firewall",
            "firewalls",
            "snmp device",
            "snmp devices",
            "access switch",
            "access switches",
            "top of rack",
            "leaf switch",
            "spine switch",
        ],
    },
];

/// Who lives on a node of this role: application groups, users and groups.
///
/// A node whose Processes tab shows `root` and `netdata` and nothing else is
/// obviously synthetic to anyone who opens it, and that tab is one of the first
/// an SRE looks at. Weights are relative load, so the workload dominates its own
/// node while the agents beside it barely register.
///
/// Returns `(apps, users, groups)`.
type Persona = (
    &'static [(&'static str, f64)],
    &'static [(&'static str, f64)],
    &'static [(&'static str, f64)],
);

fn persona(role: &str) -> Persona {
    // Everything an ordinary Linux host runs regardless of its job.
    const BASE_APPS: &[(&str, f64)] = &[
        ("systemd", 0.06),
        ("sshd", 0.03),
        ("netdata", 0.09),
        ("cron", 0.01),
        ("rsyslog", 0.02),
    ];
    match role {
        "db" => (
            &[
                ("postgres", 1.0),
                ("pgbouncer", 0.12),
                ("barman", 0.05),
                ("systemd", 0.06),
                ("sshd", 0.03),
                ("netdata", 0.09),
                ("cron", 0.01),
            ],
            &[
                ("postgres", 1.0),
                ("root", 0.12),
                ("netdata", 0.09),
                ("sshd", 0.02),
            ],
            &[("postgres", 1.0), ("root", 0.12), ("netdata", 0.09)],
        ),
        "web" => (
            &[
                ("nginx", 0.55),
                ("php-fpm", 0.8),
                ("node", 0.35),
                ("systemd", 0.06),
                ("sshd", 0.03),
                ("netdata", 0.09),
                ("filebeat", 0.07),
            ],
            &[
                ("www-data", 1.0),
                ("root", 0.14),
                ("netdata", 0.09),
                ("deploy", 0.04),
            ],
            &[("www-data", 1.0), ("root", 0.14), ("netdata", 0.09)],
        ),
        "lb" => (
            &[
                ("haproxy", 0.7),
                ("keepalived", 0.05),
                ("systemd", 0.06),
                ("sshd", 0.03),
                ("netdata", 0.09),
            ],
            &[("haproxy", 0.7), ("root", 0.16), ("netdata", 0.09)],
            &[("haproxy", 0.7), ("root", 0.16), ("netdata", 0.09)],
        ),
        "cache" => (
            &[
                ("redis", 0.8),
                ("systemd", 0.06),
                ("sshd", 0.03),
                ("netdata", 0.09),
            ],
            &[("redis", 0.8), ("root", 0.14), ("netdata", 0.09)],
            &[("redis", 0.8), ("root", 0.14), ("netdata", 0.09)],
        ),
        "k8s-control-plane" => (
            &[
                ("kube-apiserver", 1.0),
                ("etcd", 0.6),
                ("kube-scheduler", 0.2),
                ("kube-controller-manager", 0.3),
                ("containerd", 0.25),
                ("kubelet", 0.2),
                ("systemd", 0.06),
                ("netdata", 0.09),
            ],
            &[("root", 1.0), ("etcd", 0.6), ("netdata", 0.09)],
            &[("root", 1.0), ("etcd", 0.6), ("netdata", 0.09)],
        ),
        "k8s-worker" => (
            &[
                ("kubelet", 0.4),
                ("containerd", 0.6),
                ("kube-proxy", 0.1),
                ("app-api", 0.9),
                ("sidecar-proxy", 0.3),
                ("systemd", 0.06),
                ("netdata", 0.09),
            ],
            &[("root", 1.0), ("app", 0.9), ("netdata", 0.09)],
            &[("root", 1.0), ("app", 0.9), ("netdata", 0.09)],
        ),
        "edge-gateway" => (
            &[
                ("containerd", 0.35),
                ("app-agent", 0.45),
                ("systemd", 0.06),
                ("sshd", 0.02),
                ("netdata", 0.09),
            ],
            &[("root", 1.0), ("app", 0.45), ("netdata", 0.09)],
            &[("root", 1.0), ("app", 0.45), ("netdata", 0.09)],
        ),
        _ => (
            BASE_APPS,
            &[("root", 1.0), ("netdata", 0.09)],
            &[("root", 1.0), ("netdata", 0.09)],
        ),
    }
}

/// Base spec for a group, when it is not the fleet's Linux baseline.
///
/// Returning a path here is what makes a mixed fleet possible: the node carries
/// its own `generator:`.
///
/// A network device picks its own model. Selecting `cisco-catalyst` on a
/// network-device group uses the spec generated from Netdata's own Cisco
/// Catalyst SNMP profile, which reports per-supervisor CPU, chassis temperature,
/// power supplies and FRU state - none of which the generic switch spec has. The
/// device is expressed as the group's service so the integration picker needs no
/// new machinery.
fn base_spec(role: &str, services: &[String]) -> Option<String> {
    if role != "network-device" {
        return None;
    }
    // A device profile id is the one service a network device can carry.
    match services.iter().find(|s| *s != "processes") {
        Some(model) => Some(format!(
            "../specs/generated/snmp/{}.yaml",
            model.replace('-', "_")
        )),
        None => Some("../specs/network-device.yaml".to_string()),
    }
}

/// One entry in a device's instance list: name, weight, and a port speed where
/// the instance is a port.
type DeviceInstance = (String, f64, Option<u32>);

/// Instance lists a simulated network device carries.
///
/// A vendor profile tables many things; a device only charts the groups the
/// environment gives it, because a context whose instance group is absent is
/// skipped. So this populates the hardware a switch really has - ports, CPUs,
/// memory pools, sensors, fans, power supplies - and leaves the topology tables
/// (OSPF neighbours, CDP peers, MAC tables) empty. Those describe a network this
/// simulation does not have, and inventing them would be inventing peers.
fn device_instances() -> Vec<(&'static str, Vec<DeviceInstance>)> {
    let ports: Vec<(String, f64, Option<u32>)> = ports()
        .into_iter()
        .map(|(n, w, speed)| (n, w, Some(speed)))
        .collect();
    vec![
        ("interface", ports),
        (
            "cpu_index",
            vec![("1".into(), 1.0, None), ("2".into(), 0.85, None)],
        ),
        (
            "mem_pool_index",
            vec![("Processor".into(), 1.0, None), ("IO".into(), 0.4, None)],
        ),
        (
            "temp_index",
            vec![
                ("Inlet".into(), 1.0, None),
                ("Outlet".into(), 1.12, None),
                ("Supervisor".into(), 1.2, None),
            ],
        ),
        (
            "sensor_index",
            vec![("Inlet".into(), 1.0, None), ("Hotspot".into(), 1.15, None)],
        ),
        (
            "power_supply_index",
            vec![("PSU1".into(), 1.0, None), ("PSU2".into(), 1.0, None)],
        ),
        (
            "fan_status_index",
            vec![
                ("Fan1".into(), 1.0, None),
                ("Fan2".into(), 1.0, None),
                ("Fan3".into(), 1.0, None),
                ("Fan4".into(), 1.0, None),
            ],
        ),
        (
            "fru_index",
            vec![("1".into(), 1.0, None), ("2".into(), 1.0, None)],
        ),
        (
            "storage_index",
            vec![("Flash".into(), 1.0, None), ("NVRAM".into(), 0.2, None)],
        ),
        (
            "voltage_index",
            vec![("12V".into(), 1.0, None), ("3V3".into(), 0.275, None)],
        ),
        ("chassis_switch_id", vec![("1".into(), 1.0, None)]),
    ]
}

/// Ports on a simulated device, by index. A 24-port access switch with two
/// uplinks is the commonest shape in a prospect's wiring closet; the uplinks
/// carry an order of magnitude more traffic, which the weight expresses.
fn ports() -> Vec<(String, f64, u32)> {
    let mut out: Vec<(String, f64, u32)> = (1..=24)
        .map(|i| (format!("GigabitEthernet1/0/{i}"), 0.35, 1000))
        .collect();
    out.push(("TenGigabitEthernet1/1/1".into(), 4.0, 10000));
    out.push(("TenGigabitEthernet1/1/2".into(), 3.2, 10000));
    out
}

/// A role's default services, for deciding what makes a group distinctive.
fn default_services(role: &str) -> &'static [&'static str] {
    ROLES
        .iter()
        .find(|d| d.role == role)
        .map(|d| d.services)
        .unwrap_or(&[])
}

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
/// Scale a reading to roughly `target` nodes, preserving its shape.
///
/// An SE often wants "this prospect's stack, but at the size I'm demoing to" -
/// the ratio between tiers is the part worth keeping, not the absolute counts a
/// description happened to mention.
///
/// Largest-remainder allocation, so the total lands exactly on the target
/// rather than drifting by a node or two, and every group keeps at least one
/// node - a tier scaled out of existence is a tier the prospect will ask about.
/// Returns a note describing what it did, or None if nothing changed.
pub fn scale_to_target(reading: &mut Reading, target: usize) -> Option<String> {
    let before: usize = reading.groups.iter().map(|g| g.count).sum();
    if target == 0 || before == 0 || target == before {
        return None;
    }
    // A target below one-node-per-group cannot be honoured without deleting a
    // tier, which is a worse outcome than overshooting.
    let floor = reading.groups.len();
    let target = target.max(floor);

    let scale = target as f64 / before as f64;
    let mut remainders: Vec<(usize, f64)> = Vec::with_capacity(reading.groups.len());
    let mut allocated = 0usize;
    for (i, g) in reading.groups.iter_mut().enumerate() {
        let exact = g.count as f64 * scale;
        let whole = (exact.floor() as usize).max(1);
        g.count = whole;
        allocated += whole;
        remainders.push((i, exact - exact.floor()));
    }
    // Hand out what rounding left over, largest fractional part first.
    remainders.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut idx = 0;
    while allocated < target && !remainders.is_empty() {
        reading.groups[remainders[idx % remainders.len()].0].count += 1;
        allocated += 1;
        idx += 1;
    }
    // Overshoot can only come from the one-node floor; take it back from the
    // largest groups, never below one.
    while allocated > target {
        let Some(g) = reading
            .groups
            .iter_mut()
            .filter(|g| g.count > 1)
            .max_by_key(|g| g.count)
        else {
            break;
        };
        g.count -= 1;
        allocated -= 1;
    }

    Some(format!(
        "scaled from {before} to {allocated} node(s) to match the requested fleet size,          keeping the ratio between tiers"
    ))
}

pub fn parse(text: &str) -> Reading {
    parse_with_services(text, &[])
}

/// Read a description, resolving named integrations against `available`.
///
/// Without a catalogue the reader can only assign each role its default
/// collectors, so "two haproxy load balancers" becomes two `lb` nodes running
/// nginx - the right shape and the wrong software, which is exactly the kind of
/// detail a prospect notices. Given the catalogue, an integration named in the
/// text wins over the role's default.
pub fn parse_with_services(text: &str, available: &[String]) -> Reading {
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
        match match_clause(clause, available) {
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

/// Fold a clause's group into the reading.
///
/// Two clauses merge only when they describe the same role running the same
/// software. Merging on role alone silently discarded the second clause's
/// collectors: "6 nginx web servers ... and an elasticsearch cluster of 3"
/// became nine nginx nodes and no Elasticsearch anywhere. Labels merge with
/// the incoming clause winning same-key - silently dropping them would lose
/// authored state the SE can see in the form.
fn merge(groups: &mut Vec<Group>, incoming: Group) {
    if let Some(existing) = groups
        .iter_mut()
        .find(|g| g.role == incoming.role && g.services == incoming.services)
    {
        existing.count += incoming.count;
        existing.source = format!("{}; {}", existing.source, incoming.source);
        for (k, v) in incoming.labels {
            existing.labels.insert(k, v);
        }
    } else {
        groups.push(incoming);
    }
}

/// Integrations named outright in a clause.
///
/// Word-boundary matching with a minimum length, because short ids like `ping`
/// and `dns` appear inside ordinary English and would attach collectors nobody
/// asked for.
fn named_services(clause: &str, available: &[String]) -> Vec<String> {
    const MIN_LEN: usize = 4;
    let words: Vec<&str> = clause
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    let mut hits: Vec<String> = available
        .iter()
        .filter(|id| id.len() >= MIN_LEN)
        .filter(|id| {
            // Either the whole id appears as a word, or a hyphenated id appears
            // as a run of words ("otel collector" for `otel-collector`).
            words.contains(&id.as_str())
                || (id.contains('-') && clause.contains(&id.replace('-', " ")))
        })
        .cloned()
        .collect();
    hits.sort();
    hits.dedup();
    hits
}

fn match_clause(clause: &str, available: &[String]) -> Option<Group> {
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
    let named = named_services(clause, available);
    let (def, _) = match best {
        Some(b) => b,
        // "3 kafka brokers" names software but no role word. Software still
        // implies nodes, so fall back to the generic application role rather
        // than discarding the clause - `web` is this catalogue's general-purpose
        // node, not specifically an HTTP server.
        None if !named.is_empty() => (ROLES.iter().find(|d| d.role == "web")?, 0),
        None => return None,
    };

    Some(Group {
        count: extract_count(clause),
        role: def.role.to_string(),
        services: if named.is_empty() {
            def.services.iter().map(|s| s.to_string()).collect()
        } else {
            named
        },
        // Keyword matching has no evidence for a better name than the role's.
        slug: None,
        labels: BTreeMap::new(),
        source: clause.to_string(),
        site: None,
        device: None,
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

/// Base value of the shared disk-usage driver, in KB.
///
/// A mount's weight scales this into that mount, so weight and target
/// utilisation are two views of the same number.
const DISK_DRIVER_BASE_KB: f64 = 180_000_000.0;

/// Steady-state utilisation for a mount nothing is expected to fill.
const DEFAULT_MOUNT_TARGET: f64 = 0.38;

/// Steady-state utilisation for a mount a hero scenario fills.
///
/// `disk-fill` ramps `disk_space_used_kb` by 8.2x. Starting at the default 38%
/// that reaches 311%, which the engine clamps to the mount's total — so the
/// chart pins at 100% full, the ramp flattens, and `out_of_disk_space_time` has
/// no trend left to project from. The whole scenario degrades into a flat line.
/// Starting at 11% lands near 93%, which is what the hand-authored templates
/// use and what the live validation observed.
const FILLABLE_MOUNT_TARGET: f64 = 0.11;

/// Weight that scales the shared driver to `target` utilisation of `total_kb`.
fn mount_weight(total_kb: u64, target: f64) -> f64 {
    (total_kb as f64 * target / DISK_DRIVER_BASE_KB * 10_000.0).round() / 10_000.0
}

/// Mounts beyond `/` that a role should have: path, total KB, and the
/// utilisation it should sit at when nothing is wrong.
///
/// Sized so each conserves its own total rather than clamping, and so the
/// mounts the hero scenarios target keep enough headroom to actually fill.
fn extra_mounts(role: &str) -> &'static [(&'static str, u64, f64)] {
    match role {
        // disk-fill targets /var/lib/pgsql by name, so it starts low.
        "db" => &[
            ("/var/lib/pgsql", 1_572_864_000, FILLABLE_MOUNT_TARGET),
            ("/var/log", 52_428_800, DEFAULT_MOUNT_TARGET),
        ],
        "k8s-control-plane" | "k8s-worker" => {
            &[("/var/lib/containerd", 209_715_200, FILLABLE_MOUNT_TARGET)]
        }
        "edge-gateway" => &[("/var/log", 4_194_304, DEFAULT_MOUNT_TARGET)],
        _ => &[("/boot", 1_048_576, DEFAULT_MOUNT_TARGET)],
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
        "# Minor, auto-resolving incidents on a deterministic schedule, so the".into(),
        "# alert log and anomaly history have texture before a live session.".into(),
        "warmup_incidents: true".into(),
        "generator: ../specs/linux-system.yaml".into(),
        "specs: ../specs".into(),
        "scenarios: ../scenarios".into(),
    ];

    // The fleet's own site, recorded once so the simulation's agent can be given
    // the same coordinates as the nodes it serves. Per-node coordinates are
    // written on each node below; this is the fleet's home, taken from the
    // largest placed group.
    if let Some(site) = fleet_site(reading) {
        lines.push(String::new());
        lines.push("# Where this fleet is. The container's own agent is labelled".into());
        lines.push("# from here, so no node in the Space is left unplaced.".into());
        lines.push("site:".into());
        lines.push(format!("  latitude: {:.6}", site.lat));
        lines.push(format!("  longitude: {:.6}", site.lon));
    }

    lines.extend([
        String::new(),
        "# GUIDs are derived from the environment name and hostname, so".into(),
        "# regenerating this file reproduces the same node identities rather".into(),
        "# than orphaning the history of a fleet that is already running.".into(),
        "nodes:".into(),
    ]);

    for group in &reading.groups {
        let (cores, ram_kb, disk, itype) = hardware(&group.role);
        let slug = group.effective_slug();
        for i in 1..=group.count {
            let hostname = format!("{prefix}{slug}-{i:02}");
            let guid = derive_guid(name, &hostname);
            let root_kb: u64 = 104_857_600;
            let root_weight = mount_weight(root_kb, DEFAULT_MOUNT_TARGET);
            // Every Linux node reports its applications, users and groups. It
            // is a property of being a host, not a service someone chose.
            let mut svc = group.services.clone();
            if !svc.iter().any(|s| s == "processes") {
                svc.push("processes".into());
            }
            let group = &Group {
                services: svc,
                ..group.clone()
            };
            let services = if group.services.is_empty() {
                "[]".to_string()
            } else {
                format!("[{}]", group.services.join(", "))
            };

            lines.push(String::new());
            lines.push(format!("  - hostname: {hostname}"));
            lines.push(format!("    guid: {guid}"));
            lines.push(format!("    role: {}", group.role));

            // A network device has no operating system, no disks and no mounts.
            // Emitting the Linux shape here and letting the charts fall away
            // would still leave the node advertising cores and RAM it does not
            // have, in labels an SRE reads first.
            if let Some(base) = base_spec(&group.role, &group.services) {
                lines.push(format!("    generator: {base}"));
                lines.push("    services: []".into());
                lines.push("    utc_offset_secs: 0".into());
                lines.push("    attrs: {}".into());
                lines.push("    instances:".into());
                for (group_name, entries) in device_instances() {
                    lines.push(format!("      {group_name}:"));
                    for (name, weight, speed) in entries {
                        // Quoted: an SNMP index is a name that happens to look
                        // like a number, and `name: 1` parses as an integer.
                        match speed {
                            Some(mbps) => lines.push(format!(
                                "        - {{ name: \"{name}\", weight: {weight}, \
                                 attrs: {{ if_speed_mbps: {mbps} }} }}"
                            )),
                            None => lines.push(format!(
                                "        - {{ name: \"{name}\", weight: {weight} }}"
                            )),
                        }
                    }
                }
                lines.push("    labels:".into());
                lines.push("      _install_type: infra-sim".into());
                match &group.device {
                    Some(d) => {
                        lines.push(format!("      device_vendor: {}", d.vendor));
                        lines.push(format!("      device_model: {}", d.model));
                        lines.push(format!("      device_type: {}", d.kind));
                    }
                    // No model chosen: a generic managed switch, and visibly
                    // synthetic rather than borrowing a real vendor's name.
                    None => {
                        lines.push("      device_vendor: sim-networks".into());
                        lines.push("      device_model: SIM-2960X-24".into());
                        lines.push("      device_type: switch".into());
                    }
                }
                lines.push(format!("      infra_sim_role: {}", group.role));
                lines.push(format!(
                    "      infra_sim_env: {}",
                    crate::labels::yaml_scalar(&env_tier(&reading.labels, &group.labels))
                ));
                lines.extend(user_label_lines(&reading.labels, &group.labels));
                lines.extend(site_labels(group.site, &hostname));
                continue;
            }

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
            for &(path, total, target) in extra_mounts(&group.role) {
                let w = mount_weight(total, target);
                lines.push(format!(
                    "        - {{ name: \"{path}\", weight: {w}, attrs: {{ disk_total_kb: {total}, inodes_total: 6553600 }} }}"
                ));
            }
            let (apps, users, groups) = persona(&group.role);
            for (label, entries) in [("app", apps), ("user", users), ("usergroup", groups)] {
                lines.push(format!("      {label}:"));
                for (name, weight) in entries {
                    lines.push(format!("        - {{ name: {name}, weight: {weight} }}"));
                }
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
            for service in &group.services {
                if let Some((inv_group, members)) = profile_instances(service, &hostname) {
                    lines.push(format!("      {inv_group}:"));
                    for (n, w) in members {
                        lines.push(format!("        - {{ name: \"{n}\", weight: {w} }}"));
                    }
                }
            }
            if matches!(group.role.as_str(), "k8s-control-plane" | "k8s-worker") {
                lines.push("      container:".into());
                for c in ["app-api", "sidecar-proxy", "log-shipper"] {
                    lines.push(format!("        - {{ name: {c}, weight: 1.0 }}"));
                }
            }
            lines.push("    labels:".into());
            let (os_name, os_version, kernel) =
                OPERATING_SYSTEMS[pick(&hostname, 1, OPERATING_SYSTEMS.len())];
            lines.push(format!("      _os_name: {os_name}"));
            lines.push(format!("      _os_version: {os_version}"));
            lines.push(format!("      _kernel_version: {kernel}"));
            lines.push("      _architecture: x86_64".into());
            lines.push(format!("      _system_cores: \"{cores}\""));
            lines.push(format!("      _system_ram_total: \"{ram_kb}\""));
            lines.push("      _virtualization: kvm".into());
            lines.push("      _virt_detection: systemd-detect-virt".into());
            lines.push("      _container: none".into());
            lines.push("      _container_detection: none".into());
            lines.push("      _cloud_provider_type: aws".into());
            lines.push(format!("      _cloud_instance_type: {itype}"));
            lines.push(format!(
                "      _cloud_instance_region: {}",
                REGIONS[pick(&hostname, 2, REGIONS.len())]
            ));
            lines.push("      _install_type: infra-sim".into());
            lines.push(format!("      infra_sim_role: {}", group.role));
            lines.push(format!(
                "      infra_sim_env: {}",
                crate::labels::yaml_scalar(&env_tier(&reading.labels, &group.labels))
            ));
            lines.extend(user_label_lines(&reading.labels, &group.labels));
            lines.extend(site_labels(group.site, &hostname));
        }
    }

    lines.join("\n") + "\n"
}

/// A stable per-node index into a table.
///
/// Keyed on the hostname so a node keeps its operating system across
/// regenerations, for the same reason GUIDs are derived rather than random: a
/// fleet whose nodes swap distro every time the description is re-read is not a
/// fleet anyone can demo twice.
fn pick(hostname: &str, salt: u64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ salt;
    for b in hostname.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h % len as u64) as usize
}

/// Operating systems a fleet actually runs, as `(name, version, kernel)`.
///
/// A hundred nodes reporting a byte-identical `_os_version` is one of the
/// cheapest tells there is - the node list gives it away before any chart does.
/// Real estates are mostly one distro with a minority of others and a spread of
/// kernel revisions, because nodes are patched in waves rather than at once.
/// Weighted by repetition: Ubuntu LTS dominates, the rest trail.
const OPERATING_SYSTEMS: &[(&str, &str, &str)] = &[
    ("Ubuntu", "24.04.1 LTS", "6.8.0-51-generic"),
    ("Ubuntu", "24.04.1 LTS", "6.8.0-48-generic"),
    ("Ubuntu", "24.04.2 LTS", "6.8.0-53-generic"),
    ("Ubuntu", "22.04.5 LTS", "5.15.0-126-generic"),
    ("Ubuntu", "22.04.4 LTS", "5.15.0-119-generic"),
    ("Debian GNU/Linux", "12", "6.1.0-28-amd64"),
    ("Debian GNU/Linux", "12", "6.1.0-26-amd64"),
    ("Rocky Linux", "9.4", "5.14.0-427.el9.x86_64"),
    ("Rocky Linux", "9.5", "5.14.0-503.el9.x86_64"),
    ("Amazon Linux", "2023", "6.1.112-122.189.amzn2023.x86_64"),
];

/// Regions a fleet is spread across. A single-region estate is plausible; every
/// node in `us-east-1` with nodes visibly placed on three continents is not.
const REGIONS: &[&str] = &[
    "us-east-1",
    "us-east-1",
    "us-west-2",
    "eu-west-1",
    "eu-central-1",
    "ap-southeast-1",
];

/// Synthetic resource inventories for a profile-derived service.
///
/// A profile-based collector emits one chart instance per AWS or Azure resource,
/// keyed by labels rather than by node, so the environment has to say which
/// resources exist. Without this a node carrying `aws-ec2` declares the context
/// and renders nothing, which is worse than not claiming EC2 at all.
///
/// Names are obviously synthetic on purpose: `i-0sim...`, `sim-*`. A committed
/// environment must never look like it names someone's real estate.
fn profile_instances(service: &str, hostname: &str) -> Option<(String, Vec<(String, f64)>)> {
    let (prefix, rest) = service.split_once('-')?;
    if !matches!(prefix, "aws" | "azure" | "prom") {
        return None;
    }
    // `aws-api-gateway` -> `aws_api_gateway`, matching the instance group
    // `scripts/sync-profile-collectors.py` writes into the generated spec.
    let group = format!("{prefix}_{}", rest.replace('-', "_"));

    // Two or three resources per service: enough that the charts are visibly
    // per-resource, few enough that ten services on one node stay readable.
    let names: Vec<String> = match rest {
        "ec2" => {
            let a = pick(hostname, 11, 0x1000);
            vec![
                format!("i-0sim{:04x}a1b2c3d4e5", a),
                format!("i-0sim{:04x}7a8b9c0d1e", a + 1),
                format!("i-0sim{:04x}4d5e6f7081", a + 2),
            ]
        }
        "lambda" => ["order-intake", "image-resize", "webhook-fanout"]
            .iter()
            .map(|n| format!("sim-{n}"))
            .collect(),
        "ecs" => ["fargate-checkout", "fargate-search"]
            .iter()
            .map(|n| format!("sim-{n}"))
            .collect(),
        "rds" => ["orders-primary", "orders-replica"]
            .iter()
            .map(|n| format!("sim-{n}"))
            .collect(),
        "s3" => ["media-assets", "audit-logs"]
            .iter()
            .map(|n| format!("sim-{n}"))
            .collect(),
        "sqs" => ["order-events", "dlq-orders"]
            .iter()
            .map(|n| format!("sim-{n}"))
            .collect(),
        "dynamodb" => ["carts", "idempotency"]
            .iter()
            .map(|n| format!("sim-{n}"))
            .collect(),
        "elasticache" => vec!["sim-session-cache".to_string()],
        "alb" | "nlb" | "elb" => vec![format!("sim-public-{rest}")],
        "api-gateway" => vec!["sim-public-api".to_string()],
        // Any other profile - and there are 85 of them - still gets resources, so
        // a spec is never declared with nothing behind it.
        other => {
            let base = other.replace('_', "-");
            vec![format!("sim-{base}-01"), format!("sim-{base}-02")]
        }
    };

    let weights = [1.0, 0.6, 0.35];
    Some((
        group,
        names
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n, weights[i.min(weights.len() - 1)]))
            .collect(),
    ))
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
    #[test]
    fn scaling_hits_the_target_exactly_and_keeps_every_tier() {
        let mk = |counts: &[usize]| super::Reading {
            groups: counts
                .iter()
                .enumerate()
                .map(|(i, c)| super::Group {
                    count: *c,
                    role: format!("role{i}"),
                    services: vec![],
                    slug: Some(format!("g{i}")),
                    source: String::new(),
                    site: None,
                    device: None,
                    labels: Default::default(),
                })
                .collect(),
            unrecognised: vec![],
            site: None,
            labels: Default::default(),
        };
        for (from, target) in [
            (vec![2, 20, 3, 1], 50usize),
            (vec![2, 20, 3, 1], 10),
            (vec![1, 1], 7),
            (vec![5], 1),
            (vec![3, 3, 3], 100),
        ] {
            let mut r = mk(&from);
            super::scale_to_target(&mut r, target);
            let total: usize = r.groups.iter().map(|g| g.count).sum();
            // A target below one-per-group cannot be honoured without deleting
            // a tier; the floor is the group count.
            let expected = target.max(from.len());
            assert_eq!(total, expected, "from {from:?} to {target}: got {total}");
            assert!(
                r.groups.iter().all(|g| g.count >= 1),
                "a tier was scaled out of existence: {from:?} -> {target}"
            );
        }
    }

    #[test]
    fn scaling_preserves_the_shape() {
        let mut r = super::Reading {
            groups: vec![
                super::Group {
                    count: 2,
                    role: "lb".into(),
                    services: vec![],
                    slug: Some("lb".into()),
                    source: String::new(),
                    site: None,
                    device: None,
                    labels: Default::default(),
                },
                super::Group {
                    count: 20,
                    role: "web".into(),
                    services: vec![],
                    slug: Some("web".into()),
                    source: String::new(),
                    site: None,
                    device: None,
                    labels: Default::default(),
                },
            ],
            unrecognised: vec![],
            site: None,
            labels: Default::default(),
        };
        super::scale_to_target(&mut r, 55);
        // 2:20 is 1:10; at 55 nodes that is 5 and 50.
        assert_eq!(r.groups[0].count, 5);
        assert_eq!(r.groups[1].count, 50);
    }

    #[test]
    fn scaling_to_the_same_size_is_a_no_op() {
        let mut r = super::Reading {
            groups: vec![super::Group {
                count: 4,
                role: "web".into(),
                services: vec![],
                slug: None,
                source: String::new(),
                site: None,
                device: None,
                labels: Default::default(),
            }],
            unrecognised: vec![],
            site: None,
            labels: Default::default(),
        };
        assert!(super::scale_to_target(&mut r, 4).is_none());
        assert!(super::scale_to_target(&mut r, 0).is_none());
        assert_eq!(r.groups[0].count, 4);
    }

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
    fn a_node_is_named_after_its_software_not_its_role_shape() {
        // The failure this guards: a Ceph cluster whose nodes were called
        // `<name>-web-01` while `<name>-db-01` was the one running Postgres.
        let g = |role: &str, services: &[&str]| super::Group {
            count: 1,
            role: role.into(),
            services: services.iter().map(|s| s.to_string()).collect(),
            slug: None,
            source: String::new(),
            site: None,
            device: None,
            labels: Default::default(),
        };
        // Distinctive software names the node.
        assert_eq!(g("web", &["ceph"]).effective_slug(), "ceph");
        assert_eq!(g("web", &["ceph", "processes"]).effective_slug(), "ceph");
        // A web server running nginx is just a web server.
        assert_eq!(g("web", &["nginx"]).effective_slug(), "web");
        assert_eq!(g("db", &["postgres"]).effective_slug(), "db");
        // Two extras and no single name is right.
        assert_eq!(g("web", &["ceph", "redis"]).effective_slug(), "web");
        // An explicit slug always wins - the model sets it from the prospect's
        // own word for the tier.
        let mut explicit = g("web", &["ceph"]);
        explicit.slug = Some("storage".into());
        assert_eq!(explicit.effective_slug(), "storage");
    }

    #[test]
    fn colliding_hostname_elements_are_disambiguated_not_merged() {
        // Merging them would give one group the other's hardware, mounts and
        // scenario targets. Two nodes sharing a hostname share a GUID, which is
        // two nodes claiming one identity - so they cannot simply collide either.
        let mut r = super::Reading {
            groups: vec![
                super::Group {
                    count: 6,
                    role: "web".into(),
                    services: vec!["nginx".into()],
                    slug: None,
                    source: String::new(),
                    site: None,
                    device: None,
                    labels: Default::default(),
                },
                super::Group {
                    count: 2,
                    role: "lb".into(),
                    services: vec!["nginx".into()],
                    slug: None,
                    source: String::new(),
                    site: None,
                    device: None,
                    labels: Default::default(),
                },
                super::Group {
                    count: 3,
                    role: "web".into(),
                    services: vec!["nginx".into()],
                    slug: None,
                    source: String::new(),
                    site: None,
                    device: None,
                    labels: Default::default(),
                },
            ],
            unrecognised: vec![],
            site: None,
            labels: Default::default(),
        };
        r.dedupe_slugs();
        // The two identical web groups merged; the lb group did not.
        assert_eq!(r.groups.len(), 2, "{:?}", r.groups);
        assert_eq!(r.groups[0].count, 9);
        assert_eq!(r.groups[1].count, 2);
        let slugs: Vec<&str> = r.groups.iter().map(|g| g.effective_slug()).collect();
        assert_eq!(
            slugs.len(),
            slugs
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "hostname elements must be unique: {slugs:?}"
        );
    }

    #[test]
    fn renders_one_node_per_count() {
        let r = parse("2 web servers and a database");
        let yaml = render(&r, "acme", 42, "acme-");
        assert_eq!(yaml.matches("- hostname:").count(), 3);
        assert!(yaml.contains("hostname: acme-web-01"));
        assert!(yaml.contains("hostname: acme-web-02"));
        assert!(yaml.contains("hostname: acme-db-01"));
        // `processes` is composed onto every Linux node, so a db node lists it
        // alongside its own service rather than postgres alone.
        assert!(yaml.contains("services: [postgres, processes]"), "{yaml}");
    }

    #[test]
    fn user_labels_land_on_every_node_with_group_overrides() {
        let mut r = parse("2 web servers and a database");
        r.labels = BTreeMap::from([
            ("environment".to_string(), "staging".to_string()),
            ("team".to_string(), "platform".to_string()),
        ]);
        // Same key on the group wins over the fleet's - a db tier owned by
        // someone else is exactly why per-group labels exist.
        r.groups[1].labels = BTreeMap::from([("team".to_string(), "payments".to_string())]);
        let yaml = render(&r, "acme", 7, "acme-");
        // Fleet labels reach every node.
        assert_eq!(
            yaml.matches("      environment: staging").count(),
            3,
            "{yaml}"
        );
        // The web pair keeps the fleet team; the db node carries its override.
        assert_eq!(yaml.matches("      team: platform").count(), 2, "{yaml}");
        assert_eq!(yaml.matches("      team: payments").count(), 1, "{yaml}");
    }

    #[test]
    fn the_environment_label_tiers_infra_sim_env() {
        let mut r = parse("a database");
        let yaml = render(&r, "acme", 7, "acme-");
        assert!(
            yaml.contains("infra_sim_env: production"),
            "no user label means production: {yaml}"
        );

        r.labels = BTreeMap::from([("environment".to_string(), "staging".to_string())]);
        let yaml = render(&r, "acme", 7, "acme-");
        assert!(
            yaml.contains("infra_sim_env: staging"),
            "the user's own word tiers the fleet: {yaml}"
        );
        // The user's label itself is emitted too - it is theirs, not ours.
        assert!(yaml.contains("environment: staging"), "{yaml}");

        // A group's environment beats the fleet's.
        r.groups[0].labels = BTreeMap::from([("environment".to_string(), "dr".to_string())]);
        let yaml = render(&r, "acme", 7, "acme-");
        assert!(yaml.contains("infra_sim_env: dr"), "{yaml}");
    }

    #[test]
    fn user_labels_reach_network_devices_between_tier_and_site() {
        let mut r = parse("a switch");
        r.labels = BTreeMap::from([("site".to_string(), "dc-east-1".to_string())]);
        let yaml = render(&r, "acme", 7, "acme-");
        assert!(yaml.contains("device_type: switch"), "{yaml}");
        assert!(yaml.contains("      site: dc-east-1"), "{yaml}");
        // Authored labels sit after the generated ones and before placement.
        let tier = yaml.find("infra_sim_env").expect("tier label");
        let authored = yaml.find("site: dc-east-1").expect("authored label");
        assert!(tier < authored, "generated before authored: {yaml}");
    }

    #[test]
    fn numeric_label_values_stay_strings() {
        // `environment: 42` would otherwise deserialize as an integer and
        // break every consumer expecting a string.
        #[derive(serde::Deserialize)]
        struct EnvLike {
            nodes: Vec<NodeLike>,
        }
        #[derive(serde::Deserialize)]
        struct NodeLike {
            labels: BTreeMap<String, String>,
        }
        let mut r = parse("a database");
        r.labels = BTreeMap::from([("site".to_string(), "42".to_string())]);
        let yaml = render(&r, "acme", 7, "acme-");
        assert!(yaml.contains("site: '42'"), "{yaml}");
        let back: EnvLike = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(
            back.nodes[0].labels.get("site").map(String::as_str),
            Some("42")
        );
    }

    #[test]
    fn merging_same_shape_groups_keeps_both_label_sets() {
        // Two rows of the same role and services merge (hostnames would
        // collide); their labels must union rather than the second's vanish.
        let mut r = parse("a database");
        r.groups.push(Group {
            count: 1,
            role: "db".into(),
            services: vec!["postgres".into()],
            slug: None,
            source: String::new(),
            site: None,
            device: None,
            labels: BTreeMap::from([
                ("team".to_string(), "payments".to_string()),
                ("tier".to_string(), "hot".to_string()),
            ]),
        });
        r.dedupe_slugs();
        assert_eq!(r.groups.len(), 1, "same role+services merge");
        let merged = &r.groups[0].labels;
        assert_eq!(merged.get("team").map(String::as_str), Some("payments"));
        assert_eq!(merged.get("tier").map(String::as_str), Some("hot"));
    }

    #[test]
    fn an_empty_description_produces_no_groups() {
        assert!(parse("").groups.is_empty());
        assert!(parse("   ").groups.is_empty());
    }

    #[test]
    fn a_placed_fleet_labels_every_node_and_scatters_them() {
        let site = Site::new(50.1109, 8.6821).expect("Frankfurt is on Earth");
        let reading = Reading {
            groups: vec![
                Group {
                    count: 3,
                    role: "web".into(),
                    services: vec!["nginx".into()],
                    slug: None,
                    source: String::new(),
                    site: Some(site),
                    device: None,
                    labels: Default::default(),
                },
                Group {
                    count: 2,
                    role: "network-device".into(),
                    services: Vec::new(),
                    slug: None,
                    source: String::new(),
                    site: Some(Site::new(52.3676, 4.9041).unwrap()),
                    device: None,
                    labels: Default::default(),
                },
            ],
            unrecognised: Vec::new(),
            site: None,
            labels: Default::default(),
        };
        let yaml = render(&reading, "acme", 7, "acme-");
        // Every node, including the switches: a device is at a site too.
        assert_eq!(yaml.matches("      latitude:").count(), 5, "{yaml}");
        assert_eq!(yaml.matches("      longitude:").count(), 5, "{yaml}");
        // With no fleet-level location, the largest placed group stands in.
        assert!(yaml.contains("site:\n  latitude: 50.110900"), "{yaml}");

        // Both axes have to move, and stay inside roughly 500m. Deriving them
        // from two windows of one FNV hash gave every node the same longitude,
        // because adjacent hostnames barely move its upper bits.
        let lats = coords(&yaml, "      latitude: ");
        let lons = coords(&yaml, "      longitude: ");
        let web_lats = &lats[..3];
        let web_lons = &lons[..3];
        assert!(
            spread(web_lats) > 0.0005,
            "latitudes barely moved: {web_lats:?}"
        );
        assert!(
            spread(web_lons) > 0.0005,
            "longitudes barely moved: {web_lons:?}"
        );
        for (lat, lon) in web_lats.iter().zip(web_lons) {
            assert!(
                (lat - 50.1109).abs() < 0.005,
                "{lat} is too far from the site"
            );
            assert!(
                (lon - 8.6821).abs() < 0.008,
                "{lon} is too far from the site"
            );
        }
    }

    #[test]
    fn the_fleets_own_location_beats_a_group_override() {
        // Both groups hold two nodes, so inferring the fleet's site from the
        // largest group is a coin toss - and it landed on the switches' city,
        // which put the simulation's own agent in Amsterdam on a live agent while
        // the operator had typed Frankfurt.
        let reading = Reading {
            groups: vec![
                Group {
                    count: 2,
                    role: "web".into(),
                    services: Vec::new(),
                    slug: None,
                    source: String::new(),
                    site: Some(Site::new(50.1109, 8.6821).unwrap()),
                    device: None,
                    labels: Default::default(),
                },
                Group {
                    count: 2,
                    role: "network-device".into(),
                    services: Vec::new(),
                    slug: None,
                    source: String::new(),
                    site: Some(Site::new(52.3676, 4.9041).unwrap()),
                    device: None,
                    labels: Default::default(),
                },
            ],
            unrecognised: Vec::new(),
            site: Some(Site::new(50.1109, 8.6821).unwrap()),
            labels: Default::default(),
        };
        let yaml = render(&reading, "acme", 7, "acme-");
        assert!(yaml.contains("site:\n  latitude: 50.110900"), "{yaml}");
    }

    #[test]
    fn a_chosen_model_labels_the_node_as_that_device() {
        // A node loading the Cisco Catalyst profile must not also claim to be a
        // switch model this project invented.
        let reading = Reading {
            groups: vec![Group {
                count: 1,
                role: "network-device".into(),
                services: vec!["cisco_catalyst".into()],
                slug: None,
                source: String::new(),
                site: None,
                device: Some(DeviceIdentity {
                    vendor: "Cisco".into(),
                    model: "cisco_catalyst".into(),
                    kind: "switch".into(),
                }),
                labels: Default::default(),
            }],
            unrecognised: Vec::new(),
            site: None,
            labels: Default::default(),
        };
        let yaml = render(&reading, "acme", 7, "acme-");
        assert!(yaml.contains("device_vendor: Cisco"), "{yaml}");
        assert!(yaml.contains("device_model: cisco_catalyst"), "{yaml}");
        assert!(!yaml.contains("sim-networks"), "{yaml}");
    }

    #[test]
    fn a_generic_switch_keeps_visibly_synthetic_labels() {
        let r = Reading {
            groups: vec![Group {
                count: 1,
                role: "network-device".into(),
                services: Vec::new(),
                slug: None,
                source: String::new(),
                site: None,
                device: None,
                labels: Default::default(),
            }],
            unrecognised: Vec::new(),
            site: None,
            labels: Default::default(),
        };
        let yaml = render(&r, "acme", 7, "acme-");
        assert!(yaml.contains("device_vendor: sim-networks"), "{yaml}");
    }

    #[test]
    fn an_unplaced_fleet_writes_no_coordinates_at_all() {
        // Better than defaulting to 0,0, which places a prospect's estate in the
        // Gulf of Guinea and looks deliberate.
        let r = parse("2 web servers");
        let yaml = render(&r, "acme", 7, "acme-");
        assert!(!yaml.contains("latitude"), "{yaml}");
        assert!(!yaml.contains("site:"), "{yaml}");
    }

    #[test]
    fn a_site_is_a_point_on_earth_or_an_error() {
        assert!(Site::new(91.0, 0.0).is_err());
        assert!(Site::new(0.0, 181.0).is_err());
        assert!(Site::new(f64::NAN, 0.0).is_err());
        assert!(Site::new(-90.0, 180.0).is_ok());
    }

    #[test]
    fn scatter_wraps_across_the_antimeridian() {
        // Suva sits close enough to 180 that a westward offset must wrap rather
        // than clamp, or a node lands on a longitude that does not exist.
        let site = Site::new(-18.14, 179.9999).unwrap();
        for n in 0..40 {
            let (lat, lon) = site.scattered(&format!("sim-edge-{n:02}"));
            assert!((-90.0..=90.0).contains(&lat), "{lat}");
            assert!((-180.0..=180.0).contains(&lon), "{lon}");
        }
    }

    #[test]
    fn scatter_is_stable_across_runs() {
        let site = Site::new(50.1109, 8.6821).unwrap();
        assert_eq!(site.scattered("acme-web-01"), site.scattered("acme-web-01"));
        assert_ne!(site.scattered("acme-web-01"), site.scattered("acme-web-02"));
    }

    fn coords(yaml: &str, key: &str) -> Vec<f64> {
        yaml.lines()
            .filter_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().parse().expect("a rendered coordinate"))
            .collect()
    }

    fn spread(v: &[f64]) -> f64 {
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for x in v {
            lo = lo.min(*x);
            hi = hi.max(*x);
        }
        hi - lo
    }

    #[test]
    fn a_device_model_sets_the_nodes_generator_without_renaming_it() {
        // The model is the node's hardware, not software installed on it. Two
        // things must hold: the node loads the vendor profile's spec, and it is
        // still `sw-01`, which is the suffix switch-uplink-degrading targets.
        let reading = Reading {
            groups: vec![Group {
                count: 2,
                role: "network-device".into(),
                services: vec!["cisco_nexus".into()],
                slug: None,
                source: "2 x network-device".into(),
                site: None,
                device: None,
                labels: Default::default(),
            }],
            unrecognised: Vec::new(),
            site: None,
            labels: Default::default(),
        };
        let yaml = render(&reading, "acme", 7, "acme-");
        assert!(
            yaml.contains("generator: ../specs/generated/snmp/cisco_nexus.yaml"),
            "{yaml}"
        );
        assert!(yaml.contains("hostname: acme-sw-01"), "{yaml}");
        assert!(!yaml.contains("cisco-nexus-01"), "{yaml}");
        // The ports a scenario names by instance have to be there.
        assert!(yaml.contains("TenGigabitEthernet1/1/1"), "{yaml}");
        // An SNMP index is a name that looks like a number, so it must be quoted
        // or the environment fails to parse.
        assert!(yaml.contains("name: \"1\""), "{yaml}");
    }

    #[test]
    fn a_device_group_with_no_model_uses_the_generic_switch() {
        let reading = Reading {
            groups: vec![Group {
                count: 1,
                role: "network-device".into(),
                services: Vec::new(),
                slug: None,
                source: "1 x network-device".into(),
                site: None,
                device: None,
                labels: Default::default(),
            }],
            unrecognised: Vec::new(),
            site: None,
            labels: Default::default(),
        };
        let yaml = render(&reading, "acme", 7, "acme-");
        assert!(
            yaml.contains("generator: ../specs/network-device.yaml"),
            "{yaml}"
        );
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
    fn a_fillable_mount_leaves_headroom_for_the_scenario_that_fills_it() {
        // disk-fill ramps disk_space_used_kb by 8.2x. A generated environment
        // whose data volume starts at the default 38% saturates at 100%, the
        // ramp flattens, and the scenario degrades into a flat line that still
        // passes the lint - because the lint does not run scenarios.
        let target = extra_mounts("db")
            .iter()
            .find(|(path, _, _)| *path == "/var/lib/pgsql")
            .map(|(_, _, t)| *t)
            .expect("the db role has a data volume");
        assert!(
            target * 8.2 < 1.0,
            "a {:.0}% baseline reaches {:.0}% under disk-fill and clamps",
            target * 100.0,
            target * 8.2 * 100.0
        );
    }

    #[test]
    fn mount_weights_hit_their_target_utilisation() {
        // Weight and target utilisation are two views of one number; if they
        // drift apart a mount silently reports the wrong fullness.
        let total = 1_572_864_000u64;
        let w = mount_weight(total, FILLABLE_MOUNT_TARGET);
        let implied = w * DISK_DRIVER_BASE_KB / total as f64;
        assert!(
            (implied - FILLABLE_MOUNT_TARGET).abs() < 0.001,
            "weight {w} implies {implied:.4}, wanted {FILLABLE_MOUNT_TARGET}"
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
