//! Ships the application tier's logs and traces to Netdata over OTLP.
//!
//! A separate process, like the journald writer and the Prometheus exporters,
//! and for the same reason: Netdata owns the metrics plugin's lifecycle, and a
//! collector that outlives its own removal has already cost this project real
//! debugging time.
//!
//! ## Why gRPC lives here
//!
//! Netdata's receiver is gRPC-only (`otel.yaml`: "gRPC endpoint to listen on",
//! `127.0.0.1:4317`), so there is no hand-rollable HTTP/JSON path. The
//! alternative was the Python SDK inside the container, which would mean adding
//! `apt` and `pip` layers to the stock netdata image. This repository ships a
//! binary into that image already, so doing it in Rust changes nothing outside
//! this repository.
//!
//! ## What lands where
//!
//! OTLP log streams are identified by `(service.namespace, service.name)` and
//! never by host (netdata/netdata @ c23face0bd94
//! src/crates/otel-logs-identity/src/lib.rs:1-20), so everything sent here
//! arrives on the *agent's own* node as one stream per service. The simulated
//! host survives as the `host.name` resource attribute, which is queryable in
//! the `otel-logs` function - it is a filter, not a log source. Per-node log
//! sources are the journald writer's job, and both run.
//!
//! Traces are accepted and stored, and **nothing can display them yet**: trace
//! ingestion is a proof scaffold (src/crates/otel-ingestor/src/trace_service.rs)
//! and no traces function is registered anywhere in the agent. They are sent so
//! the pipeline is exercised and correct on the day a viewer ships. Do not build
//! a demo beat on them.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opentelemetry_proto::tonic::collector::logs::v1::{
    logs_service_client::LogsServiceClient, ExportLogsServiceRequest,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    trace_service_client::TraceServiceClient, ExportTraceServiceRequest,
};
use opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{status, ResourceSpans, ScopeSpans, Span, Status};
use sim_engine::otel::{AppLog, AppSpan, AppTelemetry};
use sim_engine::{NodeEngine, ScenarioSet};

/// Default OTLP gRPC endpoint: the agent's own receiver, on loopback.
pub const DEFAULT_ENDPOINT: &str = "127.0.0.1:4317";

/// The instrumentation scope every record carries.
const SCOPE: &str = "infra-sim";

/// One node's OTEL producer: the engine that supplies the numbers, and the
/// generator that turns them into records.
pub struct Node {
    pub engine: NodeEngine,
    pub telemetry: AppTelemetry,
    /// Resource attributes, built once - they never change for a node.
    resource: Vec<KeyValue>,
}

impl Node {
    pub fn new(
        engine: NodeEngine,
        telemetry: AppTelemetry,
        simulation: &str,
        namespace: &str,
    ) -> Self {
        let resource = vec![
            kv("service.name", telemetry.service_name()),
            kv("service.namespace", namespace),
            // The simulated host. Not a log source - a filter. See the module
            // note above.
            kv("host.name", telemetry.hostname()),
            kv("infra_sim_name", simulation),
            kv("infra_sim_role", telemetry.role()),
            // Same marking every simulated node carries, so nothing that reaches
            // a Space can be mistaken for real telemetry.
            kv("simulated", "true"),
        ];
        Self {
            engine,
            telemetry,
            resource,
        }
    }
}

