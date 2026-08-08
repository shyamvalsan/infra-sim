#!/usr/bin/env python3
"""Emit OTLP metrics, logs and traces for a made-up application.

The showcase: an instrumented service sending OpenTelemetry straight to
Netdata's OTLP receiver, appearing as charts and logs in Netdata Cloud without
a collector in between.

Deliberately one readable file using the official OpenTelemetry SDK. Netdata's
receiver is gRPC-only, and pulling a gRPC and protobuf stack into the Rust
runtime to support an authoring-time demo aid would be the wrong trade - the
plugins.d path stays dependency-free.

What Netdata does with each signal (probed against agent v2.10.0, not assumed):

  metrics  ingested -> charts, as `otel.*` contexts
  logs     ingested -> the `otel-logs` function, with resource attributes
                       as columns
  traces   ingested and persisted to /var/log/netdata/otel/v2/traces/,
           but there is no trace viewer on the agent yet, so spans are
           accepted and stored rather than rendered

One thing worth knowing before you demo this: OTLP ingestion does **not**
create Netdata nodes. Everything arrives under the host running the agent, with
resource attributes flattened into `resource.attributes.*` labels. For a
multi-node fleet use the plugins.d path (`infra-sim` proper); use this to show
Netdata ingesting OpenTelemetry.
"""

from __future__ import annotations

import argparse
import logging
import random
import time

from opentelemetry import metrics, trace
from opentelemetry.exporter.otlp.proto.grpc._log_exporter import OTLPLogExporter
from opentelemetry.exporter.otlp.proto.grpc.metric_exporter import OTLPMetricExporter
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk._logs import LoggerProvider, LoggingHandler
from opentelemetry.sdk._logs.export import BatchLogRecordProcessor
from opentelemetry.sdk.metrics import MeterProvider
from opentelemetry.sdk.metrics.export import PeriodicExportingMetricReader
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.trace import SpanKind, Status, StatusCode

DEFAULT_ENDPOINT = "http://127.0.0.1:4317"

# The made-up application: a storefront, because the operations are ones an
# audience recognises without explanation.
OPERATIONS = [
    # name,        base latency ms, error rate, downstream
    ("GET /catalog", 42.0, 0.002, "catalog-db"),
    ("GET /product", 58.0, 0.004, "catalog-db"),
    ("POST /cart", 31.0, 0.003, "cart-cache"),
    ("POST /checkout", 210.0, 0.018, "payments-api"),
    ("GET /orders", 74.0, 0.006, "orders-db"),
]

ERROR_MESSAGES = {
    "payments-api": "payment authorization declined by upstream processor",
    "catalog-db": "query timeout after 3000ms",
    "cart-cache": "connection reset by peer",
    "orders-db": "deadlock detected, transaction rolled back",
}


