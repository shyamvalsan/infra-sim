# SOW-0028 - Fidelity, readiness and replay

## Status

Status: completed

Sub-state: delivered 2026-09-23 with explicit evidence boundaries; reference fidelity and all external acceptance moved to SOW-0032, pre-existing application narrative mismatches to SOW-0031 (user decisions 3.B and 4.B).

## Requirements

### Purpose

Scenario-active fidelity/recovery, additive fault logs, indexed lint lookups, operational versus verified readiness, versioned self-contained replay and control history, recorded hero data and statistical checks.

### User Request

Implement all recommendations from the repository review (2026-09-09).

### Acceptance Criteria

Deliver the scope with automated checks and honest evidence boundaries. Runtime changes require live Netdata validation, capped at five local vnodes. External blind SRE and actual Mac acceptance cannot be self-certified; large fleets require a separate host.

## Analysis

The review found baseline-only fidelity, incomplete archives, misleading readiness wording and incomplete release evidence. Sources: spec.md:106,119; fidelity.rs:101,264; preflight.rs:341; sim-docker.sh:424; SOW-0027 historical deferred list.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

The lint never supplies active scenarios and searches chart metadata repeatedly per sample. Log rules evaluate multipliers rather than generated values, so zero-baseline additive faults are absent from logs. Readiness conflates no hard failures with verified demo readiness. Archives omit generator snapshots, process timing and interactive control history. RNG/walk state is sequential: exact replay must preserve each producer's observed tick/scrape times and control sequence, including restarts, rather than assume seed alone is sufficient.

Evidence reviewed:

spec.md product requirements; generator-and-engine and runtime-and-scenarios current specs; fidelity.rs:76,101,264; lib.rs NodeEngine.tick/value/signal_values; logs.rs Trigger/LogGenerator; control_file.rs load/save/prune; plugin main.rs composition and mode dispatch; exporters.rs scrape-driven integration; otlp_runtime.rs nanosecond timestamps; sim-docker.sh teardown; preflight.rs and its tests. User chose recommended A/A designs by authorizing go after the alternatives were presented.

Affected contracts and surfaces:

Generator engine inspection API, composed-spec reuse by logs, scenario fidelity CLI, readiness JSON/UI, control history and archive/replay CLI, exporter/OTLP timing metadata, tests and operator docs. Existing unrecorded archives remain readable as definitions but must not be called complete recordings.

Existing patterns to reuse:

NodeEngine and planned charts; shared ScenarioSet; per-service spec loader; separate telemetry processes; semantic violation enums; JSON/YAML serde; versioned environment contracts; disposable one-node live smoke and isolated API tests.

Risk and blast radius:

Extra evaluation in logs consumes CPU; thresholds must reflect actual modeled quantities and not fabricated alert outcomes. Scenario checks need finite horizons and must not treat legitimate incident changes as baseline flatness failures. Replay must preserve actual clocks and sequence without pretending Netdata ML/AI outcomes are deterministic. Archive copying must stay fail-closed; raw secrets and external service state are excluded. Compatibility with older definitions is explicit.

Sensitive data handling plan:

Use only synthetic workloads, generated identities and public collector schemas. Never record credentials, claim configuration, API request headers or Docker environment dumps. Reference recordings come from disposable synthetic services, not customer systems. Store evidence as test output with no private endpoints.

Implementation plan:

1. Pre-index lint chart metadata; add scenario-aware semantic checks with recovery and unaffected-target tests, preserving baseline behavior.
2. Reuse composed NodeEngine models for log rules; evaluate actual scoped values/capacity thresholds, including additive faults, and validate through live journal/Netdata.
3. Separate operational readiness from verified demo readiness; report manual evidence and stale lint honestly.
4. Add versioned self-contained archives and append-only observed control/process timing history for full raw-telemetry replay, including exporter scrape timing and OTLP nanoseconds. Verify reconstructed raw outputs against recordings, never downstream verdicts.
5. Build recorded-reference/statistical comparison tooling for a small hero stack and obtain external blind SRE review. Actual external acceptance requires the requested resources; tooling and internal probes proceed meanwhile.
6. Run Rust/shell/UI gates, live validation and targeted benchmarks; update specs/docs/skills and record actual evidence boundaries.

Validation plan:

Regression tests for scenario-active impossible values, unaffected nodes and recovery; old versus indexed lint equivalence and timings; log/metric incident probes on at most five local vnodes; archive replay byte/value equality across controls and process restarts; malformed archive refusal; reference-data statistics and explicit external review results. No fidelity acceptance without live/reference evidence.

Artifact impact plan:

Current-reality generator/runtime specs, operating docs, live-validation skill, console help and SOW evidence updated. AGENTS.md retains synthetic-world/live-product rule. No existing exported operator skill is affected. Full project acceptance is not implied by this SOW until its criteria actually pass.

Open-source reference evidence:

No additional external source is needed for the internal state/control-flow defects. Any agent interpretation or real workload configuration will be probed and its source recorded before relying on it.

Open decisions:

Recommended full raw-telemetry replay and shared modeled-signal log design approved. External test host, actual Mac and two blind SRE reviewers requested but not supplied; no claim to those results is authorized by elapsed time.

## Implications And Decisions

User approved the full long-term-best review recommendation. Each change remains minimal-complete; unresolved product choices require a concrete proposal before code.

Readiness implementation detail: expose `operational_ready` for absence of hard failures and make existing `demo_ready` require every required check to pass. The shipped UI distinguishes operational from verified readiness. This intentionally corrects the existing false-positive API meaning; missing human evidence cannot be auto-approved. No attestation system is implied. Per-simulation lint freshness remains part of this scope.

Lint-evidence investigation: preserve ordinary `--lint` as read-only; an explicit evidence-output option can record versioned results against the final installed payload and actual runtime binary. Docker currently copies inputs after the host lint and does not lint its installed image/payload. Evidence must therefore be generated against that image and final payload, validated per simulation, invalidated by input changes and retained with archives. A host binary hash alone is insufficient. Shared manifest/evidence helpers belong alongside environment resolution in the engine; runtime-binary verification must account for the container boundary. Implemented by the opt-in evidence path described below.

Decision, 2026-09-10: user replied ok to the recommended option A for explicit connection-capacity modeling, including the service limits needed to justify socket-related messages. Interpretation stated to the user before further implementation. Do not equate a global TCP safety ceiling with a service connection limit.

## Plan

1. Finish preceding SOW.
2. Investigate this scope and fill the implementation gate.
3. Implement, validate and review; identify external acceptance dependencies explicitly.

## Validation

Earlier intermediate results are kept in the dated sections below; this is the
close-out record for the exact committed tree.

Acceptance criteria evidence:

- Scenario-active fidelity and recovery: indexed semantic checks with targeted,
  unaffected and recovery tests; baseline lint output byte-identical to HEAD
  60a0c92 on seven templates up to 25 nodes; application lint adds direction,
  isolation and recovery on published values (decision 3.B).
- Additive fault logs from composed values: two-node live probe (2026-09-14).
- Operational versus verified readiness: backend and UI tests; a create with
  `lint_hours: 0` is now honestly unverified (review decision 1.A).
- Versioned self-contained recording and replay: final-tree live chain below.
- Recorded hero data, statistics and external acceptance: not delivered here;
  moved to SOW-0032 by user decision 4.B.

Tests or equivalent validation (final tree): `cargo test` 308 passed (41
console, 187 engine, 49 plugin, 31 spec); Clippy with warnings denied, `cargo fmt
--check`, `git diff --check` and `cargo +1.88.0 check --locked --workspace
--all-targets` clean; Python 19 passed; UI 5 passed. Recording tests also pass
10/10 with temporary files on disk. Release lint passes on web-stack,
k8s-microservices, otel-fleet, initech, robotics-edge and acme; the reviewer
additionally passed default, multi-db-stack, pg-ha-fleet, webinar-fleet and
showcase.

Real-use evidence (final tree, fresh static build and `infra-sim:latest` on
nightly v2.10.0-1044, state on disk): `tests/live_agent_smoke.py` passed create
with the in-image lint, telemetry stop/start, container restart, all producer
boundaries and a finalized gap-free archive; `tests/live_replay_probe.py` on
that archive with its own executable passed vnode registration, CPU samples,
4/4 exporter responses byte-identical, journal 5/5 entries identical, OTLP
1135/1135 log records stored once, 70 trace requests acknowledged and stored.
No test containers remained.

Reviewer findings: independent final review recorded below (FAIL on three P2s,
all adjudicated: two fixed, one resolved by user decision 1.A and implemented;
P3s fixed or recorded). Earlier implementation reviews are in the dated
sections.

