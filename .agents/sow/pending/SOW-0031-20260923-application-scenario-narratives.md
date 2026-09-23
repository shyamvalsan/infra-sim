# SOW-0031 - Application scenario narratives

## Status

Status: open

Sub-state: split from SOW-0028 by user decision 3.B (2026-09-23); not started.

## Requirements

### Purpose

Shipped application-tier scenarios must show the incident their manifest describes, so an SRE reading the published charts sees the story the scenario claims.

### User Request

Decision 3.B on SOW-0028: SOW-0028 fixes the p99 flattening it caused and adds direction, recovery and isolation lint assertions; the pre-existing narrative mismatches move to a new SOW.

### Assistant Understanding

Facts:

- An engine-level probe during SOW-0028 (2 web, 1 lb, 1 k8s-worker nodes on `specs/prometheus-app.yaml`, compared against an identical run with no scenario) found the mismatches listed under Analysis. Numbers are from that probe, not from live agent evidence.
- SOW-0028's application lint now asserts direction, untargeted isolation and exact recovery. It does not assert magnitude or saturation, so these mismatches pass it.

Inferences:

- Each fix changes authored scenario or spec values, which is a narrative/design choice for the user.

Unknowns:

- The intended magnitude for each scenario (for example, whether worker-saturation should pin busy workers at capacity or only approach it).

### Acceptance Criteria

- Each listed scenario shows its manifest's claimed effect in the rendered exporter/OTLP output, verified by an automated check tied to the shipped files.
- Live-agent evidence on at most five local vnodes for the changed scenarios.

## Analysis

Sources checked:

- `scenarios/worker-saturation.yaml`, `scenarios/queue-backlog.yaml`, `scenarios/db-connection-exhaustion.yaml`, `scenarios/cache-stampede.yaml`, `scenarios/memory-leak-oom.yaml`
- `specs/prometheus-app.yaml`, `crates/sim-engine/src/application.rs`, `crates/sim-engine/src/otel.rs` (saturation log rule)

Current state:

- worker-saturation claims idle workers at zero; busy workers peak at about 23 of 32, so the OTLP "worker pool saturated" log never fires.
- queue-backlog claims workers stay saturated; busy peaks at about 26 of 32.
- db-connection-exhaustion's web step shows pool waits at about 4/s while about 13 of 40 app pool connections are in use.
- cache-stampede: hits plus misses fall from about 1530/s to about 1140/s while the request rate is unchanged.
- memory-leak-oom never touches application series, and `role: web` matches every web node, so the manifest's "surviving app server" has no survivor (inferred, not probed).

Risks:

- Raising multipliers can push other signals into caps and flatten them; the capacity normalization in `application.rs` must stay the physical bound.

## Pre-Implementation Gate

Status: needs-user-decision

Problem / root-cause model:

- Authored multipliers are too small for the claimed saturation, and some scenarios move one side of a relationship without the other (pool waits without pool occupancy; lookups without requests).

Evidence reviewed:

- SOW-0028 section "Replay acceptance and closeout findings - 2026-09-23" and its decision record.

Affected contracts and surfaces:

- Scenario YAML files, possibly `specs/prometheus-app.yaml`, application lint tests, scenario descriptions shown in the console.

Existing patterns to reuse:

- `exporters::check_narrative` and the shipped-file regression test `shipped_latency_incidents_keep_the_tail_above_p95` in `crates/sim-plugin/src/main.rs`.

Risk and blast radius:

- Scenario changes alter every simulation that triggers them, including public demo spaces.

Sensitive data handling plan:

- Synthetic scenarios only; no customer data involved.

Implementation plan:

1. Present per-scenario options (magnitude and coupled signals) to the user.
2. Implement the chosen values with shipped-file regression tests.
3. Validate live on at most five vnodes.

Validation plan:

- Engine-level shipped-file tests, application lint on the shipped templates, live chart queries for the changed scenarios.

Artifact impact plan:

- AGENTS.md: likely unaffected.
- Runtime project skills: live-validation skill may gain a note on narrative checks.
- Specs: `runtime-and-scenarios.md` if scenario behavior contracts change.
- End-user/operator docs: scenario descriptions if narratives change.
- End-user/operator skills: none known.
- SOW lifecycle: standard.

Open-source reference evidence:

- Not relevant: authored synthetic scenarios.

Open decisions:

1. Target magnitude per scenario (to be presented with options).

## Implications And Decisions

None yet.

## Plan

1. Decisions, then implementation and live validation.

## Execution Log

None yet.

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
