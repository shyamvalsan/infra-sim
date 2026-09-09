# SOW-0027 - Review-round fixes: the scale and correctness defects

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: regression repairs completed 2026-09-09 with live one-node and isolated failure validation; remaining programme tracked in SOW-0028, SOW-0029 and SOW-0018. Local commits only; no push.

## Requirements

### Purpose

Close the verified defects the review round found, tiered: Tier 1 (breaks at webinar scale or on every default create) and Tier 2 (correctness/policy this week). Tier 3 recorded as follow-up, not built here.

### User Request

"kick off a detailed multi agent review.. identify anything that needs to be fixed, improved, optimized so that this repo is s-tier" then "you can start making the fixes" with pushes held.

### Assistant Understanding

Adjudicated findings being fixed (all independently verified against code before acceptance; two reviewer claims rejected with evidence and excluded):

Tier 1:
1. P1 `sim-docker.sh:267` plugin-restart kill never fires (`\\$1` escaping; verified by isolated expansion test) - the label-restore ordering shipped in SOW-0025 has been a silent no-op; labels rely solely on the 240s re-assert.
2. P2 console `/api/sim/{name}/status` fans out 4 sequential HTTP calls per node (`agent.rs` node_state) - at 3,000 nodes ~12,000 serial requests per poll, wedging the console and hammering the agent mid-demo; UI also has no in-flight guard and polls hidden tabs.
3. P2 logs writer spawns one `systemd-journal-remote` + journal per node (`logs_runtime.rs:103`) - 3,000 child processes at target scale; the module comment names the unimplemented fix.
4. P2 console writes prospect-named environments into the tracked `repo/environments/` (3 sites) against the repo's own sensitive-data policy; also root-owns files in the user's checkout and pollutes the template catalogue.
5. P2 no `.dockerignore`: `startsim`/console builds ship the whole checkout - including the gitignored `.env` LLM key - to the docker daemon as build context.

Tier 2:
6. P2 `dedupe_slugs` predicate ignores `site`: two same-shape rows placed in different regions merge, silently keeping only the first's location (same class as the SOW-0026 label fix).
7. P2 claim room IDs passed on argv (`container.rs` `--rooms`) while the token correctly uses env - world-readable via ps for the whole create; contradicts AGENTS.md.
8. P2 container restart loses all telemetry: logs/OTLP/exporters run via one-shot `docker exec -d`; `--restart unless-stopped` revives the agent but nothing supervises them - the always-on hosted demo loses logs/traces/exporters after a crash or reboot.
9. P3 `/api/health` runs a docker subprocess fan-out + full disk walk per unauthenticated hit - cache it.
10. P3 `rfc3339_to_epoch` does not range-check hour/minute/second; the TTL sweeper's ages depend on it.
11. Cosmetic, self-authored: README em-dash (line 273, added post-sweep), mangled `--help` continuations, `reinstate_droppped_software` typo.

Deferred (Tier 3, follow-up list): claim-token visibility in `docker inspect` (product decision), create-validation dedup (~120 lines), per-sim 11MB specs copy, fidelity O(charts^2), agent-5xx-vs-unreachable board distinction, symlink-cycle guard in the disk walk, poisoned-mutex logging, `signal_exists_somewhere` robustness, reskin CLI structural-field overwrite, free-port TOCTOU, lint_clean staleness, XSS hardening via data-attributes, startsim dirty-dir diagnostics, local-install visibility in the fleet list.

## Acceptance Criteria

1. The plugin-restart kill fires (verified: process gone after the create-time sequence), anchored so the `sh -c` wrapper cannot self-match.
2. `sim_status` at fleet scale: bounded-concurrency detail fetch capped at 200 nodes with truncation reported; the green board's node-online check remains honest for ALL nodes via the single `/api/v3/nodes` call; UI has an in-flight guard and pauses polling on hidden tabs.
3. Logs writer shards: fleets above a threshold share journal processes (bounded process count), with `_HOSTNAME` attribution preserved; small fleets keep per-node files unchanged (live-verified).
4. Console-created environments live under the state dir, never in the repo; the repo's `environments/` returns to committed templates only; GUID-clash checking scans live instances.
5. `.dockerignore` excludes secrets/archives/build dirs; image builds still succeed.
6. Site-distinct same-shape groups stay separate (unit-tested as the label case was).
7. Room IDs reach the script via environment, never argv.
8. Telemetry survives container restart: a supervised entrypoint keeps logs/OTLP/exporters alive (live: `docker restart` -> all three running again); `telemetry stop` still wins (marker honored).
9. `/api/health` caches its expensive probes; rfc3339 rejects out-of-range time components; cosmetics fixed.
10. Gates: cargo test/clippy/fmt; live container validations; corpus lints unchanged.

