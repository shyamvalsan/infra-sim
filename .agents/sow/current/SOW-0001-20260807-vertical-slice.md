# SOW-0001 - Infra-Sim vertical slice: plugins.d runtime, Linux baseline generator, web-stack template

## Status

Status: in-progress

Sub-state: Implementation plan items 1-7 delivered and validated against a live agent. Item 8 (thin console) not started; correlated logs were never in this SOW's plan and need their own. OTEL sim scoping tracked in `SOW-0002` (pending).

## Requirements

### Purpose

Build the first end-to-end vertical slice of Infra-Sim: a Rust external plugin that executes declarative generator specs, emits a dashboard-complete simulated Linux fleet as Netdata virtual nodes, and can have a scenario injected into it live — with the real Netdata ML/health pipeline processing it untouched.

This slice exists to make every interface real and to expose integration risk early, per user decision 1A.

### User Request

"Build this spec" (`spec.md`), followed by design decisions:

- 1A — vertical slice first (one template, dashboard-complete Linux baseline, ~5 vnodes, 1 scenario, thin console)
- 2B — dashboard-complete Linux baseline (~60–80 contexts), not full 188-context parity
- 3C — Rust runtime
- 4A — per-host journal files in `/var/log/journal/remote/` for log attribution
- 5A — initialize git + SOW before building
- 6 — cap local verification at 5 vnodes; scale testing runs later on another machine
- "also don't forget OTEL sim" — new scope, not present in `spec.md`

### Assistant Understanding

Facts (verified against a live agent, not inferred):

- The plugins.d protocol exposes vnode creation to **external** plugins. `HOST`, `HOST_DEFINE`, `HOST_DEFINE_END`, `HOST_LABEL` carry the `PARSER_INIT_PLUGINSD` flag and `HOST_DEFINE_END` calls `rrdhost_find_or_create(..., NETDATA_VIRTUAL_HOST, ...)`.
- A plain Python external plugin in `/etc/netdata/custom-plugins.d/` registered three vnodes on a live agent, each queryable at `/host/<hostname>/api/v1/...` with correctly scoped data.
- Netdata's **real** ML engine trains per vnode with no special casing: `anomaly_detection.ml_running = 1`, `netdata.training_status = 12.6 untrained / 10.4 trained` on a simulated node. The hard rule is satisfied by construction.
- A vnode carries **only** the charts its plugin emits, plus 8 ML charts Netdata adds itself. There is no automatic node-level dashboard section; dashboard menus are built from the contexts present on the node.
- The same agent's real Linux node carries **188 OS-baseline contexts / 312 chart instances** (`system/mem/disk/net/ip/ipv4/ipv6/cpu` families), versus 6 on the probe vnode.
- Netdata's logs pipeline classifies journal **files**, not nodes. `/var/log/journal/remote/remote-<host>.journal` yields source `remote-<host>`; `_HOSTNAME` is a facet. Logs are not nested inside a vnode's node view.
- Netdata v2.10 renamed the ML config keys `spec.md` quotes: `training window` (6h) and `min training window` (15m); the old names are moved to `obsolete ...`. Values are unchanged, so 18 models × 3h = 54h and the ≥72h warm-up conclusion holds.
- Netdata's own `otel-plugin` is written in Rust (`src/crates/otel-plugin`), alongside 7 sibling OTEL crates and a `netdata-plugin` crate family (protocol, rt, bridge, charts-derive, schema, types) that implements the plugins.d protocol including `HOST_DEFINE`.
- Rust 1.97.1 installed user-local at `~/.cargo`.

Inferences:

- The `netdata-plugin` crates are workspace-internal (all dependencies are `workspace = true`), so out-of-tree reuse would require vendoring or upstream publishing. Writing a thin plugins.d emitter in this repo is cheaper than coupling to Netdata's internal workspace, and keeps the standalone-repo posture `spec.md` argues for.
- "Dashboard-complete" (~60–80 contexts) is defensible precisely because real hosts also lack exotic contexts: a simulated database server with no `amdgpu.*` is realistic, not incomplete.

Unknowns (cannot be resolved by investigation here):

