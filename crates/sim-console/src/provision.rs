//! Create, claim and teardown — the parts of `spec.md` §6 the console owns
//! besides preflight and demo controls.
//!
//! These all need root: writing under `/etc/netdata`, stopping the plugin
//! process, and reading the agent's local-proof file. The console is therefore
//! run with `sudo`. The alternative — shelling out to `sudo` per action — puts
//! a password prompt in the middle of a demo, which is the worst possible
//! moment for one.
//!
//! ## The claim token is never persisted
//!
//! It arrives in a request body, goes straight to the agent, and is dropped.
//! It is never written to `environment.yaml`, never logged, never included in
//! an error message, and never held in [`AppState`]. `AGENTS.md` treats claim
//! tokens as credentials, and a demo tool that leaves one in a file an SE later
//! commits is exactly the failure that rule exists to prevent.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sim_engine::describe::{available_services, roles, Group, Reading};

/// Where `infra-sim --logs` writes, mirroring `logs_runtime::DEFAULT_JOURNAL_DIR`
/// in the plugin crate. Duplicated rather than shared because the console does
/// not depend on the plugin crate, and this is a filesystem path Netdata itself
/// defines.
const DEFAULT_JOURNAL_DIR: &str = "/var/log/journal/remote";

/// Where an installed simulation lives.
pub const INSTALL_DIR: &str = "/etc/netdata/infra-sim";
/// Where Netdata looks for external plugins.
pub const PLUGIN_DIR: &str = "/etc/netdata/custom-plugins.d";

/// One role's row in the create form.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct GroupRequest {
    pub role: String,
    pub count: usize,
    /// Collectors composed onto this role's nodes. Empty is legitimate — a
    /// base-only node is a real thing to demo.
    #[serde(default)]
    pub services: Vec<String>,
    /// Where this group's nodes are. Absent means the fleet's own location, so a
    /// single-site estate is one field and a multi-site one is still expressible.
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    /// Host labels for this group's nodes, layered over the fleet's. Validated
    /// with the agent's own label rules — see [`sim_engine::labels`].
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    /// Prospect or project name. Fixes the seed, the hostname prefix and
    /// therefore every node GUID, so it is the one field that cannot change later
    /// without orphaning the fleet's history.
    pub name: String,
    pub groups: Vec<GroupRequest>,
    /// The fleet's location, used by every group that does not override it, and
    /// recorded once so the simulation's own agent is placed too.
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    /// Fleet-wide host labels, inherited by every group and overridable per
    /// group. Validated with the agent's own label rules.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// Simulated hours to check before installing.
    ///
    /// Not an operator choice any more. It was presented as "history", which it
    /// never was: netdata's plugin protocol cannot backfill, so a fleet always
    /// starts at zero and this only ever controlled how thoroughly the data was
    /// checked. Offering it invited a decision that could not do what its name
    /// implied.
    #[serde(default = "default_lint_hours")]
    pub lint_hours: u32,
    /// Also publish a simulated Prometheus exporter per application-tier node
    /// and point Netdata's own go.d prometheus collector at it, with a shared
    /// app so the fleet's charts aggregate. On by default; the console's
    /// checkbox and `sim-docker.sh create --no-exporters` turn it off.
    #[serde(default = "default_exporters")]
    pub exporters: bool,

    /// Claim the new simulation's agent into Netdata Cloud.
    ///
    /// Part of create rather than a step afterwards: a container claims at
    /// start-up, so there is no "connect it later" for a fresh agent. Never
    /// stored, never logged, never passed on a command line.
    #[serde(default)]
    pub claim_token: String,
    #[serde(default)]
    pub claim_rooms: String,
}

/// Simulated hours the fidelity check runs before a fleet is installed.
///
/// Two hours catches the defects that matter - clamped signals, impossible
/// units, broken conservation, unresolvable scenario targets - without making
/// an operator wait minutes at fleet scale. Longer runs are still available
/// from the command line for a spec under development.
fn default_lint_hours() -> u32 {
    2
}

/// Exporters default on: application-level metrics that aggregate across the
/// fleet are part of what a modern monitoring demo shows, not an extra.
fn default_exporters() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct CreateResponse {
    pub environment: String,
    pub nodes: usize,
    pub lint_summary: String,
    pub installed: bool,
    pub notes: Vec<String>,
}

/// What the create form offers, read from disk rather than hardcoded so a spec
/// an SE adds shows up without touching this file.
#[derive(Debug, Serialize)]
pub struct Catalogue {
    pub roles: Vec<RoleOption>,
    pub services: Vec<String>,
    pub templates: Vec<TemplateOption>,
    /// Every integration the picker can offer, with its Netdata icon.
    pub integrations: Vec<Integration>,
    /// Every SNMP device model a network-device group can be, generated from
    /// Netdata's own device profiles.
    pub devices: Vec<SnmpDevice>,
}

/// One selectable network device model.
///
/// A switch is not a Linux box running software, so it is picked by vendor and
/// model rather than by collector: what a Catalyst reports is decided by the
/// device, and Netdata already ships the profile that says so.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct SnmpDevice {
    pub id: String,
    pub vendor: String,
    pub device_type: String,
    #[serde(default)]
    pub charts: usize,
}