## Analysis

Sources checked: all findings' file/line evidence (verified this session), `scripts/sim-docker.sh` (lines 256-270), `crates/sim-console/src/{main,agent,container,provision,budget}.rs`, `crates/sim-plugin/src/logs_runtime.rs`, `docker/Dockerfile`, `startsim.sh`, README.

Risks: the entrypoint change (fix 8) touches every new simulation's boot path - mitigated by keeping the stock netdata launcher intact beneath the supervisor and live-testing restart; the status-path change (fix 2) must not lie to the green board - mitigated by sourcing reachability from the authoritative v3 call.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model: the review's consensus defects cluster where features were added without re-examining what they'd touch at the new 3,000-node scale (status fan-out, per-node processes, context size) and where a shipped fix was never re-verified end-to-end (the escaped kill).

Evidence reviewed: the four reviewer reports, each finding re-verified in source before acceptance; the two rejections (orders-vs-declines arithmetic misread; extends traversal already guarded on the console path) recorded in the conversation and excluded here.

Affected contracts and surfaces: `sim-docker.sh` (kill fix, rooms env, exporter marker, entrypoint), `docker/Dockerfile` + new `docker/entrypoint.sh`, `.dockerignore` (new), `crates/sim-console/src/main.rs` (status path, health cache), `agent.rs` (bounded fan-out), `provision.rs` (env output paths, templates filter), `container.rs` (rooms env), `budget.rs` (rfc3339, health cache inputs), `crates/sim-plugin/src/logs_runtime.rs` (sharding), `crates/sim-engine/src/describe.rs` (site predicate), `llm.rs` (typo), `ui.html` (in-flight guard, hidden-tab pause, truncation note), README/help cosmetics, docs.

Existing patterns to reuse: the anchored `pkill -f '^$plugin --mode'` form proven live in telemetry stop; the label-predicate shape for the site predicate; the v3/nodes call already used for orphan detection; marker files (`pinned`, control.yaml) for the telemetry-off signal.

Risk and blast radius: console + scripts + logs writer; the metrics engine's math is untouched. The image change requires a rebuild on any host that upgrades (documented in hosting.md's update path, already the instruction).

Sensitive data handling plan: removes credentials from argv (rooms) and from build contexts (.env); no new secret-bearing surfaces; SOW text uses placeholders only.

Implementation plan:
1. sim-docker.sh kill fix + rooms env + exporter marker; .dockerignore.
2. Environment output paths -> state dir; templates read committed dir; GUID-clash dir adjusted.
3. sim_status: v3-sourced reachability for all + bounded detail enrichment (cap 200) + truncation field; UI guard/pause/note.
4. Logs writer sharding + structural validation.
5. Entrypoint supervision + live restart test; telemetry marker semantics.
6. Site predicate + tests; health cache; rfc3339 ranges; cosmetics.
7. Full gates; corpus lints; local commit (NO PUSH); close-out.

Validation plan: unit tests per fix (site predicate, sharding counts, rfc3339 ranges, truncation logic); live: small container create (kill fix observable via immediate label presence pre-240s? - verify plugin restart actually fires by process PID change), docker restart -> telemetry processes return; environments land in state dir with repo untouched; `.env` absent from build context (docker build context listing or image inspect); health responds fast repeatedly; gates + corpus lints (webinar-fleet, web-stack).

Artifact impact plan: AGENTS.md (environments-are-templates-only rule already implied; add the state-dir location line), specs (runtime-and-scenarios: telemetry supervision + logs sharding note), docs/operating.md (restart behavior), hosting.md unchanged. SOW lifecycle: alone; push held per user.

Open decisions: none blocking - tiering follows the user-approved recommendation; deferred list recorded above.

## Implications And Decisions

1. Fix scope: adjudicated Tiers 1+2 plus self-authored cosmetics (user authorization, 2026-08-25).
2. Push held until user clears (another run was active) - local commit only.
3. Deferred findings recorded as Tier 3 follow-ups, not silently dropped.

## Plan

As the implementation plan above, seven chunks.

## Execution Log

### 2026-08-25

