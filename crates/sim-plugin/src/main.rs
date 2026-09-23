//! Infra-Sim external plugin.
//!
//! Emits a simulated fleet as Netdata virtual nodes over the plugins.d
//! protocol. Netdata runs this as an external plugin from a plugins directory
//! (`/etc/netdata/custom-plugins.d/` by default) and passes the collection
//! interval as the first argument.
//!
//! The hard rule this serves: only the raw data here is synthetic. Everything
//! downstream — ML training, health evaluation, Netdata AI — is the real
//! product operating on it normally.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod control;
mod emitter;
mod environment;
mod exporters;
mod logs_runtime;
mod otlp_runtime;
mod replay;
mod shutdown;
mod warmup;

use environment::Environment;
use sim_engine::llm;
use sim_engine::{NodeEngine, ScenarioSet};
use sim_spec::GeneratorSpec;

/// Where the plugin looks for its environment when Netdata launches it.
const DEFAULT_ENVIRONMENT: &str = "/etc/netdata/infra-sim/environment.yaml";
/// Control file the console writes to trigger and resolve scenarios.
const CONTROL_FILE: &str = "control.yaml";
/// Environment variable override, used by the console and by manual runs.
const ENV_VAR: &str = "INFRA_SIM_ENVIRONMENT";
/// Default journal output directory, named in the help text.
const DEFAULT_JOURNAL_DIR: &str = logs_runtime::DEFAULT_JOURNAL_DIR;

/// Port the simulated Prometheus exporters listen on. Above the agent's 19999
/// and outside the range go.d's own service discovery probes, so it cannot be
/// picked up twice.
const DEFAULT_EXPORTER_PORT: u16 = 19998;

/// The application spec the exporters publish. Never a node `service`: the
/// plugins.d path must not emit these series as well.
const EXPORTER_SPEC: &str = "prometheus-app.yaml";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Netdata surfaces plugin stderr in the agent log. Exiting non-zero
            // stops it restarting us in a loop over a bad config.
            eprintln!("infra-sim: {err}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    update_every: i64,
    environment: PathBuf,
    /// Run the fidelity lint over this many simulated hours instead of
    /// emitting, then exit.
    lint_hours: Option<i64>,
    lint_evidence: Option<PathBuf>,
    /// Build an environment from a plain-text description instead of running.
    describe: Option<String>,
    /// Environment name and hostname prefix for --describe.
    describe_name: Option<String>,
    /// Scale the reading to roughly this many nodes.
    describe_nodes: usize,
    /// Read the description with a real model instead of the keyword parser.
    llm: Option<llm::Config>,
    /// Emit correlated logs into the journal instead of metrics.
    logs: bool,
    /// Serve simulated Prometheus exporters instead of collecting.
    exporters: bool,
    exporter_port: u16,
    /// Write the go.d scrape config (vnode registry + jobs) for the exporters
    /// instead of running anything, then exit. The containerised path calls
    /// this at create time so bash never re-implements the naming that chart
    /// identities depend on. Optional argument: netdata's config root,
    /// defaulting to /etc/netdata.
    exporter_config: Option<PathBuf>,
    /// Ship the application tier's logs and traces over OTLP instead of
    /// collecting.
    otlp: bool,
    /// Netdata's OpenTelemetry receiver. gRPC, on loopback by default.
    otlp_endpoint: String,
    /// Where journal files are written; Netdata scans this path.
    journal_dir: PathBuf,
    /// Override the `systemd-journal-remote` binary location.
    journal_remote: Option<PathBuf>,
    /// Re-skin instead of running: rewrite hostnames and labels for a new
    /// prospect while preserving every GUID.
    reskin: Option<ReskinArgs>,
    /// Unix timestamp the simulated clock starts from.
    ///
    /// `spec.md` promises an environment plus a seed replays an identical
    /// world. The seed fixes the random component, but seasonality is a
    /// function of absolute time, so without pinning the clock a replay
    /// reproduces the same *shape* only when run at the same time of day.
    /// Pinning closes that gap and makes replay bit-exact.
    replay_from: Option<i64>,
    replay_recording: Option<PathBuf>,
    replay_start_at: Option<u64>,
    allow_incomplete_recording: bool,
    finalize_recording: Option<PathBuf>,
    recording_status: Option<PathBuf>,
}

/// How often the running plugin re-emits its HOST_DEFINE/label/chart
/// handshake, in seconds.
///
/// A plugins.d define migrates the host's whole label set, and go.d's
/// prometheus jobs define the same vnodes with no labels at all (a registry
/// vnode is hostname+guid only). Whoever defines last wins - so a go.d
/// restart after this plugin's start wipes every fleet label, `simulated=true`
/// included. Re-asserting on a timer makes the labelled define win again no
/// matter who restarted what, which a one-time ordering at create can never
/// guarantee. The re-emission is exactly what a plugin restart sends, whose
/// safety is proven by every respawn.
const HANDSHAKE_REASSERT_BASE_SECS: i64 = 240;

/// When the FIRST re-assert fires after process start.
///
/// The label wipe this mechanism heals happens on the agent's ~60s plugin
/// scan after a create, when go.d's prometheus jobs define the exported
/// vnodes bare. Waiting a fleet-scaled interval would leave a 3,000-node
/// fleet unlabelled for most of an hour; one early handshake is a bounded,
/// one-time cost even at that size.
const HANDSHAKE_FIRST_REASSERT_SECS: i64 = 90;

/// Fraction of samples a signal may spend on a bound before the lint fails it.
///
/// Bounds are a safety rail. A signal that reaches one regularly has stopped
/// being modelled and is being clamped, which flattens the metric — this is how
/// `system.ram free` went perfectly constant on the cache node during
/// development, and it is the artifact class an SRE spots instantly.
const PINNED_THRESHOLD: f64 = 0.001;

/// Parse Netdata's argument convention plus our own override.
///
/// Netdata passes the collection interval as a bare integer in argv[1]. A
/// `--environment <path>` flag is accepted for running the plugin by hand.
#[derive(Default)]
struct ReskinArgs {
    from_prefix: String,
    to_prefix: String,
    name: Option<String>,
    output: Option<PathBuf>,
    labels: std::collections::BTreeMap<String, String>,
}

