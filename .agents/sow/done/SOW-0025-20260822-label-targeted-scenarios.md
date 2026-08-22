# SOW-0025 - Label-targeted scenarios and the incident catalog

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: delivered - engine feature, 10 scenarios, webinar fleet, one regression found and fixed live; closing.

## Requirements

### Purpose

A fleet-management webinar next week needs to show: monitoring a fleet through label-based views, and incidents that hit *a part* of it (a region, a team's tier). Today scenarios target by role or hostname suffix only, so "the eu-west-1 nodes lose connectivity" is not expressible. This SOW adds label targeting to the scenario engine, ships the incident catalog (6 existing + 10 new, meeting spec.md's P1 "15+"), and a labeled webinar fleet template.

### User Request

"next week we're doing a fleet management webinar and we want to showcase how to monitor a fleet and show different views across different labels.. and showcase problems (for eg. connectivity issues that affect a part of your fleet) so we'll need a way to simulate this." Catalog decisions from the same message: the assistant's recommendations (1A: 10 new scenarios, app-tier first; 2A: generated-spec scenarios out of scope; 3A: gentle scenarios join the warm-up rotation).

### Assistant Understanding

Facts:

- Target matching (`sim-spec/scenario.rs:178` `Target::matches`) sees hostname, hostname_suffix, role, instance, signal - never labels. `NodeProfile.labels` is present on every engine at tick time; `lib.rs:328` builds the perturbation query and simply does not pass them.
- `logs.rs:613` calls `perturbation` directly and needs the same parameter; the exporter path routes through `signal_values` -> `signal_value` and inherits any fix.
- Fidelity lint (`sim-plugin/main.rs` `check_scenarios`) resolves targets against roles/signals/instances on disk; label targets need the same absent-vs-typo distinction (no node carries the label key: inapplicable; label key no fleet uses and nothing authors: still runs, labels are free-form).
- Linux system spec carries interface error/discard/octet signals (`if_in_errors_rate`, `if_in_discards_rate`, `if_in_octets_rate`...) - a partial-connectivity incident is authorable today.
- The app spec (`prometheus-app`) drives exporters AND OTLP (SOW-0020: one description of one service), so app-tier scenarios finally move scraped metrics and spans.
- Environments gained per-group labels in SOW-0019; a webinar fleet (regions x teams x tiers) is pure content.
- Warm-up incidents (`warmup.rs`) currently run a fixed internal shape; joining the rotation means the library's gentle scenarios appear there on the deterministic schedule.
- Local live-agent cap is 5 vnodes; the webinar fleet lints offline (SOW-0016: 25 nodes in 29s) and runs on the hosted/separate machine.

Inferences:

- Label targeting is the webinar's load-bearing feature: the demo beat is "filter Cloud by region=eu-west-1; the incident is exactly those nodes" - which only convinces when the fault genuinely selects by label.
- A single label selector per target (`label: region=eu-west-1`) covers the webinar and the visible near-term needs; multiple selectors are expressible as separate steps.

Unknowns:

- None blocking.

## Acceptance Criteria

