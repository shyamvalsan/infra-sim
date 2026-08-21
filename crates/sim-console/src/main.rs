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

use axum::extract::{Path as AxumPath, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use sim_engine::ControlFile;
use sim_spec::{GeneratorSpec, Scenario};
use tokio::sync::Mutex;

mod agent;
mod budget;
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
/// For the help text only; the load path constant lives in `budget`.
const DEFAULT_BUDGETS: &str = budget::DEFAULT_BUDGETS_PATH;

struct AppState {
    env_path: PathBuf,
    control_path: PathBuf,
    scenario_dir: PathBuf,
    agent: Agent,
    /// Result of the most recent lint run, if the console was told about one.
    /// `None` means "not verified", which the board reports as manual rather
    /// than passing. Legacy local-install path only.
    lint_clean: Option<bool>,
    /// When this console first saw a simulation, used as a warm-up floor.
    /// Deliberately conservative: it can only under-report elapsed warm-up, so
    /// the board never claims more readiness than it can prove.
    first_seen: Mutex<std::collections::BTreeMap<String, i64>>,
    cached_guid: Mutex<std::collections::BTreeMap<String, String>>,
    /// Checkout used by the create flow to find specs, scenarios and a binary.
    repo: PathBuf,
    /// Where the SRE's budget file lives; re-read per use so a host tune needs
    /// no console restart.
    budgets_path: PathBuf,
    /// The shared bearer token; `None` = auth off (loopback single-operator
    /// mode). Fixed at startup: changing a trust boundary should restart the
    /// thing it guards.
    token: Option<String>,
    /// What each long-running operation is doing, keyed by operation id.
    progress: provision::ProgressMap,
    /// Serializes creates: the lint inside a create is CPU-parallel across all
    /// cores by design, so two at once is both a wrong answer and a host
    /// stampede. Queued creates report their position.
    create_queue: CreateQueue,
}

/// One slot, with honest queue positions.
struct CreateQueue {
    sem: tokio::sync::Semaphore,
    /// Creations waiting for the slot right now (includes the holder's
    /// pending-decrement race; reported as "ahead of you", never exactness).
    waiting: std::sync::atomic::AtomicUsize,
}

impl CreateQueue {
    fn new() -> Self {
        Self {
            sem: tokio::sync::Semaphore::new(1),
            waiting: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many creates are queued ahead (0 = you hold the slot).
    fn queued_ahead(&self) -> usize {
        self.waiting
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(1)
    }
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

/// The console's front page: every simulation, the host's budgets, and any
/// long-running operation. Per-simulation detail (nodes, scenarios, board)
/// lives on `/api/sim/{name}/status` so a big fleet's detail is fetched only
/// when someone is looking at it.
async fn status(State(app): State<Arc<AppState>>) -> impl IntoResponse {
    let now = now_secs();
    let budgets = budget::Budgets::load(&app.budgets_path).unwrap_or_default();
    let sims = container::list(&app.repo);

    let mut simulations = Vec::new();
    for a in &sims {
        let env = load_env(&a.env_path()).ok();
        // First-seen is per simulation now: a console restart must not reset
        // the warm-up clock of a fleet that has been running for days.
        // Observation seeds the warm-up floor for simulations the console
        // did not create itself (CLI-made, or predating a restart).
        let _ = app
            .first_seen
            .lock()
            .await
            .entry(a.name.clone())
            .or_insert(now);
        simulations.push(serde_json::json!({
            "name": a.name,
            "owner": if a.owner.is_empty() { "-" } else { &a.owner },
            "created_at": a.created_at,
            "age_secs": a.age_secs(now),
            "pinned": a.pinned,
            "url": a.agent_url(),
            "environment": env.as_ref().map(|e| EnvInfo {
                name: e.name.clone(),
                description: e.description.clone(),
                seed: e.seed,
                update_every: e.update_every,
                node_count: e.nodes.len(),
                generator: e.generator.display().to_string(),
                context_count: 0,
            }),
            "expires_in_secs": budgets
                .ttl_days
                .saturating_mul(86_400)
                .saturating_sub(a.age_secs(now).unwrap_or(0).max(0) as u64),
        }));
    }

    // The legacy host-install fallback: when no containers exist at all, the
    // local environment is still the one fleet this console can drive.
    if simulations.is_empty() {
        if let Ok(e) = load_env(&app.env_path) {
            simulations.push(serde_json::json!({
                "name": "local",
                "owner": "local",
                "created_at": "",
                "age_secs": null,
                "pinned": true,
                "url": app.agent.base_url(),
                "environment": EnvInfo {
                    name: e.name.clone(),
                    description: e.description.clone(),
                    seed: e.seed,
                    update_every: e.update_every,
                    node_count: e.nodes.len(),
                    generator: e.generator.display().to_string(),
                    context_count: 0,
                },
                "expires_in_secs": null,
            }));
        }
    }

    let state_dir = std::path::PathBuf::from(
        std::env::var("INFRA_SIM_STATE_DIR")
            .unwrap_or_else(|_| container::DEFAULT_STATE_DIR.to_string()),
    );
    let disk_used = budget::state_dir_bytes(&state_dir);
    let progresses: std::collections::BTreeMap<String, provision::Progress> = app
        .progress
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();

    Json(serde_json::json!({
        "simulations": simulations,
        "budgets": budgets,
        "disk_used_bytes": disk_used,
        "progress": progresses,
        "now": now,
    }))
}

/// Full detail for one simulation: node states, scenarios, the green board.
async fn sim_status(
    State(app): State<Arc<AppState>>,
    AxumPath(sim): AxumPath<String>,
) -> impl IntoResponse {
    let mut errors = Vec::new();
    let now = now_secs();
    let (agent, env_path, control_path) = match target_for(&app, &sim) {
        Ok(t) => t,
        Err(e) => return (StatusCode::NOT_FOUND, Json(json_err(e))),
    };
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

    // Resolve the agent GUID once per simulation; the ML charts are addressed
    // by it.
    let guid = {
        let mut cached = app.cached_guid.lock().await;
        if !cached.contains_key(&sim) {
            match agent.machine_guid().await {
                Ok(g) => {
                    cached.insert(sim.clone(), g);
                }
                Err(e) => errors.push(e),
            }
        }
        cached.get(&sim).cloned()
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

    let first_seen = *app
        .first_seen
        .lock()
        .await
        .entry(sim.clone())
        .or_insert(now);
    let uptime_hours = Some(((now - first_seen) as f64 / 3600.0).max(0.0));
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

    let body = serde_json::to_value(StatusResponse {
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
    .unwrap_or(serde_json::Value::Null);
    (StatusCode::OK, Json(body))
}

async fn trigger(
    State(app): State<Arc<AppState>>,
    AxumPath((sim, name)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    mutate(&app, &sim, move |c| c.trigger(&name, now_secs()))
}

async fn resolve(
    State(app): State<Arc<AppState>>,
    AxumPath((sim, name)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let now = now_secs();
    mutate(&app, &sim, move |c| c.resolve(&name, now))
}

async fn resolve_all(
    State(app): State<Arc<AppState>>,
    AxumPath(sim): AxumPath<String>,
) -> impl IntoResponse {
    let now = now_secs();
    mutate(&app, &sim, move |c| c.resolve_all(now))
}

/// Read-modify-write one simulation's control file.
///
/// Against the file rather than console-held state: the CLI writes the same
/// file, and whoever wrote last is the truth. Holding a cached copy here would
/// let the console silently revert a CLI trigger.
fn mutate<F: FnOnce(&mut ControlFile)>(app: &AppState, sim: &str, f: F) -> impl IntoResponse {
    let (_, _, control_path) = match target_for(app, sim) {
        Ok(t) => t,
        Err(e) => return (StatusCode::NOT_FOUND, Json(json_err(e))),
    };
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

/// What one long-running operation is doing. Per operation id because a
/// shared console queues operations: one global handle would show another
/// user's create as yours.
async fn progress(
    State(app): State<Arc<AppState>>,
    AxumPath(op): AxumPath<String>,
) -> impl IntoResponse {
    let mut snapshot = app.progress.lock().ok().and_then(|g| g.get(&op).cloned());
    // A queued create's position is a property of the queue right now, not of
    // when it registered - refresh it so the number counts down live.
    if let Some(p) = snapshot.as_mut() {
        if p.queue_ahead_checked.is_some() && !p.done {
            let ahead = app.create_queue.queued_ahead();
            p.stage = if ahead == 0 {
                "starting - the create slot is yours".to_string()
            } else {
                format!("queued behind {ahead} create(s) - yours starts when theirs finishes")
            };
        }
    }
    Json(serde_json::json!(snapshot))
}

/// Free-form text to a proposed fleet. Read-only: nothing is written.
async fn describe(
    State(app): State<Arc<AppState>>,
    Json(req): Json<provision::DescribeRequest>,
) -> impl IntoResponse {
    // The model call is a blocking subprocess and can take tens of seconds.
    let repo = app.repo.clone();
    let op = if req.op.is_empty() {
        format!("desc-{}", now_secs())
    } else {
        req.op.clone()
    };
    let handle = provision::ProgressHandle::new(&app.progress, &op);
    handle.set(provision::Progress::new(
        if req.provider.is_empty() {
            "reading the description"
        } else {
            "asking the model to read the description"
        },
        1,
    ));
    let out = tokio::task::spawn_blocking(move || provision::describe(&repo, &req)).await;
    handle.finish(out.as_ref().err().map(|e| e.to_string()));
    match out {
        Ok(Ok(r)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "result": r, "op": op })),
        ),
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
    // Owner first, before any work: every console-created simulation must be
    // attributable on a shared host. The UI enforces this too; the API check
    // is what makes it true.
    if let Err(e) = provision::sanitise_owner_pub(&req.owner) {
        return Json(json_err(e));
    }

    // Budgets next: an over-large request never starts building.
    let budgets = budget::Budgets::load(&app.budgets_path).unwrap_or_default();
    let requested_nodes: usize = req.groups.iter().map(|g| g.count).sum();
    let live = container::list(&app.repo).len();
    let state_dir = std::path::PathBuf::from(
        std::env::var("INFRA_SIM_STATE_DIR")
            .unwrap_or_else(|_| container::DEFAULT_STATE_DIR.to_string()),
    );
    let disk = budget::state_dir_bytes(&state_dir);
    if let Err(e) = budgets.check_create(requested_nodes, live, disk) {
        return Json(json_err(e));
    }

    // One create slot. The lint inside a create is CPU-parallel across all
    // cores by design; two at once would fight each other and the host.
    let op = if req.op.is_empty() {
        format!("create-{}", now_secs())
    } else {
        req.op.clone()
    };
    let handle = provision::ProgressHandle::new(&app.progress, &op);
    app.create_queue
        .waiting
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ahead = app.create_queue.queued_ahead();
    if ahead > 0 {
        let mut p = provision::Progress::new("waiting for the create slot", 0);
        p.steps = 4;
        p.queue_ahead_checked = Some(ahead);
        handle.set(p);
    }
    let permit = app.create_queue.sem.acquire().await;
    // `waiting` counts the slot holder too (decremented at the end of this
    // handler, not at acquire), so a first waiter's position is 1 - behind
    // the running create - rather than a silent 0.
    let Ok(_permit) = permit else {
        app.create_queue
            .waiting
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        return Json(json_err("the console is shutting down".into()));
    };

    // The slot is ours: this create's progress starts for real now, and the
    // queue position field stops applying.
    let mut start = provision::Progress::new("building the environment", 4);
    start.queue_ahead_checked = None;
    handle.set(start);

    let repo = app.repo.clone();
    let budgets_path = app.budgets_path.clone();
    let worker = handle.clone();
    let finish = handle.clone();
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
        // Host policy, not a per-create choice: whether dashboards bind
        // publicly is the SRE's call for the whole box.
        let public_dashboards = budget::Budgets::load(&budgets_path)
            .map(|b| b.public_dashboards)
            .unwrap_or(false);
        let active = container::create(
            &repo,
            &name,
            &env_path,
            container::CreateOptions {
                token: Some(req.claim_token.as_str()),
                rooms: &req.claim_rooms,
                exporters: req.exporters,
                owner: &req.owner,
                public_dashboards,
            },
        )?;
        let mut notes = vec![format!(
            "running in its own container on {}",
            active.agent_url()
        )];
        if public_dashboards {
            // The host policy opened the dashboards to the network; the person
            // who just created this fleet should hear it said plainly, not
            // only in the SRE's config file.
            notes.push(
                "this host binds dashboards publicly (public_dashboards: true) - the \
                 agent has no authentication; rely on the firewall"
                    .to_string(),
            );
        }
        Ok(serde_json::json!({
            "environment": env_path.display().to_string(),
            "nodes": nodes,
            "lint_summary": lint_summary,
            "installed": true,
            "simulation": active,
            "op": op_for(&worker),
            "notes": notes.into_iter().chain([
                // Claim state and telemetry notes, in the create's own voice.
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
            ])
            .collect::<Vec<_>>(),
        }))
    })
    .await;
    finish.finish(match &out {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(e.clone()),
        Err(e) => Some(e.to_string()),
    });
    app.create_queue
        .waiting
        .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    match out {
        Ok(Ok(r)) => Json(r),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("create task failed: {e}"))),
    }
}

/// The op id a handle reports into (echoed back so the caller can poll it).
fn op_for(handle: &provision::ProgressHandle) -> String {
    // The id is embedded when the handle is built; expose it for the response.
    // (Cheap indirection: keeps the JSON shape stable if handles grow.)
    handle.op()
}

async fn claim(
    State(app): State<Arc<AppState>>,
    AxumPath(sim): AxumPath<String>,
    Json(req): Json<provision::ClaimRequest>,
) -> impl IntoResponse {
    let (agent, _, _) = match target_for(&app, &sim) {
        Ok(t) => t,
        Err(e) => return Json(json_err(e)),
    };
    match provision::claim(&agent, &req).await {
        Ok(r) => Json(serde_json::json!(r)),
        // The error must not echo the request: it carries a credential.
        Err(e) => Json(json_err(e)),
    }
}

/// Escalate a running scenario, or move the demo clock - the same operation.
async fn advance(
    State(app): State<Arc<AppState>>,
    AxumPath((sim, name)): AxumPath<(String, String)>,
    Json(req): Json<provision::AdvanceRequest>,
) -> impl IntoResponse {
    let seconds = req.seconds;
    mutate(&app, &sim, move |c| {
        let _ = provision::advance(c, &name, seconds);
    })
}

async fn reskin(
    State(app): State<Arc<AppState>>,
    AxumPath(sim): AxumPath<String>,
    Json(req): Json<provision::ReskinRequest>,
) -> impl IntoResponse {
    let repo = app.repo.clone();
    let (_, env, _) = match target_for(&app, &sim) {
        Ok(t) => t,
        Err(e) => return Json(json_err(e)),
    };
    match tokio::task::spawn_blocking(move || provision::reskin(&repo, &env, &req)).await {
        Ok(Ok(r)) => Json(serde_json::json!(r)),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("re-skin task failed: {e}"))),
    }
}

/// Every node's current labels, for the editor's starting point.
async fn labels(
    State(app): State<Arc<AppState>>,
    AxumPath(sim): AxumPath<String>,
) -> impl IntoResponse {
    let (_, env_path, _) = match target_for(&app, &sim) {
        Ok(t) => t,
        Err(e) => return Json(json_err(e)),
    };
    match tokio::task::spawn_blocking(move || provision::read_labels(&env_path)).await {
        Ok(Ok(nodes)) => Json(serde_json::json!({ "nodes": nodes })),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("label read task failed: {e}"))),
    }
}

