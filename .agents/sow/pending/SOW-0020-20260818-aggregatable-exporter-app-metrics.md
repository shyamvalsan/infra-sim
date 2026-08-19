# SOW-0020 - Aggregatable exporter app metrics on the container path

## Status

Status: open

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: queued behind SOW-0019 per user decision; probe task outstanding.

## Requirements

### Purpose

Application-tier metrics scraped from the simulated Prometheus exporters must aggregate across the fleet in Netdata's grouped views, and must exist at all on the container path where simulations actually run.

### User Request

External tester feedback (name withheld per artifact rules): each vnode appears as its own Prometheus exporter and no aggregated metrics are available. The user wants aggregated metrics to work properly. Decisions recorded by the user:

1. Fix both defects, long-term-best: shared `app` in the go.d jobs AND wire `--exporters` into the containerized path (1. B).
2. Shared app name is a fixed literal, stable across re-skins (2. A).
3. Re-skin stability of generated chart identities verified by a live probe before implementation (3: ok).
4. This SOW executes after the labels SOW (4. A).

### Assistant Understanding

Facts:

- The exporter server is sound: one listener on :19998, `GET /metrics/<hostname>` per node, application-level metrics only, counters integrated per scrape, summaries carrying `_sum`/`_count` (`crates/sim-plugin/src/exporters.rs`).
- Containerized simulations start only `--logs` and `--otlp`; nothing starts `--exporters` and no go.d config is written inside the container (`scripts/sim-docker.sh:216-221`). The console's `exporters` flag is `#[allow(dead_code)]` with a tracked-not-done comment (`crates/sim-console/src/provision.rs:74-82`).
- On the local (non-container) path, the console writes go.d jobs with a unique per-hostname `name:` and no `app:` (`provision.rs:648-654`).
- The job name is the chart-context app segment when `app:` is unset: "Application name used as the app segment of chart contexts ('prometheus.{app}.{metric}'). When unset... falls back to the job name" (netdata/netdata @ c23face0bd94 `src/go/plugin/go.d/collector/prometheus/config_schema.json:44-48`; `runtime.go:115-139` `resolveApp`).
- Aggregated/grouped views key off chart context; per-node contexts (`prometheus.infra_sim_app_<hostname>...`) therefore can never combine across nodes.
- The plugins.d system metrics use identical contexts on every vnode and aggregate fine; the gap is specific to the scraped application tier.
- go.d reads its vnode registry once at startup (netdata/netdata @ c23face0bd94 `src/go/plugin/agent/setup.go:179`); the local path already reloads go.d after writing vnode config (`provision.rs:672-679`).

Inferences:

- Setting a shared literal `app:` (e.g. `infra_sim_app`) on every job should collapse contexts to `prometheus.infra_sim_app.<metric>` across nodes, restoring aggregation — subject to the chart-ID probe below.
- Container wiring can follow the `--logs`/`--otlp` pattern exactly: start before/with the agent, config written inside the container.

Unknowns:

- Chart-ID composition under a shared `app:`: if go.d derives chart IDs from the job name (which embeds the hostname), a re-skin renames hostnames and may churn chart IDs, orphaning scraped-chart history. Probe first, per the project rule; if confirmed, key jobs by something re-skin-stable while `vnode:` still points at the (renamable) host.
- Whether the exporter process should be started unconditionally in containers or behind a create-time toggle (the UI flag exists but is dead). Lean unconditional-with-toggle-fallback; decide after the probe.

### Acceptance Criteria

- A containerized simulation exposes per-node `/metrics/<hostname>` endpoints reachable from inside the container, with go.d scraping them and attributing charts to the correct vnodes.
- Scraped app metrics share one context across the fleet (verified via `/api/v1/charts` contexts on a live agent), and Netdata's grouped/aggregated views combine them across nodes.
- A re-skin does not orphan scraped-chart history (GUIDs and any chart identifiers stable) — probe-verified before implementation, regression-tested after.
- Local path keeps working with the same shared app; the marker/guard conventions for writing into `/etc/netdata/go.d` are preserved.
- Teardown leaves no exporter process behind (per the teardown-kills-processes rule).

## Analysis

Sources checked:

- `crates/sim-plugin/src/exporters.rs`, `crates/sim-plugin/src/main.rs` (`--exporters` mode), `crates/sim-plugin/src/otlp_runtime.rs` (sibling-process pattern)
- `crates/sim-console/src/provision.rs` (`exporters` module: vnode conf, go.d jobs, guard markers, go.d reload)
- `scripts/sim-docker.sh` (container start: which processes launch, config mounting)
- netdata/netdata @ c23face0bd94: `src/go/plugin/go.d/collector/prometheus/config_schema.json`, `runtime.go` (`resolveApp`), `chart_meta.go`, `src/go/plugin/agent/setup.go` (vnode registry load-once)
- `docs/operating.md` (exporters section — currently local-path only)

Current state:

- Exporters exist as a binary mode with console wiring on the local path only, and even there the generated job names defeat cross-node aggregation.

Risks:

- Writing go.d config inside the container: the container uses netdata's stock image; writes must survive the agent's startup order (go.d reads vnode registry at startup) — start exporter and write config before the agent, or force a go.d reload after.
- Port collisions inside the container are a non-issue (loopback inside its own netns), but the console must not assume :19998 is free on the host.
- Shared `app:` changes contexts for any existing local-path user; scraped charts effectively get new contexts once (one-time migration, not per-re-skin) — document it.
- The dead `exporters` UI flag must not stay dead if containers run exporters unconditionally; UI honesty rule.

