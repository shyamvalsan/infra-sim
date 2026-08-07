# SOW-0002 - OTEL sim: scope definition

## Status

Status: open

Sub-state: Blocked on user decision. Investigation complete; four options prepared with evidence. Not a blocker for `SOW-0001`, which is entirely plugins.d-path work.

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

To be written once scope is decided.

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

## Validation

Pending.

## Outcome

Pending.

## Lessons Extracted

Pending.

## Followup

None yet.

## Regression Log

None yet.
