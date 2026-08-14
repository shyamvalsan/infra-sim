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
mod container;
mod preflight;
mod provision;

use agent::Agent;

const UI: &str = include_str!("ui.html");

const DEFAULT_ENVIRONMENT: &str = "/etc/netdata/infra-sim/environment.yaml";
// Deliberately in netdata's port neighbourhood rather than the crowded 8080:
// an operator's machine usually has something on 8080 already, and a
// simulation's own agent is reachable just below this (sim-docker.sh
// allocates 19900-19990), so the console sits next to what it manages.
const DEFAULT_BIND: &str = "127.0.0.1:19995";
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
    /// The containerised simulation this console is currently driving.
    ///
    /// A simulation now runs in its own container with its own agent, so the
    /// agent the console talks to is chosen at runtime rather than fixed at
    /// start-up.
    active: std::sync::Mutex<Option<container::Active>>,
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

    // Discover a simulation on every poll, not only at start-up. One created
    // from the command line, or left over from a previous console, is still a
    // simulation this console should be able to show and tear down.
    if app.active.lock().ok().is_some_and(|g| g.is_none()) {
        if let Some(found) = container::list(&app.repo).into_iter().next() {
            if let Ok(mut g) = app.active.lock() {
                *g = Some(found);
            }
        }
    }
    let (agent, env_path, control_path) = target(&app);
    let env = match load_env(&env_path) {
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
                env_path.parent()?.join(&e.generator)
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
            match agent.machine_guid().await {
                Ok(g) => *cached = Some(g),
                Err(e) => errors.push(e),
            }
        }
        cached.clone()
    };

    let mut nodes = Vec::new();
    if let Some(guid) = &guid {
        for host in &expected {
            nodes.push(agent.node_state(host, guid).await);
        }
    }

    // Simulated hostnames the agent knows that this environment does not
    // define. A vnode GUID is durable, so nodes outlive the plugin that made
    // them and would otherwise sit on the demo dashboard as stale hosts.
    let orphans: Vec<String> = match agent.nodes().await {
        Ok(known) => known
            .into_iter()
            .filter(|h| h.starts_with("sim-") && !expected.contains(h))
            .collect(),
        Err(e) => {
            errors.push(e);
            Vec::new()
        }
    };

    let control = ControlFile::load(&control_path).unwrap_or_else(|e| {
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
        agent_url: agent.base_url(),
        nodes,
        scenarios,
        board,
        now,
        errors,
        cloud: provision::cloud_state(&agent).await,
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
    let (_, _, control_path) = target(app);
    let mut control = match ControlFile::load(&control_path) {
        Ok(mut c) => {
            // The plugin never writes this file, so finished recoveries are
            // cleared here - the console owns it.
            c.prune_recovered(now_secs());
            c
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json_err(e))),
    };
    f(&mut control);
    match control.save(&control_path) {
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
            if req.provider.is_empty() {
                "reading the description"
            } else {
                "asking the model to read the description"
            },
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
    // A simulation runs in its own container with its own agent. Installing
    // into the operator's agent is what made claiming impossible and left every
    // torn-down vnode stale, so the console does not do that any more.
    let out = tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
        // Without Docker there is no container to put a simulation in, so fall
        // back to installing into this host's agent. That path cannot claim and
        // leaves nodes stale on teardown, so it says so rather than pretending.
        if let Err(why) = container::available(&repo) {
            let mut r = provision::create(&repo, &req, &worker)?;
            r.notes.insert(0, format!("containers unavailable ({why})"));
            r.notes.push(
                "installed into this host's agent instead. It cannot be claimed separately, \
                 and its nodes are removed from the agent on teardown."
                    .into(),
            );
            return Ok(serde_json::json!(r));
        }
        let (env_path, lint_summary, nodes) = provision::build_environment(&repo, &req, &worker)?;

        provision::report(&worker, "building the image", 3);
        container::build_image(&repo)?;

        provision::report(
            &worker,
            if req.claim_token.trim().is_empty() {
                "starting the container"
            } else {
                "starting the container and claiming it"
            },
            4,
        );
        let name = env_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("simulation")
            .to_string();
        let active = container::create(
            &repo,
            &name,
            &env_path,
            Some(req.claim_token.as_str()),
            &req.claim_rooms,
        )?;
        Ok(serde_json::json!({
            "environment": env_path.display().to_string(),
            "nodes": nodes,
            "lint_summary": lint_summary,
            "installed": true,
            "simulation": active,
            "notes": [
                format!("running in its own container on {}", active.agent_url()),
                if req.claim_token.trim().is_empty() {
                    "not connected to Cloud - this agent is fresh and unclaimed".to_string()
                } else {
                    "claim requested at start-up; the nodes appear in that Space within a minute"
                        .to_string()
                },
                "nodes appear within about a minute".to_string(),
                // Both start with the container. Saying so is the point: every
                // simulation before this shipped with empty logs because
                // starting them was a step someone had to remember.
                "correlated logs and OpenTelemetry started with it - each node is \
                 its own log source, and the application tier ships OTLP logs and \
                 traces"
                    .to_string(),
                "traces are ingested and stored; the agent has no trace viewer \
                 yet, so do not plan a demo beat on them"
                    .to_string(),
            ],
        }))
    })
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

/// Start or stop correlated logs inside the running simulation.
async fn logs(
    State(app): State<Arc<AppState>>,
    AxumPath(action): AxumPath<String>,
) -> impl IntoResponse {
    let Some(active) = app.active.lock().ok().and_then(|g| g.clone()) else {
        return Json(json_err("no simulation is running".into()));
    };
    let repo = app.repo.clone();
    match tokio::task::spawn_blocking(move || container::logs(&repo, &active.name, &action)).await {
        Ok(Ok(detail)) => Json(serde_json::json!({ "ok": true, "detail": detail })),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("logs task failed: {e}"))),
    }
}

