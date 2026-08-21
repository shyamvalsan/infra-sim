# SOW-0024 - Self-monitoring: health endpoint, host alerts, runbook

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: approved direction (user requirement 3 of hosting prep, decision "4A": self-monitoring pack - host-agent alerts + runbook; usage telemetry deferred). Implementation started 2026-08-21.

## Requirements

### Purpose

The hosted box can be watched and its failures found: a machine-readable console health endpoint, a Netdata alert pack for the failure modes a simulation host actually has, a one-command installer the SRE runs on the hosted box, and a runbook that says what to do when each alert fires. Fourth and final hosting-prep SOW.

### User Request

"We need monitoring to find out if there are issues, so we can fix them etc." Decision 4A: self-monitoring pack on the host agent, alerts + runbook; usage telemetry deferred.

### Assistant Understanding

Facts:

- The host agent (Netdata) already charts disk, memory, CPU, and can chart docker containers via its go.d docker collector (ships disabled unless configured).
- The console already emits a TTL-sweep heartbeat line hourly (SOW-0021) - log-only today.
- The console has no health endpoint; an external monitor must parse `/api/status` (and on a tokened host would need the token) to learn anything.
- Failure modes specific to this workload: state-dir disk filling (journals + agent DBs), simulation count hitting the cap, the port range (19900-19990) exhausting, the console process dying, TTL sweeps failing repeatedly, docker itself unhappy.
- The operator's own agent on this dev machine is in active use; hosted-box configuration must therefore ship as an installer, not be applied here.

Inferences:

- `GET /api/health` must be **unauthenticated** (it carries no fleet data) so any uptime checker can poll it; everything else stays behind the token.
- Alert files are additive (`health.d/infra-sim.conf`) and namespaced; a `netdatacli reload-health` applies them without an agent restart.
- Port-exhaustion and simulation-cap states are visible in the health endpoint (counts), while disk/memory/container health belong to netdata's own charts and alerts - the split is: netdata watches the host, the health endpoint reports the console.

Unknowns:

- Whether the hosted box's agent image matches the nightly alert syntax used here - the alert file sticks to long-stable stock syntax (lookup/on lines), validated against a running agent in this SOW.

## Acceptance Criteria