/// Read the committed SNMP device catalogue.
pub fn snmp_devices(repo: &Path) -> Vec<SnmpDevice> {
    #[derive(serde::Deserialize)]
    struct File {
        devices: Vec<SnmpDevice>,
    }
    std::fs::read_to_string(repo.join("integrations").join("snmp-devices.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<File>(&t).ok())
        .map(|f| f.devices)
        .unwrap_or_default()
}

/// One selectable collector.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct Integration {
    pub id: String,
    pub name: String,
    /// Netdata's own icon URL. Served from their CDN rather than vendored:
    /// 150-odd SVGs is repo clutter, and the picker degrades to initials when
    /// offline rather than showing broken images.
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub charts: usize,
    /// "deep" for the hand-authored specs whose signals are causally coupled
    /// and which the hero scenarios target by name; "generated" for the ones
    /// built from Netdata's collector metadata. The picker shows the
    /// difference rather than implying every integration is equal.
    #[serde(default)]
    pub modelled: String,
}

/// Read the committed integration catalogue.
pub fn integrations(repo: &Path) -> Vec<Integration> {
    #[derive(serde::Deserialize)]
    struct File {
        integrations: Vec<Integration>,
    }
    std::fs::read_to_string(repo.join("integrations").join("catalogue.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<File>(&t).ok())
        .map(|f| f.integrations)
        .unwrap_or_default()
}

#[derive(Debug, Serialize)]
pub struct RoleOption {
    pub role: String,
    pub slug: String,
    pub summary: String,
    /// Pre-ticked in the UI. A db node with no postgres is a valid choice, just
    /// rarely the intended one.
    pub default_services: Vec<String>,
}

pub fn catalogue(specs_dir: &Path, env_dir: &Path, repo: &Path) -> Catalogue {
    let services = available_services(specs_dir);
    Catalogue {
        templates: templates(env_dir),
        integrations: integrations(repo),
        devices: snmp_devices(repo),
        roles: roles()
            .into_iter()
            .map(|r| RoleOption {
                role: r.role.to_string(),
                slug: r.slug.to_string(),
                summary: r.summary.to_string(),
                default_services: r
                    .services
                    .iter()
                    .map(|s| s.to_string())
                    .filter(|s| services.contains(s))
                    .collect(),
            })
            .collect(),
        services,
    }
}

/// The fleet's own location, from the create request.
fn fleet_site(req: &CreateRequest) -> Result<Option<sim_engine::describe::Site>, String> {
    match (req.latitude, req.longitude) {
        (None, None) => Ok(None),
        (Some(lat), Some(lon)) => sim_engine::describe::Site::new(lat, lon).map(Some),
        _ => Err("give both a fleet latitude and longitude, or neither".into()),
    }
}

/// The identity a chosen device model gives a group, from the catalogue.
///
/// Read here rather than in the renderer: the renderer takes a `Reading`, and the
/// catalogue is a console concern. A group with no model gets `None` and the
/// generic switch's visibly-synthetic labels.
fn device_identity(
    g: &GroupRequest,
    devices: &[SnmpDevice],
) -> Option<sim_engine::describe::DeviceIdentity> {
    if g.role != "network-device" {
        return None;
    }
    let picked = g.services.iter().find(|s| *s != "processes")?;
    let d = devices.iter().find(|d| &d.id == picked)?;
    Some(sim_engine::describe::DeviceIdentity {
        vendor: d.vendor.clone(),
        model: d.id.clone(),
        // Netdata's profiles write a device type like "Server Load Balancer";
        // labels read better lowercased with single words.
        kind: d.device_type.to_lowercase().replace(' ', "-"),
    })
}

/// The site for one group: its own coordinates, else the fleet's, else unplaced.
///
/// A half-specified pair is refused rather than guessed at - a latitude with no
/// longitude is a typo, and placing the fleet on the prime meridian because of
/// one would be worse than saying so.
fn group_site(
    g: &GroupRequest,
    fleet: (Option<f64>, Option<f64>),
) -> Result<Option<sim_engine::describe::Site>, String> {
    let (lat, lon) = match (g.latitude, g.longitude) {
        (None, None) => fleet,
        pair => pair,
    };
    match (lat, lon) {
        (None, None) => Ok(None),
        (Some(lat), Some(lon)) => sim_engine::describe::Site::new(lat, lon)
            .map(Some)
            .map_err(|e| format!("{} group: {e}", g.role)),
        _ => Err(format!(
            "the {} group has only one of latitude and longitude; give both or neither",
            g.role
        )),
    }
}

/// Reduce a prospect name to something safe in a hostname.
fn sanitise(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.chars().take(32).collect()
}

/// Build an `environment.yaml`, lint it, and install it.
///
/// `repo` is the checkout holding `specs/`, `scenarios/` and a built binary.
/// Build and check a fleet, writing its environment file.
///
/// Stops short of installing: the container path takes the file from here.
pub fn build_environment(
    repo: &Path,
    req: &CreateRequest,
    progress: &ProgressHandle,
) -> Result<(std::path::PathBuf, String, usize), String> {
    let name = sanitise(&req.name);
    if name.is_empty() {
        return Err("a name is required; it fixes the seed and every node GUID".into());
    }
    let mut known = available_services(&repo.join("specs"));
    known.extend(available_services(&repo.join("specs").join("generated")));
    known.sort();
    known.dedup();
    let known_roles: Vec<String> = roles().iter().map(|r| r.role.to_string()).collect();
    let devices = snmp_devices(repo);
    let device_ids: Vec<String> = devices.iter().map(|d| d.id.clone()).collect();

    let mut groups = Vec::new();
    for g in &req.groups {
        if g.count == 0 {
            continue;
        }
        if !known_roles.contains(&g.role) {
            return Err(format!("unknown role '{}'", g.role));
        }
        // A network-device group's one "service" is its model. Validating it
        // against the collector list would reject every device, and validating a
        // collector against the device list would accept nonsense.
        let allowed = if g.role == "network-device" {
            &device_ids
        } else {
            &known
        };
        for svc in &g.services {
            if !allowed.contains(svc) {
                return Err(if g.role == "network-device" {
                    format!("no SNMP device profile for '{svc}'")
                } else {
                    format!("no generator spec for '{svc}'")
                });
            }
        }
        sim_engine::labels::validate_map(&g.labels)
            .map_err(|e| format!("{} group: {e}", g.role))?;
        groups.push(Group {
            count: g.count.min(500),
            role: g.role.clone(),
            services: g.services.clone(),
            slug: None,
            source: format!("{} x {}", g.count, g.role),
            site: group_site(g, (req.latitude, req.longitude))?,
            device: device_identity(g, &devices),
            labels: g.labels.clone(),
        });
    }
    if groups.is_empty() {
        return Err("pick at least one node".into());
    }

    sim_engine::labels::validate_map(&req.labels).map_err(|e| format!("fleet: {e}"))?;
    let mut reading = Reading {
        groups,
        unrecognised: Vec::new(),
        // The fleet's own location, not a group override: this is what the
        // simulation's own agent is labelled with.
        site: fleet_site(req)?,
        labels: req.labels.clone(),
    };
    reading.dedupe_slugs();
    let nodes: usize = reading.groups.iter().map(|g| g.count).sum();

    let seed = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let yaml = sim_engine::describe::render(&reading, &name, seed, &format!("{name}-"));

    let env_dir = repo.join("environments");
    std::fs::create_dir_all(&env_dir).map_err(|e| format!("cannot create environments/: {e}"))?;
    let env_path = env_dir.join(format!("{name}.yaml"));
    guid_uniqueness(&env_dir, &yaml, &env_path)?;
    std::fs::write(&env_path, &yaml)
        .map_err(|e| format!("cannot write '{}': {e}", env_path.display()))?;
    inherit_owner(&env_dir, &env_path);

    report(
        progress,
        &format!(
            "checking fidelity: simulating {}h across {nodes} nodes",
            req.lint_hours
        ),
        2,
    );
    let binary = binary_path(repo)?;
    let summary = if req.lint_hours > 0 {
        lint(&binary, &env_path, req.lint_hours)?
    } else {
        String::new()
    };
    Ok((env_path, summary, nodes))
}

/// Build, check and install a fleet, reporting each stage as it goes.
pub fn create(
    repo: &Path,
    req: &CreateRequest,
    progress: &ProgressHandle,
) -> Result<CreateResponse, String> {
    let name = sanitise(&req.name);
    if name.is_empty() {
        return Err("a name is required; it fixes the seed and every node GUID".into());
    }

    // Hand-authored specs sit in specs/, the ones generated from Netdata's
    // collector metadata in specs/generated. Both are installable.
    let mut known = available_services(&repo.join("specs"));
    known.extend(available_services(&repo.join("specs").join("generated")));
    known.sort();
    known.dedup();
    let known_roles: Vec<String> = roles().iter().map(|r| r.role.to_string()).collect();
    let devices = snmp_devices(repo);
    let device_ids: Vec<String> = devices.iter().map(|d| d.id.clone()).collect();

    let mut notes = Vec::new();
    let mut groups = Vec::new();
    for g in &req.groups {
        if g.count == 0 {
            continue;
        }
        if !known_roles.contains(&g.role) {
            return Err(format!("unknown role '{}'", g.role));
        }
        // A network-device group's one "service" is its model, checked against
        // the SNMP device profiles rather than the collector specs.
        let allowed = if g.role == "network-device" {
            &device_ids
        } else {
            &known
        };
        let mut services = Vec::new();
        for s in &g.services {
            if allowed.contains(s) {
                services.push(s.clone());
            } else if g.role == "network-device" {
                return Err(format!("no SNMP device profile for '{s}'"));
            } else {
                // Silently dropping it would produce a node whose dashboard is
                // missing the service the SE ticked.
                return Err(format!(
                    "no generator spec for '{s}'; available: {}",
                    known.join(", ")
                ));
            }
        }
        sim_engine::labels::validate_map(&g.labels)
            .map_err(|e| format!("{} group: {e}", g.role))?;
        groups.push(Group {
            count: g.count.min(500),
            role: g.role.clone(),
            services,
            slug: None,
            source: format!("{} x {}", g.count, g.role),
            site: group_site(g, (req.latitude, req.longitude))?,
            device: device_identity(g, &devices),
            labels: g.labels.clone(),
        });
    }

    if groups.is_empty() {
        return Err("pick at least one node".into());
    }

    sim_engine::labels::validate_map(&req.labels).map_err(|e| format!("fleet: {e}"))?;
    let mut reading = Reading {
        groups,
        unrecognised: Vec::new(),
        // The fleet's own location, not a group override: this is what the
        // simulation's own agent is labelled with.
        site: fleet_site(req)?,
        labels: req.labels.clone(),
    };
    // Two rows of the same role would emit colliding hostnames, and the GUID
    // derives from the hostname - so that is two nodes claiming one identity.
    reading.dedupe_slugs();

    let seed = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let prefix = format!("{name}-");
    let yaml = sim_engine::describe::render(&reading, &name, seed, &prefix);

    let env_dir = repo.join("environments");
    std::fs::create_dir_all(&env_dir).map_err(|e| format!("cannot create environments/: {e}"))?;
    let env_path = env_dir.join(format!("{name}.yaml"));

    // Refuse to hand out GUIDs another environment beside this one already
    // uses; two fleets sharing a GUID cannot both be claimed.
    guid_uniqueness(&env_dir, &yaml, &env_path)?;

    std::fs::write(&env_path, &yaml)
        .map_err(|e| format!("cannot write '{}': {e}", env_path.display()))?;
    // The console runs as root, so anything it creates in the checkout would be
    // root-owned and the SE could no longer edit their own environment file.
    // Hand it back to whoever owns the directory it lives in.
    inherit_owner(&env_dir, &env_path);

    let binary = binary_path(repo)?;
    report(
        progress,
        &format!(
            "checking fidelity: simulating {}h across {} nodes",
            req.lint_hours,
            reading.groups.iter().map(|g| g.count).sum::<usize>()
        ),
        2,
    );
    let lint_summary = if req.lint_hours > 0 {
        lint(&binary, &env_path, req.lint_hours)?
    } else {
        notes.push("lint skipped".into());
        String::new()
    };

    let used: Vec<String> = {
        let mut v: Vec<String> = reading
            .groups
            .iter()
            .flat_map(|g| g.services.iter().cloned())
            .collect();
        v.sort();
        v.dedup();
        v
    };
    report(progress, "installing", 3);
    install(repo, &binary, &env_path, &used)?;

    // Lint the *installed* copy, not just the repo one.
    //
    // The repo lint proves the fleet is sound; it cannot prove the install
    // directory has everything the fleet needs, because paths resolve
    // differently there. A spec dependency that failed to travel produced a
    // fleet that installed cleanly and then died on its first tick - and
    // netdata disables a plugin that exits with an error before collecting
    // anything, until the agent restarts
    // (netdata/netdata @ c23face0bd94 src/plugins.d/plugins_d.c:94-98). So a
    // broken install does not merely fail, it poisons the next one.
    let installed = Path::new(INSTALL_DIR).join("environment.yaml");
    if let Err(e) = lint(&binary, &installed, 1) {
        return Err(format!(
            "the installed copy does not load, so it was not left runnable: {e}"
        ));
    }
    notes.push(format!("installed to {INSTALL_DIR}"));
    notes.push("the agent rescans for plugins every 60s".into());

    report(progress, "verifying the installed copy", 4);
    if req.exporters {
        match exporters::enable(repo, &binary, &env_path) {
            Ok(more) => notes.extend(more),
            // A fleet that installed cleanly is still a usable fleet; failing
            // the whole create because the optional scrape target could not be
            // set up would be the wrong trade.
            Err(e) => notes.push(format!("Prometheus exporters NOT enabled: {e}")),
        }
    } else {
        exporters::disable();
    }

    Ok(CreateResponse {
        environment: env_path.display().to_string(),
        nodes: reading.groups.iter().map(|g| g.count).sum(),
        lint_summary,
        installed: true,
        notes,
    })
}

/// What a long-running console operation is doing right now.
///
/// Create takes minutes at fleet scale - most of it inside the fidelity check -
/// and a button that goes quiet for that long is indistinguishable from a hang.
#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    /// What is happening, in the operator's language.
    pub stage: String,
    /// Which step of how many, for a determinate bar.
    pub step: usize,
    pub steps: usize,
    pub started_at: u64,
    pub done: bool,
    pub error: String,
}

impl Progress {
    pub fn new(stage: &str, steps: usize) -> Self {
        Self {
            stage: stage.to_string(),
            step: 1,
            steps,
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            done: false,
            error: String::new(),
        }
    }
}

/// Shared handle the console updates as work proceeds and the UI polls.
pub type ProgressHandle = std::sync::Arc<std::sync::Mutex<Option<Progress>>>;

pub fn report(handle: &ProgressHandle, stage: &str, step: usize) {
    if let Ok(mut g) = handle.lock() {
        if let Some(p) = g.as_mut() {
            p.stage = stage.to_string();
            p.step = step;
        }
    }
}

/// Simulated Prometheus exporters, and the Netdata config that scrapes them.
///
/// This is the one place the console writes into Netdata's *own* configuration
/// rather than its own directory, so every write is namespaced to an
/// `infra-sim` filename and guarded against clobbering an operator's file.
pub mod exporters {
    use super::{inherit_owner, INSTALL_DIR};
    use std::path::Path;

    /// Whether a role runs the instrumented application - the same tier rule
    /// the plugin's OTLP emitter and exporter server apply. A database or a
    /// switch publishing storefront orders is an artifact an SRE reads first.
    fn is_app_tier(role: &str) -> bool {
        matches!(role, "web" | "lb" | "k8s-worker")
    }

    const VNODES_CONF: &str = "/etc/netdata/vnodes/infra-sim.conf";
    const GO_D_CONF: &str = "/etc/netdata/go.d/prometheus.conf";
    const PORT: u16 = 19998;
    /// First line of every file this writes. Its absence in an existing file
    /// means an operator wrote it, and it is not ours to overwrite.
    const MARKER: &str = "# managed by infra-sim - safe to delete when the simulation is torn down";

    /// Write the vnode registry and scrape jobs, then start the exporter.
    pub fn enable(repo: &Path, binary: &Path, env_path: &Path) -> Result<Vec<String>, String> {
        let text = std::fs::read_to_string(env_path).map_err(|e| e.to_string())?;
        // Application tier only, and generated by the one module both paths
        // share - job names are chart-ID prefixes, so per-hostname names here
        // and role-index names in containers would mean different chart
        // identities per path.
        let nodes: Vec<_> = node_refs(&text)
            .into_iter()
            .filter(|n| is_app_tier(&n.role))
            .collect();
        if nodes.is_empty() {
            return Err(
                "no application-tier nodes (web, lb, k8s-worker) in this fleet to export".into(),
            );
        }

        guard(Path::new(GO_D_CONF))?;

        // go.d needs the vnode declared before a job may attribute to it. The
        // GUIDs are the fleet's own, so scraped charts land on the same virtual
        // node the plugins.d path already owns rather than on a second copy.
        write_conf(
            Path::new(VNODES_CONF),
            &format!(
                "{MARKER}\n{}",
                sim_engine::exporter_config::vnodes_conf(&nodes)
            ),
        )?;
        write_conf(
            Path::new(GO_D_CONF),
            &format!(
                "{MARKER}\n{}",
                sim_engine::exporter_config::go_d_conf(&nodes, PORT)
            ),
        )?;

        // The installed environment, not the repo copy: the exporter must move
        // on the same scenario timeline as the plugin, which means reading the
        // same control.yaml beside the installed file.
        let installed = Path::new(INSTALL_DIR).join("environment.yaml");
        let spec = repo.join("specs").join("prometheus-app.yaml");
        std::fs::copy(
            &spec,
            Path::new(INSTALL_DIR)
                .join("specs")
                .join("prometheus-app.yaml"),
        )
        .map_err(|e| format!("cannot install '{}': {e}", spec.display()))?;

        start(binary, &installed)?;

        // go.d reads its vnode registry once, at startup
        // (netdata/netdata src/go/plugin/agent/setup.go:179), so a job
        // referencing a vnode declared after it started attributes nowhere.
        // Stopping the plugin is the supported way to reload it: the daemon
        // respawns external plugins within seconds, and no netdatacli command
        // exists for this.
        let reloaded = reload_go_d();
        Ok(vec![
            format!("{} Prometheus exporters on 127.0.0.1:{PORT}", nodes.len()),
            format!("scrape jobs written to {GO_D_CONF}, vnodes to {VNODES_CONF}"),
            if reloaded {
                "go.d.plugin restarted so it picks up the new vnodes; charts appear within a minute"
                    .into()
            } else {
                format!(
                    "could not restart go.d.plugin - it reads its vnode registry only at \
                     startup, so until it restarts the scraped charts will not attach. \
                     Restart the agent, or delete {GO_D_CONF} and {VNODES_CONF} to undo."
                )
            },
        ])
    }

    /// Remove the config and stop the exporter. Idempotent.
    pub fn disable() {
        stop();
        for path in [GO_D_CONF, VNODES_CONF] {
            let p = Path::new(path);
            // Only ever remove a file this module wrote.
            if std::fs::read_to_string(p)
                .map(|t| t.starts_with(MARKER))
                .unwrap_or(false)
            {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    /// Restart go.d.plugin by stopping it; netdata respawns external plugins.
    ///
    /// Matched on the executable path from /proc, never on a process name.
    fn reload_go_d() -> bool {
        const GO_D: &str = "/usr/libexec/netdata/plugins.d/go.d.plugin";
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        let mut stopped = false;
        for pid in entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        {
            let is_go_d = std::fs::read_link(format!("/proc/{pid}/exe"))
                .map(|p| p == Path::new(GO_D))
                .unwrap_or(false);
            if is_go_d
                && std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            {
                stopped = true;
            }
        }
        stopped
    }

    pub fn running() -> bool {
        !pids().is_empty()
    }

    fn start(binary: &Path, environment: &Path) -> Result<(), String> {
        stop();
        let exe = Path::new(INSTALL_DIR).join("infra-sim");
        // Copy the binary in so the exporter does not hold the repo's build
        // directory open, and so a rebuild cannot swap it mid-demo.
        //
        // Staged and renamed rather than copied over: the previous exporter may
        // still be exiting and writing to its own executable fails with
        // ETXTBSY. Rename is atomic and leaves the dying process on the old
        // inode - the same fix the plugin install needed.
        let staged = Path::new(INSTALL_DIR).join(".infra-sim.new");
        std::fs::copy(binary, &staged).map_err(|e| format!("cannot stage the exporter: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
        }
        std::fs::rename(&staged, &exe).map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            format!("cannot install the exporter: {e}")
        })?;
        std::process::Command::new(&exe)
            .arg("--exporters")
            .arg("--environment")
            .arg(environment)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("cannot start the exporter: {e}"))
    }

    fn stop() {
        for pid in pids() {
            stop_plugin_pid(pid);
        }
    }

    /// PIDs running *our* exporter binary, matched on the executable path and
    /// the `--exporters` argument rather than on a process name.
    fn pids() -> Vec<i32> {
        let exe = Path::new(INSTALL_DIR).join("infra-sim");
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
            .filter(|pid| {
                std::fs::read_link(format!("/proc/{pid}/exe"))
                    .map(|p| p == exe)
                    .unwrap_or(false)
                    && std::fs::read(format!("/proc/{pid}/cmdline"))
                        .map(|c| c.split(|b| *b == 0).any(|a| a == b"--exporters"))
                        .unwrap_or(false)
            })
            .collect()
    }

    fn stop_plugin_pid(pid: i32) {
        // The PID came from /proc, matched on our own executable path *and* our
        // own `--exporters` argument, so this cannot reach an unrelated process.
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }

    fn guard(path: &Path) -> Result<(), String> {
        match std::fs::read_to_string(path) {
            Ok(text) if !text.starts_with(MARKER) => Err(format!(
                "'{}' already exists and was not written by infra-sim. Move it aside \
                 first - overwriting an operator's collector config is not something \
                 this will do silently.",
                path.display()
            )),
            _ => Ok(()),
        }
    }

    fn write_conf(path: &Path, body: &str) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
        }
        std::fs::write(path, body)
            .map_err(|e| format!("cannot write '{}': {e}", path.display()))?;
        if let Some(dir) = path.parent() {
            inherit_owner(dir, path);
        }
        Ok(())
    }

    /// Pull each node's `hostname:` / `guid:` / `role:` triple out of a
    /// rendered environment.
    ///
    /// Labels-block aware: a user label keyed `guid`, `hostname` or `role` is
    /// legal and must not be mistaken for node identity or structure.
    fn node_refs(text: &str) -> Vec<sim_engine::exporter_config::NodeRef> {
        let mut out = Vec::new();
        let mut host: Option<String> = None;
        let mut guid: Option<String> = None;
        let mut role = "node".to_string();
        let mut in_labels = false;
        let mut labels_indent = 0usize;
        let emit = |host: &mut Option<String>,
                    guid: &mut Option<String>,
                    role: &mut String|
         -> Option<sim_engine::exporter_config::NodeRef> {
            let h = host.take()?;
            let g = guid.take()?;
            let r = if role.is_empty() {
                "node".to_string()
            } else {
                std::mem::take(role)
            };
            Some(sim_engine::exporter_config::NodeRef {
                hostname: h,
                guid: g,
                role: r,
            })
        };
        for line in text.lines() {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if trimmed == "labels:" {
                in_labels = true;
                labels_indent = indent;
                continue;
            }
            if in_labels && !trimmed.is_empty() && indent <= labels_indent {
                in_labels = false;
            }
            if in_labels {
                continue;
            }
            let t = trimmed.trim_start_matches("- ");
            if let Some(v) = t.strip_prefix("hostname:") {
                if let Some(n) = emit(&mut host, &mut guid, &mut role) {
                    out.push(n);
                }
                host = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("guid:") {
                guid = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("role:") {
                role = v.trim().to_string();
            }
        }
        if let Some(n) = emit(&mut host, &mut guid, &mut role) {
            out.push(n);
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn extends_is_read_in_both_yaml_forms() {
            assert_eq!(
                super::super::extends_ids("name: a\nextends: generated/postgresql\nsignals:\n"),
                vec!["generated/postgresql"]
            );
            assert_eq!(
                super::super::extends_ids(
                    "name: a\nextends:\n  - generated/kubelet\n  - generated/k8s-state\nsignals:\n  x:\n"
                ),
                vec!["generated/kubelet", "generated/k8s-state"]
            );
            assert!(super::super::extends_ids("name: a\nsignals:\n").is_empty());
            // A `- ` at column zero is a node list, not a continuation.
            assert!(super::super::extends_ids("extends:\n- top-level\n").is_empty());
        }

        #[test]
        fn nodes_are_read_in_pairs() {
            let env = "nodes:\n  - hostname: sim-web-01\n    guid: aaaa\n    role: web\n                       \x20 - hostname: sim-db-01\n    guid: bbbb\n";
            assert_eq!(
                node_refs(env),
                vec![
                    sim_engine::exporter_config::NodeRef {
                        hostname: "sim-web-01".to_string(),
                        guid: "aaaa".to_string(),
                        role: "web".to_string(),
                    },
                    sim_engine::exporter_config::NodeRef {
                        hostname: "sim-db-01".to_string(),
                        guid: "bbbb".to_string(),
                        role: "node".to_string(),
                    },
                ]
            );
        }

        #[test]
        fn a_label_named_role_guid_or_hostname_is_not_node_structure() {
            // Legal user label keys that must not corrupt the triple walk.
            let env = "nodes:\n  - hostname: sim-web-01\n    guid: aaaa\n    role: web\n    labels:\n      role: frontend\n      guid: my-service\n      hostname: x\n";
            let refs = node_refs(env);
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].role, "web");
            assert_eq!(refs[0].guid, "aaaa");
        }

        #[test]
        fn an_operators_own_config_is_never_overwritten() {
            let dir = std::env::temp_dir().join("infra-sim-guard-test");
            std::fs::create_dir_all(&dir).unwrap();
            let theirs = dir.join("prometheus.conf");
            std::fs::write(&theirs, "jobs:\n  - name: their_real_exporter\n").unwrap();
            assert!(guard(&theirs).is_err());

            std::fs::write(&theirs, format!("{MARKER}\njobs: []\n")).unwrap();
            assert!(guard(&theirs).is_ok(), "our own file may be replaced");

            std::fs::remove_file(&theirs).unwrap();
            assert!(guard(&theirs).is_ok(), "absent is fine");
        }
    }
}

/// A free-form description of an estate, and how to read it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DescribeRequest {
    pub text: String,
    /// Empty for the built-in keyword reader. "anthropic" or "openai" asks a
    /// real model, using the key already in the console's environment - the SE
    /// never types a key into a web form.
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    /// Scale the reading to roughly this many nodes. Zero leaves the
    /// description's own numbers alone.
    #[serde(default)]
    pub target_nodes: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DescribeResponse {
    pub groups: Vec<GroupRequest>,
    /// Fleet-wide labels the reading suggests, as the create form's prefill.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// What produced this: "keywords", or the model id.
    pub source: String,
    /// Phrases the reader could not place. Shown, never silently dropped - the
    /// SE has to know which half of their sentence was understood.
    pub unrecognised: Vec<String>,
    /// Things the model recognised but this simulator cannot represent.
    pub unsupported: Vec<String>,
    pub notes: Vec<String>,
    pub suggested_name: String,
}

/// Turn a sentence into a proposed fleet. Nothing is written or installed; the
/// SE reviews and edits the result in the form before creating anything.
pub fn describe(repo: &Path, req: &DescribeRequest) -> Result<DescribeResponse, String> {
    let specs_dir = repo.join("specs");
    if req.text.trim().is_empty() {
        return Err("describe what the prospect runs, in your own words".into());
    }

    // Everything installable, so a description naming haproxy or kafka resolves
    // to that collector instead of the role's default.
    let mut installable = available_services(&specs_dir);
    installable.extend(available_services(&specs_dir.join("generated")));
    installable.sort();
    installable.dedup();

    let (mut reading, source, mut notes, unsupported, suggested) = if req.provider.is_empty() {
        let r = sim_engine::describe::parse_with_services(&req.text, &installable);
        (r, "keywords".to_string(), Vec::new(), Vec::new(), None)
    } else {
        let provider = sim_engine::llm::Provider::parse(&req.provider)?;
        let mut cfg = sim_engine::llm::Config::new(provider);
        cfg.repo = Some(repo.to_path_buf());
        if !req.model.is_empty() {
            cfg.model = req.model.clone();
        }
        let p = sim_engine::llm::propose(&cfg, &req.text, &specs_dir)?;
        let mut notes = p.notes;
        notes.extend(p.corrections);
        (p.reading, p.model, notes, p.unsupported, p.suggested_name)
    };

    // After the reading, never before: the model should read the description
    // as written, and the SE's fleet size is a separate, deterministic step.
    if let Some(note) = sim_engine::describe::scale_to_target(&mut reading, req.target_nodes) {
        notes.push(note);
    }

    let fleet_labels = reading.labels.clone();
    Ok(DescribeResponse {
        groups: reading
            .groups
            .iter()
            .map(|g| GroupRequest {
                role: g.role.clone(),
                count: g.count,
                services: g.services.clone(),
                // The reading places nothing: a description says what a prospect
                // runs, not where, and the location is entered separately.
                latitude: g.site.map(|s| s.lat),
                longitude: g.site.map(|s| s.lon),
                // Suggested labels come back as the row's prefill - the SE
                // edits them like any hand-entered label before creating.
                labels: g.labels.clone(),
            })
            .collect(),
        labels: fleet_labels,
        source,
        unrecognised: reading.unrecognised.clone(),
        unsupported,
        notes,
        suggested_name: suggested.unwrap_or_default(),
    })
}

/// Providers the console can actually reach, so the UI never offers one whose
/// key is missing. Resolved from the environment or a gitignored `.env`.
pub fn llm_providers(repo: &Path) -> Vec<String> {
    sim_engine::llm::available(repo)
        .into_iter()
        .map(|p| p.label().to_string())
        .collect()
}

/// Turn install-directory paths back into repo-relative ones.
fn repo_relative_paths(yaml: &str) -> String {
    yaml.replace("generator: specs/", "generator: ../specs/")
        .replace("specs: specs", "specs: ../specs")
        .replace("scenarios: scenarios", "scenarios: ../scenarios")
}

/// Wait for the running plugin to notice its file is gone and exit.
///
/// Polls `/proc` rather than sleeping blind, so the common case costs a few
/// milliseconds rather than the whole timeout.
fn wait_for_plugin_exit(plugin: &Path, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !plugin_pids(plugin).is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(150));
            continue;
        }
        return true;
    }
    plugin_pids(plugin).is_empty()
}

