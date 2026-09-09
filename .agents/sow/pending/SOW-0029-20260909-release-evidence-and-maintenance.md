# SOW-0029 - Release evidence and maintenance

## Status

Status: open

Sub-state: user-authorized programme; not executing until preceding SOW closes.

## Requirements

### Purpose

CI and isolated/live release tests, documentation and SOW reconciliation, and adjudication of historical deferred improvements. Platform verification remains in SOW-0018.

### User Request

Implement all recommendations from the repository review (2026-09-09).

### Acceptance Criteria

Deliver the scope with automated checks and honest evidence boundaries. Runtime changes require live Netdata validation, capped at five local vnodes. External blind SRE and actual Mac acceptance cannot be self-certified; large fleets require a separate host.

## Analysis

The review found baseline-only fidelity, incomplete archives, misleading readiness wording and incomplete release evidence. Sources: spec.md:106,119; fidelity.rs:101,264; preflight.rs:341; sim-docker.sh:424; SOW-0027 historical deferred list.

## Pre-Implementation Gate

Status: blocked

The implementation gate is intentionally not ready: this is a pending scope reservation. Detailed causal analysis, design choices, affected contracts, risks, ordered implementation and validation must be filled before this SOW moves to current. No code is implemented under this gate.

Sensitive data handling plan: synthetic fixtures only, no customer data, actual tokens or private endpoints in artifacts. Recorded reference data must come from disposable synthetic workloads.

Artifact impact plan: update current-reality specs, operator docs and relevant runtime skills; preserve user-authored spec.md. Reconcile validation and follow-ups without inventing historical evidence.

## Implications And Decisions

User approved the full long-term-best review recommendation. Each change remains minimal-complete; unresolved product choices require a concrete proposal before code.

## Plan

1. Finish preceding SOW.
2. Investigate this scope and fill the implementation gate.
3. Implement, validate and review; identify external acceptance dependencies explicitly.

## Validation

Not executed: pending work must not inherit passing results from another SOW.

## Outcome

Not delivered.

## Lessons Extracted

Do not equate baseline unit tests with release or fidelity acceptance.

## Followup

This file tracks the review recommendations above; SOW-0029 additionally owns every historic Tier 3 item from SOW-0027 except fidelity/indexing/lint-readiness/replay, which belong to SOW-0028.
