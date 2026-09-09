# SOW-0029 - Release evidence and maintenance

## Status

Status: completed

Sub-state: local release checks and evidence reconciliation delivered; hosted execution awaits publication. Remaining implementation in SOW-0028, SOW-0030 and platform SOW-0018.

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

Status: ready

Problem / root-cause model: no tracked CI exercises the Rust, shell and UI contracts; platform documentation contradicts the current implementation/evidence, Cargo declares 1.85 while locked tonic requires 1.88, and the SOW audit reports inconsistent status/section placement without a failing exit status.

Evidence reviewed: Cargo.toml, Cargo.lock, cargo metadata (tonic/tonic-prost rust_version 1.88), docker/builder.Dockerfile, startsim-vm.sh, startsim.sh, docs/QUICKSTART.md, docs/operating.md, README.md, SOW-0018/0003/0008/0027, local audit script. Official actions/checkout and actions/setup-node README usage checked online; refs verified by git ls-remote.

Affected contracts and surfaces: GitHub Actions, developer verification commands, compiler minimum, operator platform expectations, local audit and historical SOW navigation. No generator values or scenario behavior change in this SOW.

Existing patterns to reuse: cargo test/clippy/fmt, tests/test_shell_lifecycle.py, tests/test_console_api.py, tests/console_ui.test.cjs, existing Docker image build and lifecycle script. Live validation uses a disposable one-node container and no Cloud credentials.

Risk and blast radius: CI must not execute privileged fork code on a shared host, leak credentials, or mutate running simulations. Use hosted runners, read-only repository permission, pinned actions and unique disposable resources. Documentation must distinguish missing evidence from a known failure and avoid promising Mac acceptance.

Sensitive data handling plan:

synthetic fixture names/UUIDs, no claims, environment dumps or customer data; action checkout does not persist credentials. Archive only test artifacts. Durable historical records retain their evidence while incidental personal references are sanitized when edited.

Implementation plan:
1. Add hosted Rust/shell/UI/API gates and separately invokable one-node live smoke workflow with bounded timeouts and exact-resource cleanup.
2. Correct compiler floor and stale platform claims without changing the user-authored product spec.
3. Repair SOW status and empty regression-marker placement, strengthen audit exit status, and map historic deferred work with evidence.
4. Run local checks and actual smoke script; review and record limits (remote CI execution requires publishing, actual Mac/SRE/scale checks remain external).

Validation plan: local equivalent CI commands; workflow YAML parse, official action references, failure-injection tests; live one-node script through Netdata and lifecycle; read-only SOW audit with nonzero status on inconsistencies. No large local fleet.

Artifact impact plan: current-reality runtime spec and operator/dev docs, relevant live-validation skill, SOW records/audit. AGENTS.md contract unchanged; no output/reference operator skill exists. New unrelated hardening discoveries must be tracked rather than silently added.

Open-source reference evidence: actions/checkout @ 3d3c42e5aac5ba805825da76410c181273ba90b1 (README.md); actions/setup-node @ 249970729cb0ef3589644e2896645e5dc5ba9c38 (README.md). Remote refs, no workstation mirrors used.

Open decisions: none for independent CI/record repairs approved by the user. Platform launcher design belongs to SOW-0018; replay and signal-log design questions remain in pending SOW-0028 and do not authorize code there.

## Implications And Decisions

User approved the full long-term-best review recommendation. Each change remains minimal-complete; unresolved product choices require a concrete proposal before code.

## Plan

1. Finish preceding SOW.
2. Investigate this scope and fill the implementation gate.
3. Implement, validate and review; identify external acceptance dependencies explicitly.

## Validation

Current evidence and explicit limits are recorded in Delivery evidence below.

## Outcome

Delivered within the split automation/evidence scope; broader programme remains active in the linked SOWs.

## Lessons Extracted

Do not equate baseline unit tests with release or fidelity acceptance.

## Followup

This file tracks the review recommendations above; SOW-0029 additionally owns every historic Tier 3 item from SOW-0027 except fidelity/indexing/lint-readiness/replay, which belong to SOW-0028.

Sensitive data gate:

Current changes contain only synthetic test identities and public source references. No credential values or private endpoints were written; external operator evidence remains uncollected.

## Delivery evidence - 2026-09-09

- Added read-only hosted CI definitions with pinned checkout/setup-node revisions and no persisted checkout credentials. Standard checks cover Rust, shell/API/UI and SOW audit; compiler-floor job runs 1.88; separate manual live job builds the plugin from its checkout.
- Local compiler-floor check with Rust 1.88.0 passed against Cargo.lock. Workspace floor now matches the dependency requirement. Workflow YAML parsed locally; GitHub execution is not claimed because changes have not been pushed.
- Actual smoke script passed against the existing unchanged runtime image: one vnode and CPU samples through Netdata, exactly one process for each telemetry mode, stop/start, container restart and post-restart collection, then archive/removal of its unique container. No other service was modified. This validates the smoke machinery/current lifecycle, not a fresh image build.
- Eight Python regression tests and four Node tests pass. Existing Rust checks passed in SOW-0027; compiler-floor change additionally passed all-target checking.
- Audit was first observed failing on incomplete records, then passed after explicit sensitive-data sections/status corrections. It now exits nonzero for partial state. Empty historical regression markers were moved after their narratives; no actual historical event was discarded or fabricated.
- README/quickstart/operations distinguish verified Linux behavior, repaired Linux-container defects, unverified Mac acceptance and the known VM-token launcher defect. No spec.md edits.
- Historical Tier 3 implementation is split into real pending SOW-0030 before executing any of those changes; SOW-0028 owns fidelity/replay and SOW-0018 owns platform repair. This SOW delivers automation/evidence reconciliation only.

Artifact maintenance gate: runtime spec, operating docs and live-validation skill updated; AGENTS.md workflow unchanged; no external operator skill affected. SOW status/placement and follow-up mapping reviewed. Sensitive artifacts use synthetic data only.

Reviewer findings: final local review checked action permissions, immutable references, fork event type, unique test resource ownership and cleanup, stale platform assertions, compiler dependency evidence, and audit exit paths. External CI execution and actual Mac/blind-SRE/scale acceptance remain unverified, explicitly tracked rather than certified.

Lessons: an exit-zero audit cannot enforce acceptance; documentation must separate a repaired Linux-container defect from Mac acceptance; a green smoke test is scoped to the runtime image actually used.