/// PIDs whose argv[0] is exactly this plugin path.
fn plugin_pids(plugin: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .map(|c| {
                    c.split(|b| *b == 0).next().unwrap_or_default()
                        == plugin.as_os_str().as_encoded_bytes()
                })
                .unwrap_or(false)
        })
        .collect()
}

/// Hostnames declared in an environment file.
///
/// Labels-block aware, so a user label keyed `hostname` is not returned as a
/// node (its only consumer is journal cleanup at teardown).
fn hostnames(env_path: &Path) -> Vec<String> {
    std::fs::read_to_string(env_path)
        .map(|t| {
            let mut out = Vec::new();
            let mut in_labels = false;
            let mut labels_indent = 0usize;
            for line in t.lines() {
                let trimmed = line.trim_start();
                let indent = line.len() - trimmed.len();
                if trimmed == "labels:" {
                    in_labels = true;
                    labels_indent = indent;
                    continue;
                }
                if in_labels && !trimmed.is_empty() && indent <= labels_indent {
                    in_labels = false;
                }
                if in_labels {
                    continue;
                }
                if let Some(v) = trimmed
                    .trim_start_matches("- ")
                    .strip_prefix("hostname:")
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                {
                    out.push(v);
                }
            }
            out
        })
        .unwrap_or_default()
}

