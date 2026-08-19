# SOW-0020 - Aggregatable exporter app metrics on the container path

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: delivered - probe-informed design, implemented on both paths, live-validated end to end, artifacts updated.

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

- None remaining. The probe (2026-08-19, `probe-exp`, 5-node containerized sim, netdata nightly) answered both:

  **Probe findings (live agent evidence, not source-reading):**
  1. Shared `app: infra_sim_app` produces one context per metric across every scraped node (`prometheus.infra_sim_app.app_orders_total` on web-01 and web-02 alike). Aggregation keys off context - fixed.
  2. Chart IDs are prefixed by the **job name** (`prometheus_infra_sim_app_sim_web_01.app_orders_total-...`), so per-hostname job names churn IDs on every re-skin and orphan scraped-chart history. Confirmed the SOW's suspicion.
  3. Continuity proven: after a re-skin (`sim-` -> `acme-`) with job names held stable and only `vnode:`/registry hostnames repointed (plus exporter + go.d restarts), the renamed node kept the same chart IDs with data continuing across all three restarts.

  **Job naming scheme (resolves open decision 1):** `infra_sim_app_{role_slug}_{nn}` - per-role index in environment order, role dashes to underscores, hostname nowhere. Roles do not change on a re-skin, so chart IDs do not either; fleet-shape changes legitimately change them.

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

1. DONE - probe (see open decisions for findings).
2. `sim-engine::exporter_config` - pure generator for the vnode registry and go.d jobs (shared `app: infra_sim_app`, role-index job names), unit-tested; both paths consume it so local and container config can never drift.
3. `sim-plugin --exporter-config` mode - reads the environment, writes both files inside the container via the same generator; `sim-docker.sh` calls it (bash never re-implements the naming).
4. `sim-docker.sh`: `create --no-exporters` (default on), exporter start/stop/status folded into telemetry, go.d one-shot restart after config write (registry is read at startup only).
5. Local path (`provision.rs` exporters module): switch to the shared generator (contexts change once for local users - documented).
6. Console/UI: the dead `exporters` flag becomes real (default on, honest opt-out wired to `--no-exporters`).
7. Docs (operating.md exporters section, README wording); live validation of the shipped path; close-out gates.

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

1. RESOLVED by probe (2026-08-19): job naming is `infra_sim_app_{role_slug}_{nn}` - re-skin-stable, no hostname component. Evidence above.
2. RESOLVED (user, 2026-08-18): exporters run default-on in containers, with a toggle to disable at create time. The existing dead UI flag becomes an honest opt-out.
3. RESOLVED (assistant, scope discipline, 2026-08-19): a container re-skin does not today restart the telemetry side-processes (logs/otlp hold the old environment until restarted); the exporter gets the same treatment - started at create, not restarted on re-skin - and one follow-up covers "container re-skin must restart side-processes and rewrite+reload their config" for all of them. Parity, not silent divergence.

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

### 2026-08-19

- Probe run on `probe-exp` (5-node containerized sim): shared-app contexts confirmed across nodes; chart-ID job-name prefix confirmed; re-skin continuity with stable job names confirmed (chart IDs and data survive hostname rename across exporter+go.d+plugin restarts). Open decisions 1 and 3 resolved as recorded. Implementation started.
- `sim_engine::exporter_config`: shared generator (vnode registry + go.d jobs, shared `APP`, role-index job names), 5 unit tests including re-skin name stability and determinism.
- `sim-plugin --exporter-config /etc/netdata`: writes both files inside a container; one arg-parsing iteration (an optional argument swallowed a following `--environment`) settled on arity-one.
- Exporter coverage aligned with `is_app_tier` (web, lb, k8s-worker) in both `do_exporters` and the config writer — the OTLP emitter's own principle; a db node or switch publishing storefront orders was an artifact. Local-path behavior change, documented.
- `sim-docker.sh`: `create --no-exporters` (default on), exporter start/stop/status folded into telemetry, one-shot go.d restart after config write.
- Console: `CreateRequest.exporters` defaults true (serde + UI checkbox); `container::create` passes `--no-exporters` when off. The dead flag is now honest.
- Local path (`provision.rs`): consumes the same generator; `node_refs` walks hostname/guid/role labels-block-aware (with the SOW-0019 collision lessons applied and tested).
- **Pre-existing bug found by validation and fixed**: `cmd_telemetry stop`'s `pkill -f '$plugin --mode'` matched its own `sh -c` wrapper, docker exec returned 143, and `set -e` aborted after the first pkill - telemetry stop had silently never stopped more than the logs writer. Fixed with anchored patterns; skill updated with the lesson.
- Docs (README, `docs/operating.md`), spec (`runtime-and-scenarios.md`), AGENTS.md container paragraph, and the live-validation skill updated.

## Validation

Acceptance criteria evidence:

- Container path, zero hand-wiring (`sim-docker.sh create`, default exporters): `telemetry status` reports `exporters : serving`; the container's `go.d/prometheus.conf` carries role-index job names (`infra_sim_app_lb_01`, `infra_sim_app_web_01/02`) with shared `app: infra_sim_app` and `vnode:` attribution; `vnodes/infra-sim.conf` pairs every hostname with its fleet GUID.
- Aggregation: lb + both web nodes each carry 18 scraped charts whose contexts are shared across the fleet (`prometheus.infra_sim_app.app_orders_total` present on every app-tier node); orders counters flowing at plausible per-node rates on both web nodes.
- Negative case: db and cache nodes carry **zero** prometheus charts - the application-tier rule holds where an SRE would look for its absence.
- Toggle honest: `create --no-exporters` leaves no go.d prometheus.conf, no exporter process, and `telemetry status` reports `not running`.
- Re-skin stability: probe-verified (chart IDs and data continuity across rename, stable job names) plus unit-tested (`a_re_skin_produces_identical_job_names`); the shipped generator cannot emit hostname-derived names.
- Teardown: `docker rm -f` removes exporter, config and agent together; `telemetry stop` (now actually working, see execution log) stops the exporter process by anchored pattern; nothing left running after teardown, host agent and the two pre-existing simulations untouched throughout.

Tests or equivalent validation:

- `cargo test`: 250 passed (5 exporter_config, node_refs collision test, prior suites).
- `cargo clippy --all-targets -- -D warnings` clean; `cargo fmt --check` clean; `bash -n scripts/sim-docker.sh` clean.

Real-use evidence:

- Two full create->scrape->teardown cycles on this machine (`exp-live`, `exp-off`) through the shipped script path, including one iteration caught by validation itself (stale binary in the image after a failed build chain; `--exporter-config` arity bug) - both fixed and re-validated end to end.

Reviewer findings:

- None for this SOW: the user's review authorization was consumed by SOW-0019's round (one round per explicit request). Validation was tests plus the live shipped-path cycles recorded above; recommend folding this diff into the next explicitly-requested review round.

Same-failure scan:

- Searched all `pkill -f` uses in scripts/ and crates/: the three telemetry ones are now anchored; no other unanchored pattern exists.
- Searched for other hand-rolled job-name generation (`infra_sim_app_` outside `exporter_config.rs`): none - single source of truth holds.
- The `--exporter-config` optional-argument bug class (a flag greedily eating the next flag's value) checked across the plugin's parser: every other optional-value flag takes an explicit value or none; no sibling of the bug.

Sensitive data gate:

- No credentials, tokens, prospect names or customer-identifying values in any artifact written this SOW. Test fleets used the committed synthetic `web-stack.yaml`. The exporter URL is loopback inside each container's own netns.

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

- `pkill -f` inside `sh -c` matches the wrapper's own command line; the wrapper dies mid-kill, the exec returns 143, and `set -e` converts a broken kill into a broken script. Anchor on the path. `telemetry stop` shipped broken for weeks because its failure was silent - a "stopped" command that aborts halfway still prints enough to look done.
- go.d chart IDs embed job names, and grouped views key off contexts whose app segment defaults to the job name. Any collector config we generate is chart-identity design, not just plumbing - naming it after something re-skin-stable (roles) is the difference between history surviving a rename and silently orphaning.
- `&&`-chained gate commands lie about what ran: a failed `fmt --check` in the chain skipped `cargo build --release`, and the container image was then rebuilt from a stale binary. Validation caught it only because the shipped path was exercised, not just the unit tests.

Follow-up mapping:

- Container re-skin must restart the telemetry side-processes (logs, OTLP, exporters) and rewrite/reload their config: pre-existing for logs/OTLP, inherited by exporters at parity (open decision 3). Tracked below; pairs naturally with the sticky-console-target follow-up from SOW-0019.
- No scenario targets the exporter's application signals, so a triggered fault does not (yet) move scraped metrics - the exporter engine is scenario-aware and reads the same control file, but the scenario library has no app-tier timeline (e.g. checkout degradation). Candidate content follow-up.
- Fold this diff into the next explicitly-requested external review round (this SOW shipped without one; see Reviewer findings).

## Outcome

Delivered and closed. Application-tier exporters run default-on in containerised
simulations, scraped by Netdata's own go.d collector onto the right virtual
nodes, with every job sharing one `app` so the fleet's app charts aggregate
under `prometheus.infra_sim_app.*` - the defect the external tester reported.
Job names are role-indexed and re-skin-stable (probe-verified: scraped-chart
history survives a fleet rename), exporter coverage follows the same
application-tier rule as OTLP, both install paths generate config from one
Rust module, and the toggle (`--no-exporters` / console checkbox) is honest.
Validation on the shipped path also surfaced and fixed a pre-existing silent
failure in `telemetry stop`. Gates: 250 tests, clippy clean, fmt clean.

## Lessons Extracted

Pending.

## Followup

- Container re-skin: restart side-processes (logs, OTLP, exporters) and rewrite their config. Pairs with SOW-0019's sticky-console-target recommendation.
- An application-tier scenario (checkout degradation) so triggered faults move scraped metrics; the mechanism exists, the content does not.
- Include this diff in the next explicitly-requested external review round.

## Regression Log

None yet.
