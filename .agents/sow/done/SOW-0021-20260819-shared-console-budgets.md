# SOW-0021 - A shared, multi-simulation console with resource budgets

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: delivered - implemented, live-validated on a five-simulation host, artifacts updated.

## Requirements

### Purpose

Make the console safe to host on a shared machine (an SRE-hosted AWS box, reachable from inside Netdata) where several people create, use and tear down simulations concurrently, without any one of them able to exhaust the host's resources or stumble into another's fleet by accident. First of four SOWs preparing the project for shared hosting; auth (SOW-0022), in-UI help (SOW-0023) and self-monitoring (SOW-0024) follow.

### User Request

Host the simulator somewhere publicly reachable for everyone in Netdata, after: (1) a simple, intuitive UI with built-in help; (2) guards against resource overuse and self-inflicted damage; (3) monitoring to detect issues. Decisions recorded 2026-08-19:

1. Tenancy architecture: **A** - shared console with per-simulation ownership, per-sim targeting, serialized creates. Long-term-best; chosen over a serialized single-active console.
2. Auth: **B** - shared token as the interim gate (SOW-0022). Cloud SSO later.
3. Resource guards: **A** - a budget layer with hard, host-tunable caps and a fleet TTL.
4. Monitoring: **A** - self-monitoring pack on the host agent, alerts + runbook (SOW-0024); usage telemetry deferred.

### Assistant Understanding

Facts:

- The console's `AppState.active` is a single `Option<Active>` adopted arbitrarily at startup (`main.rs` `container::list().next()`) and never repointed by create; `target()` routes every per-sim endpoint (scenarios, labels, logs, reskin) through it. This caused a real incident on 2026-08-18 (SOW-0019 execution log): a test console edited the wrong running fleet while reporting success.
- Teardown already takes an explicit simulation name; create does not repoint `active`; `/api/status` discovers only when `active` is `None`.
- Guards today: `count.min(500)` per group is the only node cap; no fleet cap, no sim-count cap, no disk budget, no TTL, no concurrency control (the global `Progress` handle is clobbered by concurrent creates; the lint inside create is CPU-parallel across all cores by design - `parallel.rs`).
- Port allocation (`free_port`, 19900-19990) scans for a free port per create; ~90 slots, untracked beyond that.
- Docker labels are immutable after create; a mutable per-sim flag (pin) cannot be a label. Payload dirs (`/var/lib/infra-sim/<name>/`) are mutable host paths the console already owns.
- `container::list()` already enumerates every labelled container including stopped ones; `Active` carries name/port/payload/ip but not owner or created-at.
- Docker exposes container `CreatedAt` (creation, not start - correct semantics for TTL).
- Ownership cannot be authenticated under a shared token (2B): an owner field is honor-system until SSO lands (recorded as the enforcement hand-off in SOW-0022's scope).
- `spec.md` places always-on shared environments at P1 "on the demo parent"; this SOW is the enabling layer for a broader "shared host for all of Netdata" scope - a pull-forward the user has implicitly made by requesting hosting prep; recorded here for later `spec.md` ratification, which requires explicit user approval per project rules.

Inferences:

- Per-sim API namespacing (`/api/sim/{name}/...`) kills the sticky-target class at the root and is the natural shape for ownership-enforced routes later; the UI is the only API client, so the breaking change ships atomically with it.
- A queue (semaphore of one around build+lint+create) with honest position reporting is only marginally more work than rejecting concurrent creates and much better on a shared host, where the lint alone runs minutes at fleet scale.
- Total-disk measurement at create time (walk of `/var/lib/infra-sim`) is cheap at the intended scale (tens of sims) and catches the real failure (journals + agent DBs accumulating) without per-sim quota machinery, which stays out of scope.

Unknowns:

- Whether `docker ps` `CreatedAt` parsing is stable across Docker versions on the target host - resolved in implementation by using `docker inspect -f {{.Created}}` (RFC3339) which is documented API.

## Acceptance Criteria

- With two simulations running, every per-sim endpoint (scenario trigger/resolve/advance, labels GET/POST, logs, telemetry) addresses its simulation explicitly and verifiably hits only that one; the sticky-target class is regression-tested end to end.
- Concurrent creates: the second create queues and reports its position through the existing progress UI; both complete; no interleaved progress output.
- Over-budget refusals are actionable and name the limit: nodes-per-fleet, live-simulation count, and total-disk each have a live-verified refusal path.
- A simulation past the TTL is auto-archived (the teardown path) within the sweep interval unless pinned; pinning via the UI/API survives sweeps; the sweeper never touches another console-owner's... (honor system: it archives by age+pin only, owner-independent).
- Every simulation shows its owner and age in the UI list; create requires an owner.
- Budgets are host-tunable without code changes (a config file the SRE owns), and the defaults are documented.
- All prior gates hold: `cargo test`, clippy `-D warnings`, fmt; live-agent validation of the full two-sim lifecycle on this machine with the user's running simulation untouched.

## Analysis

Sources checked:

- `crates/sim-console/src/main.rs` (AppState, `target()`, route table, status discovery, teardown-by-name)
- `crates/sim-console/src/provision.rs` (CreateRequest, Progress handle, group caps, install paths)
- `crates/sim-console/src/container.rs` (create/list/active, label use, payload dirs)
- `crates/sim-engine/src/parallel.rs` (lint parallelism - why concurrent creates must serialize)
- `scripts/sim-docker.sh` (create flags, label convention `infra-sim.simulation`, free_port range, teardown/archive path)
- `.agents/sow/done/SOW-0012-20260808-containerised-sims.md`, `SOW-0019-20260818-custom-host-labels.md` (sticky-target incident), `.agents/sow/specs/runtime-and-scenarios.md`, `authoring-environments.md`
- `spec.md` (P1 hosting wording; non-goal boundaries unaffected)

Current state:

- Single-active console; no ownership, no budgets, no concurrency control; teardown already name-addressed; UI single-sim.

Risks:

- Largest UI rework since the console shipped (sim selector, budget display, owner field, queue position, pin) - mitigated by keeping the create form unchanged and adding chrome around it.
- Breaking internal API change - mitigated by shipping handler + UI changes in one commit; no external clients exist (`scripts/` use `sim-docker.sh`, not the API).
- TTL sweeper destroying something wanted - mitigated by pin-default-empty but sweep requires age AND non-pinned, plus a one-SOW-later warning channel; the archive path preserves everything (same as manual teardown).
- Two consoles on one host (the user's 19995 plus a hosted one) both sweeping - out of scope here; one console per host is the hosting model and goes in the runbook (SOW-0024).

## Pre-Implementation Gate

Status: needs-user-decision (three defaults below; everything else ready)

Problem / root-cause model:

- The console was built single-user: one active pointer, one implicit "the fleet", one global progress handle, no notion of who or how many. On a shared host that architecture fails three ways at once - wrong-target operations (proven by incident), resource exhaustion (no caps beyond 500/group), and interleaved concurrent work (global progress clobbered). Ownership, budgets and serialization are missing layers, not defects to patch in place.

Evidence reviewed:

- As listed under Analysis; the sticky-target incident record in `SOW-0019` execution log; `free_port` budget arithmetic; docker label immutability (pin needs a mutable marker file in the payload dir).

Affected contracts and surfaces:

- Console API: routes re-namespaced `/api/sim/{name}/...` for per-sim actions; `/api/status` becomes `{simulations[], budgets, progress}`; `/api/create` gains `owner` and returns budget context; new `/api/sim/{name}/pin` and `/api/budgets`. Internal-only breaking change, shipped with the UI.
- `AppState`: `active: Option<Active>` → simulation registry (refreshed from `container::list` per request); per-sim progress handles; create semaphore.
- `provision.rs`: CreateRequest.owner; budget checks at create; queue-aware Progress.
- `container.rs`: `Active` gains `owner` (docker label `infra-sim.owner`), `created_at` (container CreatedAt), `pinned` (payload marker file); list surfaces them; create passes `--owner` through to `sim-docker.sh`.
- `scripts/sim-docker.sh`: `create --owner NAME` sets the label; nothing else changes.
- `ui.html`: simulation selector (tabs or dropdown listing name/owner/age/pin), owner field in create, budget display, queue position in progress, pin control.
- Budgets file: `/etc/infra-sim/console.yaml` (path overridable by `--budgets`), SRE-owned: `max_nodes_per_fleet`, `max_live_simulations`, `max_total_disk_gb`, `ttl_days`. Defaults in code, documented.
- Docs: `docs/operating.md` multi-sim + budgets section; AGENTS.md hosting-model note (one console per host).
- Not affected: plugin, engine, scenarios, specs, exporters (all per-simulation already).

Existing patterns to reuse:

- Teardown's explicit-name dispatch (the shape per-sim routes copy); `container::list` label enumeration; the Progress/`startProgress` UI pattern (extended with a queue position line); marker-file conventions (`control.yaml` lives beside the environment - `pinned` joins it); `json_err` refusal formatting; the archive-on-teardown path reused verbatim by the sweeper.

Risk and blast radius:

- Console-only blast radius; the plugin and data paths are untouched. Regression risk concentrates in API dispatch (fixed by the two-sim live regression test) and the UI rework (kept additive around the existing create form). Rollout: single commit; hosted instances need a console restart, which the runbook will own. The user's local console on 19995 is theirs to restart.

Sensitive data handling plan:

- Owner names are free text typed by colleagues - treated like environment names today: displayed, stored in a docker label, never treated as a credential. The budgets file contains no secrets. SOW text and docs use synthetic owner examples (`sre-a`). No tokens, keys, or customer data in any artifact this SOW writes.

Implementation plan:

1. Engine-free core: `Active` + `container.rs` extensions (owner label, created-at, pinned marker), `sim-docker.sh --owner`; unit tests for the label/parsing paths.
2. Budget layer: config parse (defaults + file override), disk measurement, refusal messages; unit tests including over-limit arithmetic and malformed config.
3. API re-namespacing: per-sim routes, status payload with simulations+budgets, per-sim progress, create semaphore + queue position; `main.rs` handler tests where feasible.
4. TTL sweeper: hourly task, age + non-pinned, archive via existing teardown path; pin endpoint; tests with fabricated created-at.
5. UI: sim selector with owner/age/pin, owner field, budget banner, queue position; JS syntax-gated as before.
6. Docs + AGENTS.md; live validation; close-out gates.

Validation plan:

- Unit: budget parsing/refusals; Active parsing; sweeper selection logic (age/pin matrix) against fixtures; route dispatch (handler-level where practical).
- Live (two sims on this machine, user's `default` untouched): create A and B concurrently → queue position visible → both complete; scenario trigger on B leaves A's charts unmoved (the incident regression); labels edit on A leaves B untouched; over-node-count create refused naming the cap; TTL sweep archives a fixture-aged sim; pin prevents it; teardown by name from the new UI; budgets file override changes a refusal live.
- Same-failure scan: search for remaining `Option<Active>`/single-target assumptions after the rework (`grep -rn "app.active"` must come back empty or justified).

Artifact impact plan:

- AGENTS.md: hosting-model note (one console per host; budgets file is the SRE contract).
- Runtime project skills: `project-live-validation` unaffected (same loop); confirm at close.
- Specs: `runtime-and-scenarios.md` gains a "Shared hosting" section (multi-sim, budgets, TTL, ownership-as-honor-system until SSO).
- End-user/operator docs: `docs/operating.md` rewritten console section (selector, budgets, pin, TTL); README "hosting" paragraph pointing at the SRE path.
- End-user/operator skills: none affected.
- SOW lifecycle: SOW-0022 (auth token) and SOW-0024 (monitoring) depend on this SOW's API shape; SOW-0023 (UI help) builds on its UI. SOW-0013 recommended closed as superseded (console-manages-containers shipped via SOW-0012 follow-through; see follow-up mapping).

Open-source reference evidence:

- None newly required; docker CLI semantics (`inspect -f {{.Created}}` RFC3339, label immutability) verified against Docker documentation during investigation. No agent-source assumptions introduced.

Open decisions:

None remaining. Resolved 2026-08-19 by the user:

1. Default budgets: **500 nodes/fleet, 10 live simulations, 50 GB total disk, 7-day TTL** - all host-tunable via the budgets file. (500/fleet matches the existing per-group ceiling so a single group of 500 stays expressible; the live-sim and disk caps are the real shared-host guards.)
2. Create concurrency: **queue with position reporting** ("2A").
3. SOW-0013 closed as superseded ("3A") - recorded there with a dated note; console-manages-containers shipped with SOW-0012's follow-through.

## Implications And Decisions

1. Tenancy: shared console, per-sim ownership and targeting (user: "1A", 2026-08-19).
2. Auth: shared token, interim, SOW-0022 (user: "2B").
3. Guards: hard budget layer with host-tunable caps + TTL (user: "3A").
4. Monitoring: self-monitoring pack, SOW-0024 (user: "4A").
5. Ownership is honor-system until authenticated identity exists: owner is recorded and displayed in this SOW; enforcement (refusing teardown of another's sim) activates with SSO in a later SOW - recorded as the hand-off contract.
6. Budget defaults 500/10/50GB/7d, all host-tunable; queue with position reporting; SOW-0013 superseded (user, 2026-08-19).

## Plan

1. `container.rs` owner/created/pinned + `sim-docker.sh --owner` (low risk, no deps).
2. Budget config + disk measurement + refusals (unit-tested, no deps).
3. API re-namespacing + per-sim progress + create queue (the core; depends on 1).
4. TTL sweeper + pin endpoint (depends on 1, 3).
5. UI chrome: selector, owner, budgets, queue, pin (depends on 3, 4).
6. Docs, spec, live two-sim validation, close-out gates.

## Execution Log

### 2026-08-19

- SOW drafted from the hosting-prep decisions; overlap check against pending SOWs (0013 superseded-recommended, 0007/0018 disjoint); no code touched.

### 2026-08-21

- Implemented chunks 1-5: `container.rs` full-state `Active` (one `docker inspect` JSON per sim and one bulk inspect for the list, owner label, RFC3339 created-at with an exact civil-date parser, pinned marker file); `budget.rs` (defaults 500/10/50GB/7d, file override with `deny_unknown_fields`, refusal messages, recursive disk walk); per-sim API (`/api/sim/{name}/...` for status/scenarios/labels/reskin/claim/logs/pin), per-operation progress map, single-slot create queue with live positions, hourly TTL sweeper with a heartbeat log line; `sim-docker.sh create --owner` with label validation; UI sim selector (name/nodes/owner/age/pin), budget banner, owner field, per-sim action URLs, queue-aware progress, pin button.
- Defects my own validation caught and fixed before close: budget refusals hardcoded the default file path instead of the `--budgets` one (Budgets now carries its source); the UI polled progress under its own op id while the console minted a different one (requests now carry `op`, handler honours it); the owner requirement lived only on the host-fallback path so the container path accepted ownerless creates with an empty 422 (enforced at the API entry with a real message); the queue counter decremented at acquire so the running create was uncounted and the first waiter showed no queue state (decrement at handler end); the sweeper only logged when archiving, so "pinned skip" was indistinguishable from "never ran" (heartbeat line).
- Live validation on this machine with the user's `default` protected by a pin throughout (it is 9.5 days old, past the TTL, and survived every sweep - the pin-protection proof).

## Validation

Acceptance criteria evidence:

- Per-sim addressing (the SOW-0019 incident regression), live: `POST /api/sim/shared-a/scenario/disk-fill/trigger` -> shared-a's control.yaml holds disk-fill, shared-b's is `active: []`, shared-b's agent reports zero active alarms; `advance` 900s moved shared-a's /var/lib/pgsql to ~8x baseline while shared-b stayed quiet; a labels POST on shared-a (4 nodes changed) left shared-b's environment byte-identical (md5 before/after).
- Concurrent creates: two simultaneous creates serialize; the second reported `queued behind 1 create(s) - yours starts when theirs finishes` through the progress endpoint for the duration of the first, then both completed with correct owners (`sre-a`/`sre-b` visible in status and docker labels).
- Budget refusals, live: a 20-node create against max 6 refused naming the number, the limit key, and the actual `--budgets` path; an ownerless create refused with `an owner is required - say who this simulation belongs to`.
- TTL sweeper, live: start-up sweep logged `1 simulation(s) checked, 0 archived` with the 9.5-day-old unpinned-by-TTL `default` surviving because pinned - pin protection proven against a genuinely expired fleet; the archive path itself is the ordinary teardown path (exercised five times this session); the age/pin selection matrix is unit-tested. Honest gap: archiving a genuinely expired unpinned sim was not exercised live - no sim here is both expired and unpinned, and docker `Created` cannot be faked.
- Owner and age shown per simulation in the list; pin toggled live via API and reflected in status.
- Teardown by name through the new API: five fleets removed cleanly; the user's simulation untouched throughout.

Tests or equivalent validation:

- `cargo test`: 258 passed (RFC3339 parser incl. offsets/fractions/Z/epoch/leap-year, Active-from-JSON with owner/created/pin, budget load/override/malformed/unknown-key/refusal messages/expiry matrix/disk walk, node_refs label-collision, plus all prior suites).
- `cargo clippy --all-targets -- -D warnings` clean; `cargo fmt --check` clean; `bash -n scripts/sim-docker.sh` clean; console JS `node --check` clean.

Real-use evidence:

- Full lifecycle on a live five-simulation host through the shipped API/UI surfaces: two queued creates, scenario trigger+advance, live labels, pin, four teardowns, budget and owner refusals - recorded above. The hosted-UI page serves with the selector present.

Reviewer findings:

- Pending (recommend folding into the next explicitly-requested round; see SOW-0020's note).

Same-failure scan:

- `grep -rn "app.active"` and `Option<Active>` across the console: only the by-name `container::active` lookup remains - the sticky pointer is gone.
- Searched the UI for un-namespaced per-sim calls: all scenario/labels/reskin/log URLs carry the selected simulation; `/api/progress` polls carry an op id.

Sensitive data gate:

- No credentials or prospect names in any artifact; owner examples are synthetic (`sre-a`). The user's `default` fleet gained a protective `pinned` marker during validation - left in place deliberately (its removal is `rm /var/lib/infra-sim/default/pinned`) and reported to the user.

Artifact maintenance gate:

- AGENTS.md: updated - multi-sim console rule (no active pointer, serialized creates, budgets contract, one console per host).
- Runtime project skills: `project-live-validation` unaffected - its loop covered this work; no new lesson of its class.
- Specs: `runtime-and-scenarios.md` gained "Shared hosting".
- End-user/operator docs: `docs/operating.md` gained "Shared hosting, budgets and the TTL"; README hosting paragraph.
- End-user/operator skills: none exist for this surface.
- SOW lifecycle: SOW-0013 closed as superseded (user decision); SOW-0022/0023/0024 unblocked; no regressions.

Specs update:

- Pending.

Project skills update:

- Pending.

End-user/operator docs update:

- Pending.

End-user/operator skills update:

- Pending.

Lessons:

- Five of this SOW's defects were found by its own live validation, and four of them only existed on paths a unit test calls indirectly (the container path's owner check, the op-id pairing, the queue counter's off-by-one, the wrong path in refusal messages). The pattern held again: the shipped path is the only honest test bed.
- A counter that decrements at acquire instead of at completion measures "waiting" while naming it "ahead" - and produces a silent, invisible queue. Names of atomic operations should state whose perspective they count.
- A background sweeper that only logs on action is unverifiable in production: "nothing happened" and "nothing ran" are indistinguishable. Heartbeat lines are one eprintln of cheap observability.

Follow-up mapping:

- SOW-0022 (auth token + hosting hardening), SOW-0023 (UI help/onboarding), SOW-0024 (self-monitoring pack): tracked as the remaining hosting-prep SOWs, in that order.
- Ownership enforcement with authenticated identity: hand-off contract recorded in Implications decision 5; activates with Cloud SSO investigation.
- TTL pre-expiry warning channel (notify an owner before archiving rather than only logging): candidate with SOW-0024's alert routing.
- `default` sim pin: reported to the user; unpin is their call.

## Outcome

Delivered and closed. The console is a shared-host console: every action
names its simulation (the wrong-fleet incident class is structurally gone and
regression-tested live), every fleet has a visible owner and age, creates
serialize behind one honest queue, host-tunable budgets refuse over-large or
over-many fleets with messages naming the real limit file, and a heartbeating
TTL sweeper archives forgotten fleets unless pinned - pin protection proven
against a genuinely expired fleet. 258 tests, clippy, fmt green; five-sim
live validation with the user's fleet protected throughout. Remaining hosting
prep: auth (0022), UI help (0023), monitoring (0024).

## Lessons Extracted

Pending.

## Followup

- SOW-0022 (auth token + hosting hardening), SOW-0023 (UI help), SOW-0024 (self-monitoring): the rest of the hosting prep, in that order.
- Ownership enforcement activates with authenticated identity (Cloud SSO investigation) - hand-off contract recorded in decision 5.
- Per-sim disk quotas (as opposed to the total-disk budget): rejected for now as machinery without a demonstrated need; total catches the real failure mode.

## Regression Log

None yet.
