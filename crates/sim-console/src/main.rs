//! Infra-Sim control console.
//!
//! `spec.md` §6: the SE lives here from kick-off to teardown. This is the first
//! pass and covers what can be verified honestly today — fleet status, the
//! preflight board, and demo controls. Claim and teardown appear as an explicit
//! manual checklist rather than buttons, because Cloud API coverage for those
//! is still an open engineering question in the spec and a button that silently
//! does nothing is worse than a checklist that tells the truth.
//!
//! Single binary with the UI embedded, per the spec's design note.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use sim_engine::ControlFile;
use sim_spec::{GeneratorSpec, Scenario};
use tokio::sync::Mutex;

mod agent;
mod preflight;
mod provision;

use agent::Agent;

const UI: &str = include_str!("ui.html");

const DEFAULT_ENVIRONMENT: &str = "/etc/netdata/infra-sim/environment.yaml";
const DEFAULT_BIND: &str = "127.0.0.1:8080";
const DEFAULT_AGENT: &str = "127.0.0.1:19999";

struct AppState {
    env_path: PathBuf,
    control_path: PathBuf,
    scenario_dir: PathBuf,
    agent: Agent,
    /// Result of the most recent lint run, if the console was told about one.
    /// `None` means "not verified", which the board reports as manual rather
    /// than passing.
    lint_clean: Option<bool>,
    /// When this console first saw the environment, used as a warm-up floor.
    /// Deliberately conservative: it can only under-report elapsed warm-up, so
    /// the board never claims more readiness than it can prove.
    first_seen: i64,
    cached_guid: Mutex<Option<String>>,
    /// Checkout used by the create flow to find specs, scenarios and a binary.
    repo: PathBuf,
    /// What a long-running operation is doing, polled by the UI.
    progress: provision::ProgressHandle,
}

#[derive(Serialize)]
struct EnvInfo {
    name: String,
    description: String,
    seed: u64,
    update_every: i64,
    node_count: usize,
    generator: String,
    context_count: usize,
}

#[derive(Serialize)]
struct ScenarioInfo {
    name: String,
    description: String,
    root_cause: String,
    causal_chain: Vec<String>,
    blast_radius: Vec<String>,
    expected_finding: String,
    duration_secs: i64,
    active: bool,
    started_at: Option<i64>,
    /// Fraction of the timeline elapsed, for a progress bar.
    progress: f64,
}

#[derive(Serialize)]
struct StatusResponse {
    environment: Option<EnvInfo>,
    agent_url: String,
    nodes: Vec<agent::NodeState>,
    scenarios: Vec<ScenarioInfo>,
    board: preflight::Board,
    now: i64,
    errors: Vec<String>,
    /// The agent's Cloud connection, so the UI shows the truth instead of a
    /// claim form that would be refused.
    cloud: provision::CloudState,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Environment file, without depending on the plugin crate.
///
/// Only the fields the console displays are read; the plugin remains the
/// authority on the full format.
#[derive(serde::Deserialize)]
struct EnvFile {
    name: String,
    #[serde(default)]
    description: String,
    seed: u64,
    #[serde(default = "one")]
    update_every: i64,
    generator: PathBuf,
    nodes: Vec<EnvNode>,
}

fn one() -> i64 {
    1
}

#[derive(serde::Deserialize)]
struct EnvNode {
    hostname: String,
}

fn load_env(path: &std::path::Path) -> Result<EnvFile, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read environment '{}': {e}", path.display()))?;
    serde_yaml::from_str(&raw)
        .map_err(|e| format!("cannot parse environment '{}': {e}", path.display()))
}

fn load_scenarios(dir: &std::path::Path) -> Vec<Scenario> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<Scenario> = entries
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("yaml"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|raw| Scenario::from_yaml(&raw).ok())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

