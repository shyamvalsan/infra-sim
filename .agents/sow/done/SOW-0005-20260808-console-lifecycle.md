# SOW-0005 - Console lifecycle and warm-up incidents

## Status

Status: completed

Sub-state: Closes the four P0 gaps identified when the user asked where the UI to create and claim a simulation was. `spec.md` §6's five-stage lifecycle is now implemented end to end.

## Requirements

### Purpose

`spec.md` §6 names the control console a P0 component and defines it as five stages: create, claim, warm-up/preflight, demo, teardown. `SOW-0001` shipped preflight and demo controls and explicitly deferred the rest. The user found the gap by using the console.

### User Request

"but this is showing the console for the sim you created, where is the ui to create a brand new sim and claim it to a cloud space" (2026-08-08), followed by "i thought you finished implementing the spec, if you haven't please do it", "otel should be a checkbox in the sim creator ... also check boxes for other collectors", "A is fine" (console runs as root), and "keep going.. until we have something that is spec-worthy and complete".

### Assistant Understanding

Missing against `spec.md` §6 and the P0 checklist:

1. Create flow (picker → `environment.yaml` + seed).
2. Claim flow (token + room → whole fleet), with `<Prospect> (Simulated Demo)` naming enforced.
3. Guided teardown with archive.
4. Templates consumable by the console picker.
5. Demo controls listed as "trigger/escalate/resolve + clock" — only trigger and resolve existed.
6. Scheduled warm-up incidents (`spec.md` §3: "2-3 minor, auto-resolving").
7. Re-skin driven from the console, with move-don't-clone enforced there.

### Acceptance Criteria

1. A fleet can be created from the console, with per-role node counts and a checkbox per collector including `otel-collector`. MET.
2. Creation is lint-gated: a fleet failing the fidelity lint is never installed. MET.
3. The console can claim, without the token reaching disk, a log, or a child process's argv. MET.
4. Teardown disarms, stops the fleet for good, and archives the replay artifacts. MET.
5. Committed templates fill the create form. MET.
6. A running scenario can be escalated and the demo clock moved. MET.
7. Warm-up incidents run on a deterministic schedule and stop short of the hero demo. MET.
8. Re-skin from the console preserves every GUID. MET.

## Analysis

The console previously had five routes and read a single pre-existing `environment.yaml`. The environment renderer and the re-skin logic lived in `sim-plugin`, unreachable from `sim-console`.

Claim needed investigating rather than assuming: `netdata-claim.sh` exists, but `PUT /api/v2/claim` accepts token/rooms/url gated by a local-root proof file (`/var/lib/netdata/netdata_random_session_id`). Using the API keeps the token out of argv, which is world-readable via `ps`.

## Pre-Implementation Gate

Problem / root-cause model: the console was scoped in `SOW-0001` as an operate-what-exists surface, and that scoping was never revisited when the rest of the product caught up.

Evidence reviewed: `spec.md` §6 and the P0 checklist; `crates/sim-console/src/main.rs` (five routes, single `-e` argument); `netdata-claim.sh`; live probe of `PUT /api/v2/claim`.

Affected contracts and surfaces: console HTTP API, console UI, `sim-engine` (gains `describe` and `reskin`), environment format (`warmup_incidents`), plugin tick loop, `README.md`, `docs/QUICKSTART.md`, specs.

Existing patterns to reuse: `describe::render` for environment generation, `reskin::reskin` for renaming, `install-local.sh`'s ordering for install, the control file for scenario state.

Risk and blast radius: the console now writes under `/etc/netdata` and manages the plugin process as root. A create that half-succeeded would leave a fleet in an unknown state, so creation is lint-gated and install is atomic.

Sensitive data handling plan: the claim token is the only credential. It is never persisted, logged, or echoed in an error, and never reaches argv.

Implementation plan: share the renderer → provisioning module → console routes → UI → warm-up incidents in the plugin → templates, escalate, re-skin.