Same-failure scan: every test temporary directory helper in the workspace
(`recording.rs`, `lint_evidence.rs`, `test_recording_runtime.py`) now cleans up
after passing; every `Signal::record` failure path classifies timeouts; the
selector check shared by lint and the new template test is one function; every
shipped template x scenario pair is covered by
`shipped_templates_never_reject_a_shipped_scenario_step` (mutation proved it
catches the k8s-microservices regression).

Sensitive data gate: the full diff and every new file (7,938 lines) were scanned
for personal names, email addresses, API keys, bearer and claim tokens,
workstation paths and non-documentation public IPv4 addresses: none. Evidence
uses synthetic hostnames and redacted summaries only.

Artifact maintenance gate:

- AGENTS.md: no update; the synthetic-world rule, SOW rules and project commands
  are unchanged, and no new project-wide guardrail arose.
- Runtime project skills: `.agents/skills/project-live-validation/SKILL.md`
  updated (lock-free finalized status, disk versus tmpfs testing, replay probe,
  inspector limit).
- Specs: `.agents/sow/specs/runtime-and-scenarios.md` updated (batched capture,
  inventory-verified replay, narrative lint, shared step applicability, one
  create lint).
- End-user/operator docs: `docs/operating.md` updated (recording, replay
  verification, metrics replay rerun warning, replay probe, `--lint-hours`).
- End-user/operator skills: none exist in this repository.
- SOW lifecycle: completed and moved to `done/` in the same commit as the work;
  SOW-0031 and SOW-0032 created in `pending/`; the startsim-vm token issue is
  recorded in paused SOW-0018; SOW-0030 unchanged and pending; `TODO.md` removed
  after mapping (decision 6.A).

Specs update: `runtime-and-scenarios.md` as above.

Project skills update: `project-live-validation` as above.

End-user/operator docs update: `docs/operating.md` as above.

End-user/operator skills update: none exist.

Lessons: see Lessons Extracted.

Follow-up mapping: application narrative magnitudes: SOW-0031. Reference
capture, statistics, blind SRE review, authenticated log-function queries,
separate scale host (including 3,000-node create lint time): SOW-0032. Historic
hardening items: SOW-0030. Mac acceptance and the startsim-vm token issue:
SOW-0018. Unproven review theories are recorded as risks in the review section,
not as tracked defects.

## Additional investigation - 2026-09-10

- PostgreSQL already defines `pg_connections_max=400` (`specs/postgres.yaml:22`) and a connection partition totaling 400. Reuse this capacity instead of inventing a global TCP limit for PostgreSQL messages. Nginx requires its own modeled service capacity.
- `postgres.connections_utilization` directly publishes `pg_connections_used` with percentage units (`specs/postgres.yaml:145`), which needs emitted/live verification and correction within fidelity work.
- `GeneratorSpec::merge` retains only the first same-name role (`crates/sim-spec/src/lib.rs:145`); a Linux db role can suppress the PostgreSQL db signal patches. This is a concrete composition defect requiring targeted regression verification and original-SOW ownership reconciliation before editing. Service composition was delivered under SOW-0001 and overlay work under SOW-0008. Do not silently bundle a confirmed original-outcome regression into this new SOW.
- Delegated replay investigation failed with workspace credit exhaustion before delivering a plan. No replay findings or validation are claimed from that attempt. Completed earlier delegated findings remain available and were checked locally.

Sensitive data gate:

Intermediate review: changes use synthetic test values and code references; no claim credentials or customer inputs were recorded. Repeat the final artifact scan before completing this SOW.

## Outcome

Delivered: scenario-aware indexed fidelity lint with application direction,
isolation and recovery checks; additive fault logs from composed values;
operational versus verified readiness with per-simulation lint evidence; a
versioned raw recording of all four producer boundaries with non-blocking
batched capture, finalization and hash-verified archives; and replay that a real
Netdata receiver accepts end to end. Not delivered here, by user decision:
reference fidelity statistics and external acceptance (SOW-0032) and the
remaining application narrative magnitudes (SOW-0031).

## Lessons Extracted

- Do not equate baseline unit tests with release or fidelity acceptance.
- Test storage code on the storage it runs on: fsync is about 190 times slower
  on disk than on tmpfs here, and two concurrency tests only failed on disk.
