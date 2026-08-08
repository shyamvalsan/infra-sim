# SOW-0002 - OTEL sim: scope definition

## Status

Status: completed

Sub-state: All three plan steps delivered - OTLP vnode probe run and recorded, `specs/otel-collector.yaml` authored, `environments/otel-fleet.yaml` added. Option A (OTLP emitter backend) is now scopable and needs one user decision, recorded under Followup.

## Requirements

### Purpose

Define what "OTEL sim" means for Infra-Sim, then build it. The user added this scope with "also don't forget OTEL sim". It is not present in `spec.md`, so there is no written definition to work from.

### User Request

"also don't forget OTEL sim" (2026-08-07), appended to the decisions that opened `SOW-0001`.

### Assistant Understanding

Facts:

- Netdata ingests OTLP over gRPC: metrics from v2.7.0, logs from v2.9.0. Traces are not supported (`spec.md` lists traces as a non-goal, "future phase").
- Netdata's OTEL support is Rust: `src/crates/otel-plugin` plus `otel-ingestor`, `otel-catalog`, `otel-streams`, `otel-ledger`, `otel-logs-identity`, `flatten-otel`, `otel-legacy-logs`.
- The installed agent carries `/usr/libexec/netdata/plugins.d/otel-plugin` and a config root at `/etc/netdata/otel.d/v1/`; `otel = yes` in the live `[plugins]` section.
- OTLP resource attributes are flattened into attribute key/value pairs (`flatten-otel/src/lib.rs:62-63`, `otel-streams/src/otel.rs:121-148`).

Inference (source-level evidence, not empirically tested):

- **The OTLP ingestion path cannot create virtual nodes.** A repository-wide search for `HOST_DEFINE` across all Rust crates matches only `netdata-plugin/protocol/build.rs:31-34` (the tokenizer definition). No OTEL crate references `HOST_DEFINE`, `vnode`, or `virtual_host`. This implies OTLP-ingested telemetry lands on the local host, with resource attributes becoming labels rather than separate Netdata nodes.
- If that holds, an OTLP-based simulation produces a **single-node** view — every simulated service collapsing onto one host — which is incompatible with the multi-node fleet that makes Infra-Sim demos persuasive.
- This needs empirical confirmation before Option A below is costed. It is the same class of assumption as `spec.md`'s original vnode question, which only source-plus-experiment could settle.

Unknowns:

- Which of the four options the user means.
- Whether the vnode limitation above is real or has a config-level workaround in `/etc/netdata/otel.d/v1/`.

### Acceptance Criteria

1. The OTLP vnode question is answered empirically, with the result recorded here. MET.
2. A collector's own internal telemetry is modelled as a normal generator spec on the plugins.d path. MET.
3. An environment template places collectors in a realistic topology. MET.
4. Both pass the fidelity lint. MET.

## Analysis

Open-source reference evidence:

```text
netdata/netdata @ c23face0bd94
src/crates/otel-plugin/Cargo.toml            Netdata's OTEL plugin is a Rust binary
src/crates/flatten-otel/src/lib.rs:62-63     resource attributes flattened to key/value
src/crates/otel-streams/src/otel.rs:121-148  resource_attrs extraction for logs
src/crates/netdata-plugin/protocol/build.rs:31-34   only site referencing HOST_DEFINE
```

Risks:

- Choosing Option A without first confirming the vnode limitation risks building a demo mode that cannot show a fleet.

## Pre-Implementation Gate

Status: needs-user-decision

Open decisions:

**Decision 1 — What does "OTEL sim" mean?**

- **Option A — OTLP emitter backend.** Infra-Sim gains a second output transport: the same generator specs emit OTLP/gRPC into Netdata's OTLP receiver instead of plugins.d. Demo story: "here is your OpenTelemetry pipeline, ingested by Netdata."
  - Pro: directly answers prospects who say "we already run OpenTelemetry"; reuses the entire generator library unchanged; exercises a real, current Netdata ingestion path.
  - Con: per the evidence above, likely **single-node only** — no vnode fleet. Also a second transport to maintain, and metrics/logs only (no traces).

- **Option B — Simulate an OTel Collector fleet as a monitored service.** Generate an OTel Collector's *own* internal telemetry (receiver accepted/refused, exporter queue size and retries, processor dropped spans, pipeline throughput) as just another hero-collector generator spec.
  - Pro: fits the existing architecture exactly — it is a normal generator on the plugins.d path, so it is vnode-capable and needs no new transport. Demo story: "Netdata monitoring your OTel Collector fleet," which is a real operational pain point.
  - Con: does not demonstrate Netdata's OTLP ingestion itself.

- **Option C — Both.** B for fleet realism now; A as an additional demo mode once the vnode question is settled.
  - Pro: covers both stories. Con: roughly doubles the scope of this SOW.

- **Option D — Something else.** The user has a specific meaning not captured above.

**Recommendation: B first, then A as a separate SOW** — classified **long-term-best**.