Validation plan: exercise every flow against the live agent through the console's own HTTP API, then the browser UI. Confirm GUID preservation on re-skin by hashing before and after.

Artifact impact plan: `README.md`, `docs/QUICKSTART.md`, `.agents/sow/specs/runtime-and-scenarios.md`.

Open decisions: resolved — user chose **A**, console runs as root.

## Implications And Decisions

1. **Console runs as root** (user decision A). Create writes under `/etc/netdata`, teardown manages the plugin process, and claim reads a root-only proof file. Shelling out to `sudo` per action would put a password prompt in the middle of a demo. Consequence handled: files the console creates in the checkout are chowned back to the directory's owner, or the SE could not edit their own environment afterwards.
2. **Collector checkboxes are per role, not global.** Every collector is offered on every role with that role's defaults pre-ticked, so `otel-collector` beside an application is one tick — which is exactly how an OTel agent is really deployed — while `postgres` is not silently forced onto every node.
3. **A template fills the form; it is never installed directly.** One code path builds every environment, so a template cannot drift into producing something the picker could not.
4. **Escalate moves the scenario clock rather than adding an intensity knob.** Severity is already a function of elapsed time, so advancing `started_at` walks the authored timeline. A separate intensity control could disagree with the ground-truth manifest; this cannot.
5. **Warm-up incidents are computed, not scheduled.** A pure function of `(seed, now)` keeps the replay promise, avoids the plugin writing `control.yaml` (the console owns it), and survives restarts with no state.

## Plan

Delivered as described in the gate.

## Execution Log

### 2026-08-08

- Moved `describe` and `reskin` from `sim-plugin` into `sim-engine` so the console and CLI build fleets through one implementation.
- Added `crates/sim-console/src/provision.rs`: catalogue, create, claim, teardown, templates, advance, re-skin.
- Added `Agent::put_json`, generalising the hand-rolled client to any method.
- Added `crates/sim-plugin/src/warmup.rs` and `warmup_incidents` to the environment format.
- Rewrote the console UI: New simulation (template picker, role table, collector checkboxes), Claim, Re-skin, Teardown; escalate/clock buttons on running scenarios.
- Enabled `warmup_incidents` by default in generated environments and the committed templates.

### Findings during implementation

- **`check_scenarios` rejected any fleet missing a role any scenario mentions.** `disk-fill` reaches the load balancer in its last step, opportunistically, so a fleet without an `lb` could not be created at all. Absent roles are now fatal only when named in `requires_roles`; the rest are reported as steps that will not fire here. Found by the first real use of the create form.
- **Installing over a running plugin failed with `ETXTBSY`.** Stopping first only narrows the race, because the agent rescans every 60s. Staging beside the target and renaming is atomic.
- **Claim reported success for a rejected token.** An already-claimed agent answers a refused claim with a healthy `status: online`, which read naively means "claimed". It now compares the claim id before and after and states plainly that nothing changed. This is the failure the console's own design note warns about: a button that silently does nothing.
- **Teardown did not stick.** It stopped the process but left the plugin file, so the agent relaunched it within 60s. Removing the file alone is equally wrong — a running collector keeps writing from a deleted file. Both, in that order.
- **Root-owned artifacts.** Running as root made every created `environment.yaml` root-owned and uneditable by the SE. Created files now inherit the owner of the directory they land in.
- **A test fixture, not the code, was wrong.** The GUID-uniqueness test wrote `- guid:` where real files carry `guid:` unprefixed; the production parser was correct.

## Validation

Acceptance criteria evidence (all against live agent `v2.10.0-1022-nightly`):