/// Stop `infra-sim --logs`, wherever it was started from.
///
/// The logs writer is a separate process an operator starts by hand
/// (`scripts/logs.sh`), so it is not necessarily the installed binary. Matched
/// on an `infra-sim` executable *and* the `--logs` argument, never on a name.
fn stop_logs() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    let mut stopped = false;
    for pid in entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
    {
        let is_ours = std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|p| p.file_name().and_then(|n| n.to_str()) == Some("infra-sim"))
            .unwrap_or(false);
        let has_flag = std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|c| c.split(|b| *b == 0).any(|a| a == b"--logs"))
            .unwrap_or(false);
        if is_ours && has_flag {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            stopped = true;
        }
    }
    stopped
}

/// Delete only the journal files belonging to `hosts`.
///
/// Never the directory: `systemd-journal-remote` output from anything else on
/// this machine lives there too.
fn remove_journals(hosts: &[String]) -> usize {
    hosts
        .iter()
        .filter(|h| !h.is_empty())
        .filter(|h| {
            let path = Path::new(DEFAULT_JOURNAL_DIR).join(format!("remote-{h}.journal"));
            path.exists() && std::fs::remove_file(&path).is_ok()
        })
        .count()
}

/// GUIDs already used by another environment file in the same directory.
fn guid_uniqueness(dir: &Path, new_yaml: &str, self_path: &Path) -> Result<(), String> {
    let guids = |t: &str| -> Vec<String> {
        t.lines()
            .filter_map(|l| l.trim_start().strip_prefix("guid:"))
            .map(|g| g.trim().to_string())
            .collect()
    };
    let mine = guids(new_yaml);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") || path == self_path {
            continue;
        }
        let Ok(other) = std::fs::read_to_string(&path) else {
            continue;
        };
        let theirs = guids(&other);
        if mine.iter().any(|g| theirs.contains(g)) {
            return Err(format!(
                "'{}' already uses these node GUIDs. Two environments sharing a GUID cannot \
                 both be claimed - the second claim takes over the first node's identity. \
                 Re-skin that environment instead of creating a second one.",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Give `path` the same owner as `reference`.
///
/// Best effort: failing to chown is not a reason to fail a create, and on a
/// non-Unix target there is nothing to do.
fn inherit_owner(reference: &Path, path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = std::fs::metadata(reference) {
            let _ = std::os::unix::fs::chown(path, Some(md.uid()), Some(md.gid()));
        }
    }
    #[cfg(not(unix))]
    let _ = (reference, path);
}

fn binary_path(repo: &Path) -> Result<PathBuf, String> {
    for candidate in ["target/release/infra-sim", "target/debug/infra-sim"] {
        let p = repo.join(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    Err(format!(
        "no infra-sim binary under '{}'; run: cargo build --release",
        repo.display()
    ))
}

fn lint(binary: &Path, env_path: &Path, hours: u32) -> Result<String, String> {
    let out = Command::new(binary)
        .arg("--environment")
        .arg(env_path)
        .arg("--lint")
        .arg(hours.to_string())
        .output()
        .map_err(|e| format!("cannot run the lint: {e}"))?;

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        // Refusing here is the whole point: an environment that fails the lint
        // has an artifact an SRE would notice on a chart.
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Err(format!(
            "fidelity lint failed, so nothing was installed:\n{}",
            failure_report(&stdout, &stderr)
        ));
    }
    Ok(tail(&stdout, 12))
}

/// The part of a lint report an operator needs when the lint fails.
///
/// This used to be the last 24 lines of stdout, which at fleet scale is nothing
/// but `PASS`. The lint prints its violations *first* and one `PASS` line per
/// node afterwards, so a 25-node fleet pushed the reason off the top and a create
/// that refused to install could not say why. The summary line compounded it: the
/// plugin writes that to stderr, which was never captured at all.
///
/// So: drop the `PASS` wallpaper, keep everything else, and put the summary back.
fn failure_report(stdout: &str, stderr: &str) -> String {
    // Bounded anyway. A fleet where every node fails should not paste hundreds of
    // lines into a console, and the first failures are the informative ones.
    const MAX: usize = 40;

    let mut lines: Vec<String> = stdout
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("PASS "))
        .map(str::to_string)
        .collect();
    if lines.len() > MAX {
        let dropped = lines.len() - MAX;
        lines.truncate(MAX);
        lines.push(format!("... and {dropped} more line(s)"));
    }
    if let Some(summary) = stderr
        .lines()
        .rev()
        .find(|l| l.contains("fidelity problem"))
    {
        lines.push(summary.trim().to_string());
    }
    lines.join("\n")
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Copy the binary, environment, specs and scenarios into place, then stop the
/// previous plugin process so the agent starts the new one.
fn install(repo: &Path, binary: &Path, env_path: &Path, services: &[String]) -> Result<(), String> {
    let install_dir = Path::new(INSTALL_DIR);
    std::fs::create_dir_all(install_dir.join("specs"))
        .and_then(|_| std::fs::create_dir_all(install_dir.join("scenarios")))
        .and_then(|_| std::fs::create_dir_all(PLUGIN_DIR))
        .map_err(|e| {
            format!("cannot create {INSTALL_DIR}: {e}. The console must run as root for create.")
        })?;

    copy_dir(&repo.join("specs"), &install_dir.join("specs"))?;
    // 150-odd generated specs is 2.4MB of YAML the fleet mostly does not use.
    // Copy the ones it names, into the same `generated` subdirectory the plugin
    // falls back to.
    let gen_dst = install_dir.join("specs").join("generated");
    std::fs::create_dir_all(&gen_dst).map_err(|e| format!("cannot create {gen_dst:?}: {e}"))?;
    for svc in services {
        let from = repo
            .join("specs")
            .join("generated")
            .join(format!("{svc}.yaml"));
        if from.exists() {
            std::fs::copy(&from, gen_dst.join(format!("{svc}.yaml")))
                .map_err(|e| format!("cannot copy '{}': {e}", from.display()))?;
        }
    }

    // A node may carry its own `generator:` - a network device points at one of
    // the specs generated from Netdata's SNMP profiles, which live a directory
    // deeper than the service specs. Copy whatever the environment actually
    // names, so a nested path travels with the fleet instead of the plugin dying
    // on its first tick looking for it.
    let env_text = std::fs::read_to_string(env_path)
        .map_err(|e| format!("cannot read '{}': {e}", env_path.display()))?;
    copy_generators(repo, install_dir, &env_text)?;

    // A hand-authored spec may `extends:` a generated one for its breadth. That
    // dependency has to travel with it: without this the fleet installed
    // cleanly and then the plugin died on the first tick, because the lint had
    // run against the repo where the path resolves.
    copy_extends(repo, install_dir)?;
    copy_dir(&repo.join("scenarios"), &install_dir.join("scenarios"))?;

    std::fs::copy(env_path, install_dir.join("environment.yaml"))
        .map_err(|e| format!("cannot install environment.yaml: {e}"))?;

    // The installed copy points at its own siblings, not the repo's.
    let installed = install_dir.join("environment.yaml");
    let text = std::fs::read_to_string(&installed).map_err(|e| e.to_string())?;
    let rewritten = text
        .replace("generator: ../specs/", "generator: specs/")
        .replace("specs: ../specs", "specs: specs")
        .replace("scenarios: ../scenarios", "scenarios: scenarios");
    std::fs::write(&installed, rewritten).map_err(|e| e.to_string())?;

    // Write beside the target and rename over it. Copying directly fails with
    // ETXTBSY while the previous plugin is running, and stopping it first only
    // narrows the race - the agent rescans every 60s and could relaunch the old
    // binary mid-copy. Rename is atomic: the running process keeps the old
    // inode until it exits, and the next launch gets the new one.
    let plugin = Path::new(PLUGIN_DIR).join("infra-sim.plugin");
    let staged = Path::new(PLUGIN_DIR).join(".infra-sim.plugin.new");
    std::fs::copy(binary, &staged).map_err(|e| format!("cannot stage the plugin: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&staged, &plugin).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("cannot install the plugin: {e}")
    })?;

    stop_plugin(&plugin);
    Ok(())
}

/// Stop a running plugin so the agent restarts it with the new environment.
///
/// Removing or overwriting the file is not enough — a running collector keeps
/// its old environment and keeps writing to the same vnode GUIDs, which
/// corrupts values with interleaved writes and is very hard to diagnose.
/// Matched on the exact path, never on a name, because this machine may be
/// running other simulations.
fn stop_plugin(plugin: &Path) {
    for pid in plugin_pids(plugin) {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

/// Copy every spec an environment names in a `generator:`, preserving its path
/// under `specs/`.
///
/// Only the top level of `specs/` is copied wholesale; the generated specs are
/// 11MB and a fleet uses a handful. This picks out exactly the ones the
/// environment points at.
fn copy_generators(repo: &Path, install_dir: &Path, env_text: &str) -> Result<(), String> {
    let mut wanted: Vec<&str> = env_text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("generator:"))
        .map(str::trim)
        .filter_map(|p| {
            p.strip_prefix("../specs/")
                .or_else(|| p.strip_prefix("specs/"))
        })
        .collect();
    wanted.sort_unstable();
    wanted.dedup();

    for rel in wanted {
        // A path from the environment file must not climb out of specs/.
        if rel.contains("..") {
            return Err(format!("refusing a generator path outside specs/: '{rel}'"));
        }
        let from = repo.join("specs").join(rel);
        if !from.exists() {
            return Err(format!(
                "the environment names a missing spec: 'specs/{rel}'"
            ));
        }
        let to = install_dir.join("specs").join(rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create '{}': {e}", parent.display()))?;
        }
        std::fs::copy(&from, &to).map_err(|e| format!("cannot copy '{}': {e}", from.display()))?;
    }
    Ok(())
}

/// Copy every spec named by an `extends:` in an already-installed spec.
///
/// Reads the installed files rather than the repo's, so only what this fleet
/// actually composes is copied.
fn copy_extends(repo: &Path, install_dir: &Path) -> Result<(), String> {
    let installed_specs = install_dir.join("specs");
    let mut wanted: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&installed_specs) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        wanted.extend(extends_ids(&text));
    }
    wanted.sort();
    wanted.dedup();

    for id in wanted {
        let from = repo.join("specs").join(format!("{id}.yaml"));
        let to = installed_specs.join(format!("{id}.yaml"));
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir:?}: {e}"))?;
        }
        std::fs::copy(&from, &to).map_err(|e| {
            format!(
                "spec dependency '{id}' is missing at '{}': {e}",
                from.display()
            )
        })?;
    }
    Ok(())
}