- A step with `target.label` fires only on nodes carrying that label key=value - live-verified: labeled 5-node fleet, trigger, affected nodes' interface charts move, unlabeled nodes byte-quiet.
- The lint understands label targets: a label no node carries is "skipped", never a fatal error; a typo'd signal stays fatal as today.
- 10 new scenarios, each with an authored ground-truth manifest, demo-paced (~30 min): `network-degradation` (label-targeted partial-fleet connectivity loss; the webinar's opening incident), `checkout-degradation`, `payment-declines`, `cache-stampede`, `queue-backlog`, `worker-saturation`, `flash-sale` (positive incident), `db-connection-exhaustion`, `db-lock-contention`, `upstream-stall`.
- App-tier scenarios move the exporter charts and OTLP telemetry on a live agent (the SOW-0020 follow-up closes).
- Gentle scenarios (queue-backlog, cache-stampede, db-buffer pressure if authored) participate in the warm-up rotation; dramatic ones never do.
- `environments/webinar-fleet.yaml`: a multi-region, multi-team labeled fleet template, lint-clean offline.
- README scenario table updated (16 rows), Help tab's scenario paragraph updated, operating.md gains a webinar demo-path note.
- Gates: cargo test/clippy/fmt; corpus lint over a fleet using every new scenario; live 5-node checks incl. negative cases.

## Analysis

Sources checked:

- `crates/sim-spec/src/scenario.rs` (Target, matches), `crates/sim-engine/src/scenario_runtime.rs` (perturbation), `lib.rs:274-330` (signal path), `logs.rs:613`, `warmup.rs`
- `crates/sim-plugin/src/main.rs` `check_scenarios` (target resolution classes)
- Signal inventories: `specs/{linux-system,nginx,postgres,redis,prometheus-app,containers}.yaml` (candidate effects constrained to real signals; nginx has no status-code signal, so no 5xx-spike scenario is claimed)
- `environments/*.yaml`, `scenarios/*.yaml` (authoring conventions, manifest style, requires_roles)
- SOW-0019 (labels), SOW-0020 (exporter/OTLP coupling + the app-scenario follow-up), SOW-0006 (whole-corpus lint rule)

Current state: 6 scenarios; no label targeting; no app-tier scenarios; no labeled fleet template.

Risks:

- Label targets silently matching nothing (typo'd value) - mitigated by the lint's skip-report naming the label, and the console's trigger response already lists a scenario's resolved targets.
- The webinar fleet's size vs the local cap - linted offline here, run on the hosted box; the runbook says which.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model: incidents that select a fleet subset by an operational dimension (region, team) are not expressible because target matching never sees labels - the one identity axis the webinar is built around. The catalog gap is content; the targeting gap is engine.

Evidence reviewed: as under Analysis, with the call sites named.

Affected contracts and surfaces: `sim-spec` Target (+`label`, serde-defaulted; scenario YAML gains an optional key - additive), `matches()` and `perturbation()` signatures (internal), `lib.rs`+`logs.rs` call sites, `check_scenarios` label branch, `warmup.rs` (library-aware rotation), 10 new `scenarios/*.yaml`, `environments/webinar-fleet.yaml`, README/Help/operating docs.

Existing patterns to reuse: role/suffix matching semantics (labels compose with them: all present constraints must hold); the skip-vs-fatal lint classes; manifest authoring style from the existing six; group labels in environment authoring (SOW-0019); demo-paced multiplier timelines.

Risk and blast radius: additive engine surface; existing scenarios untouched (serde defaults). Worst case: a matching regression in `matches()` - covered by unit tests incl. every existing scenario file re-linted.

Sensitive data handling plan: synthetic regions/teams (`eu-west-1`, `payments`) - no customer names; webinar doc cites the demo fleet by its template name only.

Implementation plan:

1. Engine: `label` in Target + `matches` + `perturbation` plumbing (lib.rs, logs.rs) + unit tests.
2. Lint: label-aware skip/fatal + unit tests.
3. Warm-up rotation: gentle-flag scenarios on the deterministic schedule (flag: `warmup: true` in scenario YAML, default false; 3A's "gentle only" list encoded per scenario).
4. Scenarios: webinar-first ordering (network-degradation, then app tier, then db, then lb), manifests authored with the timelines.
5. `environments/webinar-fleet.yaml` (3 regions, 3 teams, mixed tiers; labels per group).
6. Docs; corpus lint; live validation; close.

Validation plan:

- Unit: matches() with/without label (composes with role/suffix/instance); lint classes; warmup selection.
- Corpus: lint a fleet exercising every new scenario (webinar fleet offline; multi-hundred-node scale acceptable - SOW-0016 precedent).
- Live (5-node labeled fleet): network-degradation moves errors/discards on labeled nodes only; checkout-degradation moves exporter charts AND the OTLP emitter's latency numbers; untargeted nodes quiet; resolve unwinds.
- Same-failure: re-lint all existing scenarios (targeting change must be invisible to them).

Artifact impact plan:

- AGENTS.md: one line (label targeting exists; manifests must select honestly).
- Runtime project skills: `project-live-validation` unaffected.
- Specs: `runtime-and-scenarios.md` scenarios section gains label targeting + warm-up flag.
- End-user/operator docs: README table, operating.md webinar path, Help tab paragraph.
- End-user/operator skills: none.
- SOW lifecycle: alone; unblocks nothing pending (catalog completes spec.md P1).

Open-source reference evidence: none newly required.

Open decisions: none blocking. Recorded: single label selector per target (`key=value` string); warm-up participation is a per-scenario `warmup: true` flag set only on gentle shapes; `flash-sale` is a positive incident with a manifest framing capacity strain, not a fault.

## Implications And Decisions

1. Catalog: 10 new scenarios, app-tier first (user: "go with your own recommendations", 2026-08-22 - decisions 1A/2A/3A from the proposal).
2. Generated-spec scenarios out of scope (2A): independent signals make causal-chain manifests decorative.
3. Gentle scenarios join warm-up via `warmup: true` (3A).
4. Label targeting: engine feature, single selector per target, composes with existing constraints (recorded in-spec).

## Plan

1. Engine label targeting + tests.
2. Lint label awareness + tests.
3. Warm-up flag + rotation + tests.
4. Ten scenarios, webinar-first.
5. Webinar fleet template.
6. Docs, corpus lint, live validation, close.

## Execution Log

### 2026-08-22

- SOW written under webinar urgency; decisions recorded; implementation started.
- Engine: `Target.label` (`key=value`, composes with all selectors), `matches`/`perturbation` signatures extended, call sites plumbed (metrics path, exporter path via `signal_values`, logs generator via a snapshotted label map). Unit tests: label selectivity incl. wrong-value/unlabelled/malformed-selector cases.
- Lint: label targets follow the skip-vs-fatal classes (no node carries the label: skipped; selector without `=`: fatal authoring error). Also fixed the pre-announced wart: exporter-spec signals now count as fleet-present (the exporter spec is deliberately not a node service), which removed ~24 false "skipped" lines from every lint of app-tier scenarios.
- Warm-up: `warmup` scenario field, default TRUE (participation is the shipped behavior; the SOW draft said default-false and that would have silently removed the six heroes from rotation - inversion recorded here as a deliberate correction). Dramatic scenarios (`payment-declines`, `worker-saturation`, `flash-sale`, `db-connection-exhaustion`, `db-lock-contention`, `upstream-stall`) opt out with `warmup: false`.
- Ten scenarios authored with manifests, webinar-first: `network-degradation` (label-targeted), six app-tier (checkout, payments, cache, queue, workers, flash-sale), two db, one lb. 16 total - spec.md's P1 "15+" met.
- `environments/webinar-fleet.yaml`: 20 nodes rendered via the describe path, labeled region+team per node (eu-west-1 / eu-central-1 / us-east-1; payments/checkout/catalog/platform/network/data). Corpus lint: 16/16 scenarios resolve, all 20 nodes PASS, zero skip lines.
- **REGRESSION FOUND LIVE AND FIXED**: go.d's prometheus jobs (SOW-0020 wiring) re-define the exported vnodes with no labels - and a plugins.d define migrates the host's whole label set, so go.d coming up after the metrics plugin wiped every fleet label, `simulated=true` included, on every containerised create with exporters. Invisible to SOW-0020's validation (charts and contexts were checked; labels were not). A shell-level create-ordering fix was tried first and discarded: the kill fires before the agent has started the plugin, so no ordering from the launcher can win. The shipped fix is in the plugin: the full HOST_DEFINE/label/chart handshake re-asserts every 240s (`HANDSHAKE_REASSERT_SECS`), whose semantics are exactly a plugin restart's - proven: fresh create wipes labels by t+80s, labels self-heal at the first re-assert, charts flow uninterrupted. sim-docker.sh also gained the metrics-plugin restart after the go.d restart as belt-and-braces for the common case.
- Live acceptance on a labeled 5-node containerised fleet: `network-degradation` -> ~30 err/s climbing on both eu-west-1 nodes, exactly 0 on the us-east-1 node; `checkout-degradation` -> scraped exporter chart p95 ~0.53-0.55s on web-01 vs healthy 0.28-0.35s on web-02 (app-tier scenarios move exporter charts - the SOW-0020 follow-up closes), OTLP log reporting both scenarios active from the same engine.
- Also observed during container bring-up: a stale simulation image hard-fails the whole scenario library on the new `warmup` field (library load is all-or-nothing), which presented as a one-node fleet. Image and repo move together via `sim-docker.sh build`; recorded as a lesson.

## Validation

Acceptance criteria evidence:

- Label targeting live: on a 5-node fleet labeled by region, triggering `network-degradation` produced ~30 err/s and climbing on both `region=eu-west-1` nodes and exactly 0 on the us-east-1 node; the selecting labels verified present on the nodes (after the re-assert fix, which this test is what caught).
- Lint classes live: the webinar-fleet lint reports 16/16 checked, zero skips (labels present, exporter signals recognised); web-stack lints show the skip behaviour for absent roles unchanged.
- 10 scenarios with authored manifests shipped; README table now 16 rows; Help tab names the label dimension and the count.
- App-tier + exporters live: `checkout-degradation` moved the scraped `prometheus.infra_sim_app.*` latency chart (p95 0.53-0.55 vs 0.28-0.35 on the untargeted app node) and the OTLP emitter reported the scenario active - exporter charts and traces move with incidents.
- Warm-up rotation: opt-out verified by unit test (an opted-out scenario never runs; rotation continues); gentle defaults verified by the field's default-true plus the six dramatic opt-outs in the files.
- Webinar fleet: `environments/webinar-fleet.yaml`, 20 nodes, 8/7/5 across three regions, six teams; corpus lint green.
- Label self-heal: fresh create -> labels absent at t+80s -> present at the first 240s re-assert -> charts uninterrupted.

Tests or equivalent validation:

- `cargo test`: 260 passed (label selector matching incl. composition and malformed refusals; warm-up opt-out never-runs; prior suites incl. every existing scenario file re-linted via corpus runs).
- `cargo clippy --all-targets -- -D warnings` clean; `cargo fmt --check` clean; `bash -n` on the touched script; console JS `node --check` clean.
- Corpus lint: 16/16 scenarios against the webinar fleet, 20/20 nodes PASS, no pinned signals, zero false skips.

Real-use evidence:

- Two full live cycles on containerised fleets on this machine (label selectivity; app-tier exporters+OTLP; label-wipe regression reproduced, root-caused, fixed, self-heal verified) - the exact sequences the webinar will run. The user's `default` simulation untouched.

Reviewer findings:

- Pending (fold into the next explicitly-requested round).

Same-failure scan:

- Re-linted every existing scenario against webinar-fleet and web-stack: targeting change invisible to them (same skip/resolve lines as before, minus the exporter-signal false skips).
- Searched for other agents that define our vnodes: only go.d prometheus jobs (registry) - covered by the re-assert; the OTLP and logs processes emit no HOST_DEFINE.

Sensitive data gate:

- Synthetic regions/teams only; no customer names, credentials, or host specifics in scenarios, fleet template, docs or this SOW.

Artifact maintenance gate:

- AGENTS.md: not updated - the scenario-format change is content-scoped and the specs file carries it; no workflow rule changed. Confirmed no update needed.
- Runtime project skills: `project-live-validation` gains nothing new - the label-wipe regression is its existing "verify through the product, not the file" rule applied to labels; no new class.
- Specs: `runtime-and-scenarios.md` scenarios section updated (label targeting, warm-up flag, 16-scenario library).
- End-user/operator docs: README 16-row table + label-target row; `docs/operating.md` webinar demo path; Help tab scenario paragraph.
- End-user/operator skills: none.
- SOW lifecycle: alone; spec.md P1 "scenario library 15+" met.

Specs update:

- `runtime-and-scenarios.md` - label targeting and warm-up semantics in the scenarios section.

Project skills update:

- Pending.

End-user/operator docs update:

- README scenario table (16 rows, label targeting called out), `docs/operating.md` webinar path, Help tab.

End-user/operator skills update:

- Pending.

Lessons:

- A define is a whole-label-set migration: any other collector that defines the same vnode wins your labels by defining later, and go.d registry vnodes define bare. Multi-writer vnode identity needs either one writer or a periodic re-assert; a launcher-side ordering cannot win a race whose participants start on different clocks.
- SOW-0020's validation checked charts and contexts but never labels - the wipe shipped invisibly because each SOW verifies what it changed, not what its change touches. The webinar fleet (labels as the point) is what surfaced it: acceptance criteria are the regression tests you did not know you were writing.
- The scenario library is all-or-nothing per binary: one unparseable file (a new field against an old image) blanks the whole fleet's metrics, presenting as a one-node agent. Image and repo must move together; a stale-image failure should read as "stale image", not silence.

Follow-up mapping:

- go.d restarts after settle still wipe labels until the next 240s re-assert (self-healing, not instant): acceptable; revisit only if a webinar beat needs instant settle, by re-asserting on a SIGUSR1 or shortening the timer.
- Generated-spec scenarios: rejected per decision 2A; revisit only if a generated spec ever gains coupled signals.

## Outcome

Delivered and closed. Scenarios can now select their victims by label - the
operational dimension the webinar is built around - composing with role,
suffix and instance selectors; sixteen scenarios ship with authored ground
truth (spec.md's P1 "15+" met); app-tier incidents move the scraped exporter
charts and the OTLP telemetry (the SOW-0020 follow-up closed); a labeled
20-node three-region webinar fleet template lints clean; and the live
validation caught and fixed a real SOW-0020 regression (go.d vnode defines
wiping every fleet label) with a self-healing handshake re-assert in the
plugin. The webinar's demo path: create from the template, filter a view by
region, trigger `network-degradation`, watch exactly that subset degrade.

## Lessons Extracted

Pending.

## Followup

- Instant label settle on go.d restart (SIGUSR1 re-assert or shorter timer) if a demo beat ever needs it.
- Generated-spec scenarios remain rejected (2A) until their signals are coupled.

## Regression Log

None yet.
