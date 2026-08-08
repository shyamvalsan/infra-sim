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
}

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    /// Prospect or project name. Fixes the seed, the hostname prefix and
    /// therefore every GUID, so it is the one field that cannot change later
    /// without orphaning the fleet's history.
    pub name: String,
    pub groups: Vec<GroupRequest>,
    /// Simulated hours to lint before installing. Zero skips the lint, which
    /// the UI does not offer — an unlinted fleet is how a bad demo happens.
    #[serde(default = "default_lint_hours")]
    pub lint_hours: u32,
    /// Also publish a simulated Prometheus exporter per node and point
    /// Netdata's own go.d prometheus collector at it.
    #[serde(default)]
    pub exporters: bool,
}

fn default_lint_hours() -> u32 {
    24
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
pub fn create(repo: &Path, req: &CreateRequest) -> Result<CreateResponse, String> {
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

    let mut notes = Vec::new();
    let mut groups = Vec::new();
    for g in &req.groups {
        if g.count == 0 {
            continue;
        }
        if !known_roles.contains(&g.role) {
            return Err(format!("unknown role '{}'", g.role));
        }
        let mut services = Vec::new();
        for s in &g.services {
            if known.contains(s) {
                services.push(s.clone());
            } else {
                // Silently dropping it would produce a node whose dashboard is
                // missing the service the SE ticked.
                return Err(format!(
                    "no generator spec for '{s}'; available: {}",
                    known.join(", ")
                ));
            }
        }
        groups.push(Group {
            count: g.count.min(500),
            role: g.role.clone(),
            services,
            slug: None,
            source: format!("{} x {}", g.count, g.role),
        });
    }

    if groups.is_empty() {
        return Err("pick at least one node".into());
    }

    let mut reading = Reading {
        groups,
        unrecognised: Vec::new(),
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
    install(repo, &binary, &env_path, &used)?;
    notes.push(format!("installed to {INSTALL_DIR}"));
    notes.push("the agent rescans for plugins every 60s".into());

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

/// Simulated Prometheus exporters, and the Netdata config that scrapes them.
///
/// This is the one place the console writes into Netdata's *own* configuration
/// rather than its own directory, so every write is namespaced to an
/// `infra-sim` filename and guarded against clobbering an operator's file.
pub mod exporters {
    use super::{inherit_owner, INSTALL_DIR};
    use std::path::Path;

    const VNODES_CONF: &str = "/etc/netdata/vnodes/infra-sim.conf";
    const GO_D_CONF: &str = "/etc/netdata/go.d/prometheus.conf";
    const PORT: u16 = 19998;
    /// First line of every file this writes. Its absence in an existing file
    /// means an operator wrote it, and it is not ours to overwrite.
    const MARKER: &str = "# managed by infra-sim - safe to delete when the simulation is torn down";

    /// Write the vnode registry and scrape jobs, then start the exporter.
    pub fn enable(repo: &Path, binary: &Path, env_path: &Path) -> Result<Vec<String>, String> {
        let text = std::fs::read_to_string(env_path).map_err(|e| e.to_string())?;
        let nodes = hostnames_and_guids(&text);
        if nodes.is_empty() {
            return Err("no nodes to export".into());
        }

        guard(Path::new(GO_D_CONF))?;

        // go.d needs the vnode declared before a job may attribute to it. The
        // GUIDs are the fleet's own, so scraped charts land on the same virtual
        // node the plugins.d path already owns rather than on a second copy.
        let mut vnodes = format!("{MARKER}\n");
        for (host, guid) in &nodes {
            vnodes.push_str(&format!("- hostname: {host}\n  guid: {guid}\n"));
        }
        write_conf(Path::new(VNODES_CONF), &vnodes)?;

        let mut jobs = format!("{MARKER}\njobs:\n");
        for (host, _) in &nodes {
            jobs.push_str(&format!(
                "  - name: infra_sim_app_{slug}\n    vnode: {host}\n    url: http://127.0.0.1:{PORT}/metrics/{host}\n",
                slug = host.replace(['-', '.'], "_"),
            ));
        }
        write_conf(Path::new(GO_D_CONF), &jobs)?;

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

    /// Pull `hostname:` / `guid:` pairs out of a rendered environment.
    fn hostnames_and_guids(text: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut host: Option<String> = None;
        for line in text.lines() {
            let t = line.trim_start().trim_start_matches("- ");
            if let Some(v) = t.strip_prefix("hostname:") {
                host = Some(v.trim().to_string());
            } else if let Some(v) = t.strip_prefix("guid:") {
                if let Some(h) = host.take() {
                    out.push((h, v.trim().to_string()));
                }
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn nodes_are_read_in_pairs() {
            let env = "nodes:\n  - hostname: sim-web-01\n    guid: aaaa\n    role: web\n                       \x20 - hostname: sim-db-01\n    guid: bbbb\n";
            assert_eq!(
                hostnames_and_guids(env),
                vec![
                    ("sim-web-01".to_string(), "aaaa".to_string()),
                    ("sim-db-01".to_string(), "bbbb".to_string()),
                ]
            );
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
}

#[derive(Debug, Clone, Serialize)]
pub struct DescribeResponse {
    pub groups: Vec<GroupRequest>,
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

    let (reading, source, notes, unsupported, suggested) = if req.provider.is_empty() {
        let r = sim_engine::describe::parse_with_services(&req.text, &installable);
        (r, "keywords".to_string(), Vec::new(), Vec::new(), None)
    } else {
        let provider = sim_engine::llm::Provider::parse(&req.provider)?;
        let mut cfg = sim_engine::llm::Config::new(provider);
        if !req.model.is_empty() {
            cfg.model = req.model.clone();
        }
        let p = sim_engine::llm::propose(&cfg, &req.text, &specs_dir)?;
        let mut notes = p.notes;
        notes.extend(p.corrections);
        (p.reading, p.model, notes, p.unsupported, p.suggested_name)
    };

    Ok(DescribeResponse {
        groups: reading
            .groups
            .iter()
            .map(|g| GroupRequest {
                role: g.role.clone(),
                count: g.count,
                services: g.services.clone(),
            })
            .collect(),
        source,
        unrecognised: reading.unrecognised.clone(),
        unsupported,
        notes,
        suggested_name: suggested.unwrap_or_default(),
    })
}

/// Whether a provider key is present in the console's own environment, so the
/// UI can offer the model only when it would actually work.
pub fn llm_providers() -> Vec<String> {
    ["anthropic", "openai"]
        .iter()
        .filter(|p| {
            let env = sim_engine::llm::Provider::parse(p)
                .map(|x| x.default_key_env())
                .unwrap_or("");
            std::env::var(env).map(|v| !v.is_empty()).unwrap_or(false)
        })
        .map(|p| p.to_string())
        .collect()
}

/// Hostnames declared in an environment file.
fn hostnames(env_path: &Path) -> Vec<String> {
    std::fs::read_to_string(env_path)
        .map(|t| {
            t.lines()
                .filter_map(|l| {
                    l.trim_start()
                        .trim_start_matches("- ")
                        .strip_prefix("hostname:")
                        .map(|v| v.trim().to_string())
                })
                .filter(|h| !h.is_empty())
                .collect()
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
        return Err(format!(
            "fidelity lint failed, so nothing was installed:\n{}",
            tail(&stdout, 24)
        ));
    }
    Ok(tail(&stdout, 12))
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
    if !services.is_empty() {
        let gen_src = repo.join("specs").join("generated");
        let gen_dst = install_dir.join("specs").join("generated");
        std::fs::create_dir_all(&gen_dst).map_err(|e| format!("cannot create {gen_dst:?}: {e}"))?;
        for svc in services {
            let from = gen_src.join(format!("{svc}.yaml"));
            if from.exists() {
                std::fs::copy(&from, gen_dst.join(format!("{svc}.yaml")))
                    .map_err(|e| format!("cannot copy '{}': {e}", from.display()))?;
            }
        }
    }
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
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        if argv0 == plugin.as_os_str().as_encoded_bytes() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
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
    stop_plugin(&plugin);
    steps.push(TeardownStep {
        name: "Remove the plugin and stop its process".into(),
        done: removed,
        detail: if removed {
            "removed so the agent's 60s rescan cannot relaunch it, then stopped by exact path"
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

    // 5. Archive what replays the world.
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

    // 6. Only once the archive exists, because this is the copy being removed.
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
    // from the same place.
    let repo_copy = repo.join("environments").join(format!("{name}.yaml"));
    let _ = std::fs::write(&repo_copy, &outcome.yaml);
    inherit_owner(&repo.join("environments"), &repo_copy);

    // The renamed fleet only reaches the agent when the plugin restarts.
    stop_plugin(&Path::new(PLUGIN_DIR).join("infra-sim.plugin"));

    Ok(ReskinResponse {
        renamed: outcome.renamed,
        environment: installed_env.display().to_string(),
    })
}