- **Create**: `POST /api/create` with `web x2 [nginx, otel-collector]`, `db x1 [postgres]`, `cache x1 [redis, otel-collector]` produced 4 nodes, passed the 6h lint with no semantic violations and no pinned signals, and installed. The installed `environment.yaml` carries `services: [nginx, otel-collector]` on exactly the ticked nodes, with `generator:`/`specs:`/`scenarios:` rewritten for the install directory and `otel-collector.yaml` copied in.
- **Lint gate**: a fleet whose scenario targets did not resolve was refused with the lint output and nothing was installed.
- **Claim**: wrong Space name refused ("must end with '(Simulated Demo)'"); empty token refused; a valid-shaped request against an already-claimed agent correctly reported `claimed: false` and named the existing claim id. The token appears in **no** config file, environment file, or console log (`grep` returned zero occurrences).
- **Escalate / clock**: `disk-fill` triggered at 0%, `advance +900s` → 56%, `advance -300s` → 37%.
- **Re-skin**: `globex-retail` → `initech` renamed all 5 nodes; md5 of the GUID block **identical** before and after.
- **Teardown**: disarmed scenarios, removed the plugin and stopped its process, archived environment + scenario manifests to `archive/<name>-<stamp>/`, and reported the two Cloud-side steps as MANUAL.
- **Templates**: all six committed environments offered with their role composition.
- **Warm-up incidents**: with the clock pinned inside a window, the plugin logged `warm-up incident 'warmup-db-replication-lag' running (1m of 20m)`; pinned outside, no incident. Unit tests cover one incident per 6h slot, determinism for a seed, divergence across seeds, trimming to the opening steps, and no incidents for a fleet lacking the required roles.

Tests: **168 passed, 0 failed**. `cargo clippy --all-targets`: clean. `cargo fmt --check`: clean.

Real-use evidence: every flow above was driven through the console's own HTTP API while it ran as root against the live agent, and the resulting UI was rendered and inspected in a browser.

Same-failure search: the ETXTBSY class was checked against `install-local.sh`, which stops the previous PID before copying and is only used interactively; the atomic-rename fix is applied in the console path where the race is real. The root-ownership class was checked across every path the console writes — environment files and archive directories are the only ones outside `/etc/netdata`, and both now inherit their directory's owner.

Sensitive data gate: no credentials in any committed artifact. The claim token is memory-only and cleared from the DOM after use.

Artifact maintenance gate:

- `AGENTS.md` — no change needed; no new project-wide guardrail.
- Runtime project skills — no change needed; `project-live-validation` already prescribes running the operator surface rather than reading it, which is exactly how four of the six defects above were found.
- Specs — `.agents/sow/specs/runtime-and-scenarios.md` gains warm-up incidents.
- End-user docs — `README.md` gains a console section and warm-up incidents; `docs/QUICKSTART.md` gains the console as an alternative to steps 1-4.
- SOW lifecycle — completed, in `done/`.

## Outcome

`spec.md` §6's five-stage lifecycle is implemented: create, claim, warm up, demo, tear down, plus re-skin, from one screen.

Against the P0 checklist, what remains is one item deferred by the user's own decision — a 50+ node scale test, which runs on a different machine — and the eval gym, which is P2. Everything else on that list is delivered.

## Lessons Extracted

- **Running the surface finds what reading it cannot.** Four of the six defects here — the scenario-role check, ETXTBSY, the lying claim button, the teardown that did not stick — were invisible in review and immediate on first use. The create form found the scenario-role bug on its very first invocation.
- **A status field is not an acknowledgement.** The agent answers a claim with current state, not "your request succeeded". Any integration that reads a status as confirmation of an action it just requested is guessing.
- **Deciding privilege changes the artifacts, not just the process.** Choosing to run as root immediately produced root-owned files the user could not edit. Privilege decisions carry ownership consequences that show up one step later.

## Followup

- `SOW-0003` (graceful scenario recovery) is now more visible: escalate makes it easy to reach a deep fault, and resolve still snaps back in one tick.
- `SOW-0004` (packaging, SE quickstart, a real `--llm` provider run) unchanged.
- 50+ vnode scale test remains deferred to a non-laptop machine by user decision.

## Regression Log

None yet.