/// The `extends:` list of a spec, read without a full YAML parse.
fn extends_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("extends:") {
            let rest = rest.trim();
            if rest.is_empty() {
                in_block = true;
            } else {
                out.push(rest.trim_matches(['"', '\'']).to_string());
            }
            continue;
        }
        if in_block {
            match line.trim_start().strip_prefix("- ") {
                Some(id) if line.starts_with(char::is_whitespace) => {
                    out.push(id.trim().trim_matches(['"', '\'']).to_string());
                }
                _ => in_block = false,
            }
        }
    }
    out
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    let entries =
        std::fs::read_dir(from).map_err(|e| format!("cannot read '{}': {e}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            let dest = to.join(path.file_name().unwrap_or_default());
            std::fs::copy(&path, &dest)
                .map_err(|e| format!("cannot copy '{}': {e}", path.display()))?;
        }
    }
    Ok(())
}

// --------------------------------------------------------------------------
// Claim
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ClaimRequest {
    /// Never stored, never logged, never echoed back.
    pub token: String,
    #[serde(default)]
    pub rooms: String,
    #[serde(default = "default_claim_url")]
    pub url: String,
}

fn default_claim_url() -> String {
    "https://app.netdata.cloud".to_string()
}

#[derive(Debug, Serialize)]
pub struct ClaimResponse {
    pub claimed: bool,
    pub message: String,
}