async fn status(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let mut errors = Vec::new();
    let now = now_secs();

    let env = match load_env(&app.env_path) {
        Ok(e) => Some(e),
        Err(e) => {
            errors.push(e);
            None
        }
    };

    let generator_contexts = env
        .as_ref()
        .and_then(|e| {
            let p = if e.generator.is_absolute() {
                e.generator.clone()
            } else {
                app.env_path.parent()?.join(&e.generator)
            };
            let raw = std::fs::read_to_string(p).ok()?;
            GeneratorSpec::from_yaml(&raw)
                .ok()
                .map(|s| s.contexts.len())
        })
        .unwrap_or(0);

    let expected: Vec<String> = env
        .as_ref()
        .map(|e| e.nodes.iter().map(|n| n.hostname.clone()).collect())
        .unwrap_or_default();

    // Resolve the agent GUID once; the ML charts are addressed by it.
    let guid = {
        let mut cached = app.cached_guid.lock().await;
        if cached.is_none() {
            match app.agent.machine_guid().await {
                Ok(g) => *cached = Some(g),
                Err(e) => errors.push(e),
            }
        }
        cached.clone()
    };

    let mut nodes = Vec::new();
    if let Some(guid) = &guid {
        for host in &expected {
            nodes.push(app.agent.node_state(host, guid).await);
        }
    }

    // Simulated hostnames the agent knows that this environment does not
    // define. A vnode GUID is durable, so nodes outlive the plugin that made
    // them and would otherwise sit on the demo dashboard as stale hosts.
    let orphans: Vec<String> = match app.agent.nodes().await {
        Ok(known) => known
            .into_iter()
            .filter(|h| h.starts_with("sim-") && !expected.contains(h))
            .collect(),
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };

    let control = ControlFile::load(&app.control_path).unwrap_or_else(|e| {
        errors.push(e);
        ControlFile::default()
    });

    let scenarios: Vec<ScenarioInfo> = load_scenarios(&app.scenario_dir)
        .into_iter()
        .map(|s| {
            let active = control.is_active(&s.name);
            let started_at = control.started_at(&s.name);
            let duration = s.duration();
            let progress = match (active, started_at) {
                (true, Some(t)) if duration > 0 => {
                    (((now - t) as f64) / duration as f64).clamp(0.0, 1.0)
                }
                (true, _) => 1.0,
                _ => 0.0,
            };
            ScenarioInfo {
                name: s.name.clone(),
                description: s.description.clone(),
                root_cause: s.manifest.root_cause.clone(),
                causal_chain: s.manifest.causal_chain.clone(),
                blast_radius: s.manifest.blast_radius.clone(),
                expected_finding: s.manifest.expected_finding.clone(),
                duration_secs: duration,
                active,
                started_at,
                progress,
            }
        })
        .collect();

    let uptime_hours = Some(((now - app.first_seen) as f64 / 3600.0).max(0.0));
    let board = preflight::evaluate(&preflight::Inputs {
        expected_nodes: &expected,
        states: &nodes,
        scenario_count: scenarios.len(),
        active_scenarios: scenarios.iter().filter(|s| s.active).count(),
        seed: env.as_ref().map(|e| e.seed).unwrap_or(0),
        lint_clean: app.lint_clean,
        uptime_hours,
        orphans: &orphans,
    });

    Json(StatusResponse {
        environment: env.as_ref().map(|e| EnvInfo {
            name: e.name.clone(),
            description: e.description.clone(),
            seed: e.seed,
            update_every: e.update_every,
            node_count: e.nodes.len(),
            generator: e.generator.display().to_string(),
            context_count: generator_contexts,
        }),
        agent_url: app.agent.base_url(),
        nodes,
        scenarios,
        board,
        now,
        errors,
        cloud: provision::cloud_state(&app.agent).await,
    })
}

async fn trigger(
    State(app): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    mutate(&app, move |c| c.trigger(&name, now_secs()))
}

async fn resolve(
    State(app): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> impl IntoResponse {
    let now = now_secs();
    mutate(&app, move |c| c.resolve(&name, now))
}

async fn resolve_all(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let now = now_secs();
    mutate(&app, move |c| c.resolve_all(now))
}

fn mutate<F: FnOnce(&mut ControlFile)>(app: &AppState, f: F) -> impl IntoResponse {
    // Read-modify-write against the file rather than console-held state: the
    // CLI writes the same file, and whoever wrote last is the truth. Holding a
    // cached copy here would let the console silently revert a CLI trigger.
    let mut control = match ControlFile::load(&app.control_path) {
        Ok(mut c) => {
            // The plugin never writes this file, so finished recoveries are
            // cleared here - the console owns it.
            c.prune_recovered(now_secs());
            c
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json_err(e))),
    };
    f(&mut control);
    match control.save(&app.control_path) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json_err(e))),
    }
}

// --------------------------------------------------------------------------
// Lifecycle: create, claim, teardown  (spec.md section 6)
// --------------------------------------------------------------------------

