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
    /// Build an environment from a plain-text description instead of running.
    describe: Option<String>,
    /// Environment name and hostname prefix for --describe.
    describe_name: Option<String>,
    /// Read the description with a real model instead of the keyword parser.
    llm: Option<llm::Config>,
    /// Emit correlated logs into the journal instead of metrics.
    logs: bool,
    /// Serve simulated Prometheus exporters instead of collecting.
    exporters: bool,
    exporter_port: u16,
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
}

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
    let mut replay_from: Option<i64> = None;
    let mut reskin_args: Option<ReskinArgs> = None;
    let mut describe: Option<String> = None;
    let mut describe_name: Option<String> = None;
    let mut llm_cfg: Option<llm::Config> = None;
    let mut llm_model: Option<String> = None;
    let mut llm_key_env: Option<String> = None;
    let mut logs = false;
    let mut exporters = false;
    let mut exporter_port = DEFAULT_EXPORTER_PORT;
    let mut journal_dir: Option<PathBuf> = None;
    let mut journal_remote: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
                     simulated Prometheus exporters (a separate process;                      Netdata's own go.d prometheus collector scrapes them):\n\
                     infra-sim --exporters --environment PATH\n\
                     --exporter-port N     listen port (default:                      {DEFAULT_EXPORTER_PORT})\n\
                     Serves GET /metrics/<hostname> per node, in Prometheus \n\
                     text format, moved by whatever scenario is running.\n\
                     \n\
                     build an environment from a description:\n\
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

    Ok(Args {
        update_every,
        environment,
        lint_hours,
        replay_from,
        reskin: reskin_args,
        describe,
        describe_name,
        llm: llm_cfg,
        logs,
        journal_dir: journal_dir
            .unwrap_or_else(|| PathBuf::from(logs_runtime::DEFAULT_JOURNAL_DIR)),
        journal_remote,
        exporters,
        exporter_port,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    if let Some(text) = &args.describe {
        return do_describe(
            text,
            args.describe_name.as_deref(),
            &args.environment,
            args.llm.as_ref(),
        );
    }

    if let Some(r) = &args.reskin {
        return do_reskin(&args.environment, r);
    }

    let env = Environment::load(&args.environment).map_err(|e| e.to_string())?;

    // Logs need the fleet's identities and services, not its generator specs,
    // so this branches before the (much heavier) spec composition.
    if args.logs {
        return do_logs(&env, &args);
    }

    if args.exporters {
        return do_exporters(&env, &args);
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
            let raw = std::fs::read_to_string(&path).map_err(|e| {
                format!(
                    "node '{}' needs service spec '{}': {e}",
                    node.hostname,
                    path.display()
                )
            })?;
            let svc = GeneratorSpec::from_yaml(&raw).map_err(|e| e.to_string())?;
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

    let mut engines: Vec<NodeEngine> = profiles
        .iter()
        .zip(&env.nodes)
        .map(|(p, n)| {
            let key = format!(
                "{}\0{}",
                env.node_generator_path(n, &args.environment).display(),
                n.services.join("+")
            );
            NodeEngine::new(Arc::clone(&composed[&key]), p.clone(), env.seed)
        })
        .collect();

    if let Some(hours) = args.lint_hours {
        let library = control::load_library(&env.scenario_path(&args.environment))?;
        check_scenarios(&composed, &env, &library)?;
        return lint(&mut engines, hours, update_every);
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
    let mut out = BufWriter::new(stdout.lock());

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

    loop {
        sleep_until(next);
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
/// Deliberately shares nothing with the metrics process but the environment
/// file, the seed and `control.yaml`. Determinism does the rest: both compute
/// the same values for the same tick, so the logs line up with the charts
/// without either side coordinating.
fn do_logs(env: &Environment, args: &Args) -> Result<(), String> {
    let remote_bin = logs_runtime::find_journal_remote(args.journal_remote.as_deref())?;
    let update_every = args.update_every.max(env.update_every);

    let generators: Vec<sim_engine::logs::LogGenerator> = env
        .profiles()
        .iter()
        .zip(&env.nodes)
        .map(|(profile, node)| {
            sim_engine::logs::LogGenerator::new(profile, &node.services, env.seed)
        })
        .collect();

    let base_dir = args
        .environment
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let library = control::load_library(&env.scenario_path(&args.environment))?;
    let mut control = control::ControlChannel::new(base_dir.join(CONTROL_FILE), library);

    let mut runtime = logs_runtime::LogsRuntime::start(generators, &args.journal_dir, &remote_bin)?;

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

    let interval = update_every as f64;
    let mut next = align_to_interval(now_secs(), update_every);
    let mut total = 0usize;
    let mut reported = now_secs();

    loop {
        sleep_until(next);
        let tick_at = next;
        next += update_every;

        if let Some(change) = control.poll(tick_at) {
            eprintln!("infra-sim logs: {change}");
        }

        total += runtime.tick(control.scenarios(), tick_at, interval)?;

        // Periodic, not per-tick: an operator wants to know it is alive without
        // the output becoming its own log flood.
        if tick_at - reported >= 300 {
            eprintln!("infra-sim logs: {total} entries written so far");
            reported = tick_at;
        }
    }
}

/// Serve one simulated Prometheus exporter per node until killed.
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

    let built: Vec<exporters::Exporter> = env
        .profiles()
        .into_iter()
        .map(|profile| {
            let hostname = profile.hostname.clone();
            let role = profile.role.clone().unwrap_or_else(|| "node".into());
            exporters::Exporter::new(
                hostname,
                role,
                NodeEngine::new(Arc::clone(&spec), profile, env.seed),
            )
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
    let poller = Arc::clone(&shared);
    let control_path = base_dir.join(CONTROL_FILE);
    std::thread::spawn(move || {
        let mut control = control::ControlChannel::new(control_path, library);
        loop {
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

    exporters::serve(listener, built, shared).map_err(|e| e.to_string())
}

/// Build an environment from a text description and write it out.
fn do_describe(
    text: &str,
    name: Option<&str>,
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
        None => sim_engine::describe::parse(text),
    };

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
        "Review it, then: infra-sim --environment {} --lint 72",
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
fn check_scenarios(
    composed: &std::collections::BTreeMap<String, Arc<GeneratorSpec>>,
    env: &Environment,
    library: &std::collections::BTreeMap<String, sim_spec::Scenario>,
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

            // A step aimed at a role this fleet does not have is only a fault
            // when the scenario declared that role as required. Hero scenarios
            // propagate opportunistically - disk-fill reaches the load balancer
            // last - and a fleet with no `lb` should still be able to run it,
            // minus that step. Treating every absent role as fatal made the
            // console's create flow unable to build anything but a fleet
            // containing every role any scenario happens to mention.
            if let Some(r) = &t.role {
                if !roles.contains(&r.as_str()) {
                    if sc.requires_roles.iter().any(|req| req == r) {
                        problems.push(format!(
                            "  {name} step {i}: requires role '{r}', which no node has"
                        ));
                    } else {
                        inapplicable
                            .push(format!("  {name} step {i}: no '{r}' node, step is skipped"));
                    }
                    // The rest of this step cannot fire either way.
                    continue;
                }
            }

            // A signal only has to exist on some node; a Postgres scenario
            // legitimately names signals no web node defines.
            let known = composed.values().any(|s| s.signals.contains_key(&t.signal));
            if !known {
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

    for engine in engines.iter_mut() {
        for i in 0..ticks {
            engine.tick(&ScenarioSet::default(), start + i * update_every, interval);
        }
    }

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
        std::thread::sleep(remaining);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
