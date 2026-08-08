//! Simulated Prometheus exporters.
//!
//! Prospects rarely run only off-the-shelf software. They run their own
//! services, instrumented with a Prometheus client library, exposing `/metrics`
//! on some port. "Netdata scrapes that too, and charts it without you writing a
//! dashboard" is a demo that needs a real endpoint to scrape.
//!
//! So this is a real HTTP server publishing real Prometheus text format, which
//! Netdata's **own** `go.d prometheus` collector discovers, scrapes and charts.
//! Nothing about the resulting charts is authored here - the collector's
//! auto-charting of an exporter it has never seen is precisely what is being
//! shown.
//!
//! ## One listener, one path per node
//!
//! `GET /metrics/<hostname>`. A port per node would mean 50 listeners for a
//! 50-node fleet, and go.d's job config carries the node identity anyway
//! (`vnode: <hostname>`), so the path is enough to tell them apart.
//!
//! ## The metrics are deliberately application-level
//!
//! `specs/prometheus-app.yaml` covers orders, carts, queues and worker pools -
//! things no Netdata collector provides. Emitting CPU and memory here would put
//! the same series on a node twice, once from the plugins.d path and once from
//! the scrape, which an SRE reads as a broken agent.
//!
//! ## Scenario aware
//!
//! The exporter reads the same `control.yaml` as the metrics plugin, so a
//! triggered fault moves the exporter's numbers on the same timeline as
//! everything else. An application whose infrastructure is on fire while its
//! own metrics stay flat is the contradiction this exists to avoid.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sim_engine::{NodeEngine, ScenarioSet};

/// A node's exporter: its engine plus the labels every series carries.
pub struct Exporter {
    pub hostname: String,
    pub role: String,
    engine: NodeEngine,
    /// Accumulated counter totals, keyed by series.
    ///
    /// A counter cannot be `rate * uptime`: the rate itself has a daily cycle,
    /// so that expression *falls* every evening and go.d reads the drop as a
    /// counter reset. Integrating the rate scrape by scrape is both monotonic
    /// and what a real client library does.
    counters: BTreeMap<String, f64>,
    last_scrape: i64,
}

/// A metric family: one `# HELP` / `# TYPE` pair, then its label variants.
///
/// Prometheus requires HELP and TYPE to appear once per family. Emitting them
/// again before a second label set makes strict parsers reject the whole scrape.
/// Sentinels marking a summary's `_sum` and `_count` series inside a family's
/// series list, so they are emitted under the derived name rather than as a
/// label variant.
const SUM_SUFFIX: &str = "\u{0}sum";
const COUNT_SUFFIX: &str = "\u{0}count";

struct Family<'a> {
    name: &'a str,
    kind: &'a str,
    help: &'a str,
    series: Vec<(String, f64)>,
}

impl Exporter {
    pub fn new(hostname: String, role: String, engine: NodeEngine) -> Self {
        Self {
            hostname,
            role,
            engine,
            counters: BTreeMap::new(),
            last_scrape: 0,
        }
    }

    /// Integrate `rate` over the time since the previous scrape.
    fn accumulate(&mut self, key: &str, rate: f64, elapsed: f64) -> f64 {
        let total = self.counters.entry(key.to_string()).or_insert(0.0);
        *total += rate.max(0.0) * elapsed;
        *total
    }
}