Reasoning: B is additive content on an architecture already proven end-to-end, so it costs one generator spec and carries no new risk. A is a second data path whose central value — showing a realistic fleet — is exactly what the source evidence suggests it cannot deliver. Confirming the vnode limitation empirically is cheap and should gate any investment in A. If the limitation is real, A's honest framing is "single-node OTLP ingestion demo," which is a much smaller claim than it first appears and should be scoped deliberately rather than assumed.

**Decision 2 — Should the OTLP vnode limitation be verified before scoping A?**

- **Option A — Yes, run a probe first** (send OTLP with distinct `host.name` resource attributes to the local agent, observe whether separate nodes appear). Cost: roughly the same as the vnode probe already run.
- **Option B — No, accept the source-level inference and scope accordingly.**

**Recommendation: A** — the last time an assumption of this exact shape was tested, the answer reshaped P0. Source-reading was necessary but not sufficient then either.

## Decisions

Resolved by the user on 2026-08-07:

1. **Option B** — simulate an OTel Collector fleet as a monitored service first. Option A (OTLP emitter backend) becomes a separate SOW, scoped only after Decision 2's probe result is known.
2. **Option A** — verify the OTLP vnode limitation empirically before scoping Option A work.

## Plan

1. **OTLP vnode probe.** Send OTLP metrics carrying distinct `host.name` resource attributes to the local agent's OTLP receiver and observe whether separate Netdata nodes appear or everything lands on the local host. Record the result here; it determines whether an OTLP emitter backend can ever show a fleet.
2. **OTel Collector generator spec.** Author `specs/otel-collector.yaml` covering a collector's own internal telemetry — receiver accepted/refused, exporter queue size, enqueue failures and retries, processor dropped/batch behaviour, and pipeline throughput. Same plugins.d path as every other spec, so it is vnode-capable and needs no new transport.
3. **Environment template** placing collectors across the simulated fleet.

Blocked on nothing; runs after `SOW-0001` per one-SOW-at-a-time.

## Execution Log

### 2026-08-07

- Investigated Netdata's OTEL implementation; found no vnode support in any OTEL crate.
- Prepared options; opened this SOW as pending.

### 2026-08-08

**Plan step 1 complete — OTLP vnode probe run against the live agent. The limitation is real, and now precisely characterised.**

Method: the agent's OTLP gRPC receiver is listening on `127.0.0.1:4317` (`otel-plugin` running with `ledger` and `ingestor` workers). Sent metrics from two OTLP resources carrying distinct `host.name` / `host.id` resource attributes (`otlp-probe-alpha`, `otlp-probe-beta`) via an isolated Python venv using the official OTLP gRPC exporter, for ~24s.

Result:

- **Node count unchanged: 16 before, 16 after. Neither `host.name` produced a Netdata node.**
- The data *did* arrive: contexts `otel.infra_sim_probe_value` and `otel.infra_sim_probe_total` exist.
- Both resources landed on **one** node — the local host, `laptop`.
- Resource attributes became chart labels, prefixed `resource.attributes.`: the label `resource.attributes.host.name` carries both values, `otlp-probe-alpha` and `otlp-probe-beta`.
- Each distinct attribute set produced its **own chart instance** (two instances of `infra_sim_probe_value`), so series are not merged.

This confirms the source-level inference (no OTEL crate references `HOST_DEFINE`) and sharpens it. The honest framing of Option A is now available:

- An OTLP emitter backend **cannot show a fleet of Netdata nodes.** No per-node dashboards, no node-level ML anomaly rate, no node-scoped alerts, no re-skin story — every simulated service appears under the ingesting host.
- It is *not* a single collapsed series either: distinct resource attributes yield distinct instances, and `resource.attributes.host.name` is a groupable label. An OTLP demo could show "N services distinguished by label", which is a real but much smaller claim than "here is your fleet".
- Everything that makes Infra-Sim demos persuasive — a node list that looks like the prospect's estate, per-node anomaly ranking, per-node alerts, GUID-preserving re-skin — is unavailable on this path.

Consequence for scoping: Option A remains viable only as an explicitly-framed "Netdata ingesting your OpenTelemetry pipeline" demo on a single host, and should be costed as that. It is not a substitute for the plugins.d path. Decision 7B (collector-as-monitored-service first) is unchanged and now better supported.

Probe artifacts are throwaway and were not committed; the ingested `otel.infra_sim_probe_*` contexts age out of the agent's retention on their own.

## Validation

Acceptance criteria evidence:

- **OTLP vnode probe** - run against live agent `v2.10.0-1022-nightly`, result recorded in the Execution Log above. Node count 16 before and after; both `host.name` values present as the label `resource.attributes.host.name` on the single local host, each attribute set producing its own chart instance. MET.
- **`specs/otel-collector.yaml`** - 12 contexts across receivers, processors, exporters and process families, covering accepted/refused points and logs, batch size and send triggers, dropped and memory-limiter-refused points, sent/failed points, queue size against capacity, enqueue failures, send retries, and process RSS/CPU/uptime. MET.
- **`environments/otel-fleet.yaml`** - 5 nodes in the topology people actually run: two gateway collectors doing batching and export, three app nodes running agent collectors beside nginx, spread across three regions and time zones. MET.