fn parse_args() -> Result<Args, String> {
    let mut update_every = 1_i64;
    let mut environment: Option<PathBuf> = None;
    let mut lint_hours: Option<i64> = None;
    let mut lint_evidence = None;
    let mut replay_from: Option<i64> = None;
    let mut replay_recording = None;
    let mut replay_start_at = None;
    let mut allow_incomplete_recording = false;
    let mut finalize_recording = None;
    let mut recording_status = None;
    let mut reskin_args: Option<ReskinArgs> = None;
    let mut describe: Option<String> = None;
    let mut describe_name: Option<String> = None;
    let mut describe_nodes: usize = 0;
    let mut llm_cfg: Option<llm::Config> = None;
    let mut llm_model: Option<String> = None;
    let mut llm_key_env: Option<String> = None;
    let mut logs = false;
    let mut exporters = false;
    let mut exporter_port = DEFAULT_EXPORTER_PORT;
    let mut exporter_config: Option<PathBuf> = None;
    let mut otlp = false;
    let mut otlp_endpoint = otlp_runtime::DEFAULT_ENDPOINT.to_string();
    let mut journal_dir: Option<PathBuf> = None;
    let mut journal_remote: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--lint-evidence" => {
                lint_evidence = Some(PathBuf::from(
                    args.next()
                        .ok_or("--lint-evidence requires an output path")?,
                ));
            }
            "--replay-start-at" => {
                let value = args
                    .next()
                    .ok_or("--replay-start-at requires a Unix timestamp")?;
                replay_start_at = Some(
                    value
                        .parse::<u64>()
                        .ok()
                        .and_then(|seconds| seconds.checked_mul(1_000_000_000))
                        .ok_or(
                            "--replay-start-at requires a representable nonnegative Unix timestamp",
                        )?,
                );
            }
            "--allow-incomplete-recording" => allow_incomplete_recording = true,
            "--replay-recording" | "--finalize-recording" | "--recording-status" => {
                let path = PathBuf::from(
                    args.next()
                        .ok_or_else(|| format!("{arg} requires a recording directory"))?,
                );
                match arg.as_str() {
                    "--replay-recording" => replay_recording = Some(path),
                    "--finalize-recording" => finalize_recording = Some(path),
                    _ => recording_status = Some(path),
                }
            }
            "--environment" | "-e" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--environment requires a path".to_string())?;
                environment = Some(PathBuf::from(value));
            }
            "--describe" => {
                describe =
                    Some(args.next().ok_or_else(|| {
                        "--describe requires a description in quotes".to_string()
                    })?);
            }
            "--nodes" => {
                describe_nodes = args
                    .next()
                    .ok_or_else(|| "--nodes requires a count".to_string())?
                    .parse()
                    .map_err(|e| format!("--nodes: {e}"))?;
            }
            "--name" => {
                describe_name = Some(
                    args.next()
                        .ok_or_else(|| "--name requires a value".to_string())?,
                );
            }
            "--llm" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--llm requires 'anthropic' or 'openai'".to_string())?;
                let mut c = llm::Config::new(llm::Provider::parse(&value)?);
                // The key may live in a gitignored .env beside the checkout.
                c.repo = Some(PathBuf::from("."));
                llm_cfg = Some(c);
            }
            "--llm-model" => {
                llm_model = Some(
                    args.next()
                        .ok_or_else(|| "--llm-model requires a model id".to_string())?,
                );
            }
            "--llm-key-env" => {
                llm_key_env = Some(args.next().ok_or_else(|| {
                    "--llm-key-env requires an environment variable name".to_string()
                })?);
            }
            "--logs" => logs = true,
            "--exporters" => exporters = true,
            "--exporter-config" => {
                // Arity one on purpose: an optional argument here would
                // swallow a following `--environment` and strand its value.
                let dir = args.next().ok_or_else(|| {
                    "--exporter-config requires netdata's config root (e.g. /etc/netdata)"
                        .to_string()
                })?;
                exporter_config = Some(PathBuf::from(dir));
            }
            "--otlp" => otlp = true,
            "--otlp-endpoint" => {
                otlp_endpoint = args
                    .next()
                    .ok_or_else(|| "--otlp-endpoint requires HOST:PORT".to_string())?;
            }
            "--exporter-port" => {
                exporter_port = args
                    .next()
                    .ok_or_else(|| "--exporter-port requires a port".to_string())?
                    .parse()
                    .map_err(|e| format!("--exporter-port: {e}"))?;
            }
            "--journal-dir" => {
                journal_dir =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        "--journal-dir requires a path".to_string()
                    })?));
            }
            "--journal-remote" => {
                journal_remote =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        "--journal-remote requires a path".to_string()
                    })?));
            }
            "--reskin" => {
                reskin_args.get_or_insert_with(ReskinArgs::default);
            }
            flag @ ("--from-prefix" | "--to-prefix" | "--new-name" | "--output" | "--label") => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                let r = reskin_args.get_or_insert_with(ReskinArgs::default);
                match flag {
                    "--from-prefix" => r.from_prefix = value,
                    "--to-prefix" => r.to_prefix = value,
                    "--new-name" => r.name = Some(value),
                    "--output" => r.output = Some(PathBuf::from(value)),
                    "--label" => {
                        let (k, v) = value
                            .split_once('=')
                            .ok_or_else(|| format!("--label expects key=value, got '{value}'"))?;
                        r.labels.insert(k.to_string(), v.to_string());
                    }
                    _ => unreachable!(),
                }
            }
            "--replay-from" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--replay-from requires a unix timestamp".to_string())?;
                replay_from = Some(value.parse::<i64>().map_err(|_| {
                    format!("--replay-from expects a unix timestamp, got '{value}'")
                })?);
            }
            "--lint" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--lint requires a number of hours".to_string())?;
                let hours: i64 = value
                    .parse()
                    .map_err(|_| format!("--lint expects an integer, got '{value}'"))?;
                lint_hours = Some(hours.max(1));
            }
            "--help" | "-h" => {
                return Err(format!(
                    "usage: infra-sim [UPDATE_EVERY] [--environment PATH] [--lint HOURS]\n\
                     \n\
                     UPDATE_EVERY  collection interval in seconds (Netdata passes this)\n\
                     --environment path to environment.yaml \
                     (default: ${ENV_VAR} or {DEFAULT_ENVIRONMENT})\n\
                     --lint HOURS  simulate HOURS of data, report fidelity \
                     violations, and exit non-zero if any are found\n\
                     --lint-evidence PATH save fingerprinted evidence for --lint\n\
                     --replay-recording DIR replay stored raw output (metrics by default)\n\
                     --replay-start-at TS synchronize producers to one Unix start timestamp\n\
                     --allow-incomplete-recording explicitly replay a preserved prefix\n\
                     --recording-status DIR report capture state as JSON\n\
                     --finalize-recording DIR seal a stopped recording\n\
                     --replay-from TS  pin the simulated clock to unix timestamp TS \
                     for bit-exact replay of an archived environment\n\
                     \n\
                     correlated logs (a separate process from the metrics plugin):\n\
                     sudo infra-sim --logs --environment PATH\n\
                     --journal-dir DIR     where journal files are written \
                     (default: {DEFAULT_JOURNAL_DIR})\n\
                     --journal-remote PATH override the systemd-journal-remote \
                     binary\n\
                     Needs systemd-journal-remote installed and root to write \
                     the journal.\n\
                     Each node becomes its own log source in Netdata; fault \
                     lines follow whatever scenario is running.\n\
                     \n\
                     simulated Prometheus exporters (a separate process;
                     Netdata's own go.d prometheus collector scrapes them):\n\
                     infra-sim --exporters --environment PATH\n\
                     write the go.d scrape config for them and exit:\n\
                     infra-sim --exporter-config /etc/netdata --environment PATH\n\
                     --exporter-port N     listen port (default: {DEFAULT_EXPORTER_PORT})\n\
                     Serves GET /metrics/<hostname> per node, in Prometheus \n\
                     text format, moved by whatever scenario is running.\n\
                     \n\
                     build an environment from a description:\n\
                     --nodes N             scale the fleet to roughly N nodes, \
                     keeping the ratio between tiers\n\
                     --describe \"3 web servers behind an nginx load balancer, a \\\n\
                       postgres primary and 2 redis caches\" --name acme \\\n\
                       --environment environments/acme.yaml\n\
                     \n\
                     the description is read by a keyword parser offline. For a \
                     description written in the prospect's own words, add:\n\
                     --llm anthropic          read it with Claude \
                     (needs $ANTHROPIC_API_KEY)\n\
                     --llm openai             read it with OpenAI \
                     (needs $OPENAI_API_KEY)\n\
                     --llm-model MODEL        override the model id\n\
                     --llm-key-env VAR        read the key from a different \
                     variable\n\
                     $ANTHROPIC_BASE_URL / $OPENAI_BASE_URL point at an \
                     internal gateway.\n\
                     The model only chooses among roles and service specs that \
                     exist here; it never writes the environment file.\n\
                     \n\
                     re-skin a warm environment for a new prospect:\n\
                     --reskin --from-prefix sim- --to-prefix acme- \\\n\
                       [--new-name NAME] [--label key=value]... [--output PATH]\n\
                     GUIDs are never changed; the fleet keeps its history and \
                     trained ML models."
                ));
            }
            other => {
                // Netdata's bare interval argument.
                if let Ok(v) = other.parse::<i64>() {
                    update_every = v.max(1);
                } else {
                    return Err(format!("unrecognised argument '{other}'"));
                }
            }
        }
    }

    let environment = environment
        .or_else(|| std::env::var_os(ENV_VAR).map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ENVIRONMENT));

    // Accepting these silently without --llm would run the keyword parser while
    // the operator believes a model answered.
    match &mut llm_cfg {
        Some(cfg) => {
            if let Some(m) = llm_model {
                cfg.model = m;
            }
            if let Some(v) = llm_key_env {
                cfg.key_env = v;
            }
        }
        None if llm_model.is_some() || llm_key_env.is_some() => {
            return Err("--llm-model and --llm-key-env only apply with --llm; add \
                 --llm anthropic (or --llm openai)"
                .into());
        }
        None => {}
    }

    if lint_evidence.is_some() && lint_hours.is_none() {
        return Err("--lint-evidence requires --lint".into());
    }
    let recording_commands = usize::from(replay_recording.is_some())
        + usize::from(finalize_recording.is_some())
        + usize::from(recording_status.is_some());
    if recording_commands > 1 {
        return Err("choose one recording command: replay, finalize, or status".into());
    }
    if replay_start_at.is_some() && replay_recording.is_none() {
        return Err("--replay-start-at requires --replay-recording".into());
    }
    if allow_incomplete_recording && replay_recording.is_none() {
        return Err("--allow-incomplete-recording requires --replay-recording".into());
    }
    if recording_commands != 0
        && (lint_hours.is_some()
            || replay_from.is_some()
            || describe.is_some()
            || reskin_args.is_some()
            || exporter_config.is_some())
    {
        return Err("recording commands cannot be combined with lint, regeneration, describe, reskin, or exporter configuration".into());
    }

    Ok(Args {
        update_every,
        environment,
        lint_hours,
        lint_evidence,
        replay_from,
        replay_recording,
        replay_start_at,
        allow_incomplete_recording,
        finalize_recording,
        recording_status,
        reskin: reskin_args,
        describe,
        describe_name,
        describe_nodes,
        llm: llm_cfg,
        logs,
        journal_dir: journal_dir
            .unwrap_or_else(|| PathBuf::from(logs_runtime::DEFAULT_JOURNAL_DIR)),
        journal_remote,
        exporters,
        exporter_port,
        exporter_config,
        otlp,
        otlp_endpoint,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    if let Some(dir) = &args.recording_status {
        let status = sim_engine::recording::status(dir).map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&status).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if let Some(dir) = &args.finalize_recording {
        let status = sim_engine::recording::finalize(dir).map_err(|e| e.to_string())?;
        println!(
            "{}",
            serde_json::to_string(&status).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    if let Some(dir) = &args.replay_recording {
        return replay::run(dir, &args);
    }

    if let Some(text) = &args.describe {
        return do_describe(
            text,
            args.describe_name.as_deref(),
            args.describe_nodes,
            &args.environment,
            args.llm.as_ref(),
        );
    }

    if let Some(r) = &args.reskin {
        return do_reskin(&args.environment, r);
    }

    if args.lint_hours.is_none() && args.exporter_config.is_none() {
        shutdown::install()?;
        if std::env::var_os("INFRA_SIM_LIFECYCLE_FILE")
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|state| state.trim() == "stopping")
        {
            return Ok(());
        }
    }
    let evidence_before = args
        .lint_evidence
        .as_ref()
        .map(|_| sim_engine::lint_evidence::inputs(&args.environment))
        .transpose()?;
    let env = Environment::load(&args.environment).map_err(|e| e.to_string())?;

    if args.exporters {
        return do_exporters(&env, &args);
    }

    if let Some(dir) = &args.exporter_config {
        return do_exporter_config(&env, dir);
    }

    if args.otlp {
        return do_otlp(&env, &args);
    }

    let spec_path = env.generator_path(&args.environment);
    let spec_raw = std::fs::read_to_string(&spec_path).map_err(|e| {
        format!(
            "failed to read generator spec '{}': {e}",
            spec_path.display()
        )
    })?;
    let spec = GeneratorSpec::from_yaml(&spec_raw).map_err(|e| e.to_string())?;

    // The environment's update_every is the fleet's intent; Netdata's argument
    // is the agent's. Take the coarser of the two so we never emit faster than
    // the agent collects.
    let update_every = args.update_every.max(env.update_every);

    // One composed spec per distinct service set. Nodes sharing a service set
    // share the spec, so a 50-node fleet does not hold 50 copies.
    let specs_dir = env.specs_path(&args.environment);
    let mut composed: std::collections::BTreeMap<String, Arc<GeneratorSpec>> =
        std::collections::BTreeMap::new();
    for node in &env.nodes {
        // The base spec is part of the key. Without it a mixed fleet would take
        // whichever node was composed first and hand its charts to the other
        // class - a wrong-data bug, not a crash.
        let base_path = env.node_generator_path(node, &args.environment);
        let key = format!("{}\0{}", base_path.display(), node.services.join("+"));
        if composed.contains_key(&key) {
            continue;
        }
        let mut merged = if base_path == spec_path {
            spec.clone()
        } else {
            let raw = std::fs::read_to_string(&base_path).map_err(|e| {
                format!(
                    "node '{}' needs base spec '{}': {e}",
                    node.hostname,
                    base_path.display()
                )
            })?;
            GeneratorSpec::from_yaml(&raw).map_err(|e| e.to_string())?
        };
        for service in &node.services {
            // Hand-authored specs sit directly in the specs directory; the ones
            // synced from Netdata's collector metadata sit in its `generated`
            // subdirectory. A hand-authored spec wins, so a service can be
            // promoted from generated to deeply modelled without renaming it.
            let path = specs_dir.join(format!("{service}.yaml"));
            let path = if path.exists() {
                path
            } else {
                specs_dir.join("generated").join(format!("{service}.yaml"))
            };
            let svc = load_service_spec(&specs_dir, &path, &node.hostname)?;
            merged.merge(&svc).map_err(|e| e.to_string())?;
        }
        composed.insert(key, Arc::new(merged));
    }

    let profiles = env.profiles();
    let services = env.services();
    eprintln!(
        "infra-sim: environment '{}' - {} node(s), base spec '{}' ({} contexts), \
         services: {}, seed {}, update_every {}s",
        env.name,
        profiles.len(),
        spec.name,
        spec.contexts.len(),
        if services.is_empty() {
            "none".to_string()
        } else {
            services.join(", ")
        },
        env.seed,
        update_every,
    );

    // Rank each node among its same-role peers, in environment order:
    // `node_index` scenario targets ("the first switch") resolve against it,
    // and only this loop can know the order.
    let mut role_seen: std::collections::BTreeMap<String, usize> = Default::default();
    let ranks: Vec<Option<usize>> = env
        .nodes
        .iter()
        .map(|n| {
            n.role.as_ref().map(|r| {
                let c = role_seen.entry(r.clone()).or_insert(0);
                *c += 1;
                *c - 1
            })
        })
        .collect();
    let mut engines: Vec<NodeEngine> = profiles
        .iter()
        .zip(&env.nodes)
        .zip(ranks)
        .map(|((p, n), rank)| {
            let key = format!(
                "{}\0{}",
                env.node_generator_path(n, &args.environment).display(),
                n.services.join("+")
            );
            NodeEngine::with_rank(Arc::clone(&composed[&key]), p.clone(), env.seed, rank)
        })
        .collect();

    if args.logs {
        return do_logs(&env, &args, engines);
    }

    if let Some(hours) = args.lint_hours {
        let result = (|| {
            let library = control::load_library(&env.scenario_path(&args.environment))?;
            check_scenarios(&composed, &env, &library, &specs_dir)?;
            let scenario_baseline = engines.clone();
            lint(&mut engines, hours, update_every)?;
            let roles: Vec<&str> = env.nodes.iter().filter_map(|n| n.role.as_deref()).collect();
            lint_scenarios(&scenario_baseline, &library, &roles, update_every)?;
            lint_applications(&env, &args, &library, &roles, update_every)
        })();
        if let (Some(path), Some(before)) = (&args.lint_evidence, evidence_before) {
            let after = sim_engine::lint_evidence::inputs(&args.environment)?;
            if before != after {
                return Err(
                    "lint inputs changed during validation; no evidence was written".into(),
                );
            }
            let executable = if cfg!(target_os = "linux") {
                PathBuf::from("/proc/self/exe")
            } else {
                std::env::current_exe().map_err(|e| e.to_string())?
            };
            sim_engine::lint_evidence::write(
                path,
                before,
                &executable,
                std::env::var("INFRA_SIM_LINT_RUNTIME_IMAGE").ok(),
                hours,
                result.is_ok(),
            )?;
        }
        return result;
    }

    let base_dir = args
        .environment
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let library = control::load_library(&env.scenario_path(&args.environment))?;
    eprintln!(
        "infra-sim: {} scenario(s) available: {}",
        library.len(),
        if library.is_empty() {
            "none".to_string()
        } else {
            library.keys().cloned().collect::<Vec<_>>().join(", ")
        }
    );
    let mut control = control::ControlChannel::new(base_dir.join(CONTROL_FILE), library);

    let stdout = io::stdout();
    let recorder =
        sim_engine::recording::Recorder::from_environment(sim_engine::recording::Producer::Metrics)
            .map_err(|e| format!("cannot initialize recording: {e}"))?;
    control.record_to(recorder.clone());
    let mut out = BufWriter::new(sim_engine::recording::RecordedWriter::new(
        stdout.lock(),
        recorder,
        sim_engine::recording::Kind::Metrics,
        String::new(),
    ));

    emitter::define_hosts(&mut out, &profiles).map_err(write_err)?;
    for engine in &engines {
        emitter::declare_charts(&mut out, engine, update_every).map_err(write_err)?;
    }
    out.flush().map_err(write_err)?;

    let role_list: Vec<&str> = env.nodes.iter().filter_map(|n| n.role.as_deref()).collect();
    if env.warmup_incidents {
        eprintln!(
            "infra-sim: warm-up incidents enabled - minor, auto-resolving faults on a \
             deterministic schedule so the alert log has texture before a demo"
        );
    }
    let mut warm_reported = String::new();

    let interval = update_every as f64;
    let mut next = align_to_interval(now_secs(), update_every);

    // In replay mode the simulated clock advances one interval per tick from a
    // fixed origin, while wall-clock pacing is unchanged. Netdata timestamps
    // samples on arrival, so the data still lands at "now" - only the values
    // are reproduced exactly.
    if let Some(origin) = args.replay_from {
        eprintln!(
            "infra-sim: replay mode - simulated clock pinned to {origin}, output is \
             reproducible for this environment and seed"
        );
    }
    let mut replay_clock = args.replay_from;
    // Next handshake re-assertion; see HANDSHAKE_REASSERT_SECS. Starts after
    // the initial handshake above, which every process start performs anyway.
    // The interval scales with the fleet: the re-emission cost is linear in
    // node count (a 3,000-node fleet re-asserts on the order of a million
    // protocol lines), so large fleets assert less often. 240s at demo scale,
    // ~= the node count in seconds beyond that - 50 minutes at 3,000 nodes,
    // where the fleet was created days early anyway and load, not settle
    // time, is the constraint.
    let reassert_interval = HANDSHAKE_REASSERT_BASE_SECS.max(profiles.len() as i64);
    // First one early (covers the create-time label-wipe window), then the
    // scaled interval - see HANDSHAKE_FIRST_REASSERT_SECS.
    let mut next_reassert = next + reassert_interval.min(HANDSHAKE_FIRST_REASSERT_SECS);

    // Two things we exit cleanly for: our own plugin file being removed
    // (teardown) and the environment being rewritten under us (re-skin).
    let own_path = std::env::args()
        .next()
        .map(PathBuf::from)
        .filter(|p| p.exists());
    let env_stamp = file_stamp(&args.environment);

    loop {
        sleep_until(next);
        if shutdown::requested() {
            return Ok(());
        }

        // Exit 0 when our own plugin file is removed.
        //
        // netdata disables a plugin that "exited abnormally" - dying from a
        // signal counts - and the flag survives until the agent restarts
        // (netdata/netdata @ c23face0bd94 src/plugins.d/plugins_d.c:86-91,
        // `plugin_set_disabled`). So a fleet installed after a teardown would
        // never be launched: correct files on disk, no process, nothing logged.
        //
        // Exiting cleanly the moment the file disappears means netdata takes the
        // success path instead, keeps the plugin enabled, and starts the next
        // fleet on its own scan.
        if own_path.as_deref().is_some_and(|p| !p.exists()) {
            eprintln!(
                "infra-sim: plugin file removed - exiting cleanly so the agent keeps it enabled"
            );
            out.flush().map_err(write_err)?;
            return Ok(());
        }

        // A re-skin rewrites the environment in place. Restarting is how the
        // renamed fleet reaches the agent, and exiting cleanly is how the agent
        // stays willing to restart us.
        if env_stamp.is_some() && file_stamp(&args.environment) != env_stamp {
            eprintln!(
                "infra-sim: environment changed - exiting cleanly so the agent restarts us with it"
            );
            out.flush().map_err(write_err)?;
            return Ok(());
        }

        // Re-assert the labelled handshake when due: whichever collector
        // defined these vnodes last owns their label set, and go.d defines
        // them bare. This is the tick-loop side of that contention.
        if tick_due(next_reassert, next) {
            emitter::define_hosts(&mut out, &profiles).map_err(write_err)?;
            for engine in &engines {
                emitter::declare_charts(&mut out, engine, update_every).map_err(write_err)?;
            }
            out.flush().map_err(write_err)?;
            next_reassert = next + reassert_interval;
        }

        let tick_at = match replay_clock {
            Some(t) => {
                replay_clock = Some(t + update_every);
                t
            }
            None => next,
        };
        next += update_every;

        // Cheap in the common case - a single stat - and it is what makes a
        // scenario triggerable mid-demo without restarting anything.
        if let Some(change) = control.poll(tick_at) {
            eprintln!("infra-sim: {change}");
        }
        // A deliberately triggered scenario always wins: if an SE is running a
        // demo, warm-up noise must not be layered on top of it.
        let live = control.scenarios();
        let warm;
        let scenarios = if live.is_empty() && env.warmup_incidents {
            warm = warmup::active(control.library(), &role_list, env.seed, tick_at);
            if let Some(msg) = warmup::describe_active(&warm, tick_at) {
                if warm_reported != msg {
                    eprintln!("infra-sim: {msg}");
                    warm_reported = msg;
                }
            }
            &warm
        } else {
            live
        };

        for engine in engines.iter_mut() {
            let samples = engine.tick(scenarios, tick_at, interval);
            let guid = engine.profile().guid.clone();
            emitter::emit_samples(&mut out, &guid, &samples).map_err(write_err)?;
        }

        // Netdata reads us over a pipe, so an unflushed buffer looks exactly
        // like a stalled collector.
        out.flush().map_err(write_err)?;
    }
}

/// Run the correlated-logs writer.
///
/// Separate process, shared composed model and control inputs. Matching noisy
/// values requires matching tick history; restarts begin a new history.
fn do_logs(env: &Environment, args: &Args, engines: Vec<NodeEngine>) -> Result<(), String> {
    let remote_bin = logs_runtime::find_journal_remote(args.journal_remote.as_deref())?;
    let update_every = args.update_every.max(env.update_every);
    let generators = engines
        .into_iter()
        .zip(&env.nodes)
        .map(|(engine, node)| sim_engine::logs::LogGenerator::new(engine, &node.services, env.seed))
        .collect();

    let base_dir = args
        .environment
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let library = control::load_library(&env.scenario_path(&args.environment))?;
    let mut control = control::ControlChannel::new(base_dir.join(CONTROL_FILE), library);

    let recorder =
        sim_engine::recording::Recorder::from_environment(sim_engine::recording::Producer::Journal)
            .map_err(|e| format!("cannot initialize recording: {e}"))?;
    control.record_to(recorder.clone());
    let mut runtime =
        logs_runtime::LogsRuntime::start(generators, &args.journal_dir, &remote_bin, recorder)?;

    eprintln!(
        "infra-sim logs: {} node(s) -> {} (via {})",
        env.nodes.len(),
        args.journal_dir.display(),
        remote_bin.display()
    );
    for file in runtime.files() {
        eprintln!("  {}", file.display());
    }
    eprintln!(
        "infra-sim logs: Netdata reads these as separate log sources named after each node. \
         Faults are matched on signals, so any scenario that moves a modelled signal \
         produces matching log lines."
    );

    let env_stamp = file_stamp(&args.environment);
    let roles: Vec<&str> = env.nodes.iter().filter_map(|n| n.role.as_deref()).collect();
    let interval = update_every as f64;
    let mut next = align_to_interval(now_secs(), update_every);
    let mut total = 0usize;
    let mut reported = now_secs();

    loop {
        sleep_until(next);
        if shutdown::requested() {
            return Ok(());
        }
        let tick_at = next;
        next += update_every;

        if let Some(change) = control.poll(tick_at) {
            eprintln!("infra-sim logs: {change}");
        }

        if env_stamp.is_some() && file_stamp(&args.environment) != env_stamp {
            eprintln!("infra-sim logs: environment changed; exiting for supervisor restart");
            return Ok(());
        }
        let warm;
        let live = control.scenarios();
        let scenarios = if live.is_empty() && env.warmup_incidents {
            warm = warmup::active(control.library(), &roles, env.seed, tick_at);
            &warm
        } else {
            live
        };
        total += runtime.tick(scenarios, tick_at, interval)?;

        // Periodic, not per-tick: an operator wants to know it is alive without
        // the output becoming its own log flood.
        if tick_at - reported >= 300 {
            eprintln!("infra-sim logs: {total} entries written so far");
            reported = tick_at;
        }
    }
}

/// Modification time and length of a file, for detecting a rewrite.
///
/// Both, because a re-skin can produce a file of the same length within the
/// same second, and mtime alone has one-second resolution on some filesystems.
fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// Load a service spec, resolving `extends:` first.
///
/// A hand-authored spec may build on a generated one: the generated spec brings
/// the breadth Netdata's own collector reports, the hand-authored one overrides
/// the contexts it models more carefully and contributes the signals scenarios
/// target by name.
fn load_service_spec(
    specs_dir: &Path,
    path: &Path,
    hostname: &str,
) -> Result<GeneratorSpec, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "node '{hostname}' needs service spec '{}': {e}",
            path.display()
        )
    })?;
    let spec = GeneratorSpec::from_yaml(&raw).map_err(|e| e.to_string())?;

    if spec.extends.is_empty() {
        return Ok(spec);
    }

    let mut composed: Option<GeneratorSpec> = None;
    for base_id in &spec.extends {
        let base_path = specs_dir.join(format!("{base_id}.yaml"));
        let base_raw = std::fs::read_to_string(&base_path).map_err(|e| {
            format!(
                "spec '{}' extends '{base_id}', which is missing at '{}': {e}",
                spec.name,
                base_path.display()
            )
        })?;
        let base = GeneratorSpec::from_yaml(&base_raw).map_err(|e| e.to_string())?;
        // One level only. A chain would be a spec hierarchy, which is more
        // machinery than "take the generated breadth" needs.
        if !base.extends.is_empty() {
            return Err(format!(
                "spec '{base_id}' itself uses extends:; only one level is supported"
            ));
        }
        match composed.as_mut() {
            Some(c) => c.overlay(&base),
            None => composed = Some(base),
        }
    }

    let mut out = composed.expect("extends is non-empty");
    out.overlay(&spec);
    Ok(out)
}