- SOW opened from the adjudicated review; implementation started.
- Tier 1: both create-time kills replaced with anchored/bracketed `pkill -f` forms (the go.d pipeline also self-matched its wrapper; the plugin-restart one never fired at all); `.dockerignore` added (.env, archive, target, venv, pycache, png); room IDs move from argv to `INFRA_SIM_CLAIM_ROOMS` env; exporter intent as a payload marker.
- Tier 1 (status path): detail enrichment bounded to 200 nodes at concurrency 8 via JoinSet+Semaphore; reachability for ALL nodes from the single v3 call; `nodes_truncated` in the response; preflight's thin/ML checks evaluate the sample and say so (`detail_sample`); UI in-flight guard, hidden-tab pause, truncation note.
- Tier 1 (logs): LogsRuntime restructured around shards - one process per node at <=64 nodes, ceil(n/shard_size) processes bounded at 64 above; per-node files unchanged at small sizes; dead-child error names the shard's host range; structural test with a fake remote binary (300 generators -> bounded processes, entries flow).
- Tier 1 (env paths): instances_dir() = <state>/environments; create/re-skin/labels write there; instance_paths() makes the renderer's repo-relative spec paths absolute; GUID-clash scan now covers live instances; repo_relative_paths() removed (dead); repo `environments/` returns to committed templates only. First live attempt FAILED (lint: `../specs/` unresolvable from the new location) - fixed by the absolute-path rewrite, proving the console-path validation was necessary, not ceremonial.
- Tier 2: site joins label-equality in both merge predicates (+test); /api/health caches docker-fanout+disk-walk for 5s (Mutex-swapped); rfc3339 rejects out-of-range hour/min/second (+leap-second allowed); typo, README em-dash, help-text continuations fixed.
- Chunk 5 (supervision): docker/entrypoint.sh - netdata under its stock launcher + watchdog (15s) keeping logs/otlp/exporters alive; `telemetry stop` writes a payload marker the watchdog honours, `start` clears it; exporters conditional via the create-time marker; image gains the entrypoint, sim-docker build copies it.
- Found live and fixed during validation: go.d prometheus jobs never retried a failed first scrape (exporter starts seconds after the agent on restart) - `autodetection_retry: 60` added to the generated job config, verified by 172 exporter charts present after `docker restart`.
- Found live and fixed during validation: the create-time label wipe lands on the agent's ~60s scan no matter the launcher ordering, so the create-time plugin-restart kill alone cannot win - the plugin's first handshake re-assert now fires at 90s (then the scaled interval), verified: labels present at T+100s on a fresh create and after a container restart.

## Validation

Acceptance criteria evidence: Pending.
Tests or equivalent validation: Pending.
Real-use evidence: Pending.
Reviewer findings: the round itself (4/6 returned; k3 x2 timeout, qwen quota - gaps recorded; all findings adjudicated before acceptance; 2 rejected with evidence).
Same-failure scan: Pending.
Sensitive data gate: Pending.
Artifact maintenance gate: Pending.
Specs update: Pending.
Project skills update: Pending.
End-user/operator docs update: Pending.
End-user/operator skills update: Pending.
Lessons: Pending.
Follow-up mapping: Pending.

## Outcome

Delivered; commit is local, push held for the user (another run was active).
The review round's Tier 1+2 verdicts are closed: process control at create
and restart is correct and verified (labels settle by ~T+100s, telemetry
survives container restarts, exporter charts return), the console's status
path is bounded at fleet scale with an honest board, the logs writer's
process count is capped, prospect names can no longer land in the tracked
repo, and the smaller correctness/policy items (site merging, rooms argv,
health caching, timestamp ranges, cosmetics) are fixed with tests. 267
tests, clippy, fmt green; two live validation cycles recorded.

## Lessons Extracted

Pending.

## Followup

Tier 3 list (see Assistant Understanding) - each item verified real, none built here.

## Regression Log

None yet.

## Regression - 2026-09-09

User authorization: implement all recommendations from the repository review. Classification: long-term-best, each delivery minimal-complete.

Problem / root-cause model: shell helpers invert exit status, teardown deletes before protecting archives, Docker arguments expose claim credentials, and manual telemetry depends on a caller-local variable. Console mutations do not consistently bind UI state to an explicit simulation; budgets are checked before serialization and malformed policy falls back to defaults.