## Pre-Implementation Gate

Status: blocked (probe outstanding; queued behind SOW-0019)

Problem / root-cause model:

- No aggregation because per-hostname job names become the context app segment (verified in agent source), and no app metrics at all on the container path because nothing starts the exporter or writes scrape config there.

Evidence reviewed:

- As listed under Analysis. The load-bearing agent behaviors (job-name→context fallback, vnode registry load-once) are source-verified in netdata/netdata @ c23face0bd94.

Affected contracts and surfaces:

- `sim-docker.sh` create/telemetry/teardown: start `--exporters`, write go.d + vnode conf inside the container, kill on teardown.
- `crates/sim-console/src/provision.rs` `exporters` module: shared `app` literal in job generation; possibly job-naming scheme change after the probe; container path invocation.
- `ui.html`: exporters toggle becomes honest (or removed if unconditional).
- `docs/operating.md` + README exporters wording.
- No exporter protocol/output change expected (`exporters.rs` families stay).

Existing patterns to reuse:

- Container sibling-process pattern from `--logs`/`--otlp` in `sim-docker.sh`.
- Marker/guard discipline for files written into netdata's own config tree (`provision.rs` `exporters` module constants).
- go.d reload-by-plugin-restart already implemented locally.

Risk and blast radius:

- Container lifecycle changes touch every new simulation — the highest-traffic path. Mitigate by mirroring the logs/OTLP wiring exactly and validating a full create→scrape→re-skin→teardown cycle on a live container.
- Context change is a one-time visible migration for local-path fleets; acceptable, documented.

Sensitive data handling plan:

- No credentials involved. Container names/hostnames stay synthetic per repo rules. Probe evidence recorded as sanitized curl output (contexts, chart IDs), no tokens.

Implementation plan:

1. Probe on a live containerized sim: hand-start `--exporters`, write go.d jobs with shared `app:` + re-skin the fleet, inspect chart IDs/contexts before and after rename. Decides the job-naming scheme (probe decision point).
2. Renderer/config: job generation with shared `app:` literal and probe-chosen job naming.
3. Container wiring: exporter start + config write + agent ordering in `sim-docker.sh`; teardown kill; telemetry status line for the scrape path.
4. Console/UI honesty for the toggle; docs updates.
5. Live validation + close-out gates.

Validation plan:

- Unit: job-config generation snapshot tests (shared app, stable naming).
- Live container: create → exporter endpoints reachable → go.d charts present on correct vnodes with shared context → aggregated view combines nodes → trigger a scenario → scraped metrics move → re-skin → chart continuity → teardown leaves nothing running.
- Negative: container without exporters (if toggle retained) stays clean; no go.d config leakage after teardown.

Artifact impact plan:

- AGENTS.md: likely a container-lifecycle note if exporters become default-on; confirm at close.
- Runtime project skills: `project-live-validation` may gain a scraped-metrics check; confirm at close.
- Specs: `.agents/sow/specs/runtime-and-scenarios.md` or the container spec section for exporter wiring.
- End-user/operator docs: `docs/operating.md` exporters section rewrite; README wording.
- End-user/operator skills: none affected.
- SOW lifecycle: executes after SOW-0019 completes (user decision 4A); one SOW at a time.

Open-source reference evidence:

- netdata/netdata @ c23face0bd94 — `src/go/plugin/go.d/collector/prometheus/config_schema.json:44-53` (`app`, `vnode` semantics), `src/go/plugin/go.d/collector/prometheus/runtime.go:115-139` (`resolveApp` fallback to job name), `src/go/plugin/agent/setup.go:179` (vnode registry read at startup).

Open decisions:

1. Probe outcome → job-naming scheme (per-hostname vs re-skin-stable key). Blocking; resolved by the probe, not by the user.
2. RESOLVED (user, 2026-08-18): exporters run default-on in containers, with a toggle to disable at create time. The existing dead UI flag becomes an honest opt-out.

## Implications And Decisions

1. Fix scope: both defects, long-term-best (user: "1B").
2. Shared app name: fixed literal stable across re-skins (user: "2A").
3. Chart-identity stability under re-skin: probe-first, then implement (user: "3 ok").
4. Sequencing: queued behind SOW-0019 (user: "4A").
5. Exporters in containers: default-on with a create-time toggle to disable (user: "2ok", 2026-08-18).

## Plan

1. Live probe (containerized sim): shared-app contexts, chart-ID stability across re-skin. Decides naming scheme. Highest uncertainty, first.
2. Job-config generation changes + unit tests.
3. Container lifecycle wiring in `sim-docker.sh` (+teardown kill, telemetry status).
4. Console/UI toggle honesty + docs.
5. Full live validation cycle; close-out gates.

## Execution Log

### 2026-08-18

- Root cause identified and decisions recorded from external tester feedback; agent-source verification of the job-name→context fallback; no code touched.

## Validation

Acceptance criteria evidence:

- Pending.

Tests or equivalent validation:

- Pending.

Real-use evidence:

- Pending.

Reviewer findings:

- Pending.

Same-failure scan:

- Pending.

Sensitive data gate:

- Pending.

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

Pending.

## Lessons Extracted

Pending.

## Followup

None yet.

## Regression Log

None yet.
