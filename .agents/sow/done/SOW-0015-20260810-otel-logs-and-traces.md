# SOW-0015 - Logs that actually run, and OpenTelemetry logs and traces

## Status

Status: completed

Sub-state: Delivered and validated on a live containerised agent. Logs and OTLP
start with the simulation; OTLP application logs land with the simulated host
attached; traces are confirmed accepted on a newer build and correctly refused on
stable, reported rather than retried forever.

## Requirements

### Purpose

A simulation shipped with no logs. Not a bug in the logs writer - nothing ever
started it. `scripts/sim-docker.sh` has `cmd_logs()` and `cmd_create()` never
calls it; `crates/sim-console/src/provision.rs:1062` says outright that the
writer "is a separate process an operator starts by hand". So every simulation
since the feature landed has had empty logs unless someone remembered a second
command. A demo environment with no logs is a broken demo environment.

Separately, the estate this project simulates is not journald-only. Modern
application tiers emit OpenTelemetry, and Netdata ingests it. A simulated fleet
that cannot show an `otel-logs` view is missing a surface the product has.

### User Request

"also ensure we have LOGS and TRACES being ingested.. i didn't find any logs in
the earlier sim (use OTEL for logs and traces)" (2026-08-10), followed by "Only
simulator level changes, no changes to netdata or its source".

### Assistant Understanding

Three things:

1. Logs start with the simulation, never by hand.
2. Application tiers emit OTLP logs, ingested by the agent's own OTLP receiver.
3. Application tiers emit OTLP traces, knowing they cannot be viewed yet.

Nothing outside this repository changes. The netdata checkout is read-only
reference and the container image stays exactly as netdata publishes it.

### Acceptance Criteria

1. Creating a simulation produces logs without a second command.
2. A simulated node is its own log source in the `systemd-journal` function.
3. The `otel-logs` function on the simulation's agent carries application log
   records whose resource attributes name the simulated host, role and
   simulation.
4. Spans are accepted by the receiver and written to the traces store.
5. OTEL telemetry moves with the scenario that is running, from the same signals
   the charts are drawn from - not a re-implementation of the fault logic.
6. Teardown stops everything it started.
7. No change to any file outside this repository; no change to the container
   image contents beyond the plugin binary already mounted into it.

## Analysis

Probed against the running agent and the shipped image rather than assumed.

**The receiver.** `otel.yaml` (`src/crates/otel-plugin/configs/otel.yaml.in`)
listens on `127.0.0.1:4317`, **gRPC only**, for metrics, logs and traces, storing
logs and traces under `{base_dir}` (`/var/log/netdata/otel/v2`). The stock image
runs `otel-plugin` and `otel-signal-viewer-plugin`; the viewer declares exactly
one function, `otel-logs`.

**Logs are host-agnostic.** A log stream's identity is
`(service.namespace, service.name)` - `otel-logs-identity/src/lib.rs:1-20`. There
is no host in it. A probe against the live agent confirmed what *does* survive:
every resource attribute is stored as a queryable field, including
`resource.attributes.host.name`, `infra_sim_name` and `infra_sim_role`.

So OTLP logs land on the ingesting node as one stream per service, filterable by
simulated host - they cannot make each simulated node its own log source. That is
what `systemd-journal-remote` already does, which is why both transports are
wanted rather than one replacing the other.

**Traces are not viewable.** `otel-ingestor/src/trace_service.rs:1-20` describes
itself as a "PROOF SCAFFOLD ... deliberately trivial ... to prove the
file-lifecycle substrate carries a second signal end-to-end, not to ingest traces
properly", with a single fixed partition key and ingestion-time timestamps rather
than span times. No traces function is registered anywhere, and the plugin's
offline inspector has a `logs` subcommand and no traces one. A probe sent two
spans: 36K appeared under `otel/v2/traces/`, readable by nothing.