/// Edit the user labels of the running simulation's environment. The plugin
/// picks the change up itself and restarts cleanly; the labels migrate in
/// place on the live vnodes with history intact.
async fn apply_labels(
    State(app): State<Arc<AppState>>,
    AxumPath(sim): AxumPath<String>,
    Json(req): Json<provision::LabelsRequest>,
) -> impl IntoResponse {
    let repo = app.repo.clone();
    let (_, env_path, _) = match target_for(&app, &sim) {
        Ok(t) => t,
        Err(e) => return Json(json_err(e)),
    };
    match tokio::task::spawn_blocking(move || provision::apply_labels(&repo, &env_path, &req)).await
    {
        Ok(Ok(r)) => Json(serde_json::json!(r)),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("label edit task failed: {e}"))),
    }
}

/// Start or stop correlated logs inside the running simulation.
async fn logs(
    State(app): State<Arc<AppState>>,
    AxumPath((sim, action)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    // Resolve by name rather than trusting existence: the error for a torn
    // down fleet should say so, not fail inside docker.
    if let Err(e) = target_for(&app, &sim) {
        return Json(json_err(e));
    }
    let repo = app.repo.clone();
    match tokio::task::spawn_blocking(move || container::logs(&repo, &sim, &action)).await {
        Ok(Ok(detail)) => Json(serde_json::json!({ "ok": true, "detail": detail })),
        Ok(Err(e)) => Json(json_err(e)),
        Err(e) => Json(json_err(format!("logs task failed: {e}"))),
    }
}

/// Pin or unpin a simulation against the TTL sweeper. A marker file rather
/// than a docker label: labels are immutable after create, and pin-to-keep
/// must be toggleable.
async fn pin(
    State(app): State<Arc<AppState>>,
    AxumPath(sim): AxumPath<String>,
    Json(req): Json<PinRequest>,
) -> impl IntoResponse {
    let payload = match sim_payload(&app, &sim) {
        Ok(p) => p,
        Err(e) => return Json(json_err(e)),
    };
    let marker = payload.join("pinned");
    let result = if req.pinned {
        std::fs::create_dir_all(&payload)
            .and_then(|()| std::fs::write(&marker, b""))
            .map_err(|e| format!("cannot pin: {e}"))
    } else {
        match std::fs::remove_file(&marker) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("cannot unpin: {e}")),
        }
    };
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true, "pinned": req.pinned })),
        Err(e) => Json(json_err(e)),
    }
}

