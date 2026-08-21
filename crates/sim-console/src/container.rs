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
    /// The container's own address on its docker network.
    ///
    /// Needed only when the console is itself containerised, as it is on macOS: a
    /// simulation publishes its agent on the *host's* loopback
    /// (`-p 127.0.0.1:<port>:19999`), which no container can reach, while its own
    /// address on the shared bridge is reachable from a sibling. Empty when docker
    /// does not report one.
    #[serde(default)]
    pub ip: String,
    /// Who created this simulation, as a docker label. Honor-system until the
    /// console authenticates identity: displayed everywhere, enforced nowhere
    /// yet (SOW-0022 hand-off).
    #[serde(default)]
    pub owner: String,
    /// Container creation time, RFC3339 (docker's `.Created`). Creation, not
    /// start: the TTL measures how long a simulation has existed, not when it
    /// last booted.
    #[serde(default)]
    pub created_at: String,
    /// Set by a marker file in the payload dir (`pinned`), not a label - docker
    /// labels are immutable after create, and pin-to-keep must be toggleable.
    #[serde(default)]
    pub pinned: bool,
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
    exporters: bool,
    owner: &str,
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
    if !exporters {
        cmd.arg("--no-exporters");
    }
    if !owner.trim().is_empty() {
        cmd.arg("--owner").arg(owner.trim());
    }
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run sim-docker.sh: {e}"))?;
    if !out.status.success() {
        return Err(tail(&out.stderr, 12));
    }
    active(repo, name).ok_or_else(|| "the container started but reported no port".to_string())
}

/// Look up one simulation's full state.
///
/// One `docker inspect` (JSON) instead of several Go-template calls: a shared
/// host refreshes the whole list on every status poll, and per-sim subprocess
/// fan-out adds up. Inspect also answers for *stopped* containers, which a
/// stopped simulation still has to be tearable down.
pub fn active(repo: &Path, name: &str) -> Option<Active> {
    let container = format!("infra-sim-{name}");
    let out = Command::new("docker")
        .args(["inspect", &container])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    active_from_json(repo, name, v.get(0)?)
}

/// Build an [`Active`] from one `docker inspect` JSON object.
fn active_from_json(repo: &Path, name: &str, v: &serde_json::Value) -> Option<Active> {
    // The requested binding, which for our containers is the actual one (the
    // script always passes an explicit -p). Present even when stopped, unlike
    // `docker port` output.
    let mut port = None;
    if let Some(bindings) = v
        .pointer("/HostConfig/PortBindings")
        .and_then(|b| b.as_object())
    {
        for binding in bindings.values() {
            if let Some(host) = binding.as_array().and_then(|a| a.first()) {
                port = host
                    .get("HostPort")
                    .and_then(|p| p.as_str())
                    .and_then(|p| p.trim().parse::<u16>().ok());
                if port.is_some() {
                    break;
                }
            }
        }
    }
    let port = port?;
    // Its address on the bridge, for a console that cannot use the published
    // port. The default bridge has no DNS, so the container name would not
    // resolve - the address is what works.
    let ip = v
        .pointer("/NetworkSettings/Networks")
        .and_then(|n| n.as_object())
        .map(|networks| {
            networks
                .values()
                .filter_map(|n| n.get("IPAddress").and_then(|i| i.as_str()))
                .find(|i| !i.is_empty())
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_default();
    let payload = payload_dir(repo, name);
    Some(Active {
        name: name.to_string(),
        port,
        payload: payload.clone(),
        ip,
        owner: v
            .pointer("/Config/Labels/infra-sim.owner")
            .and_then(|o| o.as_str())
            .unwrap_or_default()
            .to_string(),
        created_at: v
            .get("Created")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string(),
        // A marker file, not a label: docker labels are immutable after
        // create and pin-to-keep must be toggleable.
        pinned: std::path::Path::new(&payload).join("pinned").exists(),
    })
}

/// Parse docker's RFC3339 timestamps (`2026-08-19T14:18:44.990345676+03:00`
/// or `...Z`) to a Unix epoch. The TTL sweeper compares ages, so this needs
/// to be right, including offsets - a naive date-diff across a DST boundary
/// or timezone would archive a live fleet hours early.
fn rfc3339_to_epoch(ts: &str) -> Option<i64> {
    let bytes = ts.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let num = |a: usize, b: usize| -> Option<i64> { ts.get(a..b)?.parse::<i64>().ok() };
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let minute = num(14, 16)?;
    let second = num(17, 19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // Days from civil (Howard Hinnant's algorithm): exact for the proleptic
    // Gregorian calendar, no epoch tables.
    let (y, m) = if month <= 2 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m - 3) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    let mut epoch = days * 86_400 + hour * 3_600 + minute * 60 + second;

    // Offset: `Z`, `+hh:mm` or `-hh:mm`, after an optional fractional part.
    let rest = &ts[19..];
    let tz = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if !tz.is_empty() && !tz.eq_ignore_ascii_case("z") {
        // `+hh:mm` or `-hh:mm`; anything else is not an offset we understand.
        let off = tz.strip_prefix(['+', '-'])?;
        let sign = if tz.starts_with('-') { -1 } else { 1 };
        let oh = off.get(0..2)?.parse::<i64>().ok()?;
        let om = off.get(3..5)?.parse::<i64>().ok()?;
        epoch -= sign * (oh * 3_600 + om * 60);
    }
    Some(epoch)
}

impl Active {
    /// Seconds since this simulation was created; `None` when the timestamp
    /// could not be parsed (older containers, hand-built fixtures).
    pub fn age_secs(&self, now: i64) -> Option<i64> {
        rfc3339_to_epoch(&self.created_at).map(|t| (now - t).max(0))
    }
}

/// Every simulation this console can see, **including stopped ones**.
///
/// `create` refuses when a container of that name exists at all, so a stopped
/// simulation blocks the next one. Listing only running containers left the
/// console showing no Tear down button for a simulation it was refusing to
/// replace.
pub fn list(repo: &Path) -> Vec<Active> {
    let Ok(out) = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "label=infra-sim.simulation",
            "--format",
            "{{.Label \"infra-sim.simulation\"}}",
        ])
        .output()
    else {
        return Vec::new();
    };
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Vec::new();
    }
    // One inspect for the whole fleet rather than one per simulation: the
    // shared console refreshes this list on every status poll, and a subprocess
    // per simulation at ten simulations is ten subprocesses per poll.
    let containers: Vec<String> = names.iter().map(|n| format!("infra-sim-{n}")).collect();
    let mut cmd = Command::new("docker");
    cmd.arg("inspect").args(&containers);
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    // Inspect returns objects in argument order; pair them back to names, and
    // skip (rather than fail) any that raced a teardown mid-refresh.
    names
        .iter()
        .zip(arr)
        .filter_map(|(name, obj)| active_from_json(repo, name, obj))
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