- Exact scope of "OTEL sim" — tracked as an open decision in `SOW-0002`.
- Cloud API coverage for teardown automation (`spec.md` open question). Needs Cloud API credentials.
- Vnode scaling ceiling at the 200 target. Deferred to another machine per decision 6.

### Acceptance Criteria

- A Rust external plugin builds clean (`cargo clippy -D warnings`, `cargo fmt --check`) and runs under a live agent. Verified by: agent picks it up on the 60s plugin scan and 5 vnodes appear in `/api/v3/nodes`.
- Generator specs are declarative files, not hardcoded Rust. Verified by: adding a context to a simulated node requires editing a spec file only, with no recompilation of generator logic.
- A simulated Linux node presents 60–80 contexts covering every standard dashboard menu section. Verified by: `/host/<hostname>/api/v1/charts` context count and a menu-by-menu comparison against the real localhost node.
- Cross-metric invariants hold by construction, not by clamping. Verified by: a semantic-lint test asserting `free + used + cached + buffers == total` and counter monotonicity across a simulated run.
- The `environment.yaml` + seed pair reproduces an identical world. Verified by: two runs with the same seed produce byte-identical emitted output.
- One scenario can be triggered live and observed to change generator output. Verified by: query evidence before/after trigger on a running agent.
- Local verification never exceeds 5 vnodes.

## Analysis

Sources checked:

- `spec.md` (user-authored product definition)
- `prototypes/vnode-probe/FINDINGS.md` and `infra-sim-probe.plugin` (this repo)
- Live agent: netdata `v2.10.0-1022-nightly` at `localhost:19999`

Open-source reference evidence:

```text
netdata/netdata @ c23face0bd94
src/plugins.d/gperf-hashtable.h:139-161      HOST/HOST_DEFINE/HOST_LABEL registered PARSER_INIT_PLUGINSD
src/plugins.d/pluginsd_parser.c:147-215      HOST_DEFINE -> rrdhost_find_or_create(NETDATA_VIRTUAL_HOST)
src/plugins.d/pluginsd_parser.c:330-360      HOST switches collection context by GUID
src/plugins.d/README.md:191-260              documented protocol for HOST_DEFINE / HOST_LABEL / HOST
src/collectors/systemd-journal.plugin/systemd-journal-files.c:391-421   /remote/ -> source remote-<host>
src/collectors/systemd-journal.plugin/systemd-journal-files.c:432-433   *.<ns> -> source namespace-<ns>
src/collectors/systemd-journal.plugin/systemd-journal-files.c:881-893   scanned roots /run/log/journal, /var/log/journal
src/collectors/systemd-journal.plugin/systemd-journal.c:166             _HOSTNAME registered as a facet
src/ml/ml_config.cc:81-84                    old ML keys moved to "obsolete ..."
src/ml/ml_config.cc:131-133                  train every 3h; 18 models per dimension
src/crates/otel-plugin/                      Netdata's OTEL plugin is Rust
src/crates/netdata-plugin/protocol/build.rs:31-34   Rust plugins.d tokenizer incl. HostDefine
```

Current state:

- Repository was a single `spec.md` with no git history. Now git-initialized with SOW installed.
- Verification probe is installed on the user's live agent at `/etc/netdata/custom-plugins.d/infra-sim-probe.plugin`, running 3 vnodes. It is throwaway and must be removed or superseded before this SOW closes.

Risks:

- **Content volume is the dominant risk, not engineering.** The Linux baseline alone is 60–80 contexts under decision 2B. If generator specs are verbose, authoring cost dominates the schedule. Mitigation: the spec format must support templating/inheritance so one `system.*` family is not 80 hand-written blocks.
- **Fidelity artifacts are easy to ship and hard to detect.** The throwaway probe produced `system.ram free = 0` within four minutes — a conservation-invariant violation caused by clamping. Mitigation: invariants are declared in the spec format and enforced by construction; semantic lints run in CI.
- Coupling to Netdata's internal Rust workspace would undermine the standalone-repo posture. Mitigation: implement the emitter locally; treat `netdata-plugin` crates as reference, not dependency.
- Local agent pollution: the user's working laptop now carries simulated nodes. Mitigation: 5-vnode cap, obviously-synthetic naming, documented removal command.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