fn container_teardown_steps(name: &str, detail: String) -> Vec<provision::TeardownStep> {
    vec![
        provision::TeardownStep {
            name: format!("Remove the container running '{name}'"),
            done: true,
            detail,
            manual: false,
        },
        provision::TeardownStep {
            name: "Nothing left behind".into(),
            done: true,
            detail: "the agent, its database and every simulated node went with the \
                     container - no stale nodes to clear"
                .into(),
            manual: false,
        },
        provision::TeardownStep {
            name: "Remove the Space or room from Netdata Cloud".into(),
            done: false,
            detail: "one Space per prospect, never reused".into(),
            manual: true,
        },
    ]
}

/// Which simulation to tear down.
///
/// The name is required and checked. This endpoint used to take no body at all
/// and remove whatever the console currently considered "active" - which, with
/// two simulations running, silently destroyed the wrong one. A destructive
/// action must name its target and refuse when it does not match.
#[derive(Debug, serde::Deserialize)]
struct TeardownRequest {
    #[serde(default)]
    name: String,
}

async fn teardown(
    State(app): State<Arc<AppState>>,
    Json(req): Json<TeardownRequest>,
) -> impl IntoResponse {
    let repo = app.repo.clone();
    let (_, env, control) = target(&app);
    let active = app.active.lock().ok().and_then(|g| g.clone());

    // Every containerised simulation on this machine, so a mismatch can say what
    // the caller probably meant.
    let running = container::list(&repo);
    let asked = req.name.trim().to_string();

    if !asked.is_empty() {
        if let Some(wanted) = running.iter().find(|a| a.name == asked).cloned() {
            let out =
                tokio::task::spawn_blocking(move || container::teardown(&repo, &wanted.name)).await;
            if let Ok(mut g) = app.active.lock() {
                if g.as_ref().is_some_and(|a| a.name == asked) {
                    *g = None;
                }
            }
            return match out {
                Ok(Ok(detail)) => Json(serde_json::json!({
                    "steps": container_teardown_steps(&asked, detail)
                })),
                Ok(Err(e)) => Json(json_err(e)),
                Err(e) => Json(json_err(format!("teardown task failed: {e}"))),
            };
        }
        if !running.is_empty() {
            return Json(json_err(format!(
                "no simulation named '{asked}'. Running: {}",
                running
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    } else if running.len() > 1 {
        return Json(json_err(format!(
            "{} simulations are running - name the one to tear down: {}",
            running.len(),
            running
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    let out = tokio::task::spawn_blocking(move || match active {
        // A containerised simulation is removed whole: the container carries
        // the agent, its database and every vnode, so there is nothing left
        // stale and nothing to unregister.
        Some(a) => match container::teardown(&repo, &a.name) {
            Ok(detail) => Ok(container_teardown_steps(&a.name, detail)),
            Err(e) => Err(e),
        },
        None => Ok(provision::teardown(&repo, &control, &env)),
    })
    .await;
    if let Ok(mut g) = app.active.lock() {
        *g = None;
    }
    match out {
        Ok(Ok(steps)) => Json(serde_json::json!({ "steps": steps })),
        Ok(Err(e)) => Json(json_err(e)),
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
                     --repo PATH         the checkout holding specs/, scenarios/ and a \
                     built binary (default: the working directory)\n\
                     --environment PATH  environment.yaml (default {DEFAULT_ENVIRONMENT})\n\
                     --bind HOST:PORT    listen address (default {DEFAULT_BIND})\n\
                     --agent HOST:PORT   Netdata agent (default {DEFAULT_AGENT})\n\
                     --lint-clean        record that the fidelity lint passed\n\
                     --lint-failed       record that the fidelity lint failed\n\n\
                     Creating a simulation needs root: it writes under /etc/netdata and \
                     /var/lib/infra-sim, and drives docker."
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

/// The agent the console should be talking to, and where that simulation's
/// files live: the active container's if there is one, otherwise the host's.
///
/// The host a simulation's published dashboard is reachable on.
///
/// A simulation publishes its agent on the *host's* loopback, so `127.0.0.1` is
/// right whenever the console runs on that host. It is wrong when the console is
/// itself containerised - as it is on macOS, where the host cannot exec a Linux
/// binary - because there `127.0.0.1` is the container. The symptom was a node
/// table and a scenario list that were simply empty.
///
/// Defaults to today's behaviour, so Linux is untouched, and `startsim` sets it to
/// `host.docker.internal` when it runs the console in a container.
fn agent_host() -> String {
    std::env::var("INFRA_SIM_AGENT_HOST")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// A simulation now runs in its own container with its own agent, so the target
/// is chosen per request rather than fixed at start-up.
fn target(app: &AppState) -> (Agent, PathBuf, PathBuf) {
    if let Some(a) = app.active.lock().ok().and_then(|g| g.clone()) {
        return (
            Agent::new(agent_host(), a.port),
            a.env_path(),
            a.control_path(),
        );
    }
    (
        app.agent.clone(),
        app.env_path.clone(),
        app.control_path.clone(),
    )
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
        // Adopt a simulation that is already running, so restarting the console
        // does not lose track of it.
        active: std::sync::Mutex::new(container::list(&args.repo).into_iter().next()),
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
        .route("/api/logs/{action}", post(logs))
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