- Two checks that classify the same thing must share one classifier; the
  scenario and application lints disagreed about the same step.
- Count against the tool's real output: an inspector's default record limit made
  a lossless replay look lossy.
- A new check needs a proven failing case before its pass means anything.

## Followup

This file tracks the review recommendations above; SOW-0030 additionally owns every historic Tier 3 item from SOW-0027 except fidelity/indexing/lint-readiness/replay, which belong to SOW-0028.


## Log-model implementation detail - 2026-09-11

Approved shared-model and explicit service-capacity design is implemented by moving the logs dispatch after the existing composed engine construction, passing the ranked NodeEngine into each LogGenerator, and inspecting the values resolved by its full tick. An inspection fallback is permitted only for declared fixed constants, such as PostgreSQL's 400-slot limit; unused dynamic signals must not be invented or evaluated on a different schedule.

Rules use actual quantities: scoped disk usage >=90% of mount capacity, write latency >=20ms, positive OOM rate, available RAM <=10% of node capacity, swap out >=100KiB/s, network errors >=10/s, drops >=30/s, retransmits >=20/s and CPU >=80%. These are authored synthetic warning thresholds, not assertions about Netdata health verdicts or universal hardware limits. Messages report observed modeled readings rather than random checkpoint durations, killed-process memory sizes, link-state changes or database errors inferred from global socket counts.

Connection warnings use service occupancy: PostgreSQL's existing used count / 400 slots; nginx active connections / an explicit authored 8192-slot limit. Nginx's active-count physical bound matches the new limit. PostgreSQL's physical count bound becomes 400, and the connection-exhaustion scenario adds load to reach this capacity regardless of diurnal baseline, rather than promising exhaustion from a multiplier that often never reaches it. These implement the approved capacity design and repair its actual scenario consequences. No new collector chart identity is introduced for capacity metadata.

Capacity-dependent rules remain quiet when capacity is missing/invalid. Tests cover additive effects from zero, scoped instance weights/capacities, node-index targeting, quiet unaffected nodes, same-seed/clock output and valid service attribution. The logs process remains separate and its random history is process-local; do not claim independently started processes have identical instantaneous noisy values. Warm-up scenario selection must follow the metrics path, and profile reloads must not leave labels stale.

Replay decision, 2026-09-12: user selected A after the recommendation explicitly combined exact raw-output recording with a configurable 1 GiB per-simulation cap. Interpretation was stated before implementation. Capture each producer boundary (plugins.d, Journal Export Format, serialized OTLP requests, exporter responses), with observed clocks/control/lifecycle metadata; replay these records instead of regenerating hidden RNG histories. At the cap, preserve the recorded prefix and mark recording incomplete, without silently evicting earlier data. Downstream Netdata states remain live and are never recorded as scripted verdicts.


## Recording implementation details - 2026-09-14

Use a versioned length-framed raw recording shared by the four producer processes. A short exclusive advisory file lock serializes appends and enforces one aggregate cap, rather than four independent caps. Use fs2 0.4.3's safe FileExt API (official docs: https://docs.rs/fs2/0.4.3/fs2/trait.FileExt.html); each append opens its own lock handle and releases it on drop. Lock acquisition is bounded so a stopped writer cannot indefinitely stall live telemetry. Record errors or exhausted space stop capture, preserve the prefix and create an incomplete marker; normal telemetry remains independent of recording success.

Frames identify producer, process session, boundary kind, target and observed nanosecond clock. Payload bytes remain unmodified. Reader length/version checks reject malformed/truncated recordings before replay. Capture transports at their raw producer boundaries, not Netdata acknowledgements or derived ML/alert states. An active recording is never labeled a finalized complete archive. Ordinary standalone invocations remain read-only unless recording is explicitly enabled; managed simulations enable recording in their private payload directory.

Live log validation result: two-node probe verified targeted OOM rate 1/s, untargeted OOM rate 0/s, actual journal messages for additive OOM and 40 interface errors/s, quiet untargeted node and kernel PID 0. Both probes used a fresh image and cleaned up the exact test container. The current agent's systemd-journal query endpoint returned HTTP 412 for required Cloud SSO even on container loopback. Thus agent-visible log-query acceptance is blocked on authenticated access, not claimed passed. Source corroboration: netdata/netdata @ 8a49bad67544, src/nrpc/nrpc-calls.c:549 and src/libnetdata/facets/logs_query_status.h:19. Earlier claims that only OTLP log queries require SSO are no longer reliable on this tested nightly.