There is no Infra-Sim implementation. `spec.md` defines a multi-month product whose two load-bearing assumptions were unverified. Both are now verified, and one materially changed scope: vnodes get no dashboard for free, so generator content volume — not plugin plumbing — is the real cost driver. The correct first move is a narrow end-to-end slice that forces every interface to be real and proves the content-authoring loop is affordable.

Evidence reviewed:

See `## Analysis` — `spec.md`, `prototypes/vnode-probe/FINDINGS.md`, live agent queries, and `netdata/netdata @ c23face0bd94` citations above.

Affected contracts and surfaces:

- New: generator spec file format (declarative, versioned) — the most important contract in the project.
- New: `environment.yaml` + seed as the reproducibility pair.
- New: scenario definition format and ground-truth manifest.
- New: Rust workspace layout and the plugins.d emitter.
- Consumed, not owned: the plugins.d protocol; the agent's `custom-plugins.d` discovery; the `/host/<h>/api/...` query surface.
- Docs: README, SE quickstart (both new).

Existing patterns to reuse:

- `prototypes/vnode-probe/infra-sim-probe.plugin` — proven emission sequence (HOST_DEFINE block → HOST → CHART/DIMENSION → HOST → BEGIN/SET/END), chart definitions mirroring proc.plugin contexts, and the boundary-aligned tick loop.
- `netdata/netdata @ c23face0bd94 src/crates/netdata-plugin/protocol/` as a reference implementation of the protocol in Rust (reference only — not a dependency, see Inferences).
- The special `_os_name`/`_system_cores`/`_virtualization` host-label keys documented at `src/plugins.d/README.md:211-249`, already exercised by the probe.

Risk and blast radius:

See `## Analysis` risks. Blast radius of this slice is limited to this repository plus one plugin file on the user's local agent. No changes to the Netdata agent, no network services, no customer data. The plugin is additive and removable with a single `rm`.

Sensitive data handling plan:

This slice touches no credentials. Claim tokens and room IDs are out of scope until the console's claim flow (a later SOW) and will never be committed. All simulated hostnames/labels committed here are obviously synthetic (`sim-*`). No prospect or customer names appear in templates. Live-agent evidence quoted in this SOW is from the user's own workstation agent and contains no customer identifiers.

Implementation plan:

1. **Workspace skeleton.** Cargo workspace; crates `sim-spec` (spec model + de/serialization + validation), `sim-engine` (deterministic generator execution, seeded), `sim-plugin` (plugins.d emitter + binary). Lint/fmt config. `.gitignore`.
2. **Generator spec format.** Declarative YAML: per-context metric type, base levels, daily/weekly seasonality, noise model, instance/cardinality model, label templates, and declared cross-metric invariants. Must support family-level templating so 80 contexts are not 80 hand-written blocks.
3. **Deterministic engine.** Seeded PRNG per (environment seed, vnode GUID, context, dimension) so output is reproducible and independent of iteration order. Counter accumulators with enforced monotonicity. Invariant solver that distributes a conserved total across dimensions instead of clamping.
4. **plugins.d emitter.** Host definition, chart/dimension declaration, tick loop aligned to `update_every`. Correct quoting of label values containing spaces (the probe's first bug).
5. **Linux baseline generator specs (~60–80 contexts).** Authored to cover every standard dashboard menu section, cross-checked menu-by-menu against the real localhost node.
6. **Web-stack environment template + `environment.yaml`.** 5 vnodes with a dependency graph (LB → app → DB).
7. **Scenario engine, one scenario.** Timeline of effects (ramp/step/drift/oscillate) over generator parameters, live trigger via a control channel, ground-truth manifest emitted alongside.
8. **Thin console.** Minimal single-binary web UI: environment status, scenario trigger/resolve. Create/claim/preflight/teardown flows are later SOWs.
9. **Validation pass** against the live agent, 5 vnodes, then remove the throwaway probe.

Validation plan:

- Unit: spec parsing/validation; seeded determinism (same seed ⇒ identical output); counter monotonicity; conservation invariants.
- Semantic lints: units/ranges sane, rates consistent with their integrals, no `free = 0` clamping artifacts.
- Real-use: install on the live agent, confirm 5 vnodes register, confirm per-vnode context count and data, confirm ML trains on the simulated nodes, trigger the scenario and capture before/after query evidence.
- Menu-parity check: compare simulated node dashboard sections against the real localhost node.
- Same-failure search: grep the generator specs for any other clamped-to-bound expressions of the `free = 0` class.

Artifact impact plan:

- AGENTS.md: expect updates once the generator-authoring loop is concrete (commands, hazards).
- Runtime project skills: expect the first real `project-*` skill from this slice — the generator-authoring + fidelity loop. Deliberately not created yet (see Open decisions, resolved item 5).
- Specs: `.agents/sow/specs/` gains the generator spec format contract and the `environment.yaml` contract once stable.
- End-user/operator docs: README (hard rule first, per `spec.md` disclosure posture) and an SE quickstart.
- End-user/operator skills: none affected; no output/reference skills exist yet.
- SOW lifecycle: OTEL split out to `SOW-0002` (pending). Scale test and Cloud-API teardown remain `spec.md` open questions, not deferrals of this SOW.

Open decisions:

Resolved by the user on 2026-08-07:

1. First deliverable — **A**, vertical slice.
2. Linux baseline completeness — **B**, dashboard-complete (~60–80 contexts).
3. Runtime language — **C**, Rust. (Assistant had recommended Go on contributor-pool grounds; the user's choice is corroborated by Netdata's own `otel-plugin` and `netdata-plugin` crates being Rust, which negates that concern.)
4. Log mechanism — **A**, per-host journal files in `/var/log/journal/remote/`.
5. Process — **A**, git + SOW initialized before building. Project skills deliberately deferred until this slice yields concrete workflow knowledge.
6. Scale test — capped at 5 vnodes locally; scale testing runs later on another machine.

Resolved by the user on 2026-08-07 (second round):

9. Next work in this SOW — **B then A**: per-instance cardinality first, then the scenario engine. Reasoning accepted: a scenario injected into a fleet whose disk charts already read as wrong sells nothing; fidelity is the foundation.

Unresolved, not blocking this slice:

7. OTEL sim scope — tracked in `SOW-0002` (pending). Does not block items 1–8 above, which are all plugins.d-path work.

## Implications And Decisions

**Correction to `spec.md` scope (for user awareness, not applied).** Per `AGENTS.md`, `spec.md` is user-owned and not edited unilaterally. Three corrections are proposed:

1. P0 "~20 hero collector generators" undercounts: hero collector #1 (Linux system) alone is 60–80 contexts under decision 2B, or 188 at full parity.
2. The `§Repository` fallback ("contribute a thin go.d module upstream") is dead — external plugins can define vnodes directly. It can be struck.
3. P0 "Correlated logs ... attributed to correct vnodes in logs UI" is not achievable as literally worded; logs attribute by source/`_HOSTNAME` facet, not by node view. Proposed rewording recorded in `FINDINGS.md`.
4. `§3 History` quotes ML config keys that v2.10 marks obsolete. The 72h conclusion holds; the key names need updating before they reach a runbook.

**LLM backend for `--describe` (user-directed, implemented).** The user asked for "full blown claude or openai, with api key wired in". Three implementation decisions were taken inside that instruction and are recorded here rather than left in code comments:

1. **The model returns a plan, not YAML.** It picks from a catalogue of roles (the same table the keyword parser matches) and the service specs actually present in `specs/`, both enforced as JSON-schema enums and re-checked on our side; the existing deterministic renderer writes the file. This keeps GUIDs derived from hostnames, keeps every generated environment lintable, and bounds the blast radius of a misreading to a wrong-but-visible fleet rather than an environment naming a signal no generator defines. That failure mode is silent and surfaces mid-demo, which is the one this project can least afford.
2. **Transport is `curl`, not an HTTP crate.** Every in-process Rust TLS stack (rustls via `ring`/`aws-lc-rs`, native-tls via OpenSSL) requires a C toolchain or cmake, and `infra-sim` is the binary that ships in the runtime image. Taking ~100 crates and a build-environment dependency into the data-path binary for one authoring-time call is a bad trade; `curl` is present everywhere this runs and inherits the operator's proxy configuration. Revisit if the plugin ever needs HTTPS in the data path.
3. **No silent fallback.** If `--llm` is given and the call fails, the command fails and says so. Quietly downgrading to the keyword parser would hand the SE a weaker reading with no way to tell.

Also unresolved by the instruction and decided here: `--name` stays authoritative over the model's suggested environment name, because the name fixes the seed, the hostname prefix and therefore every GUID — letting a model move it would orphan a running fleet on a re-run.

## Plan

See `## Pre-Implementation Gate` → Implementation plan, items 1–9.

## Execution Log

### 2026-08-07

- Verified both load-bearing `spec.md` assumptions against live agent `v2.10.0-1022-nightly`; wrote `prototypes/vnode-probe/{infra-sim-probe.plugin,FINDINGS.md}`.
- Installed throwaway probe to `/etc/netdata/custom-plugins.d/`; 3 vnodes registered and confirmed ML-trained. Probe must be removed before close.
- Collected user decisions 1A / 2B / 3C / 4A / 5A / 6.
- `git init`; installed Rust 1.97.1 user-local via rustup.
- Bootstrapped SOW: `.agents/sow/{specs,pending,current,done}`, project-local `SOW.template.md` + `audit.sh`, `AGENTS.md` with project-specific guardrails, `CLAUDE.md`/`GEMINI.md` symlinks, `.claude/skills -> ../.agents/skills`.
- Opened `SOW-0002` (pending) for OTEL sim scoping.
- Implemented plan items 1-6: Cargo workspace (`sim-spec`, `sim-engine`, `sim-plugin`); generator spec format with `independent`/`partition`/`counters` shapes; deterministic SplitMix64 engine with name-addressed streams; plugins.d emitter; 50-context Linux baseline (`specs/linux-system.yaml`); 5-node web-stack environment (`environments/web-stack.yaml`); `scripts/install-local.sh`; README.
- Added `--lint HOURS` to the plugin as the first piece of the fidelity harness.
- Removed the throwaway Python probe and installed the Rust plugin on the live agent.
- Delivered, after items 1-6, in order: per-instance cardinality and the 70-context baseline; the scenario engine with live trigger and ground-truth manifests; chart labels so stock health templates attach; the control console (item 8); the four remaining hero scenarios; service generator specs and their composition; k8s and robotics-edge templates; clock pinning and the re-skin workflow; the semantic fidelity harness; `--describe` (offline keyword parser).
- Added an optional LLM backend to `--describe` (`--llm anthropic|openai`) at the user's request. The offline parser stays the default and the fallback.

### Findings during implementation

- **Noise scaled off `base`, not the current seasonal level.** The lint surfaced `ctxt_switch_rate` and `idlejitter_us` pinned to their floors. Root cause was a modelling flaw, not tuning: a signal at a 3 a.m. trough carried the same absolute jitter as its afternoon peak, driving troughs into the floor. Fixed in `sim-engine` — noise is now proportional to the current seasonal value.
- **Bound-clamping needed a physical/rail distinction.** The first lint run reported 175 violations, nearly all signals legitimately resting at zero. Zero is a real value for the non-negative quantities these specs model, so it is never flagged; any other floor needs `min_is_floor: true` and every ceiling needs `max_is_ceiling: true`. After the distinction plus the noise fix: 0 violations over 72 simulated hours.
- **`cache` role RAM driver was pinned at its max**, flattening `system.ram free` to a constant — the same artifact class as the probe's `free = 0`. Caught by inspecting real emitted output; fixed by giving the role headroom above `base * (1 + daily_amplitude)`.
- **Serde cannot combine `deny_unknown_fields` with `flatten`.** `Context` drops the strict attribute; inner shape structs stay strict.
- **Per-instance cardinality was the real fidelity gap, not context count.** Expanding contexts alone would not have fixed a node showing one unnamed disk chart. Netdata's own chart-id and family conventions are not uniform (`disk.io` -> `disk.<dev>` family `io`, but `net.packets` -> `net_packets.<iface>` family `<iface>`), so the spec states them rather than inferring; this was checked against a real node, not guessed.
- **Chart planning had to move into the engine.** Declaration and emission were deriving the chart list separately. Any divergence would have sent `SET` lines to charts that were never declared, which surfaces as silently missing data rather than an error. One plan, read by both.
- **Scenario multipliers had to widen signal bounds.** A fault is meant to push a signal past its normal operating range; without widening, the clamp would have silently defeated the scenario while the lint reported a pinned signal - the artifact machinery working against the feature.
- **Scenario start time has to live in the control file.** The trigger script wrote only the scenario name, so the plugin assigned "now" on first read and a plugin restart rewound the scenario to its opening state - indistinguishable, on screen, from the fault resolving itself mid-sentence. The control module already documented restart-resume as a property; it was only true when `started_at` was present and nothing wrote it. Caught by noticing a running scenario had progressed less than wall-clock time allowed.
- **Removing a plugin file does not stop the running plugin.** The Python probe kept running from a deleted file for over an hour, writing to the same vnode GUIDs as the new Rust plugin and corrupting `system.ram` readings with interleaved values. Diagnosis cost real time because the symptom looked like a conservation bug in new code. Teardown must kill the process, not just remove the file — this belongs in the console's teardown flow and in any operator doc.

## Validation

Acceptance criteria evidence (items 1-6; items 7-8 not started):

- Builds clean and runs under a live agent: agent picked the plugin up on its 60s scan; all 5 vnodes present in `/api/v3/nodes`; 58 charts each (50 from the spec + 8 the agent's ML adds per node). MET.
- Generator specs are declarative: adding a context is a YAML edit. The 50-context baseline required no generator-logic code. MET.
- 60-80 contexts covering standard menu sections: **70 contexts. MET.** Live agent shows 78 contexts per node (70 from the spec + 8 the agent's ML adds) across 94-117 charts depending on each node's device count, in 43-45 families.
- Per-instance cardinality: MET. `disk.io` expands to `disk.nvme0n1` / `disk.nvme1n1`, `disk.space` to one chart per mount, `net.net` per interface. Weighted instances carry visibly different load (db data disk 6,610 reads/s vs WAL disk 1,524/s).
- Invariants by construction: `cargo test` asserts conservation and monotonicity over thousands of ticks; confirmed independently through the agent's own query engine - every node's `system.ram` dimensions sum exactly to its configured total (4096 / 16384 / 16384 / 65536 / 16384 MiB) with no zero-free artifact. MET.
- Same seed reproduces identical output: unit test over 500 ticks x 3 contexts, byte-identical. MET for the engine; the plugin uses wall-clock time, so bit-exact *replay* additionally needs clock pinning - see Followup.
- **Stock health templates need chart labels, and skip silently without them.** With the disk fill at 90% the threshold alert never fired. Cause: `disk_space_usage` carries `chart labels: mount_point=...` (`/usr/lib/netdata/conf.d/health.d/disks.conf`) and our charts emitted no labels, so the template never attached - no error, no warning, the alert simply did not exist. `disk_fill_rate` and `out_of_disk_space_time` did attach because they have no label filter, which is exactly what made the gap hard to see. Fixed by emitting `CLABEL`/`CLABEL_COMMIT` per chart, declared per context in the spec. Alarm instances on sim-db-01 went from 41 to 51, and `disk_space_usage` now attaches per mount. This is the same class of finding as the vnode-dashboard one: a whole product surface silently absent because the simulated data was missing metadata nothing complains about.
- **Scenario headroom must not widen physical ceilings.** Granting every `max` headroom under an active scenario pushed `10min_disk_utilization` to 101.5% - impossible for a single device, and exactly the "is this fake?" artifact the fidelity work exists to prevent. Declared ceilings are now never widened; `disk_busy_ms_rate` and `file_nr_utilization` are marked as such. After the fix the same scenario reads 89.5%. Notable that the artifact was found through an *alert*, not a chart: the health engine surfaced an impossible value the lint could not see, because the lint only checks whether a signal is pinned, not whether its bound is physically meaningful.
- **The hard rule proven at the incident level, not just the data level.** With `disk-fill` running, the *first* alert Netdata raised was `ml_1min_node_ar` at a 1.02% node anomaly rate on sim-db-01 - its own ML detecting the injected fault roughly 18 minutes before the disk threshold could fire. Anomaly rate ranked the fleet correctly too: sim-db-01 1.02%, sim-web-01 0.76%, sim-cache-01 0.43%, matching the manifest's blast radius ordering. Nothing was faked; the scenario moved generator inputs and the real ML and health engine did the rest.
- Live scenario trigger: MET. `scripts/scenario.sh trigger disk-fill` moved the targeted mount from 11.4% to 28.3% used over five minutes while the untouched mount on the same node stayed flat at ~69%, confirming instance-scoped targeting. Ramp tracked its schedule exactly (23m elapsed, 90.3% observed against 90.5% predicted).
- **End-to-end incident chain proven.** With the scenario at 93.5%, Netdata's real health engine raised `disk_space_usage` WARNING on `disk_space./var/lib/pgsql` - precisely the mount the manifest names as root cause - while the same alert stayed CLEAR on `/` (25%) and `/var/log` (67.7%) on the same node. No false positives, nothing mocked anywhere in the chain: scenario -> generator signal -> plugins.d -> agent -> health engine -> alert.
- Local verification capped at 5 vnodes. MET.

Tests or equivalent validation:

- `cargo test`: 32 passed, 0 failed.
- `cargo clippy --all-targets`: clean. `cargo fmt --check`: clean.
- `infra-sim --lint 72`: 0 signals pinned across all 5 nodes over 72 simulated hours (259,200 samples per node).

LLM backend for `--describe` — validated against a local mock speaking each provider's wire format, since no provider key was available in this session:

- `cargo test`: 138 passed, 0 failed. Clippy and fmt clean.
- Anthropic path end-to-end: the captured request carried the key in an `x-api-key` header (proving the stdin config file reached curl), `anthropic-version: 2023-06-01`, `output_config` with both `effort` and a `json_schema` format, and role/service enums populated from the real on-disk catalogue (`lb, web, db, cache, k8s-control-plane, k8s-worker, edge-gateway` / `containers, kubernetes, nginx, postgres, redis`). The reply's plan was read past a leading `thinking` block.
- The environment it produced — 11 nodes named in the prospect's vocabulary (`acmetest-checkout-NN`, `acmetest-aurora-01`, `acmetest-elasticache-NN`) — passed `--lint 12` with **no semantic violations and no pinned signals**, and all 5 scenarios resolved against it, meaning the generated `db` node carried the data volume and replication interface the hero scenarios target. This is the safety argument demonstrated rather than asserted: the model chose the plan, the deterministic renderer produced a fleet that lints.
- OpenAI path end-to-end: `POST /v1/chat/completions`, `authorization: Bearer`, `response_format.json_schema.strict = true`, system+user message pair; plan parsed and rendered.
- Error paths all fail closed with an actionable message and exit 1: key unset, unknown provider, `--llm-model` without `--llm`, HTTP 401 (provider's own message surfaced), unreachable endpoint. The request-body temp file is removed on every path, including the failures.
- Not covered: no call was made to a real provider. Model-quality behaviour — how well `claude-opus-5` actually maps an unfamiliar description onto the catalogue — is unverified and needs a run with a live key.

Real-use evidence (live agent `v2.10.0-1022-nightly`):

- Role differentiation is physically coherent, agent-computed: `sim-lb-01` 115 Mbit/s rx with 2.5 disk reads/s and 8,606 TCP connections; `sim-db-01` 13.8 Mbit/s rx with 483 reads/s + 867 writes/s and 487 connections. Network-dominant versus IO-dominant, as the roles intend.
- CPU busy tracks role bases: lb 23.6%, web 32.4%, db 50.5%, cache 15.1%.
- Counter-derived rates render correctly, so emitted counters are well-formed monotonic series as far as the agent's `incremental` algorithm is concerned.
- Hard rule holds: `ml_running = 1` and `training_status` showing 11 trained / 86 untrained dimensions per simulated node, on charts the agent created itself.

Reviewer findings:

- No external review yet; none requested.

Same-failure scan:

- Searched for other clamped-to-bound expressions of the `free = 0` class. The `--lint` pass over 72h is that search executed rather than grepped: it covers every signal on every node and reports 0. The `cache` role instance it found is fixed.
- Searched for other partitions whose driver could reach its total: `system.ram` (all role/node pairs bounded below total), `mem.swap` (max 3,800,000 < 4,194,304), `disk.space` (max 470,000,000 < 524,288,000), `disk.inodes` (max 29,000,000 < 32,768,000). All have headroom.

Sensitive data gate:

- Pending final check. Interim: this SOW and `FINDINGS.md` contain no credentials, customer names, or customer-identifying addresses. Live-agent evidence is from the user's own workstation. Simulated hostnames committed so far are obviously synthetic (`sim-*`). Claim tokens and room IDs are out of scope for this slice.

Artifact maintenance gate:

- AGENTS.md: pending.
- Runtime project skills: pending.
- Specs: pending.
- End-user/operator docs: pending.
- End-user/operator skills: pending.
- SOW lifecycle: pending.

Specs update:

- Pending.

Project skills update:

- Pending.

End-user/operator docs update:

- Pending.

End-user/operator skills update:

- Pending.

Lessons:

- Pending.

Follow-up mapping:

- Pending.

## Outcome

Pending.

## Lessons Extracted

Pending.

## Followup

Done:

- Throwaway probe removed from the live agent and its process killed.

Remaining, in this SOW:

- Item 8: thin console. The interim control surface is `scripts/scenario.sh` (list / status / trigger / resolve), which writes the same control file the console will.
- Four of the five hero scenarios in `spec.md` are unwritten: memory leak to OOM cascade, DB replication lag, noisy neighbour, flapping edge links. The engine supports all four effect shapes they need.
- Correlated logs. Decision 4A settled the mechanism (per-host journal files in `/var/log/journal/remote/`) but no work item for it exists in this SOW's plan. Needs its own SOW.

Tracked elsewhere:

- `SOW-0002` - OTEL sim scoping (pending, blocked on user decision).

New follow-ups raised by this work, needing a decision before they become SOWs:

- **The fidelity lint cannot see physically-impossible values, only pinned ones.** The 101.5% utilisation passed the lint cleanly. A semantic-lint pass that knows a percentage cannot exceed 100 - `spec.md` section 8 calls this "units/ranges sane" - would have caught it without needing an alert to fire. Worth building before the long-tail generator batch, since it is the check that scales to collectors nobody reviews by hand.
- **`resolve` snaps a signal back to baseline in one tick.** Removing a scenario from the control file drops its multiplier straight to 1.0, so a fault vanishes instantly rather than healing. The `recover` effect exists for graceful unwinding but only within a scenario's own timeline. On screen an instant snap-back reads as artificial, and `spec.md` explicitly calls out that showing recovery is as persuasive as showing failure. Options: have `resolve` synthesise a recovery ramp, or require hero scenarios to end with a `recover` step and make `resolve` an abort rather than the normal path.

- **Clock pinning for bit-exact replay.** The engine is deterministic given a timestamp, but the plugin reads the wall clock, so replaying an archived environment reproduces the same shape at the same time-of-day rather than byte-identical output. `spec.md` promises bit-for-bit reproduction; closing that gap needs a `--replay-from <timestamp>` mode.
- **Teardown must kill the plugin process, not just remove the file.** Learned the hard way during this slice; belongs in the console's teardown flow and the SE quickstart.

`spec.md` open questions not owned by this SOW: Cloud API teardown coverage; vnode scaling ceiling at 200.

## Regression Log

None yet.