/// Serve `/metrics/<hostname>` for every node until killed.
///
/// Single-threaded on purpose: a scrape renders in well under a millisecond and
/// go.d scrapes each job once per second, so there is nothing to parallelise and
/// a thread pool would only add a way to get the shared engine state wrong.
pub fn serve(
    listener: TcpListener,
    exporters: Vec<Exporter>,
    control: Arc<Mutex<ScenarioSet>>,
) -> std::io::Result<()> {
    let mut exporters = exporters;
    // Counters count from process start, which is what a real client library
    // does. Anchoring them at the Unix epoch produces values in the hundreds of
    // billions on their first scrape - technically monotonic, and obviously fake
    // to anyone who looks. A restart resets them, exactly as a service restart
    // resets a real exporter; go.d handles counter resets natively.
    let started_at = now_secs();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        if let Err(e) = handle(stream, &mut exporters, &control, started_at) {
            // A scraper that hangs up mid-response is normal, not fatal.
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                eprintln!("infra-sim exporters: {e}");
            }
        }
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    exporters: &mut [Exporter],
    control: &Arc<Mutex<ScenarioSet>>,
    started_at: i64,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // Drain the headers so the client sees a clean response rather than a reset.
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }

    let path = request.split_whitespace().nth(1).unwrap_or("/");
    let body = match route(path, exporters, control, started_at) {
        Some(body) => body,
        None => {
            return respond(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                &format!(
                    "no exporter at '{path}'\navailable:\n{}\n",
                    exporters
                        .iter()
                        .map(|e| format!("  /metrics/{}", e.hostname))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            );
        }
    };
    respond(
        &mut stream,
        "200 OK",
        // The version parameter is what Prometheus clients advertise; go.d does
        // not require it, but an exporter that omits it looks hand-rolled.
        "text/plain; version=0.0.4; charset=utf-8",
        &body,
    )
}