Recording lifecycle refinement, 2026-09-14: the first independent implementation review found that a restart could append after a torn frame. Add a durably replaced committed-byte checkpoint; retain any interrupted suffix untouched, replay only the committed prefix, and report the recording incomplete. Finalization validates the stream and reports sessions without clean stops. Managed teardown must quiesce owned producer processes before finalizing and copying the archive. Use signal-hook's safe flag registration for SIGTERM/SIGINT so producer loops can flush and end their recording sessions instead of making every intentional stop appear to be a crash. This preserves the already-approved raw boundary design; no downstream states are recorded.


## Implementation evidence - 2026-09-15 (in progress)

- Raw recording now uses durable committed offsets, one shared append lock and
  one byte cap. Restart tests cover torn suffixes, concurrent startup during
  appends, and refusal to reset missing/emptied existing data.
- All four producer boundaries and observed typed control transitions are wired.
  OTLP replay uses a raw tonic codec, preserving protobuf bytes without decoding
  and reconstructing requests. Incomplete recordings require explicit prefix
  replay. Shared replay start timestamps are supported.
- Three real-process regression tests passed: metrics byte equality after clean
  SIGTERM, exporter route isolation with reversed scrape order, and refusal of
  an interrupted session without explicit prefix selection.
- The live one-node lifecycle probe collected real Netdata samples and recorded
  all raw boundary kinds. Its stronger archive check exposed missing metrics
  stop records across container shutdown; the archive correctly remained
  incomplete. Shutdown identification/ordering is under live re-validation.
- Managed teardown now stops before copying, includes specs/control/executable/
  recording, finalizes the copied recording with the archived executable and
  current helper image, and writes SHA-256 inventory. Copy/finalization failures retain
  the stopped source and payload. Six shell lifecycle tests passed.
- Final acceptance remains open: authenticated logs function access, end-to-end
  replay ingestion, per-simulation lint freshness, application-path lint coverage,
  reference/statistical tooling, and independent external acceptance still need
  the work/evidence described above. No final readiness or full fidelity claim.


Lint evidence implementation details, 2026-09-15: write an opt-in versioned JSON
record tied to SHA-256 hashes of the resolved environment, generator/spec tree,
scenario tree and executed binary. Hash logical relative input identities so a
copied payload remains comparable across host/container mount paths. Check inputs
before and after lint to reject edits during validation. For containers also bind
the evidence to the immutable Docker image ID used for both lint and startup;
console status compares that identity with the selected container's current image.
Missing, malformed or stale evidence is unverified, never a pass. Preserve legacy
local CLI flags only as explicit local compatibility, not cross-simulation proof.
Use the maintained sha2 implementation rather than inventing a checksum or
requiring platform-specific hash commands. Archive the evidence with its inputs.

## Closeout investigation - 2026-09-22

The application runtime currently constructs unranked engines in both OTLP and
exporters, so authored node-index selectors never match. Reuse ranked application
engine construction, preserving the existing one-based selector contract, and
cover targeted, unaffected and recovered nodes. Scenario lint must clone pristine
engines before baseline warm-up so its result does not depend on baseline duration.

Application fidelity must inspect actual application signals and their published
relationships, not only the three convenience contexts in prometheus-app.yaml.
Exporter worker/pool occupancy currently exceeds declared capacity when demand
rises, and its independent latency quantiles can cross. Apply the already-approved
explicit-capacity/physical-invariant contract to these application surfaces: share
normalization between exporter and OTLP, preserve raw modeled demand internally,
cap published occupancy to modeled capacity, and enforce ordered latency quantiles
using the existing OTLP policy. Validate the actual rendered output and counter
monotonicity during baseline, scenarios and recovery. This is a consequence of the
approved fidelity contract, not a new absolute threshold policy.

Live lifecycle evidence: the one-node probe passed restart, all producer-boundary
capture and gap-free archive finalization after explicitly triggering the authored
OOM scenario. Healthy modeled nodes may correctly emit no warning logs; the probe
now triggers that scenario itself and is being rerun without intervention. Its
fixtures also now declare inode capacity required by the shipped Linux spec.