/// Send logs and traces until killed.
///
/// `control` is the same scenario state the metrics plugin reads, so a triggered
/// fault moves spans and log lines on the same timeline as the charts.
pub async fn run(
    endpoint: &str,
    mut nodes: Vec<Node>,
    control: std::sync::Arc<std::sync::Mutex<ScenarioSet>>,
    interval: Duration,
) -> Result<(), String> {
    if nodes.is_empty() {
        return Err(
            "no application-tier nodes in this fleet, so there is nothing to instrument".into(),
        );
    }
    let uri = format!("http://{endpoint}");
    let channel = tonic::transport::Channel::from_shared(uri.clone())
        .map_err(|e| format!("bad OTLP endpoint '{endpoint}': {e}"))?
        // Reconnects on its own; a receiver that is not up yet must not be
        // fatal, because the agent and this process start together.
        .connect_timeout(Duration::from_secs(5))
        .connect_lazy();

    let mut logs_client = LogsServiceClient::new(channel.clone());
    let mut traces_client = TraceServiceClient::new(channel);

    eprintln!(
        "infra-sim otlp: {} application node(s) -> {uri}",
        nodes.len()
    );
    for n in &nodes {
        eprintln!(
            "  {} as {}",
            n.telemetry.hostname(),
            n.telemetry.service_name()
        );
    }

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // One state per signal, because they do not fail together. Netdata's stable
    // build accepts OTLP logs and has no traces section in its `otel.yaml` at
    // all, so traces fail on every tick while logs are landing perfectly. One
    // shared flag reported that as "export failed" forever and hid the fact that
    // logs were fine.
    let mut logs_state = Signal::new("logs");
    let mut traces_state = Signal::new("traces");

    loop {
        ticker.tick().await;
        let scenarios = control
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| ScenarioSet::default());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let now_secs = now.as_secs() as i64;
        let now_ns = now.as_nanos() as i64;

        let mut resource_logs = Vec::new();
        let mut resource_spans = Vec::new();

        for node in &mut nodes {
            let values: BTreeMap<String, f64> = node.engine.signal_values(&scenarios, now_secs);
            let (logs, traces) = node.telemetry.tick(&values, now_ns, interval.as_secs_f64());

            if !logs.is_empty() {
                resource_logs.push(ResourceLogs {
                    resource: Some(Resource {
                        attributes: node.resource.clone(),
                        dropped_attributes_count: 0,
                        entity_refs: Vec::new(),
                    }),
                    scope_logs: vec![ScopeLogs {
                        scope: Some(scope()),
                        log_records: logs.iter().map(log_record).collect(),
                        schema_url: String::new(),
                    }],
                    schema_url: String::new(),
                });
            }
            if !traces.is_empty() {
                resource_spans.push(ResourceSpans {
                    resource: Some(Resource {
                        attributes: node.resource.clone(),
                        dropped_attributes_count: 0,
                        entity_refs: Vec::new(),
                    }),
                    scope_spans: vec![ScopeSpans {
                        scope: Some(scope()),
                        spans: traces.iter().flatten().map(span).collect(),
                        schema_url: String::new(),
                    }],
                    schema_url: String::new(),
                });
            }
        }

        if !resource_logs.is_empty() && !logs_state.given_up {
            let result = logs_client
                .export(ExportLogsServiceRequest { resource_logs })
                .await;
            logs_state.record(result.err().map(|e| e.to_string()));
        }
        if !resource_spans.is_empty() && !traces_state.given_up {
            let result = traces_client
                .export(ExportTraceServiceRequest { resource_spans })
                .await;
            traces_state.record(result.err().map(|e| e.to_string()));
        }
    }
}

/// How many consecutive failures before a signal is abandoned.
///
/// Long enough to ride out the agent still starting up - the container starts the
/// agent and this process together - and short enough not to retry a receiver
/// that will never accept this signal for the life of the simulation.
const GIVE_UP_AFTER: u32 = 30;

/// Per-signal export health, so one signal's permanent failure does not hide
/// another's success.
struct Signal {
    name: &'static str,
    consecutive_failures: u32,
    complained: bool,
    given_up: bool,
}

impl Signal {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            consecutive_failures: 0,
            complained: false,
            given_up: false,
        }
    }

    fn record(&mut self, error: Option<String>) {
        match error {
            None => {
                if self.complained {
                    eprintln!("infra-sim otlp: {} accepted again", self.name);
                }
                self.consecutive_failures = 0;
                self.complained = false;
            }
            Some(e) => {
                self.consecutive_failures += 1;
                if !self.complained {
                    // Not fatal: the agent and this process start together, so
                    // the first few seconds are expected to fail.
                    eprintln!(
                        "infra-sim otlp: {} rejected, retrying ({})",
                        self.name,
                        first_line(&e)
                    );
                    self.complained = true;
                }
                if self.consecutive_failures >= GIVE_UP_AFTER {
                    self.given_up = true;
                    eprintln!(
                        "infra-sim otlp: giving up on {} after {GIVE_UP_AFTER} failures. This \
                         agent build does not accept it - Netdata's stable image has no `traces:` \
                         section in its otel.yaml, and only newer builds store traces. Logs are \
                         unaffected.",
                        self.name
                    );
                }
            }
        }
    }
}

