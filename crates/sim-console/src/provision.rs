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

/// Where an installed simulation lives.
pub const INSTALL_DIR: &str = "/etc/netdata/infra-sim";
/// Where Netdata looks for external plugins.
pub const PLUGIN_DIR: &str = "/etc/netdata/custom-plugins.d";

/// One role's row in the create form.
#[derive(Debug, Deserialize)]
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

pub fn catalogue(specs_dir: &Path) -> Catalogue {
    let services = available_services(specs_dir);
    Catalogue {
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

    let specs_dir = repo.join("specs");
    let known = available_services(&specs_dir);
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

    let binary = binary_path(repo)?;
    let lint_summary = if req.lint_hours > 0 {
        lint(&binary, &env_path, req.lint_hours)?
    } else {
        notes.push("lint skipped".into());
        String::new()
    };

    install(repo, &binary, &env_path)?;
    notes.push(format!("installed to {INSTALL_DIR}"));
    notes.push("the agent rescans for plugins every 60s".into());

    Ok(CreateResponse {
        environment: env_path.display().to_string(),
        nodes: reading.groups.iter().map(|g| g.count).sum(),
        lint_summary,
        installed: true,
        notes,
    })
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
fn install(repo: &Path, binary: &Path, env_path: &Path) -> Result<(), String> {
    let install_dir = Path::new(INSTALL_DIR);
    std::fs::create_dir_all(install_dir.join("specs"))
        .and_then(|_| std::fs::create_dir_all(install_dir.join("scenarios")))
        .and_then(|_| std::fs::create_dir_all(PLUGIN_DIR))
        .map_err(|e| {
            format!("cannot create {INSTALL_DIR}: {e}. The console must run as root for create.")
        })?;

    copy_dir(&repo.join("specs"), &install_dir.join("specs"))?;
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
    /// The Space this is going into. `spec.md` requires a fresh Space per
    /// prospect named `<Prospect> (Simulated Demo)`; the console cannot read
    /// Cloud membership, so it asks the SE to type it and checks the shape.
    #[serde(default)]
    pub space_name: String,
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
    if !req.space_name.contains("(Simulated Demo)") {
        return Err(
            "Space name must end with '(Simulated Demo)'. Every simulated environment is \
             visibly labelled as one - that is a hard rule, not a convention."
                .into(),
        );
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

    // 3. Archive what replays the world.
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
    async fn a_claim_needs_the_simulated_demo_space_naming() {
        // Every simulated environment is visibly labelled as one. This is the
        // hard rule, not a preference.
        let req = ClaimRequest {
            token: "t".into(),
            rooms: String::new(),
            url: default_claim_url(),
            space_name: "Acme Production".into(),
        };
        let agent = crate::agent::Agent::new("127.0.0.1", 19999);
        let err = claim(&agent, &req).await.unwrap_err();
        assert!(err.contains("Simulated Demo"), "{err}");
    }

    #[tokio::test]
    async fn a_claim_without_a_token_is_refused_before_anything_else() {
        let req = ClaimRequest {
            token: "  ".into(),
            rooms: String::new(),
            url: default_claim_url(),
            space_name: "Acme (Simulated Demo)".into(),
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
        let c = catalogue(&dir);
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