Local archive consequence: teardown currently deletes the executable before its
archive step and copies only definitions from the repository. Stage and checksum
the installed definition, control, recursive specs/scenarios and executable before
any destructive teardown. Publish the archive atomically, fail closed on staging
errors, then perform the existing stop/removal sequence. Local installs do not yet
enable raw recording; identify their archive as definitions, never exact output.

Managed recording completeness must include the configured producer set. The
entrypoint always supervises metrics, journal and OTLP, and supervises exporters
unless create disables them. Persist that expectation in the recording manifest
and require every expected producer to have a clean session at finalization.
Require sessions, not nonempty warning logs: a healthy node can legitimately be
silent. Standalone opt-in recordings retain their selected-producer behavior;
older manifests remain readable without inventing historical expectations.

## Handoff - 2026-09-23

The user requested stopping implementation and documenting remaining work for
another model. `TODO.md` contains the working-tree handoff, verified intermediate
results, unresolved acceptance and subsequent SOW ownership. Status remains
in-progress, not completed; no commit or push has been made.

The draft live replay probe failed to register a vnode: the metrics replay plugin
reported Permission denied before any collection. A likely cause is the recording
status path opening the append lock for writing while the copied archive is
mounted read-only and owned by root; this is a working theory, not a verified fix.
The exact owned probe was interrupted for handoff, and its cleanup stopped/removed
its disposable receiver. Full end-to-end replay acceptance remains outstanding.
Latest intermediate Rust suite passed 299 tests; Python passed 13 and UI passed 5.
Fresh-image lifecycle/capture/archive validation passed; these do not establish
reference fidelity, authenticated log queries or external SRE acceptance.

## Replay acceptance and closeout findings - 2026-09-23

Replay permission failure, verified root cause: `recording::status` always took
the writer lock, opening `append.lock` read-write, so a root-owned 0644 archive
failed for Netdata's UID with EACCES before any output. Reproduced with
`--recording-status` as a non-root user on the retained copy. Fix: a finalized
recording is immutable (finalization is the last write under the lock and every
later append and producer open is refused), so status reads it without the lock.
Unfinalized recordings still lock; the error now names the recording path.

Forged-marker gap: replay trusted the `finalized`/`incomplete` marker files and
checked framing only; session completeness was derived solely inside
`finalize`. The analysis is now one read-only `recording::verify` used by both,
so replay re-derives expected-producer and clean-session completeness from the
committed stream. A hand-made finalized marker over an open session is refused
unless `--allow-incomplete-recording` is given.

Disk-exhaustion finding (real, not induced): a live run whose state lived on a
full 4 GiB tmpfs stopped at a page-aligned `events.bin`, preserved the committed
prefix and produced an honest incomplete archive, but the `incomplete` marker
was empty because its reason write also failed. `status` then reported
`"incomplete": ""`, which JSON/Python consumers read as complete. An empty marker
now reports an explicit reason. Recording unit tests now remove their temporary
directories; they previously leaked one directory per test into `/tmp`.

OTLP replay connected eagerly and aborted with `transport error` when started
before the receiver listened. It now uses a lazy channel and retries
`Unavailable` for at most 60 s only until the first acknowledgement; afterwards
any failure is fatal so delivered requests are never duplicated.

`tests/live_replay_probe.py` strengthened: producer failures are reported at
once with their stderr; cleanup keys on container existence, not a success flag;
journal replay compares exact `(__REALTIME_TIMESTAMP, MESSAGE)` multisets per
file parsed from the recorded export stream (binary fields included); OTLP
compares per-(namespace, service) LogRecord counts decoded from the recorded
protobuf with the nightly inspector's stored records (exact equality; the
inspector's default `--limit 50` had masked this); exporter responses are
byte-compared; `--plugin` allows replaying an older archive with a newer build.
go.d is deliberately not pointed at the replay port, so collector ingestion of
replayed exporter bodies is not asserted.

Live evidence (fresh static build, fresh `infra-sim:latest`, nightly
v2.10.0-1044): `live_agent_smoke.py` passed all lifecycle checks and produced a
finalized gap-free archive; `live_replay_probe.py` on that archive, using its own
archived executable from a read-only root-owned copy, passed: vnode registered,
numeric CPU samples stored, 6/6 exporter responses byte-identical, all four
producers exited 0, journal 1/1 entries identical, OTLP 981/981 log records
stored once, 92 trace requests acknowledged without rejection and stored. The
recording held a single journal entry, so journal replay evidence is thin.
No test containers remained afterwards.