/// Start, stop or report the telemetry side processes inside a running
/// simulation: the correlated-logs writer and the OpenTelemetry emitter.
///
/// `create` starts both already. This exists to restart them if they were
/// stopped, and to report what they are doing.
pub fn logs(repo: &Path, name: &str, action: &str) -> Result<String, String> {
    let out = Command::new("bash")
        .arg(script(repo))
        .arg("telemetry")
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
    use std::path::Path;

    #[test]
    fn rfc3339_parses_utc_offsets_and_fractions() {
        use super::rfc3339_to_epoch;
        // A known instant in three spellings: Z, +00:00, and a +03:00 offset
        // must all agree.
        let z = rfc3339_to_epoch("2026-08-19T11:00:00Z").unwrap();
        assert_eq!(z, rfc3339_to_epoch("2026-08-19T11:00:00+00:00").unwrap());
        assert_eq!(z, rfc3339_to_epoch("2026-08-19T14:00:00+03:00").unwrap());
        // Fractional seconds are skipped, not parsed: docker emits nanoseconds.
        assert_eq!(
            z,
            rfc3339_to_epoch("2026-08-19T11:00:00.990345676Z").unwrap()
        );
        // A negative offset moves the instant the other way.
        assert_eq!(z, rfc3339_to_epoch("2026-08-19T06:00:00-05:00").unwrap());
        // Cross-check one instant against `date -u -d ... +%s` semantics:
        // the Unix epoch itself.
        assert_eq!(rfc3339_to_epoch("1970-01-01T00:00:00Z"), Some(0));
        // Leap-year day parses (2028 is one).
        assert!(rfc3339_to_epoch("2028-02-29T00:00:00Z").is_some());
        // Nonsense is refused, not guessed.
        assert!(rfc3339_to_epoch("not-a-time").is_none());
        assert!(rfc3339_to_epoch("2026-13-40T99:99:99Z").is_none());
    }

    #[test]
    fn an_active_json_object_becomes_a_full_active() {
        let v: serde_json::Value = serde_json::json!({
            "HostConfig": { "PortBindings": { "19999/tcp": [
                { "HostIp": "127.0.0.1", "HostPort": "19986" }
            ]}},
            "NetworkSettings": { "Networks": {
                "bridge": { "IPAddress": "172.17.0.4" }
            }},
            "Created": "2026-08-19T11:00:00Z",
            "Config": { "Labels": { "infra-sim.owner": "sre-a" } }
        });
        // A temp payload dir so `pinned` is absent.
        let dir = std::env::temp_dir().join(format!("infra-sim-active-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("INFRA_SIM_STATE_DIR", &dir);
        let a = super::active_from_json(Path::new("/repo"), "customer-a", &v).unwrap();
        assert_eq!(a.port, 19986);
        assert_eq!(a.ip, "172.17.0.4");
        assert_eq!(a.owner, "sre-a");
        assert!(!a.pinned);
        // 2026-08-19T11:00:00Z == 1787137200; an hour later the age is 3600.
        assert_eq!(a.age_secs(1787137200), Some(0));
        assert_eq!(a.age_secs(1787140800), Some(3600));
        std::env::remove_var("INFRA_SIM_STATE_DIR");

        // A pinned marker file flips the flag.
        std::fs::create_dir_all(dir.join("customer-a")).unwrap();
        std::fs::write(dir.join("customer-a").join("pinned"), "").unwrap();
        std::env::set_var("INFRA_SIM_STATE_DIR", &dir);
        let a = super::active_from_json(Path::new("/repo"), "customer-a", &v).unwrap();
        assert!(a.pinned);
        std::env::remove_var("INFRA_SIM_STATE_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tail_drops_the_scripts_command_echo() {
        let raw = "\u{1b}[0;90m/repo >\u{1b}[0m docker build\nreal error here\n\nsecond line\n";
        let out = super::tail(raw.as_bytes(), 4);
        assert_eq!(out, "real error here\nsecond line");
    }

    #[test]
    fn an_active_simulation_knows_where_its_files_are() {
        let a = super::Active {
            ip: String::new(),
            name: "customer-a".into(),
            port: 19990,
            payload: "/tmp/x/customer-a".into(),
            owner: String::new(),
            created_at: String::new(),
            pinned: false,
        };
        assert_eq!(a.agent_url(), "http://127.0.0.1:19990");
        assert!(a.control_path().ends_with("customer-a/control.yaml"));
        assert!(a.env_path().ends_with("customer-a/environment.yaml"));
    }
}