fn route(
    path: &str,
    exporters: &mut [Exporter],
    control: &Arc<Mutex<ScenarioSet>>,
    started_at: i64,
) -> Option<String> {
    let host = path.strip_prefix("/metrics/")?;
    let host = host.split('?').next().unwrap_or(host);
    let scenarios = control.lock().ok()?.clone();
    let exp = exporters.iter_mut().find(|e| e.hostname == host)?;
    Some(render(exp, &scenarios, now_secs(), started_at))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// One scrape's worth of Prometheus text format.
///
/// Counters are cumulative, which is what a Prometheus client library exposes
/// and what `go.d prometheus` expects to differentiate. Publishing a rate as a
/// counter would make Netdata differentiate it a second time and chart a flat
/// zero.
fn render(exp: &mut Exporter, scenarios: &ScenarioSet, now: i64, started_at: i64) -> String {
    let v = exp.engine.signal_values(scenarios, now);
    let get = |k: &str| v.get(k).copied().unwrap_or(0.0);
    // No `instance` label: in a real deployment Prometheus adds that at scrape
    // time from the target address, an exporter does not publish it. Emitting
    // it here also bloated every auto-generated chart id with the hostname the
    // vnode already carries.
    let base = format!(r#"service="storefront",role="{}""#, exp.role);

    // Seconds since the previous scrape. The first scrape of a process
    // integrates nothing, so every counter starts at zero - exactly what a
    // freshly restarted exporter reports.
    let elapsed = if exp.last_scrape == 0 {
        0.0
    } else {
        (now - exp.last_scrape).max(0) as f64
    };
    exp.last_scrape = now.max(started_at);

    let total = get("app_requests_rate");
    let errors = get("app_requests_error_rate").min(total);
    // A latency distribution's mean sits above its median and well below its
    // p99; this weighting keeps sum/count consistent with the quantiles rather
    // than contradicting them.
    let mean_latency = 0.72 * get("app_latency_p50")
        + 0.22 * get("app_latency_p95")
        + 0.06 * get("app_latency_p99");
    let acc = |exp: &mut Exporter, key: &str, rate: f64| exp.accumulate(key, rate, elapsed);

    let families = vec![
        Family {
            name: "app_http_requests_total",
            kind: "counter",
            help: "HTTP requests handled",
            series: vec![
                (r#"code="2xx""#.into(), acc(exp, "http_2xx", total - errors)),
                (r#"code="5xx""#.into(), acc(exp, "http_5xx", errors)),
            ],
        },
        Family {
            name: "app_orders_total",
            kind: "counter",
            help: "Orders placed",
            series: vec![(String::new(), acc(exp, "orders", get("app_orders_rate")))],
        },
        Family {
            name: "app_payments_declined_total",
            kind: "counter",
            help: "Payment attempts declined by the processor",
            series: vec![(
                String::new(),
                acc(exp, "declined", get("app_payment_declined_rate")),
            )],
        },
        Family {
            name: "app_cart_value_total",
            kind: "counter",
            help: "Cumulative value of completed carts",
            series: vec![(
                r#"currency="EUR""#.into(),
                acc(exp, "cart_value", get("app_cart_value_total")),
            )],
        },
        Family {
            name: "app_cache_operations_total",
            kind: "counter",
            help: "Cache lookups",
            series: vec![
                (
                    r#"result="hit""#.into(),
                    acc(exp, "cache_hit", get("app_cache_hits_rate")),
                ),
                (
                    r#"result="miss""#.into(),
                    acc(exp, "cache_miss", get("app_cache_misses_rate")),
                ),
            ],
        },
        Family {
            name: "app_db_pool_waits_total",
            kind: "counter",
            help: "Requests that had to wait for a database connection",
            series: vec![(
                String::new(),
                acc(exp, "pool_wait", get("app_db_pool_wait_rate")),
            )],
        },
        // A summary, because that is what a client library emits for latency
        // and what an SRE expects to find on a service's /metrics.
        //
        // `_sum` and `_count` are not optional decoration: go.d skips a summary
        // whose count or sum is missing (netdata/netdata
        // src/go/plugin/go.d/collector/prometheus/writer_schema.go:124), so
        // publishing quantiles alone produced no latency chart at all.
        Family {
            name: "app_request_duration_seconds",
            kind: "summary",
            help: "Request duration",
            series: vec![
                (r#"quantile="0.5""#.into(), get("app_latency_p50")),
                (r#"quantile="0.95""#.into(), get("app_latency_p95")),
                (r#"quantile="0.99""#.into(), get("app_latency_p99")),
                // Observation count and total observed seconds, both cumulative.
                (
                    SUM_SUFFIX.into(),
                    acc(exp, "latency_sum", total * mean_latency),
                ),
                (COUNT_SUFFIX.into(), acc(exp, "latency_count", total)),
            ],
        },
        Family {
            name: "app_queue_depth",
            kind: "gauge",
            help: "Items waiting in the work queue",
            series: vec![(r#"queue="default""#.into(), get("app_queue_depth"))],
        },
        Family {
            name: "app_queue_oldest_seconds",
            kind: "gauge",
            help: "Age of the oldest queued item",
            series: vec![(r#"queue="default""#.into(), get("app_queue_oldest_seconds"))],
        },
        Family {
            name: "app_workers",
            kind: "gauge",
            help: "Worker threads by state",
            series: vec![
                (r#"state="busy""#.into(), get("app_workers_busy")),
                (
                    r#"state="idle""#.into(),
                    (get("app_workers_total") - get("app_workers_busy")).max(0.0),
                ),
            ],
        },
        Family {
            name: "app_db_connections",
            kind: "gauge",
            help: "Database connection pool by state",
            series: vec![
                (r#"state="in_use""#.into(), get("app_db_pool_in_use")),
                (
                    r#"state="idle""#.into(),
                    (get("app_db_pool_size") - get("app_db_pool_in_use")).max(0.0),
                ),
            ],
        },
        Family {
            name: "app_sessions_active",
            kind: "gauge",
            help: "Sessions with activity in the last five minutes",
            series: vec![(String::new(), get("app_sessions_active"))],
        },
        // Every simulated series is labelled as simulated at the source, so a
        // scraped metric cannot be mistaken for a real one even out of context.
        Family {
            name: "app_build_info",
            kind: "gauge",
            help: "Build metadata",
            series: vec![(r#"version="1.0.0",simulated="true""#.into(), 1.0)],
        },
    ];

    let mut out = String::with_capacity(4096);
    for f in &families {
        out.push_str(&format!(
            "# HELP {} {}\n# TYPE {} {}\n",
            f.name, f.help, f.name, f.kind
        ));
        for (extra, value) in &f.series {
            // A summary's _sum and _count are separate series named after the
            // family, not label variants of it.
            let (name, extra) = match extra.as_str() {
                SUM_SUFFIX => (format!("{}_sum", f.name), ""),
                COUNT_SUFFIX => (format!("{}_count", f.name), ""),
                other => (f.name.to_string(), other),
            };
            let sep = if extra.is_empty() { "" } else { "," };
            // Counters and observation counts are whole events; the rest are not.
            let rendered = if f.kind == "counter" || extra.is_empty() && name.ends_with("_count") {
                format!("{value:.0}")
            } else {
                format!("{value:.5}")
            };
            out.push_str(&format!("{name}{{{base}{sep}{extra}}} {rendered}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_engine::NodeProfile;
    use sim_spec::GeneratorSpec;
    use std::sync::Arc;

    fn exporter() -> Exporter {
        let spec = GeneratorSpec::from_yaml(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../specs/prometheus-app.yaml"
            ))
            .unwrap(),
        )
        .unwrap();
        let profile = NodeProfile {
            hostname: "sim-app-01".into(),
            guid: "1c4d3e2f-0000-4000-8000-000000000000".into(),
            role: Some("web".into()),
            attrs: Default::default(),
            labels: Default::default(),
            instances: Default::default(),
            utc_offset_secs: 0,
        };
        Exporter::new(
            "sim-app-01".into(),
            "web".into(),
            NodeEngine::new(Arc::new(spec), profile, 7),
        )
    }

    #[test]
    fn a_scrape_parses_as_prometheus_text_format() {
        let mut e = exporter();
        let body = render(
            &mut e,
            &ScenarioSet::default(),
            1_700_000_000,
            1_699_900_000,
        );
        for line in body.lines() {
            if line.starts_with('#') {
                assert!(
                    line.starts_with("# HELP ") || line.starts_with("# TYPE "),
                    "bad comment line: {line}"
                );
                continue;
            }
            let value = line.rsplit(' ').next().unwrap();
            assert!(
                value.parse::<f64>().is_ok(),
                "not a number at end of: {line}"
            );
            assert!(
                line.contains("service=\"storefront\""),
                "unlabelled: {line}"
            );
        }
    }

    #[test]
    fn each_family_declares_help_and_type_exactly_once() {
        // A repeated HELP/TYPE for the same family makes strict Prometheus
        // parsers reject the entire scrape.
        let mut e = exporter();
        let body = render(
            &mut e,
            &ScenarioSet::default(),
            1_700_000_000,
            1_699_900_000,
        );
        let mut seen = std::collections::BTreeMap::new();
        for line in body.lines().filter(|l| l.starts_with("# TYPE ")) {
            let name = line
                .trim_start_matches("# TYPE ")
                .split(' ')
                .next()
                .unwrap();
            *seen.entry(name.to_string()).or_insert(0) += 1;
        }
        for (name, count) in seen {
            assert_eq!(count, 1, "family '{name}' declared its TYPE {count} times");
        }
    }

    #[test]
    fn counters_start_at_zero_and_grow_at_the_signal_rate() {
        // Anchoring counters at the Unix epoch produced values in the hundreds
        // of billions on the very first scrape.
        let mut e = exporter();
        let orders = |e: &mut Exporter, t: i64| -> f64 {
            render(e, &ScenarioSet::default(), t, 1_700_000_000)
                .lines()
                .find(|l| l.starts_with("app_orders_total{"))
                .and_then(|l| l.rsplit(' ').next().unwrap().parse().ok())
                .unwrap()
        };
        assert_eq!(orders(&mut e, 1_700_000_000), 0.0, "first scrape must be 0");
        let after_an_hour = (1..=3600)
            .map(|s| 1_700_000_000 + s)
            .fold(0.0, |_, t| orders(&mut e, t));
        // ~11 orders/s baseline, modulated by season - so tens of thousands an
        // hour, not billions and not zero.
        assert!(
            (5_000.0..500_000.0).contains(&after_an_hour),
            "an hour of orders was {after_an_hour}"
        );
    }

    #[test]
    fn every_series_declares_a_type() {
        let mut e = exporter();
        let body = render(
            &mut e,
            &ScenarioSet::default(),
            1_700_000_000,
            1_699_900_000,
        );
        let declared: Vec<&str> = body
            .lines()
            .filter_map(|l| l.strip_prefix("# TYPE "))
            .map(|l| l.split(' ').next().unwrap_or(""))
            .collect();
        for line in body.lines().filter(|l| !l.starts_with('#')) {
            let name = line.split('{').next().unwrap_or("");
            // A summary's quantile series carry the summary's own name; its
            // _sum and _count are derived from it and need no TYPE of their own.
            let base = name
                .strip_suffix("_sum")
                .or_else(|| name.strip_suffix("_count"))
                .unwrap_or(name);
            assert!(
                declared.contains(&name) || declared.contains(&base),
                "series '{name}' emitted with no # TYPE"
            );
        }
    }

    #[test]
    fn counters_never_go_backwards() {
        // go.d differentiates counters; a counter that decreases produces a
        // negative rate, which reads as a broken exporter.
        let mut e = exporter();
        let at = |e: &mut Exporter, t: i64| -> f64 {
            render(e, &ScenarioSet::default(), t, 1_700_000_000)
                .lines()
                .find(|l| l.starts_with("app_orders_total{"))
                .and_then(|l| l.rsplit(' ').next().unwrap().parse::<f64>().ok())
                .unwrap()
        };
        // Walk a full day: the failure this guards against only appeared once
        // the daily cycle turned the request rate downward.
        let mut previous = 0.0;
        for step in 0..(24 * 60) {
            let v = at(&mut e, 1_700_000_000 + step * 60);
            assert!(v >= previous, "counter went backwards: {previous} -> {v}");
            previous = v;
        }
    }

    #[test]
    fn a_summary_carries_its_sum_and_count() {
        // go.d skips a summary whose _sum or _count is missing, so quantiles
        // alone produce no latency chart at all - the exact failure this had.
        let mut e = exporter();
        // The first scrape of a process integrates nothing, so take a second.
        render(
            &mut e,
            &ScenarioSet::default(),
            1_700_000_000,
            1_700_000_000,
        );
        let body = render(
            &mut e,
            &ScenarioSet::default(),
            1_700_000_060,
            1_700_000_000,
        );
        for suffix in ["_sum", "_count"] {
            let name = format!("app_request_duration_seconds{suffix}");
            let line = body
                .lines()
                .find(|l| l.starts_with(&format!("{name}{{")))
                .unwrap_or_else(|| panic!("no {name} series"));
            let v: f64 = line.rsplit(' ').next().unwrap().parse().unwrap();
            assert!(v > 0.0, "{name} was {v}");
            assert!(!line.contains("quantile="), "{name} must not be a quantile");
        }
    }

    #[test]
    fn latency_survives_the_round_trip_as_a_float() {
        // The bug this guards: sampling through `tick()` rounds to integers, so
        // a 42ms p50 would publish as 0.
        let mut e = exporter();
        let body = render(
            &mut e,
            &ScenarioSet::default(),
            1_700_000_000,
            1_699_900_000,
        );
        let p50: f64 = body
            .lines()
            .find(|l| l.contains("quantile=\"0.5\""))
            .and_then(|l| l.rsplit(' ').next().unwrap().parse().ok())
            .unwrap();
        assert!(p50 > 0.0 && p50 < 1.0, "p50 was {p50}");
    }

    #[test]
    fn an_unknown_host_is_a_404_not_a_panic() {
        let mut exporters = vec![exporter()];
        let control = Arc::new(Mutex::new(ScenarioSet::default()));
        assert!(route("/metrics/nope", &mut exporters, &control, 0).is_none());
        assert!(route("/", &mut exporters, &control, 0).is_none());
        assert!(route("/metrics/sim-app-01", &mut exporters, &control, 0).is_some());
    }
}