/// Claim the agent, and with it every simulated node.
///
/// Uses the agent's own claim API rather than `netdata-claim.sh`, so the token
/// never reaches a child process's argv — argv is world-readable via `ps` for
/// the life of the process.
pub async fn claim(
    agent: &crate::agent::Agent,
    req: &ClaimRequest,
) -> Result<ClaimResponse, String> {
    if req.token.trim().is_empty() {
        return Err("a claim token is required".into());
    }
    // The agent requires proof of local root access before it will accept a
    // claim over HTTP, which is what stops a web page claiming someone's agent.
    let key = std::fs::read_to_string("/var/lib/netdata/netdata_random_session_id")
        .map_err(|e| {
            format!(
                "cannot read the agent's local-proof file ({e}). \
                 The console must run as root to claim."
            )
        })?
        .trim()
        .to_string();

    // The claim id before the attempt. The agent's response reports current
    // state rather than "your claim succeeded", so an already-claimed agent
    // answers a rejected claim with a perfectly healthy `status: online` -
    // which read naively says "claimed" when nothing happened at all. Comparing
    // the id is the only way to know the attempt did something.
    let before = claim_id(agent).await;

    let body = serde_json::json!({
        "token": req.token,
        "rooms": req.rooms,
        "url": req.url,
    });

    let response = agent
        .put_json(&format!("/api/v2/claim?key={key}"), &body)
        .await
        .map_err(|e| format!("claim request failed: {e}"))?;

    let after = response
        .get("cloud")
        .and_then(|c| c.get("claim_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let can_be_claimed = response
        .get("can_be_claimed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // An agent refusing new claims while already holding one means this fleet
    // would appear in whatever Space it is currently in, not the one the
    // operator just typed.
    if !can_be_claimed && before.is_some() && before == after {
        return Ok(ClaimResponse {
            claimed: false,
            message: format!(
                "This agent is already claimed (claim id {}), so nothing changed and the \
                 fleet stays in its current Space. Unclaim it first:\n  \
                 sudo netdata-claim.sh -token=... -rooms=... \n\
                 or remove /var/lib/netdata/cloud.d/claimed_id and restart netdata.",
                before.unwrap_or_default()
            ),
        });
    }

    let reason = response
        .get("cloud")
        .and_then(|c| c.get("reason"))
        .and_then(|r| r.as_str())
        .unwrap_or("");

    let status = response
        .get("cloud")
        .and_then(|c| c.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("unknown");

    let claimed = before != after && matches!(status, "online" | "connecting");
    let message = if claimed {
        format!("claimed ({status}); nodes appear in the Space within a minute")
    } else if !reason.is_empty() {
        format!("agent reported: {reason}")
    } else {
        format!("the claim did not take effect (agent status: {status})")
    };

    Ok(ClaimResponse { claimed, message })
}

/// Current claim id, or `None` if the agent is unclaimed or unreachable.
/// What the agent's Cloud connection currently looks like.
#[derive(Debug, Clone, Serialize, Default)]
pub struct CloudState {
    pub claimed: bool,
    pub claim_id: String,
    pub url: String,
    /// "online", "offline", whatever the agent reports.
    pub status: String,
    /// False once claimed. The console uses it to stop offering a form that
    /// cannot succeed.
    pub can_be_claimed: bool,
}

/// Read the agent's claim state, for display rather than for action.
pub async fn cloud_state(agent: &crate::agent::Agent) -> CloudState {
    let Ok(v) = agent.get_json("/api/v2/claim").await else {
        return CloudState::default();
    };
    let cloud = v.get("cloud");
    let text = |k: &str| {
        cloud
            .and_then(|c| c.get(k))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let claim_id = text("claim_id");
    CloudState {
        claimed: !claim_id.is_empty(),
        claim_id,
        url: text("url"),
        status: text("status"),
        can_be_claimed: v
            .get("can_be_claimed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

async fn claim_id(agent: &crate::agent::Agent) -> Option<String> {
    agent
        .get_json("/api/v2/claim")
        .await
        .ok()?
        .get("cloud")?
        .get("claim_id")?
        .as_str()
        .map(str::to_string)
}

// --------------------------------------------------------------------------
// Teardown
// --------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct TeardownStep {
    pub name: String,
    pub done: bool,
    pub detail: String,
    /// Steps the console cannot do, so an operator knows what is left.
    pub manual: bool,
}

/// Disarm scenarios, stop the processes, and archive the artifacts that replay
/// the world.
///
/// Cloud-side steps stay manual and say so. `spec.md` leaves node removal and
/// Space deletion as an open question, and a button that silently does nothing
/// is worse than a checklist that tells the truth.
pub fn teardown(repo: &Path, control_path: &Path, env_path: &Path) -> Vec<TeardownStep> {
    let mut steps = Vec::new();

    // 1. Disarm scenarios so nothing is mid-fault when the fleet stops.
    let disarmed = std::fs::write(control_path, "active: []\n").is_ok();
    steps.push(TeardownStep {
        name: "Disarm scenarios".into(),
        done: disarmed,
        detail: if disarmed {
            format!("{} set to active: []", control_path.display())
        } else {
            format!("could not write {}", control_path.display())
        },
        manual: false,
    });

    // 2. Remove the plugin, then stop the process.
    //
    // Both, and in that order. Stopping alone does not stick: the agent
    // rescans every 60s and relaunches the binary it still finds. Removing
    // alone does not stop anything: a running collector keeps its old
    // environment and keeps writing to the same vnode GUIDs from a deleted
    // file - which this project has already spent an hour diagnosing once.
    let plugin = Path::new(PLUGIN_DIR).join("infra-sim.plugin");
    let removed = !plugin.exists() || std::fs::remove_file(&plugin).is_ok();
    // Removing the file first is what lets the plugin exit with status 0 on its
    // own. That matters more than it looks: netdata disables a plugin that dies
    // from a signal and keeps it disabled until the agent restarts, so killing
    // it here would mean the *next* fleet never launches
    // (netdata/netdata @ c23face0bd94 src/plugins.d/plugins_d.c:86-91).
    //
    // The kill stays as a fallback for a plugin that is wedged, because a
    // collector outliving its own removal has already cost this project an hour
    // of debugging once.
    let exited = wait_for_plugin_exit(&plugin, std::time::Duration::from_secs(4));
    if !exited {
        stop_plugin(&plugin);
    }
    steps.push(TeardownStep {
        name: "Remove the plugin and stop its process".into(),
        done: removed,
        detail: if removed && exited {
            "removed; the plugin saw that and exited cleanly, so the agent keeps it enabled \
             and will start the next fleet on its own"
                .into()
        } else if removed {
            "removed, then stopped by exact path - it did not exit on its own, so the agent \
             may need a restart before the next fleet starts"
                .into()
        } else {
            format!("could not remove {}", plugin.display())
        },
        manual: false,
    });

    // 3. Stop the correlated-logs writer and take its journal files with it.
    //
    // Leaving them behind means Netdata keeps offering a log source per node
    // for a fleet that no longer exists - an SE opening logs mid-demo finds
    // the last prospect's hostnames.
    let hosts = hostnames(env_path);
    let stopped_logs = stop_logs();
    let journals = remove_journals(&hosts);
    steps.push(TeardownStep {
        name: "Stop the correlated logs and remove their journals".into(),
        done: true,
        detail: match (stopped_logs, journals) {
            (false, 0) => "none were running".into(),
            (_, n) => format!("{n} journal file(s) removed from {DEFAULT_JOURNAL_DIR}"),
        },
        manual: false,
    });

    // 4. Stop the exporters and take back the Netdata config they installed.
    let had_exporters = exporters::running();
    exporters::disable();
    steps.push(TeardownStep {
        name: "Stop the Prometheus exporters".into(),
        done: true,
        detail: if had_exporters {
            "stopped, and the scrape jobs and vnode entries this wrote were removed".into()
        } else {
            "none were running".into()
        },
        manual: false,
    });

    // 5. Take the fleet's nodes out of the agent.
    //
    // Stopping the collector leaves every vnode *stale*: retention but no
    // collection, so it stays in the agent's database and in the Cloud Space
    // as a dead node. After a demo that is 50 corpses in the prospect's Space.
    // Removed by hostname, one at a time - never ALL_NODES, which would take
    // the operator's own node with it.
    let removed_nodes = hosts
        .iter()
        .filter(|h| {
            Command::new("netdatacli")
                .arg("remove-stale-node")
                .arg(h)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        })
        .count();
    steps.push(TeardownStep {
        name: "Remove the fleet's nodes from the agent".into(),
        done: removed_nodes == hosts.len(),
        detail: if hosts.is_empty() {
            "no nodes to remove".into()
        } else if removed_nodes == hosts.len() {
            format!("{removed_nodes} node(s) unregistered, so none are left stale")
        } else {
            format!(
                "{removed_nodes} of {} unregistered; the rest stay stale until the agent is \
                 restarted or `netdatacli remove-stale-node <host>` is run",
                hosts.len()
            )
        },
        manual: false,
    });

    // 6. Archive what replays the world.
    let archive = repo.join("archive");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Named from the environment's own `name:`, not the filename - the
    // installed copy is always called environment.yaml, so the file stem would
    // make every archive indistinguishable from every other.
    let name = std::fs::read_to_string(env_path)
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("name:").map(|v| v.trim().to_string()))
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "environment".to_string());
    let dest = archive.join(format!("{name}-{stamp}"));
    let archived = std::fs::create_dir_all(dest.join("scenarios"))
        .and_then(|_| std::fs::copy(env_path, dest.join("environment.yaml")).map(|_| ()))
        .is_ok()
        && copy_dir(&repo.join("scenarios"), &dest.join("scenarios")).is_ok();
    if archived {
        inherit_owner(repo, &archive);
        inherit_owner(repo, &dest);
        inherit_owner(repo, &dest.join("environment.yaml"));
    }
    steps.push(TeardownStep {
        name: "Archive environment, seed and scenario manifests".into(),
        done: archived,
        detail: if archived {
            format!("{} - these replay the identical world", dest.display())
        } else {
            "archive failed".into()
        },
        manual: false,
    });

    // 7. Only once the archive exists, because this is the copy being removed.
    let install = Path::new(INSTALL_DIR);
    let removed_install = if !archived {
        false
    } else {
        !install.exists() || std::fs::remove_dir_all(install).is_ok()
    };
    steps.push(TeardownStep {
        name: "Remove the install directory".into(),
        done: removed_install,
        detail: if removed_install {
            format!("{INSTALL_DIR} removed - nothing left for the agent to find")
        } else if archived {
            format!("could not remove {INSTALL_DIR}")
        } else {
            format!("skipped: {INSTALL_DIR} is kept because the archive failed")
        },
        manual: false,
    });

    steps.push(TeardownStep {
        name: "Remove now-offline nodes from Cloud".into(),
        done: false,
        detail: "Cloud API coverage for this is an open question in spec.md".into(),
        manual: true,
    });
    steps.push(TeardownStep {
        name: "Delete the Space".into(),
        done: false,
        detail: "one Space per prospect, never reused".into(),
        manual: true,
    });

    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_reduced_to_something_safe_in_a_hostname() {
        assert_eq!(sanitise("Acme Corp"), "acme-corp");
        assert_eq!(sanitise("  ACME//Retail "), "acme-retail");
        assert_eq!(sanitise("!!!"), "");
    }

    #[test]
    fn a_lint_failure_survives_a_wall_of_passing_nodes() {
        // Reproduces the shape that hid a real failure: violations first, then one
        // PASS per node, with the summary on stderr.
        let mut stdout = String::from(
            "infra-sim lint: 2h simulated, 7200 samples per node\n\n  \
             semantic checks: 1 violation(s)\n    perfectly flat (1 distinct):\n      \
             node-03 app_mem.cron: dimension 'rss' held 2\n\n",
        );
        for i in 0..40 {
            stdout.push_str(&format!("  PASS  node-{i:02}\n"));
        }
        let stderr = "infra-sim: env loaded\ninfra-sim: 1 fidelity problem(s): signals clamped\n";

        let report = failure_report(&stdout, stderr);
        assert!(report.contains("perfectly flat"), "{report}");
        assert!(report.contains("app_mem.cron"), "{report}");
        assert!(report.contains("1 fidelity problem(s)"), "{report}");
        assert!(!report.contains("PASS"), "{report}");
    }

    #[test]
    fn read_labels_treats_a_role_label_as_a_label_not_the_nodes_role() {
        // `role` is a legal label key; matching its line as the node field
        // corrupted the role and truncated the label set on the live path.
        let dir = std::env::temp_dir().join(format!("infra-sim-readlbl-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let env = dir.join("environment.yaml");
        std::fs::write(
            &env,
            "nodes:\n  - hostname: sim-web-01\n    guid: aaaa\n    role: web\n    labels:\n      role: frontend\n      team: platform\n",
        )
        .unwrap();
        let nodes = read_labels(&env).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].role.as_deref(), Some("web"), "node role survives");
        assert_eq!(
            nodes[0].labels.get("role").map(String::as_str),
            Some("frontend")
        );
        assert_eq!(
            nodes[0].labels.get("team").map(String::as_str),
            Some("platform")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_labels_skips_comments_and_blank_lines_and_unquotes_once() {
        let dir = std::env::temp_dir().join(format!("infra-sim-readlbl2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let env = dir.join("environment.yaml");
        std::fs::write(
            &env,
            "nodes:\n  - hostname: sim-web-01\n    guid: aaaa\n    labels:\n      # team: old\n\n      count: '42'\n      spaced: 'rack 12: primary'\n",
        )
        .unwrap();
        let nodes = read_labels(&env).unwrap();
        let l = &nodes[0].labels;
        assert!(!l.contains_key("# team"), "comment not ingested: {l:?}");
        assert_eq!(
            l.get("count").map(String::as_str),
            Some("42"),
            "quotes stripped once"
        );
        assert_eq!(
            l.get("spaced").map(String::as_str),
            Some("rack 12: primary"),
            "quoted colon+space value round-trips"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failure_report_stays_bounded() {
        let mut stdout = String::new();
        for i in 0..200 {
            stdout.push_str(&format!("      node-{i:03} chart.x: broken\n"));
        }
        let report = failure_report(&stdout, "");
        assert!(report.lines().count() <= 41, "{}", report.lines().count());
        assert!(report.contains("more line(s)"), "{report}");
    }

    #[tokio::test]
    async fn a_claim_without_a_token_is_refused_before_anything_else() {
        let req = ClaimRequest {
            token: "  ".into(),
            rooms: String::new(),
            url: default_claim_url(),
        };
        let agent = crate::agent::Agent::new("127.0.0.1", 19999);
        assert!(claim(&agent, &req)
            .await
            .unwrap_err()
            .contains("token is required"));
    }

    #[test]
    fn guid_reuse_across_environments_is_refused() {
        let dir = std::env::temp_dir().join(format!("infra-sim-prov-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let existing = dir.join("existing.yaml");
        let env = "nodes:\n  - hostname: a-01\n    guid: abc-123\n";
        std::fs::write(&existing, env).unwrap();
        let err = guid_uniqueness(&dir, env, &dir.join("new.yaml")).unwrap_err();
        assert!(err.contains("already uses these node GUIDs"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_catalogue_only_offers_specs_that_exist() {
        let dir = std::env::temp_dir().join(format!("infra-sim-cat-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("redis.yaml"), "name: redis\n").unwrap();
        std::fs::write(dir.join("linux-system.yaml"), "name: base\n").unwrap();
        let c = catalogue(&dir, &dir, &dir);
        assert_eq!(c.services, vec!["redis"]);
        // The base spec is composed onto every node already; offering it as a
        // service would merge it into itself.
        assert!(!c.services.contains(&"linux-system".to_string()));
        // Roles still list, but their defaults are filtered to what exists.
        let db = c.roles.iter().find(|r| r.role == "db").unwrap();
        assert!(db.default_services.is_empty(), "{:?}", db.default_services);
        let cache = c.roles.iter().find(|r| r.role == "cache").unwrap();
        assert_eq!(cache.default_services, vec!["redis"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// --------------------------------------------------------------------------
// Templates, escalation and re-skin
// --------------------------------------------------------------------------

/// One committed environment, offered as a starting point for a new fleet.
#[derive(Debug, Serialize)]
pub struct TemplateOption {
    pub name: String,
    pub description: String,
    pub nodes: usize,
    /// Role composition, so picking a template fills the create form rather
    /// than becoming a second, parallel way to build an environment.
    pub groups: Vec<TemplateGroup>,
}

#[derive(Debug, Serialize)]
pub struct TemplateGroup {
    pub role: String,
    pub count: usize,
    pub services: Vec<String>,
}

/// Read the committed environments as create-form presets.
///
/// A template fills the picker; it is never installed directly. One code path
/// builds every environment, so a template cannot drift into producing
/// something the picker could not.
pub fn templates(env_dir: &Path) -> Vec<TemplateOption> {
    #[derive(serde::Deserialize)]
    struct Node {
        role: Option<String>,
        #[serde(default)]
        services: Vec<String>,
    }
    #[derive(serde::Deserialize)]
    struct Env {
        name: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        nodes: Vec<Node>,
    }

    let Ok(entries) = std::fs::read_dir(env_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(env) = serde_yaml::from_str::<Env>(&text) else {
            continue;
        };

        // Collapse nodes into (role, services) groups, which is exactly the
        // shape the create form works in.
        let mut groups: Vec<TemplateGroup> = Vec::new();
        for n in &env.nodes {
            let Some(role) = n.role.clone() else { continue };
            match groups
                .iter_mut()
                .find(|g| g.role == role && g.services == n.services)
            {
                Some(g) => g.count += 1,
                None => groups.push(TemplateGroup {
                    role,
                    count: 1,
                    services: n.services.clone(),
                }),
            }
        }
        if groups.is_empty() {
            continue;
        }
        out.push(TemplateOption {
            name: env.name,
            description: env
                .description
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
            nodes: env.nodes.len(),
            groups,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[derive(Debug, Deserialize)]
pub struct AdvanceRequest {
    /// Seconds to push the scenario clock forward. Negative rewinds.
    pub seconds: i64,
}

/// Move a running scenario's clock.
///
/// This is both "escalate" and the demo clock from `spec.md` section 6. A
/// scenario's severity is a function of elapsed time, so moving `started_at`
/// earlier advances it through its own timeline — the fault deepens exactly as
/// authored rather than by some separate intensity knob that could disagree
/// with the manifest.
pub fn advance(
    control: &mut sim_engine::ControlFile,
    name: &str,
    seconds: i64,
) -> Result<(), String> {
    let entry = control
        .active
        .iter_mut()
        .find(|e| e.scenario == name)
        .ok_or_else(|| format!("'{name}' is not running"))?;
    // started_at moves back to advance the scenario forward.
    entry.started_at = entry.started_at.map(|t| t - seconds);
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ReskinRequest {
    /// New prospect name. Becomes the environment name and hostname prefix.
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ReskinResponse {
    pub renamed: Vec<(String, String)>,
    pub environment: String,
}

/// Re-skin the installed environment for a new prospect, preserving GUIDs.
///
/// This is the move-don't-clone path `spec.md` says the console must enforce:
/// the fleet keeps its history, trained ML models and alert log, turning a cold
/// 72-hour start into a change measured in minutes. The result replaces the
/// installed environment rather than sitting beside it, because two files
/// carrying the same GUIDs cannot both be claimed.
pub fn reskin(
    repo: &Path,
    installed_env: &Path,
    req: &ReskinRequest,
) -> Result<ReskinResponse, String> {
    let name = sanitise(&req.name);
    if name.is_empty() {
        return Err("a name is required".into());
    }

    let source = std::fs::read_to_string(installed_env)
        .map_err(|e| format!("cannot read '{}': {e}", installed_env.display()))?;

    let current = source
        .lines()
        .find_map(|l| l.strip_prefix("name:").map(|v| v.trim().to_string()))
        .ok_or_else(|| "the installed environment has no name".to_string())?;
    if current == name {
        return Err(format!("already skinned as '{name}'"));
    }

    let plan = sim_engine::reskin::Plan {
        from_prefix: format!("{current}-"),
        to_prefix: format!("{name}-"),
        name: Some(name.clone()),
        labels: Default::default(),
    };
    // Refuses if any GUID changed - that would orphan every node's history.
    let outcome = sim_engine::reskin::reskin(&source, &plan)?;

    std::fs::write(installed_env, &outcome.yaml)
        .map_err(|e| format!("cannot write '{}': {e}", installed_env.display()))?;

    // Keep the repo copy in step so the archive and any later re-skin start
    // from the same place - with the paths a repo copy needs. The installed
    // file points at its own siblings; writing that verbatim into
    // `environments/` produced a committed template that could not be linted
    // or used from the checkout.
    let repo_copy = repo.join("environments").join(format!("{name}.yaml"));
    let _ = std::fs::write(&repo_copy, repo_relative_paths(&outcome.yaml));
    inherit_owner(&repo.join("environments"), &repo_copy);

    // The renamed fleet reaches the agent when the plugin restarts, and the
    // plugin restarts itself: it notices the environment changed under it and
    // exits 0. Killing it here would have netdata mark it as having failed and
    // refuse to start it again until the agent restarts
    // (netdata/netdata @ c23face0bd94 src/plugins.d/plugins_d.c:86-91).

    Ok(ReskinResponse {
        renamed: outcome.renamed,
        environment: installed_env.display().to_string(),
    })
}

/// One node's labels as the editor needs them.
#[derive(Debug, Serialize)]
pub struct NodeLabels {
    pub hostname: String,
    pub role: Option<String>,
    pub labels: std::collections::BTreeMap<String, String>,
}

/// Read each node's hostname, role and labels from an environment file.
///
/// The same conservative text-walk re-skinning uses: the file is ours, and a
/// parse that could not cope with comments or hand edits would make the label
/// editor the one surface that breaks on them.
pub fn read_labels(env_path: &Path) -> Result<Vec<NodeLabels>, String> {
    let source = std::fs::read_to_string(env_path)
        .map_err(|e| format!("cannot read '{}': {e}", env_path.display()))?;
    let mut nodes = Vec::new();
    let mut cur: Option<NodeLabels> = None;
    let mut in_labels = false;
    let mut labels_indent = 0usize;

    for line in source.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some(rest) = trimmed.strip_prefix("- hostname:") {
            if let Some(n) = cur.take() {
                nodes.push(n);
            }
            cur = Some(NodeLabels {
                hostname: rest.trim().to_string(),
                role: None,
                labels: Default::default(),
            });
            in_labels = false;
            continue;
        }
        let Some(node) = cur.as_mut() else { continue };
        // The node-level `role:` field, never a label of the same name: a
        // `role` label key is legal (nothing reserves it) and matching it here
        // corrupted the node's role and truncated its label set.
        if let Some(role) = trimmed.strip_prefix("role:") {
            if !in_labels {
                node.role = Some(role.trim().to_string());
                continue;
            }
        }
        if trimmed == "labels:" {
            in_labels = true;
            labels_indent = indent;
            continue;
        }
        if in_labels {
            // Blank and comment lines are transparent, so a hand-edited block
            // survives the round trip; a comment would otherwise be ingested
            // as a pseudo-label and poison every later edit.
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // Deeper-indented `key: value` lines are labels; anything at or
            // above the header's indent (instances:, a comment column, ...)
            // ends the block.
            if indent > labels_indent {
                if let Some((key, value)) = trimmed.split_once(':') {
                    let value = unquote_yaml_scalar(value.trim());
                    node.labels
                        .insert(key.trim().to_string(), value.to_string());
                    continue;
                }
            }
            in_labels = false;
        }
    }
    if let Some(n) = cur.take() {
        nodes.push(n);
    }
    Ok(nodes)
}

/// Strip one balanced pair of single quotes, as [`yaml_scalar`] writes them.
///
/// `trim_matches` strips every edge quote, so a value that legitimately starts
/// and ends with `'`-adjacent characters lost data on read-back.
fn unquote_yaml_scalar(value: &str) -> &str {
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// The complete desired user-label state of a running simulation.
///
/// Fleet labels apply to every node; a role's labels override the fleet's on
/// that role's nodes. Sending the whole desired state (not a diff) keeps the
/// editor idempotent: the console computes per-node set/remove from what the
/// environment currently says.
#[derive(Debug, Deserialize, Default)]
pub struct LabelsRequest {
    #[serde(default)]
    pub fleet: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub groups: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Serialize)]
pub struct LabelsResponse {
    pub changed: Vec<String>,
    pub environment: String,
}

/// Edit the user labels of a running simulation's environment.
///
/// The desired state is fleet-plus-roles because that is what the editor
/// shows; this is where it becomes per-node edits the engine can apply. The
/// plugin notices the rewritten file on its own and restarts cleanly, the
/// agent migrates the labels in place, and history survives — the same path
/// re-skinning takes.
pub fn apply_labels(
    repo: &Path,
    installed_env: &Path,
    req: &LabelsRequest,
) -> Result<LabelsResponse, String> {
    sim_engine::labels::validate_map(&req.fleet).map_err(|e| format!("fleet: {e}"))?;
    for (role, labels) in &req.groups {
        sim_engine::labels::validate_map(labels).map_err(|e| format!("{role} group: {e}"))?;
    }

    let nodes = read_labels(installed_env)?;
    if nodes.is_empty() {
        return Err("the environment defines no nodes".into());
    }

    let mut per_host = std::collections::BTreeMap::new();
    for n in &nodes {
        // Desired: the fleet's labels with the role's layered on top.
        let mut desired = req.fleet.clone();
        if let Some(group) = n.role.as_ref().and_then(|r| req.groups.get(r)) {
            for (k, v) in group {
                desired.insert(k.clone(), v.clone());
            }
        }
        let current: std::collections::BTreeMap<&String, &String> = n
            .labels
            .iter()
            .filter(|(k, _)| !sim_engine::labels::is_generated_key(k))
            .collect();
        let mut changes = sim_engine::reskin::LabelChanges::default();
        for (k, v) in &desired {
            if current.get(k).copied() != Some(v) {
                changes.set.insert(k.clone(), v.clone());
            }
        }
        for k in current.keys() {
            if !desired.contains_key(k.as_str()) {
                changes.remove.insert((*k).clone());
            }
        }
        if !changes.is_empty() {
            per_host.insert(n.hostname.clone(), changes);
        }
    }

    if per_host.is_empty() {
        return Ok(LabelsResponse {
            changed: Vec::new(),
            environment: installed_env.display().to_string(),
        });
    }

    let source = std::fs::read_to_string(installed_env)
        .map_err(|e| format!("cannot read '{}': {e}", installed_env.display()))?;
    let outcome =
        sim_engine::reskin::apply_labels(&source, &per_host).map_err(|e| e.to_string())?;

    std::fs::write(installed_env, &outcome.yaml)
        .map_err(|e| format!("cannot write '{}': {e}", installed_env.display()))?;

    // Keep the repo copy in step, as re-skin does, so the archive and any
    // later edit start from the same place.
    if let Some(name) = source
        .lines()
        .find_map(|l| l.strip_prefix("name:").map(|v| v.trim().to_string()))
    {
        let repo_copy = repo.join("environments").join(format!("{name}.yaml"));
        let _ = std::fs::write(&repo_copy, repo_relative_paths(&outcome.yaml));
        inherit_owner(&repo.join("environments"), &repo_copy);
    }

    Ok(LabelsResponse {
        changed: outcome.changed,
        environment: installed_env.display().to_string(),
    })
}