def build(endpoint: str, service: str, version: str, environment: str):
    """Wire up the three signal pipelines against one shared resource.

    The resource is what identifies the service in Netdata; its attributes
    arrive as `resource.attributes.*` on both charts and logs.
    """
    resource = Resource.create(
        {
            "service.name": service,
            "service.version": version,
            "deployment.environment": environment,
            # Netdata will not turn this into a node - see the module docstring -
            # but it is the attribute people group by, so it is worth setting.
            "host.name": f"{service}-01",
        }
    )

    meter_provider = MeterProvider(
        resource=resource,
        metric_readers=[
            PeriodicExportingMetricReader(
                OTLPMetricExporter(endpoint=endpoint, insecure=True),
                export_interval_millis=5000,
            )
        ],
    )
    metrics.set_meter_provider(meter_provider)

    tracer_provider = TracerProvider(resource=resource)
    tracer_provider.add_span_processor(
        BatchSpanProcessor(OTLPSpanExporter(endpoint=endpoint, insecure=True))
    )
    trace.set_tracer_provider(tracer_provider)

    logger_provider = LoggerProvider(resource=resource)
    logger_provider.add_log_record_processor(
        BatchLogRecordProcessor(OTLPLogExporter(endpoint=endpoint, insecure=True))
    )

    app_log = logging.getLogger(service)
    app_log.setLevel(logging.INFO)
    app_log.addHandler(LoggingHandler(level=logging.INFO, logger_provider=logger_provider))
    # Keep the console readable; the interesting copy goes to Netdata.
    app_log.propagate = False

    return meter_provider, tracer_provider, logger_provider, app_log


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--endpoint", default=DEFAULT_ENDPOINT,
                    help=f"Netdata OTLP gRPC endpoint (default: {DEFAULT_ENDPOINT})")
    ap.add_argument("--service", default="storefront", help="service.name to report as")
    ap.add_argument("--version", default="2.4.1", help="service.version")
    ap.add_argument("--environment", default="production", help="deployment.environment")
    ap.add_argument("--rps", type=float, default=6.0,
                    help="simulated requests per second (default: 6)")
    ap.add_argument("--duration", type=float, default=0,
                    help="seconds to run; 0 runs until interrupted")
    ap.add_argument("--seed", type=int, default=None,
                    help="fix the RNG so a run is reproducible")
    args = ap.parse_args()

    rng = random.Random(args.seed)

    meter_provider, tracer_provider, logger_provider, app_log = build(
        args.endpoint, args.service, args.version, args.environment
    )
    meter = metrics.get_meter(args.service)
    tracer = trace.get_tracer(args.service)

    requests = meter.create_counter(
        "http.server.requests", unit="{request}", description="HTTP requests served")
    errors = meter.create_counter(
        "http.server.errors", unit="{request}", description="HTTP requests that failed")
    latency = meter.create_histogram(
        "http.server.duration", unit="ms", description="HTTP request duration")
    in_flight = meter.create_up_down_counter(
        "http.server.active_requests", unit="{request}", description="Requests in flight")
    cart_value = meter.create_gauge(
        "storefront.cart.value", unit="USD", description="Value of the cart just checked out")

    print(f"emitting OTLP to {args.endpoint} as service.name={args.service}")
    print("  metrics -> Netdata charts (otel.* contexts)")
    print("  logs    -> the otel-logs function")
    print("  traces  -> stored by the agent; no trace viewer yet")
    print("Ctrl-C to stop.")

    started = time.monotonic()
    interval = 1.0 / max(args.rps, 0.1)

    try:
        while args.duration <= 0 or (time.monotonic() - started) < args.duration:
            name, base_ms, err_rate, downstream = rng.choices(
                OPERATIONS, weights=[5, 4, 3, 2, 2]
            )[0]

            # Lognormal-ish: most requests near the base, a long right tail, which
            # is what real latency looks like and what makes a p99 chart honest.
            duration_ms = base_ms * rng.lognormvariate(0, 0.45)
            failed = rng.random() < err_rate

            attrs = {"http.route": name.split(" ", 1)[1], "http.method": name.split(" ", 1)[0]}

            in_flight.add(1, attrs)
            with tracer.start_as_current_span(name, kind=SpanKind.SERVER) as span:
                span.set_attribute("http.request.method", attrs["http.method"])
                span.set_attribute("http.route", attrs["http.route"])
                span.set_attribute("service.version", args.version)

                # A child span for the downstream call, so the trace has shape
                # rather than being a single flat span.
                with tracer.start_as_current_span(
                    f"{downstream} query", kind=SpanKind.CLIENT
                ) as child:
                    child.set_attribute("peer.service", downstream)
                    child.set_attribute("db.system", "postgresql"
                                        if downstream.endswith("db") else "redis")
                    time.sleep(min(duration_ms, 400) / 1000.0 * 0.6)
                    if failed:
                        child.set_status(Status(StatusCode.ERROR, ERROR_MESSAGES[downstream]))

                status = 500 if failed else 200
                span.set_attribute("http.response.status_code", status)
                if failed:
                    span.set_status(Status(StatusCode.ERROR, ERROR_MESSAGES[downstream]))

                # Logged inside the span, so the log record carries the trace and
                # span ids - this is the correlation the whole demo rests on.
                if failed:
                    app_log.error("%s failed: %s", name, ERROR_MESSAGES[downstream])
                elif duration_ms > base_ms * 2.5:
                    app_log.warning("%s slow: %.0fms via %s", name, duration_ms, downstream)
                elif rng.random() < 0.05:
                    app_log.info("%s served in %.0fms", name, duration_ms)

            in_flight.add(-1, attrs)

            done = dict(attrs, **{"http.response.status_code": status})
            requests.add(1, done)
            latency.record(duration_ms, done)
            if failed:
                errors.add(1, done)
            if name == "POST /checkout" and not failed:
                cart_value.set(round(rng.lognormvariate(4.0, 0.7), 2), attrs)

            time.sleep(max(interval - duration_ms / 1000.0 * 0.6, 0.0))
    except KeyboardInterrupt:
        print("\nstopping")
    finally:
        # Flush before exit or the last few seconds never leave the process.
        for provider in (meter_provider, tracer_provider, logger_provider):
            try:
                provider.force_flush()
                provider.shutdown()
            except Exception:
                pass
        print("flushed and stopped")


if __name__ == "__main__":
    main()