/// Roles and collectors the create form can offer, read from disk.
async fn catalogue(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let cat = provision::catalogue(
        &app.repo.join("specs"),
        &app.repo.join("environments"),
        &app.repo,
    );
    let mut v = serde_json::json!(cat);
    v["llm_providers"] = serde_json::json!(provision::llm_providers(&app.repo));
    Json(v)
}

/// What the console is doing right now, for the UI's progress bar.
async fn progress(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let snapshot = app.progress.lock().ok().and_then(|g| g.clone());
    Json(serde_json::json!(snapshot))
}

/// Free-form text to a proposed fleet. Read-only: nothing is written.
async fn describe(
    State(app): State<Arc<AppState>>,
    Json(req): Json<provision::DescribeRequest>,
) -> impl IntoResponse {
    // The model call is a blocking subprocess and can take tens of seconds.
    let repo = app.repo.clone();
    let handle = app.progress.clone();
    if let Ok(mut g) = handle.lock() {
        *g = Some(provision::Progress::new(
            if req.provider.is_empty() { "reading the description" } else { "asking the model to read the description" },
            1,
        ));
    }
    let finish = app.progress.clone();
    let out = tokio::task::spawn_blocking(move || provision::describe(&repo, &req)).await;
    if let Ok(mut g) = finish.lock() {
        if let Some(p) = g.as_mut() {
            p.done = true;
        }
    }
    match out {
        Ok(Ok(r)) => (StatusCode::OK, Json(serde_json::json!(r))),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn create(
    State(app): State<Arc<AppState>>,
    Json(req): Json<provision::CreateRequest>,
) -> impl IntoResponse {
    // Blocking: the lint simulates hours of data and the install copies files.
    // Holding the executor here is fine - the console serves one operator.
    let repo = app.repo.clone();
    let handle = app.progress.clone();
    if let Ok(mut g) = handle.lock() {
        // Four stages: build, check, install, verify.
        *g = Some(provision::Progress::new("building the environment", 4));
    }
    let worker = app.progress.clone();
    let finish = app.progress.clone();
    let out =
        tokio::task::spawn_blocking(move || provision::create_with_progress(&repo, &req, &worker))
            .await;
    if let Ok(mut g) = finish.lock() {
        if let Some(p) = g.as_mut() {
            p.done = true;
            if let Ok(Err(e)) = &out {
                p.error = e.clone();
            }
        }
    }
    match out {
        Ok(Ok(r)) => Json(serde_json::json!(r)),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("create task failed: {e}"))),
    }
}

async fn claim(
    State(app): State<Arc<AppState>>,
    Json(req): Json<provision::ClaimRequest>,
) -> impl IntoResponse {
    match provision::claim(&app.agent, &req).await {
        Ok(r) => Json(serde_json::json!(r)),
        // The error must not echo the request: it carries a credential.
        Err(e) => Json(json_err(e)),
    }
}

/// Escalate a running scenario, or move the demo clock - the same operation.
async fn advance(
    State(app): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
    Json(req): Json<provision::AdvanceRequest>,
) -> impl IntoResponse {
    let seconds = req.seconds;
    mutate(&app, move |c| {
        let _ = provision::advance(c, &name, seconds);
    })
}

async fn reskin(
    State(app): State<Arc<AppState>>,
    Json(req): Json<provision::ReskinRequest>,
) -> impl IntoResponse {
    let repo = app.repo.clone();
    let env = app.env_path.clone();
    match tokio::task::spawn_blocking(move || provision::reskin(&repo, &env, &req)).await {
        Ok(Ok(r)) => Json(serde_json::json!(r)),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("re-skin task failed: {e}"))),
    }
}

async fn teardown(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let repo = app.repo.clone();
    let control = app.control_path.clone();
    let env = app.env_path.clone();
    match tokio::task::spawn_blocking(move || provision::teardown(&repo, &control, &env)).await {
        Ok(steps) => Json(serde_json::json!({ "steps": steps })),
        Err(e) => Json(json_err(format!("teardown task failed: {e}"))),
    }
}

fn json_err(e: String) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": e })
}

async fn ui() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Html(UI),
    )
}