- `GET /api/health` answers unauthenticated (even with a token set): process uptime, auth mode, simulation count vs cap, disk used vs cap, docker reachable, seconds since the last successful sweep, and degraded flags - live-verified in both auth modes.
- A Netdata health config (`monitoring/infra-sim-host.conf`) alerts on: state-dir disk filling (warning/critical), the simulation count approaching the cap (from a file the console publishes), memory pressure, and docker daemon trouble - syntax-validated against a real running agent (alarms visible, no parse errors).
- The console publishes a tiny metrics file for the alert pack to consume (simulation count, disk used) - file-based so netdata's `filestat`-style lookup or a simplest alarm can read it without new collectors; refreshed on each status poll.
- `scripts/host-monitoring.sh install` (run by the hosted box's SRE): installs the alert file, enables docker charts when absent, reloads health; idempotent; prints what it did. Not run against the operator's personal agent in this SOW.
- `docs/hosting.md` "What to watch" expands into the runbook: each alert, its meaning, the fix.
- Gates green; no mutation of the operator's own agent on this machine.

## Analysis

Sources checked:

- `crates/sim-console/src/main.rs` (route table, auth layer scope, sweep loop - where last-sweep state must be recorded)
- Netdata agent source (mirrored): `src/daemon/commands.c` `reload-health`; stock `health.d` examples for stable syntax (`lookup`, `on`, `calc`/`warn`/`crit` lines)
- `docs/hosting.md` (the "What to watch" stub this SOW replaces)
- SOW-0021/0022 outcomes (budgets, sweep heartbeat, token)

Current state:

- Log heartbeat only; nothing machine-readable; no alerts.

Risks:

- Publishing state for alerts: a file under the state dir (`/var/lib/infra-sim/health.json`) read by alarms - netdata's `filestat` charts sizes/mtime, not contents. Honest fallback: the *file's mtime* proves the console is alive (stale file = console dead), while counts ride `/api/health` for external monitors; in-agent content alarms would need a custom collector, which is rejected as machinery for v1. The alert set is therefore host-level (disk/mem/docker) plus an mtime-based "console silent" alarm - all stock syntax.
- Alert thresholds are guesses until a real host runs hot; defaults documented as tunable in the conf's comments.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- A hosted service nobody watches fails quietly: the console can die, the disk can fill with journals, sweeps can fail - and today nothing would notice. The pieces that make noticing possible are a machine-readable health signal, host-level alerts on the real failure modes, and a runbook connecting alert to action.

Evidence reviewed:

- As listed under Analysis; SOW-0021's sweep loop and heartbeat log; the auth layer's `/api`-only scope (health must join the shell in the unauthenticated set).

Affected contracts and surfaces:

- Console: `GET /api/health` (unauthenticated - the auth layer exempts it alongside the shell); AppState gains `last_sweep` (std Mutex) updated by the sweeper; `/api/status` writes `health.json` into the state dir on each poll (mtime = liveness).
- New `monitoring/infra-sim-host.conf` (disk, memory, docker, console-silence alarms).
- New `scripts/host-monitoring.sh` (install/verify; SRE-run).
- `docs/hosting.md` runbook expansion.
- No engine/plugin/spec changes.

Existing patterns to reuse:

- The marker-file pattern (`pinned`) for state-dir artifacts; `json_err`/`json!` response shapes; the auth layer's path check (extend the exemption); budget arithmetic for cap fractions.

Risk and blast radius:

- Console-only plus operator-run script; the operator's agent untouched here. The unauthenticated `/api/health` is a deliberate, minimal disclosure (counts and timestamps, no names, no URLs) - the SOW records why.

Sensitive data handling plan:

- health.json and the endpoint carry counts and timestamps only - no fleet names, owners, URLs or tokens. Alert comments use synthetic paths.

Implementation plan:

1. `/api/health` + sweep-state recording + health.json publication (on status poll).
2. `monitoring/infra-sim-host.conf` alert pack.
3. `scripts/host-monitoring.sh` installer.
4. Runbook expansion; live validation (both auth modes, throwaway agent for alert syntax); gates; close.

Validation plan:

- Live: `/api/health` without token under a tokened console (200) while `/api/status` stays 401; fields present; `last_sweep` advances after a sweep; health.json mtime fresh.
- Alert syntax: `docker run --rm -d netdata/netdata` (nightly, matching the simulation image), install the conf inside it, `netdatacli reload-health`, `/api/v1/alarms` shows the infra-sim alarms, error.log clean; container removed.
- `bash -n` on the installer; idempotence by running install twice in the container.
- Gates: cargo test/clippy/fmt.

Artifact impact plan:

- AGENTS.md: one line on the health endpoint's unauthenticated scope and why.
- Runtime project skills: unaffected.
- Specs: Shared hosting section gains a monitoring paragraph.
- End-user/operator docs: hosting.md runbook (the deliverable).
- End-user/operator skills: none.
- SOW lifecycle: final hosting-prep SOW.

Open-source reference evidence:

- netdata/netdata (mirror) `src/daemon/commands.c` - `reload-health` behavior; `src/health/health.d.conf` examples for syntax shape.

Open decisions:

- None blocking. Recorded: `/api/health` unauthenticated (counts/timestamps only); file-mtime liveness rather than an in-agent content collector (machinery rejected for v1); thresholds documented as tunable.

## Implications And Decisions

1. Self-monitoring pack (user: "4A"): health endpoint + host alerts + installer + runbook; usage telemetry explicitly deferred.
2. `/api/health` unauthenticated, minimal fields (engineering default, recorded here and in AGENTS.md).
3. Alert thresholds are first guesses, tunable in the conf (recorded in its comments).

## Plan

1. Health endpoint + state + health.json.
2. Alert pack.
3. Installer script.
4. Runbook + validation + close.

## Execution Log

### 2026-08-21

- SOW written; implementation started.
- `GET /api/health` (unauthenticated alongside the shell; counts/timestamps only): ok flag (sweep fresh + under sim cap), uptime, auth mode, simulations vs cap, disk vs cap, docker reachability, seconds since last sweep. Sweeper records completion; a sweep that saw zero simulations with docker unreachable is recorded as a failure, not an empty host.
- Alert pack scope corrected against the agent source mid-SOW: stock netdata already alarms on disk (`health.d/disks.conf` `disk_space_usage`) and memory (`ram.conf`) - duplicating them would create two sources of truth for the same threshold. The pack ships exactly one alarm, `infra_sim_unhealthy_containers` on `docker.containers_health_status` (chart verified in `charts.go`), with console-side signals deliberately left on `/api/health` for external monitors. In-agent content alarms rejected as machinery for v1.
- `scripts/host-monitoring.sh install|verify`: idempotent alert install + docker-chart enable from either stock template location (the netdata image keeps it under `/usr/lib/netdata/conf.d`, found by validation), health reload, verify mode reporting what the agent took.
- `docs/hosting.md` "What to watch" expanded to the three-layer runbook: stock agent alarms + our pack (table: signal, meaning, fix), the health endpoint with its JSON shape, and the log heartbeat.
- Validation caught: a template with no matching chart does not materialize (first throwaway agent had no docker socket - re-run with the socket mounted); the release binary needed rebuilding before the health-endpoint test (same `&&`-chain lesson as SOW-0020, caught this time before concluding anything).

## Validation

Acceptance criteria evidence:

- `/api/health` under a tokened console: 200 without a token (fields verified: ok=true, simulations=1, docker=true, last_sweep_secs_ago small and advancing) while `/api/status` stays 401 - the unauthenticated surface is health alone.
- Alert syntax against a real running agent (throwaway `netdata/netdata:latest` container with the docker socket mounted, removed afterwards): after install + `netdatacli reload-health`, `/api/v1/alarms?all` lists `docker_local.healthy_containers.infra_sim_unhealthy_containers` (UNINITIALIZED - normal pre-lookup); the `docker.containers_health_status` chart exists on that agent; error.log clean of health parse failures.
- Installer: `bash -n` clean; docker-chart stock-template search covers both known locations; never run against the operator's own agent in this SOW.
- Runbook: hosting.md "What to watch" now the three-layer table + endpoint shape + log path.

Tests or equivalent validation:

- `cargo test` 258 passed; clippy `-D warnings` clean; `cargo fmt --check` clean; `bash -n` clean on the installer.

Real-use evidence:

- Health endpoint exercised on this machine against the real console in both auth modes; the alert pack exercised inside a real (throwaway) agent container; the user's own agent and simulation untouched throughout.

Reviewer findings:

- Pending (fold into the next explicitly-requested round).

Same-failure scan:

- Route table checked for other endpoints assumed unauthenticated: only `/` and `/api/health` bypass the layer, both deliberate and documented.
- Alert pack checked against stock alarms for overlap: none (single alarm, docker-specific).

Sensitive data gate:

- Health endpoint and alert comments carry counts, timestamps and synthetic paths only - no names, owners, URLs, tokens. Test token session-scratch, never committed.

Artifact maintenance gate:

- AGENTS.md: `/api/health` scope note (unauthenticated, minimal, why).
- Runtime project skills: unaffected.
- Specs: Shared hosting section gained the self-monitoring paragraph.
- End-user/operator docs: `docs/hosting.md` "What to watch" runbook (the deliverable).
- End-user/operator skills: none.
- SOW lifecycle: fourth and final hosting-prep SOW; queue returns to the user.

Specs update:

- `runtime-and-scenarios.md` - self-monitoring paragraph in Shared hosting.

Project skills update:

- Pending.

End-user/operator docs update:

- `docs/hosting.md` - "What to watch" runbook expansion.

End-user/operator skills update:

- Pending.

Lessons:

- Check what the stock product already alarms on before writing an alert pack: the draft duplicated disk and memory alarms that netdata has shipped for years, and the correct pack turned out to be one alarm, not four. Scope is discovered by reading the stock config, not by listing failure modes.
- A health template with no matching chart silently does not exist - validating alert syntax requires an agent that actually has the chart (docker socket mounted), not just any agent.

Follow-up mapping:

- Usage telemetry (creates/teardowns/errors as metrics): deferred with decision 4A, tracked in Followup.
- In-agent content alarms: rejected for v1 (machinery); `/api/health` serves external monitors.

## Outcome

Delivered and closed. A hosted box can now be watched from three directions:
the host agent (stock alarms + the one simulation-specific alarm the pack
adds, installed by one idempotent command), any uptime monitor (the
unauthenticated, minimal `/api/health` with sweep freshness, caps and docker
reachability), and the console logs (the sweep heartbeat). The runbook in
`docs/hosting.md` maps each signal to its fix. This closes the fourth and
final hosting-prep SOW: the simulator is now shareable (0021), gated (0022),
self-explaining (0023) and observable (0024).

## Lessons Extracted

Pending.

## Followup

- Usage telemetry (create/teardown/error counts as metrics): deferred with decision 4A; revisit with the Cloud-team SSO conversation.
- In-agent content alarms (simulation count inside netdata charts): rejected for v1 as machinery; the health endpoint serves external monitors meanwhile.

## Regression Log

None yet.