/// A tonic error's first line. The full chain names the same failure four times.
fn first_line(e: &str) -> &str {
    e.split(", source").next().unwrap_or(e)
}

fn scope() -> InstrumentationScope {
    InstrumentationScope {
        name: SCOPE.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        attributes: Vec::new(),
        dropped_attributes_count: 0,
    }
}

fn kv(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.into(),
        // The string-table index form; 0 means "not interned", which is what a
        // sender that does not use the shared string table reports.
        key_strindex: 0,
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value.into())),
        }),
    }
}

fn log_record(l: &AppLog) -> LogRecord {
    LogRecord {
        time_unix_nano: l.time_ns as u64,
        observed_time_unix_nano: l.time_ns as u64,
        severity_number: l.severity as i32,
        severity_text: l.severity.text().into(),
        body: Some(AnyValue {
            value: Some(any_value::Value::StringValue(l.body.clone())),
        }),
        attributes: l.attrs.iter().map(|(k, v)| kv(k, v)).collect(),
        dropped_attributes_count: 0,
        flags: 0,
        trace_id: Vec::new(),
        span_id: Vec::new(),
        event_name: String::new(),
    }
}

fn span(s: &AppSpan) -> Span {
    Span {
        trace_id: s.trace_id.to_vec(),
        span_id: s.span_id.to_vec(),
        trace_state: String::new(),
        parent_span_id: s.parent_span_id.map(|p| p.to_vec()).unwrap_or_default(),
        flags: 0,
        name: s.name.clone(),
        kind: s.kind,
        start_time_unix_nano: s.start_ns as u64,
        end_time_unix_nano: s.end_ns as u64,
        attributes: s.attrs.iter().map(|(k, v)| kv(k, v)).collect(),
        dropped_attributes_count: 0,
        events: Vec::new(),
        dropped_events_count: 0,
        links: Vec::new(),
        dropped_links_count: 0,
        status: Some(match &s.error {
            Some(message) => Status {
                message: message.clone(),
                code: status::StatusCode::Error as i32,
            },
            None => Status {
                message: String::new(),
                code: status::StatusCode::Ok as i32,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_engine::otel::Severity;

    #[test]
    fn a_log_record_carries_body_severity_and_attributes() {
        let r = log_record(&AppLog {
            time_ns: 1_700_000_000_000_000_000,
            severity: Severity::Error,
            body: "GET /checkout failed after 900ms: db.timeout".into(),
            attrs: vec![("error.type".into(), "db.timeout".into())],
        });
        assert_eq!(r.severity_number, 17);
        assert_eq!(r.severity_text, "ERROR");
        assert_eq!(r.time_unix_nano, 1_700_000_000_000_000_000);
        assert_eq!(r.attributes.len(), 1);
        assert!(matches!(
            r.body.and_then(|b| b.value),
            Some(any_value::Value::StringValue(s)) if s.contains("db.timeout")
        ));
    }

    #[test]
    fn a_failed_span_is_marked_error_and_a_root_has_no_parent() {
        let root = AppSpan {
            name: "POST /checkout".into(),
            kind: 1,
            start_ns: 10,
            end_ns: 20,
            trace_id: [7u8; 16],
            span_id: [3u8; 8],
            parent_span_id: None,
            attrs: Vec::new(),
            error: Some("db.timeout".into()),
        };
        let s = span(&root);
        assert!(s.parent_span_id.is_empty());
        assert_eq!(s.trace_id.len(), 16);
        assert_eq!(s.span_id.len(), 8);
        assert_eq!(s.status.unwrap().code, status::StatusCode::Error as i32);
    }

    #[test]
    fn a_child_span_reports_its_parent() {
        let child = AppSpan {
            name: "SELECT orders".into(),
            kind: 3,
            start_ns: 12,
            end_ns: 18,
            trace_id: [7u8; 16],
            span_id: [4u8; 8],
            parent_span_id: Some([3u8; 8]),
            attrs: vec![("db.system".into(), "postgresql".into())],
            error: None,
        };
        let s = span(&child);
        assert_eq!(s.parent_span_id, vec![3u8; 8]);
        assert_eq!(s.status.unwrap().code, status::StatusCode::Ok as i32);
    }
}