struct Args {
    environment: PathBuf,
    bind: String,
    agent: String,
    lint_clean: Option<bool>,
    /// Checkout holding specs/, scenarios/ and a built binary. Create needs
    /// these; without it the console is read-only.
    repo: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut environment = PathBuf::from(DEFAULT_ENVIRONMENT);
    let mut bind = DEFAULT_BIND.to_string();
    let mut agent = DEFAULT_AGENT.to_string();
    let mut lint_clean = None;
    let mut repo = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--environment" | "-e" => {
                environment = PathBuf::from(
                    it.next()
                        .ok_or_else(|| "--environment requires a path".to_string())?,
                )
            }
            "--repo" => {
                repo = PathBuf::from(
                    it.next()
                        .ok_or_else(|| "--repo requires a path".to_string())?,
                )
            }
            "--bind" | "-b" => {
                bind = it
                    .next()
                    .ok_or_else(|| "--bind requires host:port".to_string())?
            }
            "--agent" => {
                agent = it
                    .next()
                    .ok_or_else(|| "--agent requires host:port".to_string())?
            }
            "--lint-clean" => lint_clean = Some(true),
            "--lint-failed" => lint_clean = Some(false),
            "--help" | "-h" => {
                return Err(format!(
                    "usage: infra-sim-console [options]\n\n\
                     --environment PATH  environment.yaml (default {DEFAULT_ENVIRONMENT})\n\
                     --bind HOST:PORT    listen address (default {DEFAULT_BIND})\n\
                     --agent HOST:PORT   Netdata agent (default {DEFAULT_AGENT})\n\
                     --lint-clean        record that the fidelity lint passed\n\
                     --lint-failed       record that the fidelity lint failed"
                ))
            }
            other => return Err(format!("unrecognised argument '{other}'")),
        }
    }
    Ok(Args {
        repo,
        environment,
        bind,
        agent,
        lint_clean,
    })
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let (host, port) = match args.agent.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(p) => (h.to_string(), p),
            Err(_) => {
                eprintln!("invalid agent port in '{}'", args.agent);
                return std::process::ExitCode::FAILURE;
            }
        },
        None => {
            eprintln!("--agent must be host:port");
            return std::process::ExitCode::FAILURE;
        }
    };

    let base = args
        .environment
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let state = Arc::new(AppState {
        env_path: args.environment.clone(),
        control_path: base.join("control.yaml"),
        scenario_dir: base.join("scenarios"),
        agent: Agent::new(host, port),
        lint_clean: args.lint_clean,
        first_seen: now_secs(),
        cached_guid: Mutex::new(None),
        repo: args.repo.clone(),
        progress: Default::default(),
    });

    let app = Router::new()
        .route("/", get(ui))
        .route("/api/status", get(status))
        .route("/api/scenario/{name}/trigger", post(trigger))
        .route("/api/scenario/{name}/resolve", post(resolve))
        .route("/api/scenario/resolve-all", post(resolve_all))
        .route("/api/catalogue", get(catalogue))
        .route("/api/progress", get(progress))
        .route("/api/describe", post(describe))
        .route("/api/create", post(create))
        .route("/api/claim", post(claim))
        .route("/api/teardown", post(teardown))
        .route("/api/scenario/{name}/advance", post(advance))
        .route("/api/reskin", post(reskin))
        .with_state(state);

    let addr: SocketAddr = match args.bind.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("invalid --bind '{}': {e}", args.bind);
            return std::process::ExitCode::FAILURE;
        }
    };

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot bind {addr}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    eprintln!("infra-sim console: http://{addr}");
    eprintln!("  environment: {}", args.environment.display());
    eprintln!("  agent:       {}", args.agent);

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    {
        eprintln!("server error: {e}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_yaml_parses_the_fields_the_console_shows() {
        let yaml = r#"
version: 1
name: web-stack
seed: 8675309
update_every: 1
generator: ../specs/linux-system.yaml
nodes:
  - hostname: sim-lb-01
    guid: 4a1f9d20-5e83-4c17-b6a2-0d94e7fc3518
  - hostname: sim-db-01
    guid: 3d81f570-6c24-4a9b-8f13-7e55ab29d4c2
"#;
        let e: EnvFile = serde_yaml::from_str(yaml).expect("parses");
        assert_eq!(e.name, "web-stack");
        assert_eq!(e.seed, 8675309);
        assert_eq!(e.nodes.len(), 2);
    }

    #[test]
    fn the_embedded_ui_is_present_and_self_contained() {
        assert!(UI.contains("<h1"), "UI missing");
        // A strict environment may block outbound requests, and the console
        // must work on an SE's laptop with no network at all.
        assert!(
            !UI.contains("http://") || !UI.contains("<script src="),
            "UI must not load external scripts"
        );
    }
}