**The application already exists.** `specs/prometheus-app.yaml` models an
application tier - request rate, error rate, p50/p95/p99 latency, orders, carts,
payment declines - and `exporters.rs` samples it through
`NodeEngine::signal_values()`. OTEL telemetry can be driven from the same spec and
the same call, so a span's duration *is* the latency the chart draws.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- Empty logs: the writer is opt-in and nothing opts in. Evidence above.
- No OTEL: the only emitter is `otlp/emit.py`, a standalone one-service demo that
  knows nothing about the fleet, the environment or the running scenario.

Evidence reviewed:

- `netdata/netdata @ c23face0bd94`:
  - `src/crates/otel-plugin/configs/otel.yaml.in` - gRPC endpoint, storage layout,
    retention.
  - `src/crates/otel-logs-identity/src/lib.rs:1-20` - stream identity is
    `(service.namespace, service.name)`.
  - `src/crates/otel-ingestor/src/trace_service.rs:1-20` - trace ingestion is a
    proof scaffold.
  - `src/crates/otel-ledger/src/ledger/rpc/handler.rs:624` - `otel-logs` is the
    only declared function.
- Live agent probe: OTLP logs and spans accepted; every resource attribute stored
  as a field; traces written and unreadable.
- Shipped image (`infra-sim:latest`, from `netdata/netdata:stable`, agent
  v2.10.4): `otel-plugin` and `otel-signal-viewer-plugin` present, python3
  present, **no pip and no OpenTelemetry SDK**.
- Local: `crates/sim-engine/src/logs.rs`, `crates/sim-plugin/src/logs_runtime.rs`,
  `crates/sim-plugin/src/exporters.rs`, `specs/prometheus-app.yaml`,
  `scripts/sim-docker.sh`, `crates/sim-console/src/provision.rs`.
- Prior SOWs: `SOW-0012` (containerised sims), the correlated-logs work, and the
  OTLP vnode probe that produced `otlp/emit.py`.

Affected contracts and surfaces:

- New: `crates/sim-engine/src/otel.rs`, `crates/sim-plugin/src/otlp_runtime.rs`,
  an `--otlp` mode on the plugin.
- Changed: `scripts/sim-docker.sh` (start logs and OTLP with the simulation, stop
  them on teardown), `crates/sim-console/src/provision.rs` and `main.rs` (same,
  plus status), `crates/sim-plugin/src/main.rs` (the new mode), `Cargo.toml`
  (OTLP client dependencies), README and `docs/operating.md`.
- Operator-visible: logs are on by default; a new process exists per simulation.

Existing patterns to reuse:

- `--exporters` is the precedent for a signal-driven side process serving one
  spec across the fleet; `--otlp` mirrors it.
- `NodeEngine::signal_values(scenarios, now)` is how `exporters.rs` reads the
  world; OTEL uses the same call, so the two surfaces cannot disagree.
- `logs_runtime.rs` is the precedent for a supervised child process that must not
  outlive its simulation.
- `control.yaml` is already the shared contract for which scenario is running.

Risk and blast radius:

- New heavy dependencies (`tonic`, `prost`, `opentelemetry-proto`) in a crate that
  had none. Verified they resolve and compile before committing to the approach.
  They are confined to the plugin; the engine stays transport-free.
- A dead or unreachable receiver must not kill the simulation: the OTLP process
  retries with backoff and its failure is reported, never fatal.
- Auto-starting two extra processes per simulation costs CPU and memory in the
  container. Both are per-simulation and die with it.
- `systemd-journal-remote` failing (missing binary, permissions) must degrade to a
  warning, not a failed create.
- No security surface: the receiver is bound to loopback inside the container.

Sensitive data handling plan:

- No credentials of any kind are involved. The OTLP endpoint is loopback and
  unauthenticated (`auth.enabled: false` by default); no tenant header is sent.
- Log bodies and span names are synthetic application vocabulary already used by
  `prometheus-app.yaml` (orders, carts, checkout), never customer wording.
- Evidence in this SOW cites file paths and field names, not captured payloads.

Implementation plan:

1. `crates/sim-engine/src/otel.rs`: from a node's signal values, produce
   application log records and spans - request completed, order placed, payment
   declined, upstream timeout; a root request span with database and cache
   children, durations sampled from p50/p95/p99 and error status from the error
   rate. Transport-free and unit-testable.
2. `crates/sim-plugin/src/otlp_runtime.rs`: a tonic client for `LogsService` and
   `TraceService`, batching per tick, reconnect with backoff.
3. `--otlp` mode in `main.rs`, matching `--exporters` in shape.
4. `scripts/sim-docker.sh`: start the logs writer and the OTLP emitter as part of
   `create`; stop both on teardown; report both in `status`.
5. Console: same, plus surfacing them.
6. Docs, spec, tests, live validation.

Validation plan:

- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- Unit tests for the generator: a healthy tier produces no error spans; a faulted
  one does; durations track the latency signals.
- Live: create a containerised simulation, then with no second command confirm
  (a) per-node journal files exist and the `systemd-journal` function sees them,
  (b) the container's `otel-plugin logs` inspector shows application records
  carrying the simulated host, (c) the traces store grows, (d) triggering a
  scenario changes what OTEL reports.
- Teardown leaves no process behind.

Artifact impact plan:

- AGENTS.md: likely a line about logs no longer being a manual step.
- Runtime project skills: `project-live-validation` may gain the OTLP inspection
  commands, since they are not obvious.
- Specs: `.agents/sow/specs/runtime-and-scenarios.md` for the new process.
- End-user/operator docs: README (what can be simulated) and `docs/operating.md`.
- SOW lifecycle: new SOW; no split or merge.

Open-source reference evidence:

- `netdata/netdata @ c23face0bd94`, paths listed above.

Open decisions:

- All four resolved by the user on 2026-08-10; recorded below.

## Implications And Decisions

1. **Traces are emitted although nothing can display them** (user decision).
   Documented as write-only in the console and the docs so no SE builds a demo
   beat on them. The pipeline is then already correct when a viewer ships.

2. **Both log transports** (user decision). `systemd-journal-remote` keeps giving
   every node its own log source; OTLP adds an application-tier stream on the
   simulation's agent. This is also what a real estate looks like.

3. **Logs are always on** (user decision). Starting with the simulation, not by
   hand. This is the direct cause of the empty logs and the reason to change it.

