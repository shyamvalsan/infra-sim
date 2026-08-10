//! Application telemetry for OpenTelemetry: log records and traces.
//!
//! This is the application tier talking about itself. `logs.rs` writes what the
//! *machine* says - systemd, the kernel, a package manager - and that reaches
//! Netdata as journald, one log source per simulated node. This module writes
//! what the *service* says, and that reaches Netdata as OTLP.
//!
//! Both are wanted, because a real estate has both, and because OTLP cannot do
//! what journald does here: an OTel log stream is identified by
//! `(service.namespace, service.name)` and never by host
//! (netdata/netdata @ c23face0bd94 src/crates/otel-logs-identity/src/lib.rs:1-20),
//! so OTLP records land on the ingesting agent as one stream per service. The
//! simulated host survives as a resource attribute, which is queryable, but it
//! is not a log source.
//!
//! ## Everything here is derived from signals
//!
//! Nothing in this module invents a fault. Durations come from
//! `app_latency_p50/p95/p99`, error rates from `app_requests_error_rate`, order
//! volume from `app_orders_rate` - the same values `exporters.rs` publishes and
//! the same values the charts are drawn from, read through one
//! `NodeEngine::signal_values()` call. So a scenario that triples request latency
//! triples span durations, without this file knowing that scenario exists.
//!
//! That is the whole point. A demo where the latency chart climbs but the traces
//! do not is worse than no traces.

use std::collections::BTreeMap;

use crate::rng::Rng;

/// OTel severity numbers, as the specification defines them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Debug = 5,
    Info = 9,
    Warn = 13,
    Error = 17,
}

impl Severity {
    pub fn text(self) -> &'static str {
        match self {
            Severity::Debug => "DEBUG",
            Severity::Info => "INFO",
            Severity::Warn => "WARN",
            Severity::Error => "ERROR",
        }
    }
}

/// One application log record, before it becomes OTLP.
#[derive(Debug, Clone, PartialEq)]
pub struct AppLog {
    pub time_ns: i64,
    pub severity: Severity,
    pub body: String,
    /// Log-record attributes. Resource attributes (host, service, simulation)
    /// are added once per batch by the transport, not repeated per record.
    pub attrs: Vec<(String, String)>,
}

/// One span.
#[derive(Debug, Clone, PartialEq)]
pub struct AppSpan {
    pub name: String,
    /// 1 = server, 3 = client, per the OTel span-kind enum.
    pub kind: i32,
    pub start_ns: i64,
    pub end_ns: i64,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub attrs: Vec<(String, String)>,
    /// Sets the span's status to Error and, on the root, the log record's
    /// severity too.
    pub error: Option<String>,
}

/// Routes a storefront serves, with their relative share of traffic and whether
/// they touch the database and the cache. Shaped so a trace looks like a request
/// and not like a benchmark: a static asset does not query Postgres.
const ROUTES: &[(&str, f64, bool, bool)] = &[
    ("GET /", 0.28, false, true),
    ("GET /products/{id}", 0.24, true, true),
    ("GET /search", 0.14, true, false),
    ("POST /cart", 0.12, true, true),
    ("GET /cart", 0.10, false, true),
    ("POST /checkout", 0.07, true, true),
    ("GET /health", 0.05, false, false),
];

/// Head sampling: the share of requests that produce a trace.
///
/// A real service samples - one span per request at a few hundred requests a
/// second would be a load test of the receiver rather than a simulation.
/// Expressing it as a *ratio* rather than a fixed rate matters: a scenario that
/// doubles traffic then doubles the traces, so trace volume moves with the
/// request chart instead of sitting at a constant while the chart climbs.
const TRACE_SAMPLE_RATIO: f64 = 0.01;

/// Ceiling on sampled traces per second per node, so a fleet under a traffic
/// scenario cannot flood the receiver.
const MAX_TRACES_PER_SEC: f64 = 8.0;

/// Per-node application telemetry.
pub struct AppTelemetry {
    hostname: String,
    role: String,
    service_name: String,
    rng: Rng,
    /// Fractional carry, so a rate below one per tick still fires sometimes
    /// instead of never.
    trace_credit: f64,
    order_credit: f64,
    decline_credit: f64,
}

