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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod control;
mod emitter;
mod environment;

use environment::Environment;
use sim_engine::{NodeEngine, ScenarioSet};
use sim_spec::GeneratorSpec;

/// Where the plugin looks for its environment when Netdata launches it.
const DEFAULT_ENVIRONMENT: &str = "/etc/netdata/infra-sim/environment.yaml";
/// Scenario library, relative to the environment file's directory.
const SCENARIO_DIR: &str = "scenarios";
/// Control file the console writes to trigger and resolve scenarios.
const CONTROL_FILE: &str = "control.yaml";
/// Environment variable override, used by the console and by manual runs.
const ENV_VAR: &str = "INFRA_SIM_ENVIRONMENT";

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
fn parse_args() -> Result<Args, String> {
    let mut update_every = 1_i64;
    let mut environment: Option<PathBuf> = None;
    let mut lint_hours: Option<i64> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--environment" | "-e" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--environment requires a path".to_string())?;
                environment = Some(PathBuf::from(value));
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
                     violations, and exit non-zero if any are found"
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

    Ok(Args {
        update_every,
        environment,
        lint_hours,
    })
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let env = Environment::load(&args.environment).map_err(|e| e.to_string())?;
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

    let profiles = env.profiles();
    eprintln!(
        "infra-sim: environment '{}' - {} node(s), spec '{}' ({} contexts), seed {}, update_every {}s",
        env.name,
        profiles.len(),
        spec.name,
        spec.contexts.len(),
        env.seed,
        update_every,
    );

    let mut engines: Vec<NodeEngine> = profiles
        .iter()
        .map(|p| NodeEngine::new(&spec, p.clone(), env.seed))
        .collect();

    if let Some(hours) = args.lint_hours {
        return lint(&spec, &mut engines, hours, update_every);
    }

    let base_dir = args
        .environment
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let library = control::load_library(&base_dir.join(SCENARIO_DIR))?;
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
        emitter::declare_charts(&mut out, &spec, engine, update_every).map_err(write_err)?;
    }
    out.flush().map_err(write_err)?;

    let interval = update_every as f64;
    let mut next = align_to_interval(now_secs(), update_every);

    loop {
        sleep_until(next);
        let tick_at = next;
        next += update_every;

        // Cheap in the common case - a single stat - and it is what makes a
        // scenario triggerable mid-demo without restarting anything.
        if let Some(change) = control.poll(tick_at) {
            eprintln!("infra-sim: {change}");
        }
        let scenarios = control.scenarios();

        for engine in engines.iter_mut() {
            let samples = engine.tick(&spec, scenarios, tick_at, interval);
            let guid = engine.profile().guid.clone();
            emitter::emit_samples(&mut out, &guid, &samples).map_err(write_err)?;
        }

        // Netdata reads us over a pipe, so an unflushed buffer looks exactly
        // like a stalled collector.
        out.flush().map_err(write_err)?;
    }
}

/// Simulate `hours` of data and report fidelity violations without touching an
/// agent. This is the first piece of the fidelity harness: cheap enough to run
/// in CI on every spec change, and it catches the clamping artifacts that are
/// invisible in a four-second smoke test but obvious on a demo's daily chart.
fn lint(
    spec: &GeneratorSpec,
    engines: &mut [NodeEngine],
    hours: i64,
    update_every: i64,
) -> Result<(), String> {
    // Walk a fixed window rather than wall-clock time so the result is
    // reproducible and covers a full diurnal cycle regardless of when it runs.
    let start = 1_700_000_000_i64;
    let ticks = (hours * 3600) / update_every.max(1);
    let interval = update_every as f64;

    for engine in engines.iter_mut() {
        for i in 0..ticks {
            engine.tick(
                spec,
                &ScenarioSet::default(),
                start + i * update_every,
                interval,
            );
        }
    }

    let mut failures = 0usize;
    println!("infra-sim lint: {hours}h simulated, {ticks} samples per node\n");
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
            "{failures} signal(s) spend more than {:.1}% of samples clamped to a bound; \
             widen the bound or lower the base",
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