#[derive(Debug, serde::Deserialize)]
struct PinRequest {
    pinned: bool,
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
    // Every containerised simulation on this machine, so a mismatch can say
    // what the caller probably meant.
    let running = container::list(&repo);
    let asked = req.name.trim().to_string();

    if !asked.is_empty() {
        if let Some(wanted) = running.iter().find(|a| a.name == asked) {
            let name = wanted.name.clone();
            let out = tokio::task::spawn_blocking(move || container::teardown(&repo, &name)).await;
            return match out {
                Ok(Ok(detail)) => Json(serde_json::json!({
                    "steps": container_teardown_steps(&asked, detail)
                })),
                Ok(Err(e)) => Json(json_err(e)),
                Err(e) => Json(json_err(format!("teardown task failed: {e}"))),
            };
        }
        return Json(json_err(format!(
            "no simulation named '{asked}'. Running: {}",
            running
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if let Some(single) = running.first() {
        let name = single.name.clone();
        let display = name.clone();
        let out = tokio::task::spawn_blocking(move || container::teardown(&repo, &name)).await;
        return match out {
            Ok(Ok(detail)) => Json(serde_json::json!({
                "steps": container_teardown_steps(&display, detail)
            })),
            Ok(Err(e)) => Json(json_err(e)),
            Err(e) => Json(json_err(format!("teardown task failed: {e}"))),
        };
    }
    if !running.is_empty() {
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

    // No containers: the legacy local install under this console's own paths.
    let env = app.env_path.clone();
    let control = app.control_path.clone();
    let out = tokio::task::spawn_blocking(move || provision::teardown(&repo, &control, &env)).await;
    match out {
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
    /// The SRE's budget file; re-read per use so tuning needs no restart.
    budgets: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut environment = PathBuf::from(DEFAULT_ENVIRONMENT);
    let mut bind = DEFAULT_BIND.to_string();
    let mut agent = DEFAULT_AGENT.to_string();
    let mut lint_clean = None;
    let mut repo = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut budgets = PathBuf::from(budget::DEFAULT_BUDGETS_PATH);

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
            "--budgets" => {
                budgets = PathBuf::from(
                    it.next()
                        .ok_or_else(|| "--budgets requires a path".to_string())?,
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
                     --budgets PATH      host budget file (default {DEFAULT_BUDGETS})\n\
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
        budgets,
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
/// Resolve a named simulation into (agent, environment, control file).
///
/// Per-name resolution is the whole point of the shared console: the previous
/// single sticky pointer meant one wrong adoption silently edited another
/// user's running fleet (SOW-0019 incident). `local` is the legacy host-install
/// fallback when no containers exist at all.
fn target_for(app: &AppState, name: &str) -> Result<(Agent, PathBuf, PathBuf), String> {
    if name == "local" {
        return Ok((
            app.agent.clone(),
            app.env_path.clone(),
            app.control_path.clone(),
        ));
    }
    let a = container::active(&app.repo, name)
        .ok_or_else(|| format!("no simulation named '{name}' - it may have been torn down"))?;
    // A containerised console cannot use the published port: the simulation
    // binds it to the host's loopback, and `host.docker.internal` is the host's
    // gateway, so nothing is listening there. Its address on the shared bridge
    // is reachable, on the agent's own port rather than the published one.
    let agent = if std::env::var("INFRA_SIM_AGENT_VIA").as_deref() == Ok("container")
        && !a.ip.trim().is_empty()
    {
        Agent::new(a.ip.clone(), 19999)
    } else {
        Agent::new(agent_host(), a.port)
    };
    Ok((agent, a.env_path(), a.control_path()))
}

/// The payload dir of a named simulation (its control file and environment
/// live there). `local` maps to the legacy install dir.
fn sim_payload(app: &AppState, name: &str) -> Result<PathBuf, String> {
    if name == "local" {
        return Ok(app
            .env_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/etc/netdata/infra-sim")));
    }
    let a = container::active(&app.repo, name)
        .ok_or_else(|| format!("no simulation named '{name}'"))?;
    Ok(PathBuf::from(a.payload))
}

/// The shared console token, from the environment. `None` means auth is off -
/// the single-operator loopback flow must keep working with no setup.
///
/// Env-or-input only, like every credential in this project: never argv
/// (world-readable via `ps`), never a committed file.
fn console_token() -> Option<String> {
    std::env::var("INFRA_SIM_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Constant-time equality, so a shared token is not side-channeled one
/// character at a time. Cheap and habitual even for an internal tool.
fn token_matches(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff: u8 = a.len().wrapping_sub(b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

/// Require a valid bearer token on every `/api` request when a token is
/// configured. The UI shell (`GET /`) stays open: it is static HTML with no
/// data, and it is where the token prompt lives.
async fn auth_layer(
    State(app): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    // `/api` only: the UI shell at `/` is static HTML with no data and is
    // where the token prompt lives - gating it would hide the prompt that
    // unlocks everything else.
    if !req.uri().path().starts_with("/api") {
        return next.run(req).await;
    }
    if let Some(expected) = &app.token {
        let presented = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !token_matches(presented, expected) {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json_err(
                    "this console requires a token - set the Authorization: Bearer header \
                     (the web UI asks for it once)"
                        .into(),
                )),
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// Archive simulations past the TTL unless pinned. The same teardown path a
/// manual removal takes, so everything is archived and nothing is lost.
async fn sweep_expired(app: &AppState) {
    let budgets = budget::Budgets::load(&app.budgets_path).unwrap_or_default();
    let now = now_secs();
    let mut checked = 0usize;
    let mut archived = 0usize;
    for a in container::list(&app.repo) {
        let Some(age) = a.age_secs(now) else {
            continue;
        };
        checked += 1;
        if !budgets.expired(age, a.pinned) {
            continue;
        }
        eprintln!(
            "infra-sim: TTL sweep - archiving '{}' ({} days old, unpinned)",
            a.name,
            age / 86_400
        );
        match container::teardown(&app.repo, &a.name) {
            Ok(detail) => {
                archived += 1;
                eprintln!("infra-sim: swept '{}': {detail}", a.name);
            }
            Err(e) => eprintln!(
                "infra-sim: TTL sweep failed for '{}' (will retry next hour): {e}",
                a.name
            ),
        }
    }
    // Heartbeat: an unattended host must be able to tell "sweep ran, nothing
    // to do" from "sweep never ran" in the logs.
    eprintln!("infra-sim: TTL sweep - {checked} simulation(s) checked, {archived} archived");
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
        first_seen: Mutex::new(Default::default()),
        cached_guid: Mutex::new(Default::default()),
        repo: args.repo.clone(),
        budgets_path: args.budgets.clone(),
        token: console_token(),
        progress: Default::default(),
        create_queue: CreateQueue::new(),
    });

    let auth_state = Arc::clone(&state);
    let token_set = state.token.is_some();

    // The TTL sweeper: simulations nobody pinned and nobody tore down are
    // disk and port budget leaking by default. Hourly is plenty - the TTL is
    // in days.
    {
        let sweeper_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tick.tick().await;
                sweep_expired(&sweeper_state).await;
            }
        });
    }

    let app = Router::new()
        .route("/", get(ui))
        .route("/api/status", get(status))
        .route("/api/sim/{name}/status", get(sim_status))
        .route("/api/sim/{name}/scenario/{scenario}/trigger", post(trigger))
        .route("/api/sim/{name}/scenario/{scenario}/resolve", post(resolve))
        .route("/api/sim/{name}/scenarios/resolve-all", post(resolve_all))
        .route("/api/sim/{name}/scenario/{scenario}/advance", post(advance))
        .route("/api/catalogue", get(catalogue))
        .route("/api/progress/{op}", get(progress))
        .route("/api/describe", post(describe))
        .route("/api/create", post(create))
        .route("/api/teardown", post(teardown))
        .route("/api/sim/{name}/claim", post(claim))
        .route("/api/sim/{name}/logs/{action}", post(logs))
        .route("/api/sim/{name}/reskin", post(reskin))
        .route("/api/sim/{name}/labels", get(labels).post(apply_labels))
        .route("/api/sim/{name}/pin", post(pin))
        .layer(middleware::from_fn_with_state(auth_state, auth_layer))
        .with_state(state);

    let addr: SocketAddr = match args.bind.parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("invalid --bind '{}': {e}", args.bind);
            return std::process::ExitCode::FAILURE;
        }
    };

    // A trust boundary is enforced, not advised. A console bound off-loopback
    // without a token is exactly the accident this SOW exists to prevent, and
    // startup is the only moment the message is guaranteed to be read.
    let loopback = addr.ip().is_loopback();
    if !loopback && !token_set {
        eprintln!(
            "refusing to bind {} without a token: everyone who can reach this port could \
             create, edit and tear down simulations.\n\
             Set one and restart:  INFRA_SIM_TOKEN=<secret> infra-sim-console --bind {}\n\
             (or bind a loopback address and put an authenticating proxy in front)",
            args.bind, args.bind,
        );
        return std::process::ExitCode::FAILURE;
    }
    eprintln!(
        "infra-sim console: auth {}",
        if token_set {
            "ON (shared token)"
        } else {
            "OFF (loopback, single operator)"
        }
    );

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