impl AppTelemetry {
    pub fn new(hostname: &str, role: &str, service_name: &str, master_seed: u64) -> Self {
        Self {
            hostname: hostname.to_string(),
            role: role.to_string(),
            service_name: service_name.to_string(),
            rng: Rng::from_stream(master_seed, &format!("otel:{hostname}")),
            trace_credit: 0.0,
            order_credit: 0.0,
            decline_credit: 0.0,
        }
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub fn role(&self) -> &str {
        &self.role
    }

    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// Telemetry for one tick, from this node's current signal values.
    ///
    /// `now_ns` is wall-clock nanoseconds; `interval` is seconds since the last
    /// tick.
    pub fn tick(
        &mut self,
        values: &BTreeMap<String, f64>,
        now_ns: i64,
        interval: f64,
    ) -> (Vec<AppLog>, Vec<Vec<AppSpan>>) {
        let get = |k: &str| values.get(k).copied().unwrap_or(0.0);
        let requests = get("app_requests_rate").max(0.0);
        let errors = get("app_requests_error_rate").clamp(0.0, requests);
        let error_ratio = if requests > 0.0 {
            errors / requests
        } else {
            0.0
        };
        // The spec declares these in **seconds** (`units: seconds`, p50 base
        // 0.042). Reading them as milliseconds made every span sub-millisecond
        // and printed "200 in 0ms" - the kind of detail that reads as fake
        // immediately.
        let p50 = get("app_latency_p50").max(0.0005);
        let p95 = get("app_latency_p95").max(p50);
        let p99 = get("app_latency_p99").max(p95);

        let mut logs = Vec::new();
        let mut traces = Vec::new();

        // --- traces ---------------------------------------------------------
        self.trace_credit +=
            (requests * TRACE_SAMPLE_RATIO * interval).min(MAX_TRACES_PER_SEC * interval);
        while self.trace_credit >= 1.0 {
            self.trace_credit -= 1.0;
            let (spans, root_error) = self.one_trace(now_ns, p50, p95, p99, error_ratio, values);
            // A failed request is exactly the line an engineer greps for, so it
            // is always logged; a successful one is sampled, because a service
            // logging every 200 is a service nobody reads the logs of.
            if let Some(reason) = root_error {
                let root = &spans[0];
                logs.push(AppLog {
                    time_ns: root.end_ns,
                    severity: Severity::Error,
                    body: format!(
                        "{} failed after {}ms: {reason}",
                        root.name,
                        ms(root.end_ns - root.start_ns)
                    ),
                    attrs: vec![
                        ("http.route".into(), route_of(&root.name).into()),
                        ("trace_id".into(), hex16(&root.trace_id)),
                        ("error.type".into(), reason),
                    ],
                });
            } else if self.rng.next_f64() < 0.25 {
                let root = &spans[0];
                logs.push(AppLog {
                    time_ns: root.end_ns,
                    severity: Severity::Info,
                    body: format!("{} 200 in {}ms", root.name, ms(root.end_ns - root.start_ns)),
                    attrs: vec![
                        ("http.route".into(), route_of(&root.name).into()),
                        ("trace_id".into(), hex16(&root.trace_id)),
                    ],
                });
            }
            traces.push(spans);
        }

        // --- business events -------------------------------------------------
        self.order_credit += get("app_orders_rate").max(0.0) * interval;
        while self.order_credit >= 1.0 {
            self.order_credit -= 1.0;
            // The signal is the tier's cart value; one order is a draw around
            // it. Emitting the signal verbatim gave every order in a tick the
            // same total to the cent.
            let spread = 0.35 + self.rng.next_f64() * 1.6;
            let value = get("app_cart_value_total").max(1.0) * spread;
            logs.push(AppLog {
                time_ns: now_ns,
                severity: Severity::Info,
                body: format!("order placed, {value:.2} total"),
                attrs: vec![
                    ("event".into(), "order.placed".into()),
                    ("order.value".into(), format!("{value:.2}")),
                ],
            });
        }

        self.decline_credit += get("app_payment_declined_rate").max(0.0) * interval;
        while self.decline_credit >= 1.0 {
            self.decline_credit -= 1.0;
            logs.push(AppLog {
                time_ns: now_ns,
                severity: Severity::Warn,
                body: "payment declined by processor".into(),
                attrs: vec![("event".into(), "payment.declined".into())],
            });
        }

        // --- saturation ------------------------------------------------------
        // Reported only when it is true, so a healthy tier stays quiet and these
        // lines mean something when they appear.
        let busy = get("app_workers_busy");
        let total_workers = get("app_workers_total");
        if total_workers > 0.0 && busy / total_workers > 0.9 {
            logs.push(AppLog {
                time_ns: now_ns,
                severity: Severity::Warn,
                body: format!("worker pool saturated: {busy:.0}/{total_workers:.0} busy"),
                attrs: vec![("event".into(), "workers.saturated".into())],
            });
        }
        let in_use = get("app_db_pool_in_use");
        let pool = get("app_db_pool_size");
        if pool > 0.0 && in_use / pool > 0.9 {
            logs.push(AppLog {
                time_ns: now_ns,
                severity: Severity::Warn,
                body: format!("database pool exhausted: {in_use:.0}/{pool:.0} connections in use"),
                attrs: vec![("event".into(), "db.pool.exhausted".into())],
            });
        }
        let oldest = get("app_queue_oldest_seconds");
        if oldest > 60.0 {
            logs.push(AppLog {
                time_ns: now_ns,
                severity: Severity::Warn,
                body: format!("queue backlog: oldest job {oldest:.0}s old"),
                attrs: vec![("event".into(), "queue.backlog".into())],
            });
        }

        (logs, traces)
    }

    /// One request, as a root span plus whatever it called.
    fn one_trace(
        &mut self,
        now_ns: i64,
        p50: f64,
        p95: f64,
        p99: f64,
        error_ratio: f64,
        values: &BTreeMap<String, f64>,
    ) -> (Vec<AppSpan>, Option<String>) {
        let (route, _, hits_db, hits_cache) = self.pick_route();
        let total_s = self.sample_latency(p50, p95, p99);
        let failed = self.rng.next_f64() < error_ratio;

        let trace_id = self.trace_id();
        let root_id = self.span_id();
        let start = now_ns;
        let end = start + secs_to_ns(total_s);

        // The child spans have to fit inside the parent, or the trace is
        // nonsense: a database call cannot outlast the request that made it.
        let mut children = Vec::new();
        let mut cursor = start + secs_to_ns(total_s * 0.08);

        if hits_cache {
            let hits = values.get("app_cache_hits_rate").copied().unwrap_or(0.0);
            let misses = values.get("app_cache_misses_rate").copied().unwrap_or(0.0);
            let miss = hits + misses > 0.0 && self.rng.next_f64() < misses / (hits + misses);
            let share = if miss { 0.10 } else { 0.02 };
            let span_end = cursor + secs_to_ns(total_s * share);
            children.push(AppSpan {
                name: "redis GET session".into(),
                kind: 3,
                start_ns: cursor,
                end_ns: span_end.min(end),
                trace_id,
                span_id: self.span_id(),
                parent_span_id: Some(root_id),
                attrs: vec![
                    ("db.system".into(), "redis".into()),
                    ("cache.hit".into(), (!miss).to_string()),
                ],
                error: None,
            });
            cursor = span_end;
        }

        if hits_db {
            // A pool wait is the thing worth seeing when the tier is saturated,
            // so it is its own span rather than hidden inside the query.
            let waits = values.get("app_db_pool_wait_rate").copied().unwrap_or(0.0);
            if waits > 0.0 && self.rng.next_f64() < (waits / 20.0).min(0.6) {
                let span_end = cursor + secs_to_ns(total_s * 0.25);
                children.push(AppSpan {
                    name: "db.pool acquire".into(),
                    kind: 3,
                    start_ns: cursor,
                    end_ns: span_end.min(end),
                    trace_id,
                    span_id: self.span_id(),
                    parent_span_id: Some(root_id),
                    attrs: vec![("db.system".into(), "postgresql".into())],
                    error: None,
                });
                cursor = span_end;
            }
            let span_end = cursor + secs_to_ns(total_s * 0.55);
            children.push(AppSpan {
                name: sql_for(route).into(),
                kind: 3,
                start_ns: cursor,
                end_ns: span_end.min(end),
                trace_id,
                span_id: self.span_id(),
                parent_span_id: Some(root_id),
                attrs: vec![
                    ("db.system".into(), "postgresql".into()),
                    ("db.operation".into(), "SELECT".into()),
                ],
                error: failed.then(|| "timeout".to_string()),
            });
        }

        let reason = failed.then(|| {
            if hits_db {
                "db.timeout".to_string()
            } else {
                "upstream.timeout".to_string()
            }
        });

        let mut spans = vec![AppSpan {
            name: route.to_string(),
            kind: 1,
            start_ns: start,
            end_ns: end,
            trace_id,
            span_id: root_id,
            parent_span_id: None,
            attrs: vec![
                ("http.route".into(), route_of(route).into()),
                ("http.request.method".into(), method_of(route).into()),
                (
                    "http.response.status_code".into(),
                    if failed { "500" } else { "200" }.into(),
                ),
            ],
            error: reason.clone(),
        }];
        spans.extend(children);
        (spans, reason)
    }

    fn pick_route(&mut self) -> (&'static str, f64, bool, bool) {
        let total: f64 = ROUTES.iter().map(|(_, w, _, _)| w).sum();
        let mut pick = self.rng.next_f64() * total;
        for r in ROUTES {
            pick -= r.1;
            if pick <= 0.0 {
                return *r;
            }
        }
        ROUTES[0]
    }

    /// A latency in seconds, drawn from the shape p50/p95/p99 describe.
    ///
    /// Sampling uniformly between p50 and p99 would make the mean sit far above
    /// the median and contradict the summary the exporter publishes for the same
    /// node.
    fn sample_latency(&mut self, p50: f64, p95: f64, p99: f64) -> f64 {
        let u = self.rng.next_f64();
        let jitter = 0.8 + self.rng.next_f64() * 0.4;
        let v = if u < 0.5 {
            p50 * (0.35 + u)
        } else if u < 0.95 {
            p50 + (p95 - p50) * ((u - 0.5) / 0.45)
        } else {
            p95 + (p99 - p95) * ((u - 0.95) / 0.05)
        };
        (v * jitter).max(0.0002)
    }

    fn trace_id(&mut self) -> [u8; 16] {
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&self.rng.next_u64().to_be_bytes());
        id[8..].copy_from_slice(&self.rng.next_u64().to_be_bytes());
        // An all-zero id is invalid; the odds are negligible but the check is
        // cheaper than the debugging.
        if id == [0u8; 16] {
            id[15] = 1;
        }
        id
    }