4. **The OTLP client is Rust, in the plugin** (user decision, "Only simulator
   level changes, no changes to netdata or its source"). The Python route needed
   `apt` and `pip` inside `netdata/netdata:stable`; the Rust route needs nothing
   outside this repository, because the plugin binary is already mounted into the
   container. It also reverses the note in `otlp/emit.py` that kept gRPC out of
   the Rust runtime - recorded here so the reversal is deliberate and traceable.

## Plan

1. Generator in the engine. (transport-free, tested)
2. OTLP client and `--otlp` mode in the plugin.
3. Lifecycle: start with create, stop with teardown, show in status.
4. Docs, specs, live validation.

## Execution Log

### 2026-08-10

- Gate filled after probing the receiver, the shipped image and the live agent.
- `crates/sim-engine/src/otel.rs`: the generator. Application log records and
  traces from a node's signal values - routes with realistic shares, a root span
  plus cache/pool/query children, business events, saturation warnings. Six unit
  tests, including that a healthy tier produces no error spans and that ten times
  the latency shows up in span durations.
- `crates/sim-plugin/src/otlp_runtime.rs`: the tonic client, OTLP message
  construction, per-signal health.
- `crates/sim-plugin/src/main.rs`: `--otlp`, `--otlp-endpoint`, `do_otlp` mirroring
  `do_exporters`, restricted to `web`/`lb`/`k8s-worker`.
- `scripts/sim-docker.sh`: `cmd_telemetry` starts both writers as part of
  `create`, stops both, and reports both; `cmd_logs` kept as an alias; a `warn`
  helper so telemetry failing cannot fail a create.
- Console: `container::logs` now drives `telemetry`, and create says what started.
- `Cargo.toml`: `tonic`, `prost`, `opentelemetry-proto`, plus `tokio` for the
  plugin. Verified they resolve and compile before committing to the approach.
- Docs, spec, `AGENTS.md`, and the live-validation skill.

Faults found and fixed:

- **Latency read as milliseconds.** `specs/prometheus-app.yaml` declares latency in
  **seconds** (`units: seconds`, p50 base `0.042`). Every span was sub-millisecond
  and log lines read `200 in 0ms`. Found by reading what actually landed, not by a
  test - the tests were relative and passed either way.
- **Every order had the same total to the cent.** `app_cart_value_total` is the
  tier's gauge; emitting it verbatim gave identical values within a tick. Orders
  now draw around it.
- **Trace volume did not move with load.** A fixed cap of 4/s meant a scenario
  could double traffic while trace volume sat still - the exact incoherence this
  work exists to avoid. Sampling is now a ratio of request rate, with a ceiling.
- **One health flag hid a working signal.** Logs and traces were reported together,
  so stable's permanent trace refusal printed "export failed" forever while logs
  were landing perfectly. Each signal now has its own state, and a signal is
  abandoned after 30 consecutive failures with an explanation.
- **Status showed the start-up race.** `tail -3 | head -1` reported the first line,
  which is always the failure against an agent still opening its port.

## Validation

Acceptance criteria evidence:

1. `sudo ./scripts/sim-docker.sh create teltest environments/web-stack.yaml` with
   no second command reported `logs : 5 journal file(s) in the container`.
2. Inside the container: `remote-sim-web-01.journal`, `remote-sim-web-02.journal`,
   `remote-sim-db-01.journal`, `remote-sim-lb-01.journal`,
   `remote-sim-cache-01.journal` - one per node.
3. OTLP records read back from the container's store carry
   `ND3AE_RA_HOST_NAME: sim-web-02`, `RA_INFRA_SIM_NAME: web-stack`,
   `RA_INFRA_SIM_ROLE: web`, `RA_SIMULATED: true`,
   `RA_SERVICE_NAME: web-stack-storefront`, with bodies such as
   `order placed, 524.55 total` and `payment declined by processor` at INFO/WARN.
4. Against the newer nightly the traces store grew 199,910 -> 246,526 bytes with
   no failures reported. Against stable, traces are refused and the emitter says
   so and stops - both behaviours are correct for their build.
5. Span durations come from `app_latency_p50/p95/p99` and error status from
   `app_requests_error_rate`, read through one `signal_values()` call.
   `span_durations_follow_the_latency_signals` asserts a tenfold latency change
   moves them.
6. `teardown` removes the container, and both processes are inside it. Verified:
   no `infra-sim.simulation` containers remain.
7. `git status` shows changes only inside this repository. The container image
   gained nothing: the same `netdata/netdata:stable` base, `systemd-journal-remote`
   as before, no `apt`/`pip` layers, and the plugin binary that was already
   mounted.

Tests or equivalent validation:

- `cargo test`: 213 passed, 0 failed (9 added: 6 generator, 3 transport).
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --check`: clean.

Real-use evidence:

- Two containerised simulations created and torn down; one host-side `--otlp` run
  against the nightly agent to prove the trace path where it is supported.
- Read-back through three different paths: `journalctl` inside the container, the
  plugin's offline inspector on the host, and `telemetry status`.

Reviewer findings:

- No external reviewer. The live agent again caught what review would not: the
  seconds/milliseconds error and the masked signal health were both invisible in
  the source and obvious in the output.

Same-failure scan:

- Other places a unit could be misread: `exporters.rs` computes `mean_latency`
  from the same three signals and publishes them into a summary whose `units:
  seconds` is declared in the spec - consistent, no change needed.
- Other opt-in steps that should be automatic: `--exporters` is still opt-in, and
  deliberately so (the user's create form has a checkbox for it and it is not yet
  wired on the container path - tracked in `SOW-0013`). Nothing else in
  `sim-docker.sh` requires a second command to be useful.
- Other shared-flag reporting: the exporters' failure path reports per request, not
  per signal, so it cannot mask anything.

Sensitive data gate:

- No credentials of any kind. The OTLP endpoint is loopback and unauthenticated;
  no tenant header is sent. Log bodies and span names are the synthetic storefront
  vocabulary already in `specs/prometheus-app.yaml`. Evidence in this SOW cites
  field names and paths, and the probe data it quotes is simulated.

Artifact maintenance gate:

- AGENTS.md: updated - telemetry starts itself, and OTEL support differs by agent
  build.
- Runtime project skills: `project-live-validation` gained an OpenTelemetry
  probing section, including the 412 from the SSO-gated function and the two traps.
- Specs: `.agents/sow/specs/runtime-and-scenarios.md` gained an OpenTelemetry
  section and a note that telemetry starts with create.
- End-user/operator docs: `README.md` (logs two ways, traces with the caveat) and
  `docs/operating.md` (a combined logs/OTEL section, the `telemetry` command, and
  how to read the store without Cloud SSO).
- End-user/operator skills: none exist.
- SOW lifecycle: `Status: completed`, moved to `.agents/sow/done/`, committed with
  the work in one commit.

Specs update:

- As above.

Project skills update:

- As above.

End-user/operator docs update:

- As above.

End-user/operator skills update:

- None exist.

Lessons:

- **Read the units before reading the numbers.** A spec that says `units: seconds`
  and a generator that assumes milliseconds produce output that is wrong by a
  factor of a thousand and still passes every relative test. The tell was in the
  rendered text, not the code.
- **Health is per signal.** Two things exported over one channel fail
  independently. A single flag turned a permanent, expected refusal into a report
  that the whole exporter was broken.
- **An opt-in step is an off step.** Correlated logs worked for the project's whole
  history and shipped empty every time, because using them needed a second
  command. If a feature is part of what the product is, it starts with it.
- **Agent builds are not interchangeable.** Stable and nightly differ in whether
  OTLP traces are accepted at all, and in where OTLP logs are stored. Probing the
  host and concluding for the container would have been wrong in both directions.

Follow-up mapping:

1. Prometheus exporters on the container path - **tracked** in `SOW-0013`, not
   widened into this SOW.
2. A trace viewer - **rejected as out of scope**: it belongs to the agent, and
   building one here would break the project's one hard rule.
3. Nothing else deferred. The `defer|later|follow-up|future|TODO|pending` scan over
   this SOW returns only these two items.

## Outcome

Delivered:

- Correlated logs and OpenTelemetry both start with a simulation. The empty-logs
  failure mode is gone, and it was the direct cause of the user's report.
- The application tier ships OTLP logs carrying the simulated host, role and
  simulation name as resource attributes, queryable in the `otel-logs` function.
- The application tier emits OTLP traces: accepted and stored on builds that
  support them, correctly identified and abandoned on builds that do not.
- All of it derived from `specs/prometheus-app.yaml` through the same
  `signal_values()` call the Prometheus exporters use, so metrics, logs and spans
  are one description of one service that cannot drift apart.
- Nothing outside this repository changed, and the container image is still the
  stock netdata image plus the binary it already mounted.

Not delivered: nothing in scope. Two follow-ups tracked above.

## Lessons Extracted

Recorded under Validation -> Lessons.

## Followup

1. **Prometheus exporters on the container path.** Tracked in `SOW-0013`. The
   create form offers the checkbox and the container path does not honour it yet.
2. **A trace viewer** is the agent's to build, not this project's.

## Regression Log

None yet.

Append regression entries here only after this SOW was completed or closed and
later testing or use found broken behavior. Use a dated
`## Regression - YYYY-MM-DD` heading at the end of the file. Never prepend
regression content above the original SOW narrative.