Archive tests: new `tests/test_archive_manifest.py` covers refusal of an
unfinalized recording, a missing required artifact and symlinks, plus nested
hashes. `archive.json` hashes are an unsigned integrity inventory. (Superseded:
replay now verifies them, decision 5.A below.)

Indexed lint equivalence, bounded local measurement (16 cores, `--lint 1`, HEAD
60a0c92 built from `git archive` versus the working tree): baseline sections are
byte-identical on web-stack, k8s-microservices, otel-fleet, initech,
robotics-edge (5 nodes), acme (10) and multi-db-stack (25). Total wall time
rises because per-scenario windows were added: 5-node 1.5-6.1 s to 6.2-24.6 s,
acme 2.9 s to 27.8 s, multi-db-stack 28.8 s to 41.6 s. No larger fleet was run
here.

Regression found in this SOW's own uncommitted work: application lint
(`crates/sim-plugin/src/main.rs::lint_applications`) rejects the committed
`environments/k8s-microservices.yaml` with `application scenario
db-connection-exhaustion: target 'app_db_pool_wait_rate' matches no application
node`, after the scenario lint in the same run reported that step as skipped
(no web node; `web` is not in `requires_roles: [db]`). CI lints no templates.

Application narrative review (engine-level probe, not lint or live evidence):
capacity caps never flatten a shipped scenario and all app incidents recover and
leave untargeted nodes unchanged, but checkout-degradation p99 is raised onto
p95 for about 70% of the incident by the ordering floor; worker-saturation and
queue-backlog peak near 23-26 of 32 workers despite saturation narratives, so
the OTLP saturation log never fires; db-connection-exhaustion shows pool waits
with about 67% of the app pool idle; cache-stampede lookups shrink while request
rate stays flat. At the time, application lint asserted physical validity only.
(Superseded: decision 3.B added direction, recovery and isolation; magnitude
remains unasserted, SOW-0031.)

Gates on this tree: 301 Rust tests passed, Clippy with warnings denied and
formatting clean (before the two latest recording fixes; recording tests 11/11
after), Python 17 passed, UI 5 passed.

## Decisions - 2026-09-23

User replied `1.B 2.C 3.B 4.B 5.A 6.A` to the options presented with evidence:

1. B: compute scenario-step applicability once and share it between the
   scenario lint and the application lint, so a step skipped by one can never
   be rejected by the other.
2. C: recording frames are handed to a per-producer background writer that
   appends in batches under one lock and one durable commit. Telemetry output
   never waits on storage. A crash can lose at most the last unwritten batch;
   such a recording already lacks a clean stop and is reported incomplete.
3. B: SOW-0028 fixes the p99-onto-p95 flattening its ordering floor causes and
   adds direction, recovery and untargeted-isolation assertions to application
   lint. Pre-existing narrative mismatches (worker-saturation and queue-backlog
   never saturating, db-connection-exhaustion waits with an idle pool,
   cache-stampede lookups shrinking at flat request rate) move to a new SOW.
4. B: real-service reference capture, statistical tooling and all external
   acceptance (blind SRE review, authenticated log-function queries, separate
   scale host) move to a new SOW; SOW-0028 closes with explicit boundaries.
5. A: replay verifies the recording's SHA-256 entries against `archive.json`
   when present and refuses a mismatch (integrity, not forgery protection).
6. A: remove the untracked `TODO.md` at close once every item is mapped.

## Decision implementation evidence - 2026-09-23

1.B: `step_fit` in `crates/sim-plugin/src/main.rs` decides role, node-index,
label and instance applicability once; `check_scenarios`, `lint_applications`
and `application_targets` all use it. The scenario-lint report is byte-identical
to the pre-refactor build on five templates; `k8s-microservices` now passes.
Unit test `an_absent_optional_role_is_skipped_by_both_lints`.

2.C: `Recorder::capture` only queues; a per-producer `infra-sim-recorder` thread
group-commits batches (at most 8 MiB) under the shared lock with one
`fdatasync` and one committed checkpoint. Frames keep their capture time. Start
is still written synchronously; Drop queues Stop and joins the writer. Pending
memory above 64 MiB marks the recording incomplete. The lock wait bound is 5 s,
now reached only by writer threads, startup, status and finalization. Test
`capture_never_waits_for_storage` (200 captures return in under 100 ms while
another holder has the lock; all 200 persist). Recording tests pass 10/10 on
disk and 10/10 on tmpfs; previously two failed on disk.