    fn span_id(&mut self) -> [u8; 8] {
        let mut id = self.rng.next_u64().to_be_bytes();
        if id == [0u8; 8] {
            id[7] = 1;
        }
        id
    }
}

/// Whole milliseconds, for a log line a human reads.
fn ms(nanos: i64) -> i64 {
    nanos / 1_000_000
}

fn secs_to_ns(seconds: f64) -> i64 {
    (seconds * 1_000_000_000.0) as i64
}

fn route_of(name: &str) -> &str {
    name.split_once(' ').map(|(_, p)| p).unwrap_or(name)
}

fn method_of(name: &str) -> &str {
    name.split_once(' ').map(|(m, _)| m).unwrap_or("GET")
}

fn sql_for(route: &str) -> &'static str {
    match route {
        "GET /products/{id}" => "SELECT products",
        "GET /search" => "SELECT products WHERE name LIKE",
        "POST /cart" => "INSERT cart_items",
        "POST /checkout" => "SELECT orders",
        _ => "SELECT sessions",
    }
}

fn hex16(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()
    }

    fn healthy() -> BTreeMap<String, f64> {
        values(&[
            ("app_requests_rate", 40.0),
            ("app_requests_error_rate", 0.0),
            ("app_latency_p50", 0.042),
            ("app_latency_p95", 0.19),
            ("app_latency_p99", 0.63),
            ("app_orders_rate", 0.5),
            ("app_cart_value_total", 74.0),
            ("app_workers_busy", 4.0),
            ("app_workers_total", 32.0),
            ("app_db_pool_in_use", 3.0),
            ("app_db_pool_size", 20.0),
            ("app_cache_hits_rate", 90.0),
            ("app_cache_misses_rate", 10.0),
        ])
    }

    #[test]
    fn a_healthy_tier_reports_no_errors() {
        let mut t = AppTelemetry::new("sim-web-01", "web", "storefront", 7);
        let mut logs = Vec::new();
        let mut traces = Vec::new();
        for i in 0..60 {
            let (l, tr) = t.tick(&healthy(), 1_000_000_000 * i, 1.0);
            logs.extend(l);
            traces.extend(tr);
        }
        assert!(!traces.is_empty(), "a busy tier must produce traces");
        assert!(
            traces.iter().flatten().all(|s| s.error.is_none()),
            "no span may fail when the error rate is zero"
        );
        assert!(
            logs.iter().all(|l| l.severity != Severity::Error),
            "no ERROR line may appear on a healthy tier"
        );
    }

    #[test]
    fn a_faulted_tier_fails_spans_and_logs_them() {
        let mut v = healthy();
        v.insert("app_requests_error_rate".into(), 20.0); // half of them
        let mut t = AppTelemetry::new("sim-web-01", "web", "storefront", 7);
        let mut logs = Vec::new();
        let mut traces = Vec::new();
        for i in 0..60 {
            let (l, tr) = t.tick(&v, 1_000_000_000 * i, 1.0);
            logs.extend(l);
            traces.extend(tr);
        }
        assert!(traces.iter().flatten().any(|s| s.error.is_some()));
        assert!(logs.iter().any(|l| l.severity == Severity::Error));
    }

    #[test]
    fn span_durations_follow_the_latency_signals() {
        // The whole point of driving this from signals: when a scenario raises
        // latency, traces have to move with the chart.
        let mut slow = healthy();
        for k in ["app_latency_p50", "app_latency_p95", "app_latency_p99"] {
            let v = slow[k] * 10.0;
            slow.insert(k.into(), v);
        }
        let mean = |v: &BTreeMap<String, f64>| {
            let mut t = AppTelemetry::new("sim-web-01", "web", "storefront", 11);
            let mut total = 0i64;
            let mut n = 0i64;
            for i in 0..120 {
                let (_, traces) = t.tick(v, 1_000_000_000 * i, 1.0);
                for spans in traces {
                    total += spans[0].end_ns - spans[0].start_ns;
                    n += 1;
                }
            }
            total as f64 / n.max(1) as f64
        };
        let fast = mean(&healthy());
        let slow = mean(&slow);
        assert!(
            slow > fast * 5.0,
            "ten times the latency must show up in span durations: {fast} -> {slow}"
        );
    }

    #[test]
    fn children_never_outlive_their_parent() {
        let mut t = AppTelemetry::new("sim-web-01", "web", "storefront", 3);
        for i in 0..200 {
            let (_, traces) = t.tick(&healthy(), 1_000_000_000 * i, 1.0);
            for spans in traces {
                let root = &spans[0];
                assert!(root.end_ns >= root.start_ns);
                for child in &spans[1..] {
                    assert_eq!(child.trace_id, root.trace_id);
                    assert_eq!(child.parent_span_id, Some(root.span_id));
                    assert!(
                        child.start_ns >= root.start_ns && child.end_ns <= root.end_ns,
                        "a call cannot outlast the request that made it"
                    );
                }
            }
        }
    }

    #[test]
    fn an_idle_tier_produces_nothing() {
        let mut t = AppTelemetry::new("sim-web-01", "web", "storefront", 5);
        let idle = values(&[("app_requests_rate", 0.0), ("app_latency_p50", 0.042)]);
        let (logs, traces) = t.tick(&idle, 0, 1.0);
        assert!(logs.is_empty() && traces.is_empty());
    }

    #[test]
    fn saturation_is_reported_only_when_it_is_real() {
        let mut t = AppTelemetry::new("sim-web-01", "web", "storefront", 5);
        let (quiet, _) = t.tick(&healthy(), 0, 1.0);
        assert!(!quiet.iter().any(|l| l.body.contains("saturated")));

        let mut hot = healthy();
        hot.insert("app_workers_busy".into(), 31.0);
        let (loud, _) = t.tick(&hot, 0, 1.0);
        assert!(loud
            .iter()
            .any(|l| l.body.contains("worker pool saturated")));
    }
}