/// Serve one simulated Prometheus exporter per node until killed.
/// Ship the application tier's logs and traces over OTLP.
///
/// Mirrors `do_exporters`: the same application spec, sampled through the same
/// engine, so the metrics a prospect scrapes and the telemetry they trace are
/// one description of one service rather than two that can disagree.
fn do_otlp(env: &Environment, args: &Args) -> Result<(), String> {
    // Deliberately the exporter spec, not a node `service`: these signals must
    // never also reach the plugins.d path, or the node would carry the same
    // series twice.
    let spec_path = env.specs_path(&args.environment).join(EXPORTER_SPEC);
    let raw = std::fs::read_to_string(&spec_path).map_err(|e| {
        format!(
            "OTLP telemetry needs the application spec '{}': {e}",
            spec_path.display()
        )
    })?;
    let spec = Arc::new(GeneratorSpec::from_yaml(&raw).map_err(|e| e.to_string())?);

    // One service name for the whole tier, because that is how a service is
    // deployed: many hosts, one `service.name`. The host stays visible as a
    // resource attribute.
    let service = format!("{}-storefront", env.name);
    let namespace = env.name.clone();

    let built: Vec<otlp_runtime::Node> = application_engines(env, spec)
        .into_iter()
        .map(|(profile, engine)| {
            let role = profile.role.as_deref().unwrap_or("node");
            let telemetry =
                sim_engine::otel::AppTelemetry::new(&profile.hostname, role, &service, env.seed);
            otlp_runtime::Node::new(engine, telemetry, &env.name, &namespace)
        })
        .collect();

    let base_dir = args
        .environment
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let library = control::load_library(&env.scenario_path(&args.environment))?;
    let shared = Arc::new(std::sync::Mutex::new(sim_engine::ScenarioSet::default()));
    let recorder =
        sim_engine::recording::Recorder::from_environment(sim_engine::recording::Producer::Otlp)
            .map_err(|e| format!("cannot initialize recording: {e}"))?;
    let control_recorder = recorder.clone();
    let poller = Arc::clone(&shared);
    let control_path = base_dir.join(CONTROL_FILE);
    let control_thread = std::thread::spawn(move || {
        let mut control = control::ControlChannel::new(control_path, library);
        control.record_to(control_recorder);
        while !shutdown::requested() {
            let at = now_secs();
            if let Some(change) = control.poll(at) {
                eprintln!("infra-sim otlp: {change}");
            }
            if let Ok(mut guard) = poller.lock() {
                *guard = control.scenarios().clone();
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the OTLP runtime: {e}"))?;
    let result = runtime.block_on(otlp_runtime::run(
        &args.otlp_endpoint,
        built,
        shared,
        std::time::Duration::from_secs(1),
        recorder,
    ));
    shutdown::request();
    let _ = control_thread.join();
    result
}

/// Whether a role runs the instrumented application.
///
/// A switch does not emit storefront spans, and a database node is the thing
/// *called* by one rather than the caller. Restricting this keeps a trace
/// looking like a request instead of like every node pretending to be a web
/// server.
fn is_app_tier(role: Option<&str>) -> bool {
    matches!(role, Some("web") | Some("lb") | Some("k8s-worker"))
}

/// Keep application scenario targeting aligned with the plugins.d fleet order.
fn application_engines(
    env: &Environment,
    spec: Arc<GeneratorSpec>,
) -> Vec<(sim_engine::NodeProfile, NodeEngine)> {
    let mut role_seen = std::collections::BTreeMap::<String, usize>::new();
    env.profiles()
        .into_iter()
        .filter(|profile| is_app_tier(profile.role.as_deref()))
        .map(|profile| {
            let rank = profile.role.as_ref().map(|role| {
                let next = role_seen.entry(role.clone()).or_default();
                let rank = *next;
                *next += 1;
                rank
            });
            let engine = NodeEngine::with_rank(Arc::clone(&spec), profile.clone(), env.seed, rank);
            (profile, engine)
        })
        .collect()
}

fn do_exporters(env: &Environment, args: &Args) -> Result<(), String> {
    // The exporter spec is deliberately *not* one of the node's `services`: it
    // must not also be composed onto the plugins.d path, or every series would
    // appear on the node twice.
    let spec_path = env.specs_path(&args.environment).join(EXPORTER_SPEC);
    let raw = std::fs::read_to_string(&spec_path).map_err(|e| {
        format!(
            "exporters need the application spec '{}': {e}",
            spec_path.display()
        )
    })?;
    let spec = Arc::new(GeneratorSpec::from_yaml(&raw).map_err(|e| e.to_string())?);

    let built: Vec<exporters::Exporter> = application_engines(env, spec)
        .into_iter()
        .map(|(profile, engine)| {
            let role = profile.role.clone().unwrap_or_else(|| "node".into());
            exporters::Exporter::new(profile.hostname, role, engine)
        })
        .collect();

    let addr = (std::net::Ipv4Addr::LOCALHOST, args.exporter_port);
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|e| format!("cannot listen on 127.0.0.1:{}: {e}", args.exporter_port))?;

    eprintln!(
        "infra-sim exporters: {} endpoint(s) on http://127.0.0.1:{}",
        built.len(),
        args.exporter_port
    );
    for e in &built {
        eprintln!("  /metrics/{}", e.hostname);
    }

    // The control channel is polled on its own thread so a scrape never waits
    // on a file read, and so scenario state is shared with the metrics plugin
    // through exactly the same file.
    let base_dir = args
        .environment
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let library = control::load_library(&env.scenario_path(&args.environment))?;
    let shared = Arc::new(std::sync::Mutex::new(sim_engine::ScenarioSet::default()));
    let recorder = sim_engine::recording::Recorder::from_environment(
        sim_engine::recording::Producer::Exporters,
    )
    .map_err(|e| format!("cannot initialize recording: {e}"))?;
    let control_recorder = recorder.clone();
    let poller = Arc::clone(&shared);
    let control_path = base_dir.join(CONTROL_FILE);
    let control_thread = std::thread::spawn(move || {
        let mut control = control::ControlChannel::new(control_path, library);
        control.record_to(control_recorder);
        while !shutdown::requested() {
            let at = now_secs();
            if let Some(change) = control.poll(at) {
                eprintln!("infra-sim exporters: {change}");
            }
            if let Ok(mut guard) = poller.lock() {
                *guard = control.scenarios().clone();
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });

    let result = exporters::serve(listener, built, shared, recorder).map_err(|e| e.to_string());
    shutdown::request();
    let _ = control_thread.join();
    result
}

/// Write the go.d scrape config for a fleet's application tier.
///
/// Two files under netdata's own configuration tree: the vnode registry go.d
/// reads once at startup, and the prometheus jobs with their shared app and
/// re-skin-stable job names (see `sim_engine::exporter_config` for why the
/// names look the way they do). The caller must restart go.d once afterwards
/// - nothing else makes it re-read the registry.
fn do_exporter_config(env: &Environment, netdata_dir: &Path) -> Result<(), String> {
    let nodes: Vec<sim_engine::exporter_config::NodeRef> = env
        .profiles()
        .into_iter()
        .filter(|p| is_app_tier(p.role.as_deref()))
        .map(|p| sim_engine::exporter_config::NodeRef {
            hostname: p.hostname.clone(),
            guid: p.guid.clone(),
            role: p.role.clone().unwrap_or_else(|| "node".into()),
        })
        .collect();
    if nodes.is_empty() {
        println!(
            "infra-sim exporter config: no application-tier nodes (web, lb, k8s-worker) - \
             nothing to export"
        );
        return Ok(());
    }

    let vnodes_path = netdata_dir.join("vnodes/infra-sim.conf");
    let god_path = netdata_dir.join("go.d/prometheus.conf");
    for path in [&vnodes_path, &god_path] {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(
        &vnodes_path,
        sim_engine::exporter_config::vnodes_conf(&nodes),
    )
    .map_err(|e| format!("cannot write {}: {e}", vnodes_path.display()))?;
    std::fs::write(
        &god_path,
        sim_engine::exporter_config::go_d_conf(&nodes, DEFAULT_EXPORTER_PORT),
    )
    .map_err(|e| format!("cannot write {}: {e}", god_path.display()))?;

    println!(
        "infra-sim exporter config: {} job(s), app '{}' -> {} (restart go.d to load it)",
        nodes.len(),
        sim_engine::exporter_config::APP,
        god_path.display()
    );
    Ok(())
}

/// Build an environment from a text description and write it out.
fn do_describe(
    text: &str,
    name: Option<&str>,
    target_nodes: usize,
    output: &Path,
    llm: Option<&llm::Config>,
) -> Result<(), String> {
    // The rendered file points its `specs:` at ../specs, so the model is offered
    // exactly the specs the resulting environment will be able to load.
    let specs_dir = output
        .parent()
        .unwrap_or(Path::new("."))
        .join("..")
        .join("specs");

    let mut suggested_name = None;
    let reading = match llm {
        Some(cfg) => {
            eprintln!(
                "infra-sim: reading the description with {} ({})...",
                cfg.model, cfg.key_env
            );
            // A failure here is reported, never quietly downgraded to the
            // keyword parser: an SE who asked for a model reading and silently
            // got a weaker one has no way to tell.
            let p = llm::propose(cfg, text, &specs_dir)?;
            println!("read by {}:\n", p.model);
            for note in &p.notes {
                println!("  note: {note}");
            }
            for c in &p.corrections {
                // A plan we had to adjust is not a plan the model produced.
                println!("  adjusted: {c}");
            }
            if !p.notes.is_empty() || !p.corrections.is_empty() {
                println!();
            }
            suggested_name = p.suggested_name;
            let mut r = p.reading;
            r.unrecognised = p.unsupported;
            r
        }
        None => {
            // The offline reader still gets the full catalogue, so a named
            // integration resolves whether or not a model was used.
            let installed = llm::installable_services(&specs_dir);
            sim_engine::describe::parse_with_services(text, &installed)
        }
    };
    let mut reading = reading;

    // Applied after the reading, so the description is read as written and the
    // fleet size is a separate, deterministic step.
    if let Some(note) = sim_engine::describe::scale_to_target(&mut reading, target_nodes) {
        println!("  adjusted: {note}\n");
    }

    if reading.groups.is_empty() {
        return Err(format!(
            "nothing recognisable in '{text}'.\n\
             Known roles: load balancer, web/app server, database (postgres/mysql), \
             cache (redis), kubernetes control plane, kubernetes worker, edge gateway.\n\
             Try: --describe \"3 web servers behind an nginx load balancer, a postgres \
             primary and 2 redis caches\"\n\
             A description in the prospect's own vocabulary reads better with \
             --llm anthropic."
        ));
    }

    // --name stays authoritative: it fixes the seed, the hostname prefix and
    // therefore every GUID, so it must not move when a model is re-run.
    let name = name
        .map(str::to_string)
        .or(suggested_name)
        .unwrap_or_else(|| "described".to_string());
    let prefix = format!("{name}-");
    // Derived from the name, so the same description reproduces the same world
    // rather than a new one each time it is run.
    let seed = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });

    let yaml = sim_engine::describe::render(&reading, &name, seed, &prefix);

    if let Some(dir) = output.parent() {
        sim_engine::reskin::check_guid_uniqueness(dir, &yaml, output)?;
    }
    std::fs::write(output, &yaml)
        .map_err(|e| format!("cannot write '{}': {e}", output.display()))?;

    let total: usize = reading.groups.iter().map(|g| g.count).sum();
    println!("read {total} node(s) from the description:\n");
    for g in &reading.groups {
        println!(
            "  {:>3} x {:<18} {:<26} {:<22} <- \"{}\"",
            g.count,
            g.role,
            format!("{prefix}{}-NN", g.effective_slug()),
            if g.services.is_empty() {
                "(base only)".into()
            } else {
                g.services.join(", ")
            },
            g.source.trim()
        );
    }
    if !reading.unrecognised.is_empty() {
        // Surfaced rather than ignored: a fleet missing what the prospect asked
        // about is worse than one that admits it.
        println!("\nnot modelled, so nothing was generated for these:");
        for u in &reading.unrecognised {
            println!("  \"{}\"", u.trim());
        }
    }
    println!("\nwritten to {}", output.display());
    println!(
        "Review it, then: infra-sim --environment {} --lint 2",
        output.display()
    );
    Ok(())
}

/// Re-skin an environment and write the result.
fn do_reskin(env_path: &Path, r: &ReskinArgs) -> Result<(), String> {
    let source = std::fs::read_to_string(env_path)
        .map_err(|e| format!("cannot read '{}': {e}", env_path.display()))?;

    let plan = sim_engine::reskin::Plan {
        from_prefix: r.from_prefix.clone(),
        to_prefix: r.to_prefix.clone(),
        name: r.name.clone(),
        labels: r.labels.clone(),
    };
    let outcome = sim_engine::reskin::reskin(&source, &plan)?;

    let output = r.output.clone().unwrap_or_else(|| env_path.to_path_buf());

    // Writing a new file beside the original creates a second environment
    // carrying the same GUIDs, which cannot both be claimed.
    if output != env_path {
        let dir = output.parent().unwrap_or(Path::new("."));
        sim_engine::reskin::check_guid_uniqueness(dir, &outcome.yaml, &output)?;
    }

    std::fs::write(&output, &outcome.yaml)
        .map_err(|e| format!("cannot write '{}': {e}", output.display()))?;

    println!("re-skinned {} node(s):", outcome.renamed.len());
    for (old, new) in &outcome.renamed {
        println!("  {old} -> {new}");
    }
    println!("\nwritten to {}", output.display());
    println!(
        "GUIDs unchanged, so the fleet keeps its history, trained ML models and alert log.\n\
         Only one environment with these GUIDs may be claimed at a time."
    );
    Ok(())
}

/// Verify every scenario targets things that actually exist.
///
/// A scenario naming a signal the generator does not define, or a host the
/// environment does not contain, produces no effect at all - the trigger
/// appears to work and nothing happens. That is the worst failure mode this
/// project has: it surfaces in front of a prospect, mid-sentence, with no error
/// anywhere to explain it.
/// Whether any spec on disk defines this signal.
///
/// Distinguishes "this fleet does not run that software" from "this signal name
/// is a typo". The first is normal and the second is a bug that would otherwise
/// surface as a scenario step doing nothing, mid-demo, with nothing logged.
fn signal_exists_somewhere(specs_dir: &Path, signal: &str) -> bool {
    let needle = format!("\n  {signal}:");
    for dir in [specs_dir.to_path_buf(), specs_dir.join("generated")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            if std::fs::read_to_string(&path)
                .map(|t| t.contains(&needle))
                .unwrap_or(false)
            {
                return true;
            }
        }
    }
    false
}

/// The exporter spec's signal names, loaded once per lint. The application
/// tier publishes these through the exporter server and the OTLP emitter, so
/// a scenario targeting them is live even though no plugins.d spec defines
/// them.
fn exporter_signals(specs_dir: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_to_string(specs_dir.join(EXPORTER_SPEC))
        .ok()
        .and_then(|raw| GeneratorSpec::from_yaml(&raw).ok())
        .map(|s| s.signals.keys().cloned().collect())
        .unwrap_or_default()
}

/// Whether a scenario step's structural selectors can fire in this fleet.
/// Shared by the scenario and application lints so one can never reject a step
/// the other reports as skipped.
enum StepFit {
    Applies,
    /// Not a fault: this fleet simply lacks what the step aims at.
    Skip(String),
    /// An authoring error, or a required role this fleet does not have.
    Problem(String),
}

fn step_fit(
    env: &Environment,
    roles: &[&str],
    name: &str,
    sc: &sim_spec::Scenario,
    i: usize,
    t: &sim_spec::Target,
) -> StepFit {
    // A step aimed at a role this fleet does not have is only a fault
    // when the scenario declared that role as required. Hero scenarios
    // propagate opportunistically - disk-fill reaches the load balancer
    // last - and a fleet with no `lb` should still be able to run it,
    // minus that step. Treating every absent role as fatal made the
    // console's create flow unable to build anything but a fleet
    // containing every role any scenario happens to mention.
    if let Some(r) = &t.role {
        if !roles.contains(&r.as_str()) {
            // The rest of this step cannot fire either way.
            return if sc.requires_roles.iter().any(|req| req == r) {
                StepFit::Problem(format!(
                    "  {name} step {i}: requires role '{r}', which no node has"
                ))
            } else {
                StepFit::Skip(format!("  {name} step {i}: no '{r}' node, step is skipped"))
            };
        }
    }

    // An index target beyond the role's node count is skipped (the
    // same class as an absent role); the role missing entirely is
    // handled by the requires_roles/absent-role branch above.
    if let Some(idx) = t.node_index {
        let nodes_of_role = env
            .nodes
            .iter()
            .filter(|n| t.role.as_ref().is_none_or(|r| n.role.as_deref() == Some(r)))
            .count();
        if nodes_of_role < idx {
            return StepFit::Skip(format!(
                "  {name} step {i}: only {nodes_of_role} node(s) match the selectors, \
                 index {idx} does not exist, step is skipped"
            ));
        }
    }

    // A label target no node satisfies is skipped, not fatal - the
    // same class as an absent role. But a selector with no `=` is an
    // authoring error: it matches nothing anywhere, on any fleet.
    if let Some(sel) = &t.label {
        if sel.split_once('=').is_none() {
            return StepFit::Problem(format!(
                "  {name} step {i}: label selector '{sel}' has no '=' - it would \
                 match nothing on any fleet"
            ));
        }
        let Some((key, value)) = sel.split_once('=') else {
            unreachable!("the malformed case is refused above");
        };
        let any_labelled = env
            .nodes
            .iter()
            .any(|n| n.labels.get(key).map(String::as_str) == Some(value));
        if !any_labelled {
            return StepFit::Skip(format!(
                "  {name} step {i}: no node carries label '{sel}', step is skipped"
            ));
        }
    }

    // An instance pin that no matching node declares would do
    // nothing - the same silent-nothing class the signal check below
    // exists to catch. Reported as a skip (a fleet without that
    // device model simply lacks the port), never a refusal.
    if let Some(want_inst) = &t.instance {
        let role_matches = |n: &crate::environment::NodeDef| {
            t.role.as_ref().is_none_or(|r| n.role.as_deref() == Some(r))
        };
        let any_instance = env.nodes.iter().filter(|n| role_matches(n)).any(|n| {
            n.instances
                .values()
                .flatten()
                .any(|i| i.name() == want_inst)
        });
        if !any_instance {
            return StepFit::Skip(format!(
                "  {name} step {i}: no node declares instance '{want_inst}', \
                 step is skipped"
            ));
        }
    }
    StepFit::Applies
}

fn check_scenarios(
    composed: &std::collections::BTreeMap<String, Arc<GeneratorSpec>>,
    env: &Environment,
    library: &std::collections::BTreeMap<String, sim_spec::Scenario>,
    specs_dir: &Path,
) -> Result<(), String> {
    let hosts: Vec<&str> = env.nodes.iter().map(|n| n.hostname.as_str()).collect();
    let roles: Vec<&str> = env.nodes.iter().filter_map(|n| n.role.as_deref()).collect();
    let instances: Vec<&str> = env
        .nodes
        .iter()
        .flat_map(|n| n.instances.values())
        .flatten()
        .map(|i| i.name())
        .collect();

    let mut problems = Vec::new();
    let mut skipped = Vec::new();
    // Steps that cannot fire here but are not faults - reported so an operator
    // knows which parts of a scenario this fleet will not show.
    let mut inapplicable: Vec<String> = Vec::new();
    for (name, sc) in library {
        // A scenario whose required roles are absent is not an error; it simply
        // does not belong to this environment and is not offered.
        if !sc.applies_to(&roles) {
            skipped.push(name.as_str());
            continue;
        }
        for (i, step) in sc.timeline.iter().enumerate() {
            let t = &step.target;

            match step_fit(env, &roles, name, sc, i, t) {
                StepFit::Applies => {}
                StepFit::Skip(line) => {
                    inapplicable.push(line);
                    continue;
                }
                StepFit::Problem(line) => {
                    problems.push(line);
                    continue;
                }
            }

            // A signal only has to exist on some node; a Postgres scenario
            // legitimately names signals no web node defines. The exporter
            // spec's signals count too: it is deliberately not a node
            // `service` (that would double-chart every series), but its
            // signals fire on the application tier through the exporter and
            // OTLP engines - reporting them as "skipped" was noise that
            // buried the real skips.
            let known = composed.values().any(|s| s.signals.contains_key(&t.signal))
                || exporter_signals(specs_dir).contains(&t.signal);
            if !known {
                // Absent from this fleet is not the same as absent everywhere.
                // A blast-radius step reaching into a tier this fleet does not
                // run - nginx latency on a fleet with no nginx - is the same
                // class as a missing role, which is reported and not fatal.
                // Only a signal no spec on disk defines is an authoring error,
                // and that is the case worth failing on: it is how a typo comes
                // to silently do nothing in front of a prospect.
                if signal_exists_somewhere(specs_dir, &t.signal) {
                    inapplicable.push(format!(
                        "  {name} step {i}: nothing here defines '{}', step is skipped",
                        t.signal
                    ));
                    continue;
                }
                problems.push(format!(
                    "  {name} step {i}: unknown signal '{}' - the step would do nothing",
                    t.signal
                ));
            }
            if let Some(sfx) = &t.hostname_suffix {
                if !hosts.iter().any(|h| h.ends_with(sfx.as_str())) {
                    problems.push(format!(
                        "  {name} step {i}: no hostname ends with '{sfx}' - the step would do nothing"
                    ));
                }
            }
            if let Some(h) = &t.hostname {
                if !hosts.contains(&h.as_str()) {
                    problems.push(format!("  {name} step {i}: unknown hostname '{h}'"));
                }
            }
            if let Some(inst) = &t.instance {
                if !instances.contains(&inst.as_str()) {
                    problems.push(format!("  {name} step {i}: no node has instance '{inst}'"));
                }
            }
        }
    }

    println!(
        "infra-sim: checked {} of {} scenario(s) against the environment",
        library.len() - skipped.len(),
        library.len()
    );
    if !skipped.is_empty() {
        println!(
            "  not applicable here (missing required roles): {}",
            skipped.join(", ")
        );
    }
    if !inapplicable.is_empty() {
        println!("  steps that will not fire in this fleet:");
        for line in &inapplicable {
            println!("{line}");
        }
    }
    if problems.is_empty() {
        println!("  all required scenario targets resolve\n");
        Ok(())
    } else {
        println!("{}\n", problems.join("\n"));
        Err(format!(
            "{} scenario target(s) do not resolve; those steps would silently do nothing",
            problems.len()
        ))
    }
}

/// Exercise each applicable incident separately, including its recovery.
fn lint_applications(
    env: &Environment,
    args: &Args,
    library: &std::collections::BTreeMap<String, sim_spec::Scenario>,
    roles: &[&str],
    interval: i64,
) -> Result<(), String> {
    if !env
        .nodes
        .iter()
        .any(|node| is_app_tier(node.role.as_deref()))
    {
        return Ok(());
    }
    let path = env.specs_path(&args.environment).join(EXPORTER_SPEC);
    let raw = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read application spec {}: {error}", path.display()))?;
    let spec = Arc::new(GeneratorSpec::from_yaml(&raw).map_err(|error| error.to_string())?);
    let engines = application_engines(env, Arc::clone(&spec));
    let start = 1_700_100_000;
    for (_, engine) in &engines {
        exporters::check_window(
            engine.clone(),
            &ScenarioSet::default(),
            start,
            7200,
            interval,
        )?;
    }
    println!("  PASS application baseline (published series)");
    for scenario in library
        .values()
        .filter(|scenario| scenario.applies_to(roles))
    {
        application_selectors_resolve(&engines, &spec, env, roles, scenario)?;
        let targets = application_targets(&engines, &spec, env, roles, scenario);
        let duration = scenario.duration().max(3600);
        if duration > 86_400 {
            return Err(format!(
                "scenario '{}' exceeds the one-day validation horizon",
                scenario.name
            ));
        }
        let active = ScenarioSet::new(vec![sim_engine::ActiveScenario {
            scenario: scenario.clone(),
            started_at: start,
            recovering_since: Some(start + duration),
        }]);
        for (_, engine) in &engines {
            exporters::check_window(
                engine.clone(),
                &active,
                start,
                duration + sim_engine::RECOVERY_SECONDS + 60,
                interval,
            )
            .map_err(|error| format!("application scenario {}: {error}", scenario.name))?;
        }
        for ((_, engine), targets) in engines.iter().zip(&targets) {
            exporters::check_narrative(engine.clone(), &active, start, duration, interval, targets)
                .map_err(|error| format!("application scenario {}: {error}", scenario.name))?;
        }
        println!(
            "  PASS application scenario {} (published series, direction, isolation and recovery)",
            scenario.name
        );
    }
    Ok(())
}

/// Every applicable step on an application signal must reach an application
/// node; otherwise it would silently do nothing on the exporter and OTLP paths.
fn application_selectors_resolve(
    engines: &[(sim_engine::NodeProfile, NodeEngine)],
    spec: &GeneratorSpec,
    env: &Environment,
    roles: &[&str],
    scenario: &sim_spec::Scenario,
) -> Result<(), String> {
    for (i, step) in scenario.timeline.iter().enumerate() {
        if !spec.signals.contains_key(&step.target.signal)
            || !matches!(
                step_fit(env, roles, &scenario.name, scenario, i, &step.target),
                StepFit::Applies
            )
        {
            continue;
        }
        let mut ranks = std::collections::BTreeMap::<String, usize>::new();
        let matched = engines.iter().any(|(profile, _)| {
            let rank = profile.role.as_ref().map(|role| {
                let next = ranks.entry(role.clone()).or_default();
                let rank = *next;
                *next += 1;
                rank
            });
            step.target.matches(
                &profile.hostname,
                profile.role.as_deref(),
                "",
                &step.target.signal,
                &profile.labels,
                rank,
            )
        });
        if !matched {
            return Err(format!(
                "application scenario {}: target '{}' matches no application node",
                scenario.name, step.target.signal
            ));
        }
    }
    Ok(())
}

/// Intended sign of a step's effect on its signal; `None` when it only has to
/// change (oscillation) or restores normal (recovery).
fn intended_direction(effect: &sim_spec::Effect) -> Option<f64> {
    let sign = |value: f64| (value != 0.0).then(|| value.signum());
    match effect {
        sim_spec::Effect::Step { multiplier } | sim_spec::Effect::Ramp { multiplier, .. } => {
            sign(multiplier - 1.0)
        }
        sim_spec::Effect::Add { amount } | sim_spec::Effect::AddRamp { amount, .. } => {
            sign(*amount)
        }
        sim_spec::Effect::Drift { rate_per_hour } => sign(*rate_per_hour),
        sim_spec::Effect::Oscillate { .. } | sim_spec::Effect::Recover { .. } => None,
    }
}

/// Per application engine, the published signals a scenario targets there and
/// their intended direction. Opposing steps on one signal only require change.
fn application_targets(
    engines: &[(sim_engine::NodeProfile, NodeEngine)],
    spec: &GeneratorSpec,
    env: &Environment,
    roles: &[&str],
    scenario: &sim_spec::Scenario,
) -> Vec<std::collections::BTreeMap<String, Option<f64>>> {
    let mut ranks = std::collections::BTreeMap::<String, usize>::new();
    engines
        .iter()
        .map(|(profile, _)| {
            let rank = profile.role.as_ref().map(|role| {
                let next = ranks.entry(role.clone()).or_default();
                let rank = *next;
                *next += 1;
                rank
            });
            let mut targets = std::collections::BTreeMap::<String, Option<f64>>::new();
            for (i, step) in scenario.timeline.iter().enumerate() {
                let t = &step.target;
                if !spec.signals.contains_key(&t.signal)
                    || !matches!(
                        step_fit(env, roles, &scenario.name, scenario, i, t),
                        StepFit::Applies
                    )
                    || !t.matches(
                        &profile.hostname,
                        profile.role.as_deref(),
                        "",
                        &t.signal,
                        &profile.labels,
                        rank,
                    )
                {
                    continue;
                }
                if matches!(step.effect, sim_spec::Effect::Recover { .. }) {
                    continue;
                }
                let direction = intended_direction(&step.effect);
                targets
                    .entry(t.signal.clone())
                    .and_modify(|existing| {
                        if *existing != direction {
                            *existing = None;
                        }
                    })
                    .or_insert(direction);
            }
            targets
        })
        .collect()
}

fn lint_scenarios(
    baseline: &[NodeEngine],
    library: &std::collections::BTreeMap<String, sim_spec::Scenario>,
    roles: &[&str],
    interval: i64,
) -> Result<(), String> {
    let start = 1_700_100_000;
    let mut failures = 0;
    for scenario in library.values().filter(|s| s.applies_to(roles)) {
        // Drifts do not settle. Use at least an hour, plus the authored horizon.
        let duration = scenario.duration().max(3600);
        if duration > 86_400 {
            return Err(format!(
                "scenario '{}' exceeds the one-day validation horizon",
                scenario.name
            ));
        }
        let active = ScenarioSet::new(vec![sim_engine::ActiveScenario {
            scenario: scenario.clone(),
            started_at: start,
            recovering_since: Some(start + duration),
        }]);
        let mut engines = baseline.to_vec();
        let ticks = (duration + sim_engine::RECOVERY_SECONDS + 60) / interval.max(1);
        let problems = sim_engine::fidelity::check_with_scenarios(
            &mut engines,
            ticks,
            start,
            interval,
            &active,
        );
        let unique: std::collections::BTreeSet<String> = problems
            .iter()
            .map(|v| format!("{} {}: {}", v.node, v.chart, v.detail))
            .collect();
        if unique.is_empty() {
            println!(
                "  PASS scenario {} ({}s plus recovery)",
                scenario.name, duration
            );
        } else {
            failures += unique.len();
            println!(
                "  FAIL scenario {} ({} distinct violations)",
                scenario.name,
                unique.len()
            );
            for problem in unique.iter().take(12) {
                println!("    {problem}");
            }
        }
    }
    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} scenario fidelity violations"))
    }
}

/// Simulate `hours` of data and report fidelity violations without touching an
/// agent. This is the first piece of the fidelity harness: cheap enough to run
/// in CI on every spec change, and it catches the clamping artifacts that are
/// invisible in a four-second smoke test but obvious on a demo's daily chart.
fn lint(engines: &mut [NodeEngine], hours: i64, update_every: i64) -> Result<(), String> {
    // Walk a fixed window rather than wall-clock time so the result is
    // reproducible and covers a full diurnal cycle regardless of when it runs.
    let start = 1_700_000_000_i64;
    let ticks = (hours * 3600) / update_every.max(1);
    let interval = update_every as f64;

    // Warm-up runs one node per core. Sequentially this was the whole cost of
    // `create` at fleet scale: 25 nodes took 115s of a single core, against an
    // operator expectation of a simulation that comes up in a minute or two.
    // Nodes share no mutable state, so this changes timing and nothing else.
    sim_engine::parallel::map_engines(&mut *engines, |engine| {
        for i in 0..ticks {
            engine.tick(&ScenarioSet::default(), start + i * update_every, interval);
        }
    });

    // Semantic checks over the emitted samples. These catch what the
    // pinned-signal check structurally cannot: a bound that is itself wrong,
    // a partition whose total does not resolve, a counter that goes backwards.
    // A scenario once pushed disk utilisation to 101.5% and the pinned-signal
    // check passed it cleanly.
    let semantic = sim_engine::fidelity::check(
        engines,
        (2 * 3600) / update_every.max(1),
        1_700_000_000,
        update_every,
    );

    let mut failures = 0usize;
    println!("infra-sim lint: {hours}h simulated, {ticks} samples per node\n");

    if semantic.is_empty() {
        println!("  semantic checks: no violations\n");
    } else {
        // Grouped by kind: one broken context produces a violation per sample,
        // and a thousand identical lines hide the other problems.
        let mut by_kind: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
        for v in &semantic {
            by_kind
                .entry(v.kind.as_str())
                .or_default()
                .push(format!("{} {}: {}", v.node, v.chart, v.detail));
        }
        println!("  semantic checks: {} violation(s)", semantic.len());
        for (kind, mut items) in by_kind {
            items.sort();
            items.dedup();
            println!("    {kind} ({} distinct):", items.len());
            for i in items.iter().take(8) {
                println!("      {i}");
            }
            if items.len() > 8 {
                println!("      ... and {} more", items.len() - 8);
            }
            failures += items.len();
        }
        println!();
    }
    for engine in engines.iter() {
        let host = &engine.profile().hostname;
        let pinned = engine.lint().pinned_signals(PINNED_THRESHOLD);
        if pinned.is_empty() {
            println!("  PASS  {host}");
        } else {
            failures += pinned.len();
            println!("  FAIL  {host}");
            for (signal, rate) in pinned {
                println!(
                    "          {signal}: pinned for {:.2}% of samples",
                    rate * 100.0
                );
            }
        }
    }

    if failures > 0 {
        println!();
        Err(format!(
            "{failures} fidelity problem(s): signals clamped against a bound for more than \
             {:.1}% of samples, or semantic violations above",
            PINNED_THRESHOLD * 100.0
        ))
    } else {
        println!("\nno signals pinned to their bounds");
        Ok(())
    }
}

fn write_err(e: io::Error) -> String {
    // A closed pipe means Netdata shut the plugin down; that is orderly, not a
    // fault, but there is nothing left to write to either way.
    format!("write failed: {e}")
}

/// Whether `due` arrives at or before the tick being prepared for.
fn tick_due(due: i64, now: i64) -> bool {
    due <= now
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Round up to the next interval boundary so sample timestamps stay regular.
fn align_to_interval(now: i64, interval: i64) -> i64 {
    now - now.rem_euclid(interval) + interval
}

fn sleep_until(target: i64) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let target = Duration::from_secs(target.max(0) as u64);
    if let Some(remaining) = target.checked_sub(now) {
        let deadline = std::time::Instant::now() + remaining;
        while !shutdown::requested() && std::time::Instant::now() < deadline {
            std::thread::sleep(
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(Duration::from_millis(100)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CI lints no template, so this is what stops a shipped scenario step from
    /// being rejected on a shipped fleet: every step applies or is skipped, and
    /// every applicable application step reaches an application node.
    #[test]
    fn shipped_templates_never_reject_a_shipped_scenario_step() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let library = control::load_library(&root.join("scenarios")).unwrap();
        let raw = std::fs::read_to_string(root.join("specs").join(EXPORTER_SPEC)).unwrap();
        let spec = Arc::new(GeneratorSpec::from_yaml(&raw).unwrap());
        let mut templates = 0;
        for entry in std::fs::read_dir(root.join("environments")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            templates += 1;
            let env = Environment::load(&path).unwrap();
            let roles: Vec<&str> = env.nodes.iter().filter_map(|n| n.role.as_deref()).collect();
            let engines = application_engines(&env, Arc::clone(&spec));
            for scenario in library.values().filter(|s| s.applies_to(&roles)) {
                for (i, step) in scenario.timeline.iter().enumerate() {
                    if let StepFit::Problem(line) =
                        step_fit(&env, &roles, &scenario.name, scenario, i, &step.target)
                    {
                        panic!("{}: {line}", path.display());
                    }
                }
                if !engines.is_empty() {
                    if let Err(error) =
                        application_selectors_resolve(&engines, &spec, &env, &roles, scenario)
                    {
                        panic!("{}: {error}", path.display());
                    }
                }
            }
        }
        assert!(templates >= 10, "found only {templates} templates");
    }

    #[test]
    fn an_absent_optional_role_is_skipped_by_both_lints() {
        // Shaped like k8s-microservices: an application tier with no web node.
        let env: Environment = serde_yaml::from_str(
            r#"
version: 1
name: sim-fit-test
seed: 7
generator: unused.yaml
nodes:
  - {hostname: sim-lb-01, guid: lb, role: lb}
  - {hostname: sim-db-01, guid: db, role: db}
"#,
        )
        .unwrap();
        let roles = ["lb", "db"];
        let scenario = |requires: &str| {
            sim_spec::Scenario::from_yaml(&format!(
                r#"
version: 1
name: fit
requires_roles: [{requires}]
manifest: {{root_cause: synthetic}}
timeline:
  - at: 0s
    target: {{signal: app_db_pool_wait_rate, role: web}}
    effect: add
    amount: 1
"#
            ))
            .unwrap()
        };
        let fit =
            |sc: &sim_spec::Scenario| step_fit(&env, &roles, "fit", sc, 0, &sc.timeline[0].target);
        assert!(matches!(fit(&scenario("db")), StepFit::Skip(_)));
        assert!(matches!(fit(&scenario("db, web")), StepFit::Problem(_)));
    }

    #[test]
    fn application_scenarios_target_the_ranked_node_and_recover() {
        let env: Environment = serde_yaml::from_str(
            r#"
version: 1
name: sim-rank-test
seed: 7
generator: unused.yaml
nodes:
  - {hostname: sim-web-01, guid: first, role: web}
  - {hostname: sim-db-01, guid: database, role: db}
  - {hostname: sim-web-02, guid: second, role: web}
"#,
        )
        .unwrap();
        let spec = Arc::new(
            GeneratorSpec::from_yaml(
                r#"
version: 1
name: app-test
signals:
  app_queue_depth: {base: 0, min: 0, max: 100}
contexts:
  - id: app.queue
    title: Queue
    priority: 1
    units: items
    family: app
    shape: independent
    dimensions:
      - {id: depth, signal: app_queue_depth}
"#,
            )
            .unwrap(),
        );
        let scenario = sim_spec::Scenario::from_yaml(
            r#"
version: 1
name: targeted-app
manifest: {root_cause: synthetic queue pressure}
timeline:
  - at: 0s
    target: {signal: app_queue_depth, role: web, node_index: 2}
    effect: add
    amount: 10
"#,
        )
        .unwrap();
        let active = ScenarioSet::new(vec![sim_engine::ActiveScenario {
            scenario,
            started_at: 1000,
            recovering_since: Some(1010),
        }]);
        let mut engines = application_engines(&env, spec);
        assert_eq!(engines.len(), 2);
        assert_eq!(engines[0].0.hostname, "sim-web-01");
        assert_eq!(engines[1].0.hostname, "sim-web-02");
        for (index, (profile, engine)) in engines.iter_mut().enumerate() {
            let mut healthy =
                exporters::Exporter::new(profile.hostname.clone(), "web".into(), engine.clone());
            let mut affected =
                exporters::Exporter::new(profile.hostname.clone(), "web".into(), engine.clone());
            let normal = exporters::render(&mut healthy, &ScenarioSet::default(), 1001, 1001);
            let fault = exporters::render(&mut affected, &active, 1001, 1001);
            assert_eq!(
                normal != fault,
                index == 1,
                "published output must change only on the targeted node"
            );
            let recovered = exporters::render(
                &mut affected,
                &active,
                1011 + sim_engine::RECOVERY_SECONDS,
                1001,
            );
            let queue = |body: &str| {
                body.lines()
                    .find(|line| line.starts_with("app_queue_depth{"))
                    .unwrap()
                    .to_string()
            };
            assert_eq!(queue(&normal), queue(&recovered));
            assert_eq!(
                engine.signal_values(&active, 1001)["app_queue_depth"],
                if index == 1 { 10.0 } else { 0.0 }
            );
            assert_eq!(
                engine.signal_values(&active, 1011 + sim_engine::RECOVERY_SECONDS)
                    ["app_queue_depth"],
                0.0
            );
        }
        // The narrative check has teeth: a wrong direction, or a change on a
        // node claimed to be untargeted, is refused.
        let up = std::collections::BTreeMap::from([("app_queue_depth".to_string(), Some(1.0))]);
        let down = std::collections::BTreeMap::from([("app_queue_depth".to_string(), Some(-1.0))]);
        let none = std::collections::BTreeMap::new();
        let narrative = |index: usize, targets| {
            exporters::check_narrative(engines[index].1.clone(), &active, 1000, 10, 1, targets)
        };
        assert!(narrative(1, &up).is_ok());
        assert!(narrative(0, &none).is_ok());
        assert!(narrative(1, &down).unwrap_err().contains("never moved"));
        assert!(narrative(1, &none).unwrap_err().contains("not targeted"));
    }

    #[test]
    fn shipped_latency_incidents_keep_the_tail_above_p95() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let raw = std::fs::read_to_string(root.join("specs").join(EXPORTER_SPEC)).unwrap();
        let spec = Arc::new(GeneratorSpec::from_yaml(&raw).unwrap());
        let env: Environment = serde_yaml::from_str(
            "version: 1\nname: sim-tail\nseed: 11\ngenerator: unused.yaml\n\
             nodes:\n  - {hostname: sim-web-01, guid: tail, role: web}\n",
        )
        .unwrap();
        for name in ["checkout-degradation", "worker-saturation"] {
            let path = root.join("scenarios").join(format!("{name}.yaml"));
            let scenario =
                sim_spec::Scenario::from_yaml(&std::fs::read_to_string(path).unwrap()).unwrap();
            let start = 1_700_100_000;
            let end = start + scenario.duration();
            let active = ScenarioSet::new(vec![sim_engine::ActiveScenario {
                scenario,
                started_at: start,
                recovering_since: Some(end),
            }]);
            let mut engine = application_engines(&env, Arc::clone(&spec)).remove(0).1;
            let (mut samples, mut flattened) = (0, 0);
            for now in start..end {
                let mut values = engine.signal_values(&active, now);
                sim_engine::application::normalize(&mut values);
                samples += 1;
                flattened += usize::from(values["app_latency_p99"] <= values["app_latency_p95"]);
            }
            // Baseline noise alone crosses a few samples; the incident must not.
            assert!(
                flattened * 100 <= samples,
                "{name}: p99 sat on p95 for {flattened} of {samples} samples"
            );
        }
    }

    #[test]
    fn alignment_lands_on_the_next_boundary() {
        assert_eq!(align_to_interval(100, 10), 110);
        assert_eq!(align_to_interval(101, 10), 110);
        assert_eq!(align_to_interval(109, 10), 110);
        assert_eq!(align_to_interval(110, 10), 120);
    }

    #[test]
    fn alignment_handles_a_one_second_interval() {
        assert_eq!(align_to_interval(1_700_000_000, 1), 1_700_000_001);
    }
}