3.B: checkout-degradation and worker-saturation now ramp p99 with the same
multiplier, start and duration as p95, preserving the baseline tail ratio.
Shipped-file test `shipped_latency_incidents_keep_the_tail_above_p95` fails on
the committed scenario (p99 on p95 for 852 of 1380 incident samples) and passes
with the fix (at most 1%). `exporters::check_narrative` adds direction (>1% of
baseline), untargeted-node isolation (exact) and exact recovery; the ranked-node
test proves a wrong direction and a change on an untargeted node are refused.
All applicable scenarios pass it on web-stack, k8s-microservices, otel-fleet,
initech, robotics-edge and acme. Remaining narrative mismatches: SOW-0031.

4.B: SOW-0032 created for reference capture, statistics and all external
acceptance.

5.A: `recording::verify_archive_inventory` runs before replay: every recording
file must match `archive.json`, with none added or missing; standalone
recordings without an inventory replay as before. Unit test covers a flipped
byte, an added file and a missing file; the live replay below passed it on a real
teardown archive.

Also: `tests/test_recording_runtime.py` proves an exporter process on a fleet with
no application-tier node records exactly one clean session (the watchdog edge
case in the handoff). `live_agent_smoke.py` now waits for at least five journal
frames.

Final-tree live evidence (fresh static build and image): smoke passed every
lifecycle check with a finalized gap-free archive; replay of that archive with its
own executable passed: vnode and CPU samples, 6/6 exporter responses identical,
journal 5/5 entries identical, OTLP 1406/1406 log records stored once, 94 trace
requests acknowledged and stored, all producers exited 0. Simulation state and
temporary files were on disk (`TMPDIR` under `target/`).

## Independent final review - 2026-09-23

A fresh read-only reviewer (not the implementer) ran the final-review mandate on
the working tree: its own builds, 306 Rust tests, Clippy, fmt, 18 Python tests,
5 UI tests, release lint on ten templates plus showcase at `--lint 2`, and an
end-to-end record/finalize/manifest/replay round trip including tamper refusal.
Verdict FAIL on three P2 findings; each was checked against the code:

- P2-1 confirmed and fixed: a console local install writes no `control.yaml`
  (`provision::install`), but `archive_local` copied it unconditionally, so
  teardown stopped before any change, a regression from HEAD. A missing control
  file is now archived as `active: []`, the state it means; the archive test
  covers an install without one.
- P2-2 confirmed: stale Outcome/Validation text and two superseded statements.
  The superseded statements are corrected in place; the close sections are
  rewritten at close.
- P2-3 confirmed, needs a user decision: SOW-0028 added a mandatory in-image
  `--lint 2 --lint-evidence` to every Docker create (`scripts/sim-docker.sh`),
  on top of the console's host lint, and it ignores `lint_hours: 0` while the
  console reports lint skipped. Reviewer's approximate showcase (30 node)
  `--lint 2` timing: 73 s at HEAD, 150 s now. No 3,000-node measurement exists.
- P3 fixed: the console status handler reads recording status in
  `spawn_blocking`; `lint_evidence` and `test_recording_runtime.py` no longer
  leak temporary directories after passing tests; a request timeout no longer
  counts toward permanently abandoning an OTLP signal (the 5 s timeout is new in
  this SOW), with a test.
- P3 open, decision: CI lints no environment templates, which is how the
  application-lint regression went unnoticed.
- Unproven, recorded as risks: an `Unavailable` response after the receiver
  processed the first OTLP replay request could duplicate it (retry applies only
  before the first acknowledgement); a custom scenario pinning an `instance:` on
  an application signal passes step applicability and is then rejected by the
  application lint (no shipped scenario does this, and such a step would do
  nothing). Metrics replay rerun by Netdata now carries an operator warning in
  `docs/operating.md`. Lock starvation was not reproducible with the batched
  writer (a new producer opened within 0.9 s under three busy writers).

Decisions on the review findings, user reply `1.A 2.B` (2026-09-23):

1. A: Docker creates skip the console's host lint; the in-image evidence lint is
   the only lint and honors `lint_hours` (0 skips it, leaving the simulation
   unverified). Local installs keep the host lint.
2. B: a fast Rust test checks every shipped template against every shipped
   scenario: each step either applies or is skipped, never rejected.
