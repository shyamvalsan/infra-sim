# SOW-0032 - Reference fidelity and external acceptance

## Status

Status: open

Sub-state: split from SOW-0028 by user decision 4.B (2026-09-23); not started. External resources requested earlier have not been supplied.

## Requirements

### Purpose

Show, with evidence that cannot come from the simulator itself, that simulated output is statistically comparable to a real service and survives expert review.

### User Request

Decision 4.B on SOW-0028: move real-service reference capture, statistical tooling and all external acceptance into a new SOW so SOW-0028 closes with explicit boundaries.

### Assistant Understanding

Facts:

- No reference recorder, reference corpus or statistics tool exists.
- Approved direction (SOW-0028): a disposable synthetic service workload; sanitized provenance (service, version, image, workload, units, cadence); distribution/quantile, autocorrelation, seasonality and cross-signal comparisons; counter, cardinality and restart-seam checks. No invented universal PASS thresholds. An observation window too short for daily seasonality is reported as insufficient, never as proof.
- A small Redis reference was investigated, not selected. `specs/redis.yaml` supplies authored signals and chart mappings. Collector source checked: `netdata/netdata @ 10a879ac7f78`, `src/go/plugin/go.d/collector/redis/charts.go` and `src/go/plugin/go.d/collector/redis/collect_info.go`. Native fields include `total_commands_processed`, `keyspace_hits`, `keyspace_misses`, `connected_clients`, `used_memory`, `used_memory_rss`; counter-versus-rate and chart scaling semantics must be preserved.
- On the nightly agent tested in SOW-0028, the `systemd-journal` and `otel-logs` functions return HTTP 412 without Cloud SSO, including on container loopback.

Inferences:

- Simulator output must never be labeled reference data.

Unknowns:

- Which service is the hero reference (Redis proposed).
- Who the two blind SRE reviewers are, and when a separate scale host and authenticated Cloud access become available.

### Acceptance Criteria

- A reference capture from a disposable real service with sanitized provenance, and a descriptive comparison report against simulated output for the same charts.
- Authenticated log-function query evidence for simulated journal and OTLP logs.
- Two independent blind SRE reviews recorded with their findings.
- Scale validation above five vnodes on a separate host.

## Analysis

Sources checked:

- SOW-0028 requirements, decisions and closeout findings; `TODO.md` handoff (removed at SOW-0028 close, content mapped here).

Current state:

- None of the acceptance criteria are met. They are external or new tooling.

Risks:

- Treating elapsed time, unit tests or automated reviewers as a substitute for the external evidence.

## Pre-Implementation Gate

Status: needs-user-decision

Problem / root-cause model:

- Internal checks prove physical validity and raw replay, not realism; realism needs outside evidence.

Evidence reviewed:

- As above.

Affected contracts and surfaces:

- New reference-capture and statistics tooling (location to be decided), docs, live-validation skill.

Existing patterns to reuse:

- Recording format and replay in `crates/sim-engine/src/recording.rs`; disposable-container probes in `tests/live_*.py`.

Risk and blast radius:

- New tooling only; no runtime change expected.

Sensitive data handling plan:

- Reference data comes from disposable synthetic services only; no customer systems, credentials or private endpoints are recorded.

Implementation plan:

1. User selects the hero reference service and tooling form.
2. Build capture and descriptive statistics; record a reference corpus.
3. Obtain external reviews and access when supplied.

Validation plan:

- Reference capture from a real disposable container, statistics report, external review records.

Artifact impact plan:

- AGENTS.md: likely unaffected.
- Runtime project skills: live-validation skill gains the reference workflow.
- Specs: new current-reality spec section for reference tooling.
- End-user/operator docs: operating guide section for reference comparison.
- End-user/operator skills: none known.
- SOW lifecycle: external items may be split again if resources arrive separately.

Open-source reference evidence:

- `netdata/netdata @ 10a879ac7f78`, paths listed under Facts.

Open decisions:

1. Hero reference service (Redis proposed) and tooling form.
2. External resource ownership: reviewers, scale host, Cloud access.

## Implications And Decisions

None yet.

## Plan

1. Decisions, then tooling, then external evidence.

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