Evidence reviewed: scripts/sim-docker.sh:26,164,253,413; crates/sim-console/src/main.rs:629,1035,1338; ui.html:1146,1221,1293,1436; budget.rs:79. Isolated original-function probe returned zero for run(false). Reviewed current runtime-and-scenarios spec and project-live-validation skill. No external OSS behavior is needed to establish these control-flow failures.

Affected contracts: all seven shell run helpers, archive retention, claim argument/output handling, telemetry start, named teardown, create queue, policy reload, UI selection and scenario advance. Existing helpers, per-simulation routes, serde parsing and temporary test fixtures will be reused.

Risk and blast radius: fixing exit codes exposes previously ignored failures across launch/install scripts; archive ordering must preserve both simulation and payload on failure; UI response guards must preserve refresh and editing. Test normal and injected failure paths.

Sensitive data handling plan: use synthetic credentials and simulation names only; inspect service metadata selectively, never dump environments or Docker inspect secrets. Preserve existing user services. Local validation maximum five vnodes.

Implementation plan:
1. Correct every affected run helper; archive successfully before removing container/payload; redact Docker claim arguments; make telemetry directory explicit.
2. Require teardown name, enforce policy inside create slot, fail closed on malformed policy, bind UI editor and responses to simulation, correct advance route/error handling.
3. Add isolated shell and UI regression tests, run Rust gates and live container smoke checks.
4. Update runtime spec, operations docs and regression evidence; complete final review and commit together.

Validation plan: shell failure injection with fake Docker and temporary files, synthetic-secret output capture, UI functions exercised with fake fetch, Rust unit tests and clippy/fmt, real small-container telemetry lifecycle where Docker is available. Same-failure search across scripts and per-sim mutations.

Artifact impact: AGENTS.md workflow unchanged; live-validation skill gains command-failure lesson if confirmed; runtime spec and operating docs describe fail-closed lifecycle; no exported operator skill exists. Preserve historical evidence but distinguish it from current validation. Remaining review programme tracked by pending SOW-0028 and SOW-0029, and existing platform SOW-0018.

Open decisions: none for the repairs above; user approved the review's recommended corrections. New product forks will be presented before their implementation.

Previous validation missed this because tests exercised component helpers but not shell failure status, standalone telemetry, UI route compatibility or concurrent policy checks. Current validation remains outstanding; no completion claim is made.

### Regression validation and scope mapping

- Acceptance: seven helpers preserve nonzero status; injected archive failure leaves container and payload; failed Docker removal preserves payload; successful teardown archives first. Synthetic claim tokens/rooms are absent from captured arguments/output.
- Console: HTTP tests refuse missing/empty teardown names (422), inactive clock advance returns 400, malformed policy refuses create and marks health without paths. UI tests exercise named clock route, HTTP error reporting and cross-simulation label rejection. Owned-slot test proves background work retains serialization after caller handle is dropped.
- Live use: disposable one-vnode container created without a Cloud claim; actual Netdata API returned the test vnode and 108 charts including system.cpu. Standalone telemetry stop/start returned success with journal and exporter status. Teardown archived successfully before removing only that test container and payload. Existing services were untouched. Runtime binary/image was unchanged by this repair; the current lifecycle scripts were exercised against the installed image.
- Gates: 270 Rust tests, Clippy and formatting; eight isolated Python shell/API tests and four Node UI tests. Final source review checked every changed control flow and similar run helpers; found and corrected lost mutation errors, unreadable-policy fallback, inventory failure masking and disconnected-create slot lifetime.
- Sensitive-data gate: only synthetic credentials and node identities used. No real tokens or Docker environment dumps retained. Historic incidental personal references in this SOW were sanitized.
- Artifact maintenance: runtime spec and operating docs updated; live-validation skill records failure-injection tests. AGENTS.md unchanged because ownership/workflow did not change. No exported operator skill is affected.
- Historical validation fields above are incomplete original records, not newly verified claims. This regression section records current evidence without inventing the missing historic evidence. Broader historical closure reconciliation and every Tier 3 item from line 40 are assigned to pending SOW-0029; fidelity/indexing, lint staleness and replay are assigned to pending SOW-0028. Platform acceptance remains SOW-0018.
- Remaining review recommendations are implemented here (lifecycle, credentials, telemetry, UI routes/state, budgets), or tracked in SOW-0028, SOW-0029 and SOW-0018. No claim of complete programme or expert-grade fidelity is made.

Regression outcome: completed. Status and done-directory agree; this record and repair are committed together. Remaining programme is not claimed complete.
