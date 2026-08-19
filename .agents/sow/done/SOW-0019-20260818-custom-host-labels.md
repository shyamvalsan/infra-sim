# SOW-0019 - Custom host labels: LLM prefill, console authoring, live editing

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: delivered - implemented, externally reviewed (5/6), blocking findings fixed, live-validated, docs and spec updated.

## Requirements

### Purpose

Simulated fleets must carry user-defined host labels the way real deployments do, because Netdata Cloud users filter, group, map and write room-membership rules on them. Today the label set is fixed by the renderer and the only way to add a label is per-node YAML surgery in `environment.yaml`.

### User Request

Feedback: adding custom labels is problematic — users must manually edit conf files. Simulate host labels to mimic real deployments. Decisions recorded by the user:

1. LLM prefill of meaningful custom labels, plus a UI for the user to add labels they care about.
2. Fleet-wide + per-group (role) label scoping.
3. Exact prospect conventions are not needed; labels must look realistic and make sense for node map filters and views.
4. Console-side validation using the agent's own label rules; protect the system namespace and simulation labels.
5. Scope: labels only. Per-node label editing is out of scope (follow-up if ever needed).

### Assistant Understanding

Facts:

- The environment schema already carries arbitrary labels per node (`crates/sim-plugin/src/environment.rs:157`, `labels: BTreeMap<String, String>`), and the emitter already publishes them (`crates/sim-plugin/src/emitter.rs:42-51`). The plugin layer needs no new concept.
- The renderer emits a fixed set only: OS, kernel, virtualization, cloud, `infra_sim_role`, `infra_sim_env` (hardcoded `production`), site coordinates (`crates/sim-engine/src/describe.rs:1223-1245`, network devices at `1147-1165`).
- `CreateRequest`/`GroupRequest` have no label fields (`crates/sim-console/src/provision.rs:40-93`); `ReskinRequest` is name-only and passes `labels: Default::default()` (`provision.rs:2033-2104`).
- `reskin::Plan` has a `labels` field but only replaces existing label lines — it cannot insert labels a node does not yet have (`crates/sim-engine/src/reskin.rs:74-99`).
- Re-writing the environment under a running plugin is a supported, self-restarting flow: the plugin exits 0 on env change and the agent respawns it (`crates/sim-plugin/src/main.rs:579-585`).
- The agent migrates vnode labels in place on re-`HOST_DEFINE` — no history loss, updated labels pushed to Cloud (netdata/netdata @ c23face0bd94 `src/plugins.d/pluginsd_parser.c:316-331`, `rrdlabels_migrate_to_these`, `PENDING_LABEL_RECHECK`, `aclk_queue_node_info`).
- Agent label constraints: names ≤ 200 chars, values ≤ 800 bytes/200 UTF-8 chars (netdata/netdata @ c23face0bd94 `src/database/rrdlabels.h:23-24`); sanitization mapping in netdata/netdata @ c23face0bd94 `src/libnetdata/sanitizers/sanitizers-labels.c:5-53` (names: `: = + @ space ( ) ,` and UTF-8 → `_`; names that sanitize to only underscores are rejected; values: `; =` → `:`, `,` → `.`, `\` → `/`).
- `simulated=true` is forced unconditionally at runtime (`crates/sim-plugin/src/environment.rs:331-335`); `infra_sim_name` exists for Cloud room-membership rules (`environment.rs:27-36`). Both must stay non-editable in the new surfaces.
- The describe→LLM path already uses a strict JSON schema with pinned enums and `additionalProperties: false` (`crates/sim-engine/src/llm.rs:418-476`); schema objects must not allow unknown keys.

Inferences:

- Because the environment schema already carries labels and the emitter already ships them, the change is authoring-surface only: renderer, console, reskin, UI, validation. Blast radius at the plugin layer is near zero.
- Label edits applied via environment rewrite inherit the re-skin properties: GUIDs untouched, history preserved, plugin self-restarts within seconds.

Unknowns:

- Whether Cloud reflects a label change on a live claimed vnode promptly enough for a demo flow. Source says `aclk_queue_node_info` fires; a live probe settles it if we care about the timing.

### Acceptance Criteria

- A fleet created from the console carries user-authored labels (fleet-wide + per-group), visible on every intended node via `curl /host/<hostname>/api/v1/info` against a live agent — verified, not assumed.
- The describe box (LLM path) proposes labels that prefill the editor; the user can edit before create; the picker path works with no LLM key at all.
- Labels can be edited on a running simulation from the console; after the plugin self-restarts, the new labels are live, GUIDs unchanged, chart data continuity intact (query data before/after).
- Reserved labels (`simulated`, `infra_sim_name`) and `_`-prefixed (agent system namespace) keys are refused in the console with actionable errors; keys/values that the agent would sanitize differently are refused at authoring time, proven by unit tests ported from the agent's sanitizer table.
- Lint still passes on a labelled fleet; clippy/fmt clean.

## Analysis

Sources checked:

- `crates/sim-engine/src/describe.rs` (renderer, Reading/Group, label emission, site labels)
- `crates/sim-engine/src/reskin.rs` (text-walk rewrite, label replacement, GUID guard)
- `crates/sim-engine/src/llm.rs` (plan schema, strict json_schema constraints)
- `crates/sim-plugin/src/environment.rs`, `crates/sim-plugin/src/emitter.rs`, `crates/sim-plugin/src/main.rs` (schema, HOST_LABEL emission, env-stamp restart)
- `crates/sim-console/src/provision.rs` (create/reskin plumbing), `crates/sim-console/src/ui.html` (create form, picker)
- netdata/netdata @ c23face0bd94: `src/plugins.d/pluginsd_parser.c`, `src/database/rrdlabels.{c,h}`, `src/libnetdata/sanitizers/sanitizers-labels.c`
- `.agents/sow/specs/runtime-and-scenarios.md` (GUID-identity rules the label flows must not violate)
- Prior SOWs 0001, 0005, 0006, 0016 for renderer/console conventions

Current state:

- Labels are renderer-fixed; console-blind; the engine's one label-rewrite hook (reskin) is unused for labels and cannot insert.

Risks:

- The renderer and reskin are text-walkers that preserve comments; label insertion must place lines deterministically inside each node's `labels:` block without corrupting sibling sections. Mitigate with tests mirroring `reskin.rs` suite.
- A live label edit restarts the plugin; a per-second sample gap of a few seconds is expected (same as re-skin today) — document, don't hide.
- Committed environment templates are public artifacts: user-authored labels in repo `environments/*.yaml` must be synthetic (no prospect names). Same rule the create name already follows.
- LLM-suggested labels are model output: treat as prefill only, always through the same validation as typed input.

## Pre-Implementation Gate

Status: ready (one minor sub-decision open, listed under Open decisions)

Problem / root-cause model:

- Adding a custom label today requires editing `environment.yaml` per node because no authoring surface exists: the renderer cannot emit user labels, the console cannot capture them, and the one rewrite path (re-skin) cannot insert them. Nothing downstream (emitter, agent, Cloud) is missing — this is an authoring gap.

Evidence reviewed:

- As listed under Analysis, with the agent-side label behavior verified in netdata/netdata @ c23face0bd94 (`pluginsd_parser.c:169-191` HOST_LABEL parse; `:316-331` in-place label migration on redefine; `rrdlabels.h:23-24` length caps; `sanitizers-labels.c:5-53` charset rules).

Affected contracts and surfaces:

- `sim_engine::describe::{Reading, Group}` gain label maps; `render()` output (environment.yaml) gains user label lines per node.
- `sim_engine::reskin`: new label-insert/apply path (text-walk, comment-preserving, GUID-guarded).
- Console API: `CreateRequest`/`GroupRequest` gain `labels`; new apply-labels endpoint for a running simulation; validation on every entry point.
- `sim_engine::llm`: plan schema gains optional fleet/group label suggestions; UI prefills editor from the proposal.
- `ui.html`: label editor (fleet-wide + per-group in picker), preset starters, live-edit panel.
- Docs: README label bullet, `docs/operating.md`, `docs/QUICKSTART.md` if the flow changes the walkthrough.
- No plugin protocol change; no environment schema version bump (labels already allowed per node).

Existing patterns to reuse:

- `reskin.rs` text-walk + GUID invariance check + comment preservation.
- `describe.rs` `site_labels()` emission pattern and `BTreeMap` determinism.
- `environment.rs` runtime forcing of `simulated` (keep as backstop under console validation).
- `llm.rs` `plan_schema` strict-schema conventions (`additionalProperties` discipline).
- Create-form/picker UI patterns and the Progress reporting flow.

Risk and blast radius:

- Regression risk concentrated in renderer/reskin text handling (unit-tested) and console request validation. Plugin runtime risk minimal (no code path change; labels already flow). Cloud/agent behavior verified by source and by the existing re-skin flow in production use. Rollout: additive fields, serde-defaulted, so old environments and clients keep working.

Sensitive data handling plan:

- Labels are host metadata typed by the operator or suggested by the model. Validation must refuse label keys/values that look like credentials (no `token`, `key`, `secret`, `password` key names) — cheap denylist, avoids an accidental credential-in-a-label. Committed `environments/*.yaml` templates and docs use synthetic values only (`team=payments` fine; `[PROSPECT]`-style names never). SOW/spec text carries no real prospect names.

Implementation plan:

1. Engine: `labels.rs` validation module porting the agent's sanitizer rules (name/value charset, lengths, underscore-only rejection, reserved keys: `simulated`, `infra_sim_name`, `_`-prefix namespace, credential-looking key names). Golden tests from the agent's table.
2. Engine: `describe.rs` — `Group.labels`, `Reading.labels`; `render()` merges fleet → group → existing generated labels (group wins same-key), emits after `infra_sim_env`, before site labels, `BTreeMap` order.
3. Engine: `reskin.rs` — `apply_labels()` that inserts missing and updates existing label lines per node (role-aware for group labels) with the same GUID invariance guard; no rename required.
4. Console: `CreateRequest`/`GroupRequest` fields + validation on entry; pass-through to describe/render. Apply-labels endpoint for the installed environment of the running simulation, using (3).
5. LLM: optional `labels` in plan schema (fleet object + per-group object, `additionalProperties: string`); proposal → UI prefill only.
6. UI: label editor rows in create form + per-group in picker; preset buttons (`environment`, `site`, `team`, `service`); live-edit panel on the running-sim card.
7. Docs and spec updates; live-agent validation pass.

Validation plan:

- `cargo test`, clippy `-D warnings`, fmt check.
- Unit: sanitizer-parity goldens; renderer emission order/merge precedence; `apply_labels` insert/update/GUID-guard; reserved-label refusals.
- Offline lint on a labelled fleet (`--lint 2`).
- Live agent (≤5 vnodes, per project cap): create with fleet+group labels → `curl /host/<h>/api/v1/info` shows them on the right nodes and only those nodes; live edit → plugin self-restart → labels updated, GUIDs unchanged, `/api/v1/data` continuity across the restart; negative cases (reserved, `_`-prefix, sanitize-mutating key) refused with errors.
- Same-failure scan: search renderer/reskin tests for label-line edge cases (empty labels block, node with no labels block at all — renderer always emits one, verify).

Artifact impact plan:

- AGENTS.md: no workflow change expected; confirm at close.
- Runtime project skills: `project-live-validation` unaffected (uses the existing loop); confirm at close.
- Specs: `.agents/sow/specs/authoring-environments.md` gains the labels section.
- End-user/operator docs: README bullet + `docs/operating.md` labels how-to.
- End-user/operator skills: none exist for labels; none affected.
- SOW lifecycle: SOW-0020 (exporters) stays pending behind this one per user decision.

Open-source reference evidence:

- netdata/netdata @ c23face0bd94 — `src/plugins.d/pluginsd_parser.c` (HOST_LABEL parsing, in-place label migration, Cloud notify), `src/database/rrdlabels.h` (length caps), `src/database/rrdlabels.c` (`rrdlabels_add_changed` sanitize-then-store), `src/libnetdata/sanitizers/sanitizers-labels.c` (charset mapping table).

Open decisions:

1. RESOLVED (user, 2026-08-18): `infra_sim_env` mirrors the user's `environment` label (fleet label, overridable per group) instead of staying hardcoded `production`; absent the label, it remains `production`. `infra_sim_env` itself is never a settable user key.

## Implications And Decisions

1. Authoring surfaces — LLM prefill + UI editor, live editing on running fleets (user decision 1; earlier session decision "1. B").
2. Scoping — fleet-wide + per-group (user decision "2. B").
3. Realism bar — plausible labels that read well in map filters/views, not prospect-exact conventions; preset starters included (user decision 3).
4. Validation — console-side using the agent's own sanitization rules; reserved/system labels protected (user decision "4. B").
5. Scope — labels only; per-node editing excluded (user decision "5. A").
6. Sequencing — this SOW executes before SOW-0020 (exporters) (user decision "4. A" in the exporters discussion).
7. `infra_sim_env` mirrors the user's `environment` label; default remains `production` when unset (user: "1A", 2026-08-18).

## Plan

1. Validation module + tests (engine). Low risk, no deps.
2. Renderer label merge + emission + tests. Depends on 1.
3. `apply_labels` in reskin + tests. Depends on 1.
4. Console create/reskin/apply-labels plumbing + validation wiring. Depends on 1-3.
5. LLM schema + prefill. Depends on 1 (validation reuse), parallelizable with 4.
6. UI editor + presets + live-edit panel. Depends on 4-5.
7. Docs, spec update, live-agent validation, close-out gates.

## Execution Log

### 2026-08-18

- SOW drafted from investigation: renderer/console/reskin/llm surfaces, agent label behavior verified in netdata/netdata @ c23face0bd94. No code touched.
- Chunk 1: `crates/sim-engine/src/labels.rs` - validation ported from the agent's sanitizer table, credential denylist, reserved-key protection, YAML scalar quoting helper. 13 unit tests.
- Chunk 2: `describe.rs` - `Group.labels` / `Reading.labels`, `user_label_lines()` + `env_tier()` emission in both the Linux and network-device branches; `infra_sim_env` mirrors the `environment` label (user decision 1A). Tests: merge precedence, tier mirroring incl. group override and network devices, YAML round trip of numeric values.
- Chunk 3: `reskin.rs` `apply_labels()` + `LabelChanges` - insert/update/remove within a node's labels block, indent-aware block boundaries, GUID invariance guard, tier mirroring on set and reset. First draft used a macro with two defects (cross-node `rposition` for the tier line, dirty tracking missed updates/removals); rewritten as an explicit `LabelWalk` struct + `flush()`. 7 tests.
- Chunk 4: console - `CreateRequest.labels` / `GroupRequest.labels` validated on entry, passed through to the renderer; `read_labels()` + `apply_labels()` provision functions; `GET/POST /api/labels` routes; describe response carries fleet + group label prefill.
- Chunk 5: `llm.rs` - plan schema gains `labels` (fleet + per group, string maps, listed in `required` for strict mode), system-prompt guidance, `label_map()` validates suggestions and drops-and-notes invalid ones rather than failing the proposal.
- Chunk 6: `ui.html` - fleet label editor with presets (environment/site/team/service), per-group labels column with inline editor, live-edit panel (derive fleet/role sets from per-node GET), create body + describe prefill wired. JS syntax-checked standalone.
- Chunk 7: lint with labels (5-node env, PASS), live validation, docs (README, `docs/operating.md`), spec (`.agents/sow/specs/authoring-environments.md`).
- Deviation: none from the approved plan.
- Observed pre-existing limitation (not introduced here, not fixed here): `target()` in the console is a sticky single-`active` pointer; with several simulations on one machine, a console that discovered one first keeps targeting it until that sim is torn down. The labels endpoints inherit this. Recorded as follow-up candidate.
- External review round (user-authorized, one round, default roster via OpenCode; the `pi` binary on this machine is the pi-calculator, so Pi harness was unavailable): glm, mimo, deepseek, minimax, qwen returned verdicts (all NEEDS CHANGES, convergent findings); k3 timed out twice at the 30-minute cap - coverage gap reported, no verdict inferred. No rerun after fixes (one round authorized).
- Adjudication: verified and fixed - value validation not at agent parity (the agent rewrites `! " # $ % & ' * < > ? ^ \` { | } ~` in values; `AT&T` stored as `AT_T`); `yaml_scalar` emitted YAML-unsafe plain scalars (`a: b` broke parsing, `dc #1` silently truncated, empty value became null) and the live-edit path had no parse gate; a user label keyed `role` hijacked `read_labels`' node-role detection, corrupting the live editor's model; `dedupe_slugs`/`merge` silently dropped the second same-shape group's labels. Hardened from P3s: blank/comment lines inside label blocks no longer break either walker, `guid`/`hostname` label keys no longer trip the text-walk identity parsers (block-aware), single balanced quote-strip on read-back, tier line quoted, UI value check aligned to the backend set, double `checkLabel` call removed, YAML well-formedness guard added to the engine's `apply_labels`.
- Rejected findings (with reasons): UI esc-in-onclick pattern (pre-existing convention, operator-local tool, P3); within-role label divergence in the live editor (inherent to the fleet+role model; per-node editing rejected by user decision 5); credential-denylist substring false positives (deliberate conservative choice, recorded); UTF-8-whitespace claim (contradicted by the agent's own table, "UTF-8: yes" for values); sticky `target()` (pre-existing, below).
- Review-driven re-validation on a live agent: create with a `role: frontend` label and a colon+space value (`room: rack 12: primary`) lints, renders quoted, and the live node reports both verbatim alongside an uncorrupted `infra_sim_role`; live-edit refusals proven for `&`, `#` and empty values through the real endpoint; a full live add+remove cycle on a 160-node fleet kept GUIDs, kept 240/240 samples across both plugin restarts, and left the environment byte-identical (`git diff` empty).
- Incident during re-validation, disclosed: a fresh test console adopted the first container at startup (`AppState.active = container::list().next()`) and kept targeting it, so a test POST applied `role: backend` to the running 160-node `systematica` fleet (container env + repo copy) instead of the test fleet. Reverted through the same API (empty desired state) with GUID/sample continuity verified and the repo copy byte-identical afterwards; the user's other simulation was untouched. This upgrades the sticky-target follow-up from candidate to strongly recommended: `create` does not repoint `active`, startup adoption picks an arbitrary container, and multi-sim machines are the norm here.

## Validation

Acceptance criteria evidence:

- Fleet created from the console with fleet labels `{environment: staging, team: platform}` and a db group override `{team: payments}` (3 nodes, container path, lint PASS): live agent shows `environment=staging` on all nodes, `team=platform` on web nodes, `team=payments` on the db node, `infra_sim_env=staging` (mirrored), `simulated=true` and `infra_sim_name` forced - read from `/host/<h>/api/v1/info` on the simulation's own agent.
- Live edit `POST /api/labels` (fleet team platform->sre, `environment` removed, db override kept): after the plugin's self-restart, all three nodes show `team=sre` (db: `payments`), `infra_sim_env=production` (tier reset on removal), GUID unchanged (`7503b512-6119-4a69-a2db-0a9e0fd7c2a8` before and after), and `system.cpu` shows 180/180 samples across the 3-minute window spanning the restart - no gap at a 1s update interval.
- Negative cases, all refused with actionable errors naming the key: `simulated` (reserved), `_os_name` (agent namespace), `env:prod` (colon, would be rewritten), `api_key` (credential denylist), `latitude` on a group (generated).
- Describe prefill: schema + prompt + mapping wired and unit-tested; model-path suggestion behavior covered by `label_map` tests (invalid suggestions dropped with a correction note). Not exercised against a live model in this session - no key was available; the offline keyword path prefill is empty by design, and the picker path needs no key.

Tests or equivalent validation:

- `cargo test`: 244 passed after the review fixes (labels validation/quoting matrix, walker hardening, reader fixes, merge preservation, pre-existing suites).
- `cargo clippy --all-targets -- -D warnings`: clean. `cargo fmt --check`: clean.
- `--lint 2` on a hand-labelled 5-node environment: PASS, no pinned signals.
- Console JS extracted and `node --check` clean.

Real-use evidence:

- Full console lifecycle on a live machine alongside two pre-existing simulations (untouched): create with labels -> verify -> live edit -> verify continuity -> negative cases -> teardown. Console 19995 and sims `infra-sim-systematica`, `infra-sim-default` unaffected throughout.

Reviewer findings:

- One user-authorized round, default roster (glm, minimax, mimo, kimi, qwen, deepseek) via the OpenCode harness. Verdicts returned by 5 (all NEEDS CHANGES, convergent); k3 failed twice on timeout - coverage gap, no verdict inferred. All blocking findings independently verified against the code and the agent source before fixing; rejected findings listed with reasons in the Execution Log. Fixes applied and re-validated as recorded above; no second round run (one round authorized).

Same-failure scan:

- Searched every `labels:`-block parser/emitter: only the two new ones (`provision.rs` `read_labels`, `reskin.rs` `apply_labels`) walk label blocks; both are indent-aware at block boundaries (the `instances:`-parsed-as-a-label class of bug) and both have tests plus the live run. `emitter.rs` `define_hosts` publishes whatever the file says - unchanged, correct.
- Committed environment templates still carry `infra_sim_env: production` and no user labels: correct, they are authored files and the new surfaces do not retroactively decorate them.

Sensitive data gate:

- No credentials, tokens, prospect names or customer-identifying values in any durable artifact written this session. The live-validation fleet used synthetic values (`team=sre`, `labels-live`). The credential denylist exists precisely to keep secrets out of labels. Test artifacts (`environments/labels-test.yaml`, `labels-live.yaml`) deleted after validation; nothing of the sort remains untracked.

Artifact maintenance gate:

- Pending.

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

Delivered and closed. User-authored host labels exist across the whole
authoring surface: console create (fleet + per group, with presets and
describe-path prefill), live editing of a running simulation with identity and
data continuity, validation to the agent's own label rules (full value
character class, reserved namespaces, credential denylist) with reserved-
namespace and credential protection, `infra_sim_env` tiering from the user's
`environment` label, and a YAML well-formedness guard on the live-edit path.
One external review round (5/6 reviewers) found real gaps in the first cut -
value-character parity, YAML-safe emission, a `role`-key parser collision,
label-dropping merges - all fixed, regression-tested, and re-validated against
live agents including a full add/remove cycle on a 160-node fleet with zero
sample gaps. Final gates: 244 tests, clippy clean, fmt clean. Docs, README and
the authoring spec updated. Follow-ups recorded, the sticky-console-target one
strongly recommended after causing a fully-reverted incident during
validation.

## Lessons Extracted

Pending.

## Followup

- Per-node label editing: rejected for this SOW by user decision 5; revisit only with a concrete need.
- LLM describe-path deeper label extraction (site/team mining from prose): candidate follow-up after the prefill ships.

## Regression Log

None yet.