Tests or equivalent validation:

- `infra-sim --lint 72` on `otel-fleet`: **0 semantic violations, 0 pinned signals** across all 5 nodes over 259,200 samples per node. All applicable scenario targets resolve.
- Emitted plugins.d protocol inspected directly: 12 distinct `otelcol.*` contexts declared with correct families and chart types, 5 `HOST_DEFINE` blocks, and `otelcol.exporter_queue` emitting `size` against `capacity` (347 / 10000) so the chart reads as headroom rather than an unanchored number.

Real-use evidence:

- The probe itself is live-agent evidence: the agent's OTLP gRPC receiver on `127.0.0.1:4317` accepted the metrics and created `otel.infra_sim_probe_value` / `otel.infra_sim_probe_total` contexts, which is how the negative result on vnodes was established rather than inferred.
- **Not installed on the live agent.** This SOW adds a generator spec and an environment; the runtime is unchanged and already live-validated. Installing `otel-fleet` would create five further permanent vnodes on the user's agent, which was not done without asking. Deferred to the user.

Same-failure search:

- Zero-baseline signals: `otelcol_receiver_refused_*`, `otelcol_processor_dropped_points_rate`, `otelcol_processor_memlimiter_refused_rate`, `otelcol_exporter_failed_points_rate` and `otelcol_exporter_enqueue_failed_rate` all sit at 0 and are reachable only by additive effects - the same class that `db-replication-lag` already documents for `net_drop_rate`.
- Declared constants: `otelcol_exporter_queue_capacity` and `otelcol_process_uptime_tick` carry both `min_is_floor` and `max_is_ceiling`, which is what exempts them from the flat-signal check; verified by the clean lint.

Sensitive data gate: no credentials or customer-identifying values. All hostnames are `sim-*`; probe hostnames were `otlp-probe-*`. Probe artifacts (venv, script) live in the session scratchpad and were not committed.

Artifact maintenance gate:

- `AGENTS.md` - no change needed; no new workflow or guardrail.
- Runtime project skills - no change needed; `project-live-validation` already covers the probe-first rule this SOW exercised.
- Specs - `.agents/sow/specs/runtime-and-scenarios.md` covers the plugins.d path this rides on; the OTLP finding is recorded here rather than duplicated, since it describes what Netdata does, not what Infra-Sim does.
- End-user/operator docs - `README.md` updated with the collector fleet and the OTLP limitation.
- SOW lifecycle - completed and moved to `done/`.

## Outcome

Delivered, and the central question is answered with evidence rather than inference.

**Netdata's OTLP ingestion cannot create virtual nodes.** Resources carrying distinct `host.name` attributes all land on the ingesting host, with the attributes flattened into `resource.attributes.*` chart labels and one chart instance per attribute set. That is better than a single collapsed series - the instances stay distinct and `host.name` is groupable - but it means an OTLP-based simulation has no node list, no per-node dashboards, no node-level ML anomaly ranking, no node-scoped alerts, and no re-skin story.

So the collector is modelled as a *monitored service* on the plugins.d path, which keeps all of that. `specs/otel-collector.yaml` plus `environments/otel-fleet.yaml` answer "we already run OpenTelemetry" with "then Netdata monitors your collector fleet", which is a real operational pain point: a collector that silently drops telemetry is the worst failure in an observability pipeline, because the thing that would have told you is the thing that broke.

## Lessons Extracted

- **The probe-first rule paid out a third time.** Source-reading had already concluded no OTEL crate references `HOST_DEFINE`, and the probe confirmed it - but the probe also produced the detail that source-reading had not: distinct instances and a groupable `host.name` label. That detail is the difference between "OTLP is useless for demos" and "OTLP demos a single host with labelled services", which is what an honest Option A scoping needs.
- **A negative result is a deliverable.** Knowing what the OTLP path cannot do prevents building a demo mode that cannot show a fleet, which was the risk this SOW's analysis flagged at the start.

## Followup

Needs a user decision:

- **Option A - OTLP emitter backend.** The user's original decision was that this becomes a separate SOW "scoped only after Decision 2's probe result is known". It is now known. The honest scope is: a second output transport that emits the same generator specs as OTLP into Netdata's receiver, producing a **single-node** view with services distinguished by `resource.attributes.host.name`. It demonstrates Netdata's OTLP ingestion; it cannot demonstrate a fleet. No SOW opened - this needs the user to say whether that smaller demo is worth a second transport to maintain.

Not needed:

- Traces remain out of scope: Netdata does not ingest them and `spec.md` lists them as a future phase.

## Regression Log

None yet.
