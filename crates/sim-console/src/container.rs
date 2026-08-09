//! Containerised simulations, driven from the console.
//!
//! A simulation runs in its own container with its own agent, because sharing
//! the operator's agent makes the agent's identity the simulation's: it can be
//! in one Cloud Space, and its database outlives every fleet installed into it.
//! Both of those were dead ends the console could not work around - the claim
//! button could only ever be refused, and teardown left every vnode stale.
//!
//! The lifecycle lives in `scripts/sim-docker.sh` and this shells out to it, so
//! there is one implementation of it and the script stays usable on its own.
//! Duplicating it here in Rust would give two things to keep correct.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

/// A running containerised simulation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Active {
    pub name: String,
    /// Host port the container's agent is published on.
    pub port: u16,
    /// Where its environment, specs, scenarios and control file live.
    pub payload: String,
}

impl Active {
    pub fn agent_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn control_path(&self) -> std::path::PathBuf {
        Path::new(&self.payload).join("control.yaml")
    }

    pub fn env_path(&self) -> std::path::PathBuf {
        Path::new(&self.payload).join("environment.yaml")
    }
}

fn script(repo: &Path) -> std::path::PathBuf {
    repo.join("scripts").join("sim-docker.sh")
}

/// Whether the console can run containers at all.
pub fn available(repo: &Path) -> Result<(), String> {
    if !script(repo).exists() {
        return Err(format!("{} is missing", script(repo).display()));
    }
    let out = Command::new("docker")
        .arg("info")
        .output()
        .map_err(|e| format!("docker is not installed or not on PATH: {e}"))?;
    if !out.status.success() {
        return Err(
            "cannot talk to docker. Is the daemon running, and is this user in the docker group?"
                .into(),
        );
    }
    Ok(())
}

/// Build the image, which carries the plugin binary.
pub fn build_image(repo: &Path) -> Result<String, String> {
    let out = Command::new("bash")
        .arg(script(repo))
        .arg("build")
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run sim-docker.sh: {e}"))?;
    if !out.status.success() {
        return Err(tail(&out.stderr, 12));
    }
    Ok("image built".into())
}

/// Start a simulation. `token` is passed through the environment, never argv:
/// argv is world-readable via `ps` for the life of the process.
pub fn create(
    repo: &Path,
    name: &str,
    env_file: &Path,
    token: Option<&str>,
    rooms: &str,
) -> Result<Active, String> {
    let mut cmd = Command::new("bash");
    cmd.arg(script(repo))
        .arg("create")
        .arg(name)
        .arg(env_file)
        .current_dir(repo);
    if let Some(t) = token.filter(|t| !t.trim().is_empty()) {
        cmd.env("NETDATA_CLAIM_TOKEN", t).arg("--claim");
        if !rooms.trim().is_empty() {
            cmd.arg("--rooms").arg(rooms.trim());
        }
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run sim-docker.sh: {e}"))?;
    if !out.status.success() {
        return Err(tail(&out.stderr, 12));
    }
    active(repo, name).ok_or_else(|| "the container started but reported no port".to_string())
}

/// Look up a running simulation's port and payload directory.
pub fn active(repo: &Path, name: &str) -> Option<Active> {
    let out = Command::new("docker")
        .args(["port", &format!("infra-sim-{name}"), "19999/tcp"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let port = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .rsplit(':')
        .next()?
        .trim()
        .parse()
        .ok()?;
    Some(Active {
        name: name.to_string(),
        port,
        payload: payload_dir(repo, name),
    })
}

/// Every simulation this console can see, running or not.
pub fn list(repo: &Path) -> Vec<Active> {
    let Ok(out) = Command::new("docker")
        .args([
            "ps",
            "--filter",
            "label=infra-sim.simulation",
            "--format",
            "{{.Label \"infra-sim.simulation\"}}",
        ])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|name| active(repo, name.trim()))
        .collect()
}

pub fn teardown(repo: &Path, name: &str) -> Result<String, String> {
    let out = Command::new("bash")
        .arg(script(repo))
        .arg("teardown")
        .arg(name)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run sim-docker.sh: {e}"))?;
    if !out.status.success() {
        return Err(tail(&out.stderr, 12));
    }
    Ok(tail(&out.stderr, 4))
}

/// Start the correlated-logs writer inside a running simulation.
pub fn logs(repo: &Path, name: &str, action: &str) -> Result<String, String> {
    let out = Command::new("bash")
        .arg(script(repo))
        .arg("logs")
        .arg(name)
        .arg(action)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run sim-docker.sh: {e}"))?;
    if !out.status.success() {
        return Err(tail(&out.stderr, 8));
    }
    Ok(tail(&out.stderr, 4))
}

/// Where `sim-docker.sh` keeps a simulation's payload. Kept in step with the
/// script's own default.
fn payload_dir(_repo: &Path, name: &str) -> String {
    // A fixed location, not $HOME. The console runs under sudo and the script
    // may not, so keying off HOME meant the two disagreed about where a
    // simulation lived: the console could not find one created from the command
    // line, and vice versa.
    let base =
        std::env::var("INFRA_SIM_STATE_DIR").unwrap_or_else(|_| DEFAULT_STATE_DIR.to_string());
    format!("{base}/{name}")
}

/// Where simulation payloads live, shared by the console and the script.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/infra-sim";

/// Last `n` non-empty lines, for reporting a script failure without dumping a
/// whole build log into the UI.
fn tail(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        // The script's own `run()` echoes each command in colour; those are for
        // a terminal, not a browser.
        .filter(|l| !l.contains("\u{1b}[0;90m"))
        .collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn tail_drops_the_scripts_command_echo() {
        let raw = "\u{1b}[0;90m/repo >\u{1b}[0m docker build\nreal error here\n\nsecond line\n";
        let out = super::tail(raw.as_bytes(), 4);
        assert_eq!(out, "real error here\nsecond line");
    }

    #[test]
    fn an_active_simulation_knows_where_its_files_are() {
        let a = super::Active {
            name: "customer-a".into(),
            port: 19990,
            payload: "/tmp/x/customer-a".into(),
        };
        assert_eq!(a.agent_url(), "http://127.0.0.1:19990");
        assert!(a.control_path().ends_with("customer-a/control.yaml"));
        assert!(a.env_path().ends_with("customer-a/environment.yaml"));
    }
}
