# Runtime, vnodes, scenarios and logs

How a simulated fleet reaches Netdata, and how faults are injected. Describes
current reality as of `SOW-0001`.

## The plugins.d path

`infra-sim` is a Netdata external plugin (`/etc/netdata/custom-plugins.d/`).
The agent rescans for new plugins every 60s; no restart is needed.

Per node it emits `HOST_DEFINE` / `HOST_LABEL` / `HOST_DEFINE_END`, then
`CHART` / `DIMENSION` / `CLABEL` / `CLABEL_COMMIT` declarations, then `BEGIN` /
`SET` / `END` per tick.

**Verified against a live agent, not assumed:** a vnode's dashboard is exactly
what the plugin emits — the agent adds only its own ML charts (8 per node).
This changed the P0 estimate by roughly an order of magnitude and is why this
project runs probes before designing.

### The GUID is the identity

Changing a node's `guid` orphans its history. Changing its `hostname` renames
it in place, preserving history, trained ML models and alert log. The entire
re-skin workflow rests on that distinction, and `reskin.rs` refuses to emit an
environment where any GUID changed.

Two environments sharing a GUID cannot both be claimed — the second claim takes
over the first node's identity — so `check_guid_uniqueness` refuses to write a
new environment carrying GUIDs already used by a sibling file.

### Chart labels are load-bearing

Netdata health templates filter on `chart labels:`. Without `CLABEL` /
`CLABEL_COMMIT`, stock templates silently never attach — 41 alarms looked
perfectly healthy while `disk_space_usage` was simply not evaluated. Emitting
labels took it to 51.

### Removing a plugin file does not stop the plugin

A removed collector kept running for over an hour, writing to the same vnode
GUIDs as its replacement and corrupting values with interleaved writes.
**Teardown must kill the process**, not just delete the file. The install
script kills previous PIDs matched on exact path.

## Shared hosting

The console serves a shared host (SOW-0021): every per-simulation action is
namespaced (`/api/sim/{name}/...`) and resolves its target by name per
request - no cached active pointer, after one such pointer silently edited the
wrong fleet (SOW-0019 incident). Simulations carry an `owner` (docker label,
honor-system until identity auth), a creation timestamp (docker `Created`),
and a `pinned` marker file in their payload dir (docker labels are immutable;
pinning must toggle).

Host budgets (`/etc/infra-sim/console.yaml`, re-read per use): nodes per
fleet, live simulations, total state-dir disk, TTL days. Refusals name the
limit and the real file path. An hourly sweeper archives unpinned
simulations past the TTL via the ordinary teardown path (everything is
archived, nothing deleted) and logs a heartbeat line. Creates serialize
behind one slot with live queue positions, because the fidelity lint inside a
create is CPU-parallel across all cores.

One console per host; two consoles race each other's sweeps and slots.

Self-monitoring (SOW-0024): `GET /api/health` (unauthenticated, counts and
timestamps only) reports simulations-vs-cap, disk-vs-cap, docker reachability
and TTL-sweep freshness; the sweeper records its completion for it. Host-side,
`scripts/host-monitoring.sh install` drops the docker-unhealthy alarm and
enables docker charts on the host agent - stock alarms cover disk and memory,
so the pack adds only what is specific to a simulation host. The runbook is
`docs/hosting.md` "What to watch".

Auth (SOW-0022): a shared bearer token from `INFRA_SIM_TOKEN`, off unless
set, mandatory for off-loopback binds (refused at startup). Everything under
`/api` is gated; the UI shell at `/` is not, because it carries the token
prompt. Dashboard ports bind loopback by default; `public_dashboards: true`
in the host policy opens them (warned, firewalled-host-only) - Netdata agents
carry no authentication. The runbook is `docs/hosting.md`. The shared token
is interim: Cloud SSO is the end state.

## Prometheus exporters and aggregation

The application tier (web, lb, k8s-worker — the same rule the OTLP emitter
applies) publishes one `/metrics` endpoint per node, scraped by Netdata's own
go.d prometheus collector with `vnode:` attribution onto the same virtual
nodes the plugins.d path owns. A database or a switch publishing storefront
orders is an artifact; it does not happen.

**Scraped charts aggregate because every job shares one `app`**
(`infra_sim_app`): contexts, which grouped views key off, become
`prometheus.infra_sim_app.<metric>` on every node. Before SOW-0020 each job
took its name from the hostname with no `app`, so every node's app metrics
lived in a per-node context — structurally impossible to aggregate.

**Job names are `infra_sim_app_{role}_{nn}` and never contain a hostname.**
go.d prefixes chart IDs with the job name, so hostname-derived names would
churn every chart ID on each re-skin and orphan scraped history. Role-index
names are re-skin-stable by construction (verified live: after a rename, with
job names held and only `vnode:` repointed, the renamed nodes kept their
scraped charts with data continuity).

Config generation lives in `sim_engine::exporter_config` and is consumed by
both the console's local install and the containerised path
(`infra-sim.plugin --exporter-config`), so the two can never drift into
different chart identities. go.d reads its vnode registry once at startup, so
whoever writes the config restarts go.d once (the agent respawns it within
seconds).

## Scenarios

A scenario (`scenarios/*.yaml`) is a timeline of effects over generator
signals, plus a **ground-truth manifest**.

- Effects: `step`, `ramp`, `drift`, `oscillate`, `recover`, `add`, `add_ramp`.
- `add`/`add_ramp` exist because a multiplier cannot lift a zero-baseline
  signal — drop rates need an absolute term.
- Targets select by `signal` plus optional `hostname`, `role`, `instance`.
  Targeting by **role** keeps a scenario correct after a re-skin.
- Effects compound across scenarios; `recover` scales the accumulated fault
  back toward neutral rather than adding another multiplier.
- **Resolve unwinds rather than deletes.** Pressing resolve sets
  `recovering_since` on the control entry and the fault eases to neutral over
  `RECOVERY_SECONDS` (180s, smoothstep). Deleting the entry cleared the fault
  between two samples, which reads as a rendering glitch rather than a system
  recovering. The console prunes finished entries; the plugin still never writes
  `control.yaml`. Re-triggering mid-recovery cancels the unwind and keeps the
  original `started_at`.
- `requires_roles` means an environment lacking those roles is not offered the
  scenario, rather than offered a button that cannot work.
- A target may select by **label** (`label: region=eu-west-1`): the operational
  dimension roles cannot express, and the pairing that makes label-filtered
  Cloud views land - the filter shows exactly the incident's subset. Composes
  with role/suffix/instance selectors (all present constraints must hold).
- `warmup` (default true) keeps a scenario in the scheduled minor-incident
  rotation; dramatic shapes opt out with `warmup: false`. Rotation trims any
  scenario to its opening steps, so a rotated scenario never looks like its
  own hero demo.
- `hostname_suffix` pins a fault to one node of a role. Role alone is right for
  a fault that really is fleet-wide, and wrong for a physical one: a dirty optic
  is in one switch, not all of them. A suffix rather than a hostname because the
  prefix is the prospect's name and changes on every re-skin, while `-sw-01`
  does not. Targeting by role alone put the "dirty optic" on every switch at
  once, contradicting a manifest that claims one port of one switch - and the
  manifest is what Netdata AI is scored against.

The manifest (root cause, causal chain, blast radius, expected finding) is
authored with the scenario and never reconstructed from what the product did —
that is the point of scoring against it.

### Control channel

`control.yaml` beside the environment lists active scenarios and their
`started_at`. The plugin polls it (one `stat` in the common case).

`started_at` **must** be written explicitly. Without it the plugin assigns
"now" on first read, so a plugin restart rewinds a running scenario to its
opening state — indistinguishable on screen from the fault resolving itself
mid-sentence.

### Scenario targets are checked, not trusted

A signal absent from *this fleet* is not the same as a signal that does not
exist. A blast-radius step reaching into a tier the fleet does not run - nginx
latency on a fleet with no nginx - is reported as a step that will not fire, the
same as a missing role. Only a signal no spec on disk defines fails the lint,
because that is a typo, and a typo is how a scenario step comes to do nothing in
front of a prospect with nothing in any log.

`check_scenarios()` verifies every scenario's signal, hostname, role and
instance resolve against the environment. A step naming something absent
produces no effect at all: the trigger appears to work and nothing happens,
in front of a prospect, with nothing in any log.

### Warm-up incidents

`warmup_incidents: true` in an environment runs minor, auto-resolving faults on
a deterministic schedule (one per 6h slot, 20 minutes, jittered within the
slot), so the alert log has texture before a demo — `spec.md` §3.

- Which incident is active at `t` is a pure function of `(seed, t)`. No state,
  no timer, no writes to `control.yaml` (the console owns that file), and a
  restart resumes mid-incident.
- Each incident keeps only the scenario steps beginning in the first half of the
  window, so it reaches a first-order symptom and stops short of the crisis the
  same scenario produces when triggered deliberately.
- A deliberately triggered scenario suppresses warm-up entirely: an SE mid-demo
  must not have warm-up noise layered on top.

## Correlated logs

A **separate process** (`infra-sim --logs`), not part of the metrics plugin.

```text
infra-sim --logs
  -> Journal Export Format
  -> systemd-journal-remote --output=/var/log/journal/remote/remote-<host>.journal
  -> Netdata systemd-journal.plugin (reads root-owned files via cap_dac_read_search)
  -> logs UI, one source per node
```

- **The journal-remote hop is not optional.** journald refuses trusted fields
  (`_HOSTNAME`) from a local client, so anything written to the local journal
  attributes to the demo machine. `systemd-journal-remote` accepts them because
  ingesting entries formed elsewhere is its purpose.
- Netdata derives the logs source name from the **filename**
  (`remote-<host>.journal` → source `<host>`).
- `--split-mode=host` is rejected for stdin sources, so per-node files mean one
  `systemd-journal-remote` child per node. A fleet in the hundreds should share
  one file and filter on the `_HOSTNAME` facet instead.
- Children exit on EOF when the parent's pipes close, however the parent dies —
  so no signal handling is needed and nothing outlives its parent.

**Fault rules match on signals, not scenario names.** A rule fires when
`ScenarioSet::perturbation` reports a signal driven past a threshold — the same
question the metrics engine asks. Nothing in the log generator knows `disk-fill`
exists, so a future scenario moving the same signal gets matching logs for free.

**No access logs.** A node reporting 1,200 req/s while its logs show three lines
a second is the contradiction an SRE notices. Real deployments send access logs
to a file and only errors to journald; this emits what journald would hold. A
healthy node logs nothing above `notice`.

Requires `systemd-journal-remote` installed and root.

**Started with the simulation, not by hand.** `sim-docker.sh create` starts the
logs writer and the OTLP emitter as one unit (`cmd_telemetry`), and the console's
create path goes through the same script. Before this, `logs start` was an opt-in
second command, and so every simulation ever built shipped with empty logs. A
telemetry process that fails to start warns; it never fails the create.

## OpenTelemetry logs and traces

A **separate process** (`infra-sim --otlp`), the third of the side processes, and
the only one that speaks gRPC.

```text
infra-sim --otlp
  -> specs/prometheus-app.yaml through NodeEngine::signal_values()
  -> sim_engine::otel  (application log records and spans, transport-free)
  -> OTLP/gRPC 127.0.0.1:4317  (the agent's own receiver, inside the container)
  -> logs: the otel-logs function      traces: stored, or refused
```

**Everything is derived from signals.** Span durations are `app_latency_p50/p95/p99`,
error status is `app_requests_error_rate`, order volume is `app_orders_rate` - the
same values `--exporters` publishes, read through the same call. A scenario that
triples latency triples span durations, and `sim_engine::otel` does not know that
scenario exists. Trace volume is a *ratio* of request rate rather than a fixed
rate, so it moves with the traffic chart instead of sitting still while it climbs.

**Only the application tier.** `web`, `lb` and `k8s-worker`. A switch does not
emit storefront spans, and a database is the thing called by one.

**OTLP logs are not per node, and cannot be.** A stream's identity is
`(service.namespace, service.name)` (netdata/netdata @ c23face0bd94
src/crates/otel-logs-identity/src/lib.rs:1-20), so records land on the agent's own
node as one stream per service. `host.name` survives as a resource attribute and
is queryable - a filter, not a log source. That is why journald still runs: the two
transports answer different questions.

**Traces depend on the agent build**, verified against both. A simulation's image
is pinned to nightly (`docker/Dockerfile`: `ARG NETDATA_TAG=latest`) for exactly
this reason:

| build | OTLP logs | traces |
|---|---|---|
| `netdata/netdata:latest` (nightly, **what simulations run**) | WAL/SFST under `otel/v2/logs` | accepted, stored under `otel/v2/traces` |
| `netdata/netdata:stable` (v2.10.4) | journal files under `otel/v1` | no `traces:` section in `otel.yaml`; refused |

No build can *display* them: trace ingestion is a proof scaffold
(src/crates/otel-ingestor/src/trace_service.rs) and no traces function is
registered anywhere. Spans are sent so the path is already right when a viewer
ships. The emitter tracks each signal's health separately and abandons one after
30 consecutive failures with an explanation - one shared flag reported "export
failed" forever on stable and hid the fact that logs were landing perfectly.

**gRPC lives in the plugin** because Netdata's receiver is gRPC-only. The
alternative was the Python SDK inside the container, which would mean `apt` and
`pip` layers on the stock netdata image; this repository already mounts a binary
into that image, so Rust changes nothing outside this repository.

## Simulated Prometheus exporters

Optional, and a **separate process** (`infra-sim --exporters`).

```text
infra-sim --exporters
  -> GET http://127.0.0.1:19998/metrics/<hostname>   (Prometheus text format)
  -> Netdata's own go.d prometheus collector
  -> charts auto-generated on the matching virtual node
```

The point is that Netdata charts an exporter it has never seen. Nothing about
the resulting charts is authored here.

- **One listener, one path per node.** A port per node would mean 50 listeners
  for a 50-node fleet, and the go.d job config carries the node identity anyway.
- **`vnode: <hostname>` in the job attributes the scrape to the fleet's own
  virtual node**, so a node carries both its plugins.d charts and its scraped
  ones. `netdata/netdata @ c23face0bd94`
  `src/go/plugin/framework/confgroup/config.go:23`.
- **go.d reads its vnode registry once, at startup**
  (`src/go/plugin/agent/setup.go:179`). A job referencing a vnode declared
  afterwards attributes nowhere, with no error, so the console restarts
  `go.d.plugin` - the daemon respawns it, and no netdatacli command exists for
  this.
- **The metrics are application-level only** (`specs/prometheus-app.yaml`):
  orders, carts, queues, worker pools. Emitting CPU here would put the same
  series on a node twice, once from plugins.d and once from the scrape.
- **Counters integrate scrape by scrape.** `rate * uptime` is not monotonic,
  because the rate has a daily cycle - it falls every evening and go.d reads the
  drop as a counter reset.
- **A summary must publish `_sum` and `_count`.** go.d skips one that does not
  (`src/go/plugin/go.d/collector/prometheus/writer_schema.go:124`), so quantiles
  alone produce no latency chart and no error anywhere.
- **No `instance` label.** Prometheus adds that at scrape time from the target
  address; an exporter does not publish it.
- Scenario aware: the exporter reads the same `control.yaml` as the metrics
  plugin, so a fault moves application metrics on the same timeline.

The console writes `/etc/netdata/go.d/prometheus.conf` and
`/etc/netdata/vnodes/infra-sim.conf`. Both carry a marker line and are never
overwritten without it; teardown removes only files carrying that marker.

## Cloud rooms

A virtual node cannot be placed in a Cloud room from the agent side.
`NETDATA_CLAIM_ROOMS` becomes a `rooms` array in the one-off claim POST
(`netdata/netdata @ c23face0bd94` `src/claim/claim-with-api.c:259`) and applies
to the claiming node only; the `CreateNodeInstance` message that registers a
vnode carries `{claim_id, machine_guid, hostname, hops}` and nothing else
(`src/aclk/schema-wrappers/node_creation.h:10-16`).

Netdata engineering confirmed the intended mechanism is **room membership rules
matching host labels** - Cloud used to inherit the parent's rooms and stopped,
because it was confusing and uncontrollable.

So every node carries `infra_sim_name` (the simulation's name), applied in
`Environment::profiles()` rather than by the renderer so hand-authored
environments get it too. One rule captures a fleet.

## Teardown

One button, and it has to leave nothing behind - the next prospect's demo runs
on the same machine.

1. Disarm scenarios, so nothing is mid-fault when the fleet stops.
2. Remove the plugin **and** stop its process. Either alone is insufficient: the
   agent rescans every 60s and relaunches a file it still finds, and a running
   collector keeps writing from a deleted file.
3. Stop `infra-sim --logs` and delete **this fleet's** journal files. Only files
   named for the environment's own hostnames - `systemd-journal-remote` output
   from anything else on the machine lives in the same directory. Left behind,
   Netdata keeps offering a log source per node for a fleet that no longer
   exists, and an SE opening logs mid-demo finds the last prospect's hostnames.
4. Stop the exporters and remove the config they added to Netdata, matching on
   the marker line so an operator's own `prometheus.conf` is never touched.
5. Archive the environment, seed and scenario manifests.
6. Remove the install directory - **only if the archive succeeded**, because
   that is the copy being removed.

Processes are matched on executable path plus argument from `/proc`, never on a
process name. Cloud-side removal stays manual and says so.

## One container per simulation

The container path (`scripts/sim-docker.sh`, `docker/`) gives a simulation its
own identity. The host path installs into the operator's agent, which means the
agent's identity *is* the simulation's: it can be in one Cloud Space, teardown
leaves stale vnodes in its database, and a plugin that exits badly disables
itself for the next fleet.

- **Image**: `netdata/netdata:<tag>` plus `systemd-journal-remote` (absent from
  the stock image, and correlated logs need it) and the plugin binary. The
  binary is baked in rather than mounted so an image tag identifies what a demo
  ran.
- **The container's own node** is named `<sim>-parent` with its internal
  collectors off. Left alone it appears as a machine named after the container
  id reporting 527 charts of container metrics, 136 of them host-style.
- **Claiming** is `NETDATA_CLAIM_TOKEN` / `NETDATA_CLAIM_ROOMS` on `docker run`
  (`netdata/netdata @ c23face0bd94` `src/claim/claim-with-api.c:603` calls this
  the choice for container users). Per simulation, so a prospect gets their own
  Space.
- **The payload directory is mounted whole**, not file by file. A bind-mounted
  *file* is bound to one inode, so anything writing by replace-and-rename leaves
  the container reading the old inode: a triggered scenario was visible on the
  host and invisible inside the container. `scripts/control_file.py` also writes
  in place for the same reason.
- **Teardown is `docker rm -f`**, which takes the agent, its database and every
  vnode's history with it. Nothing to disarm, no stale nodes, no config left
  behind.

No engine or plugin change was needed: a probe confirmed vnodes work in a
container with no host mounts before any of this was designed.

## Proven end to end

With `disk-fill` running on a live agent:

- Netdata's **own ML** raised `ml_1min_node_ar` at a 1.02% node anomaly rate
  ~18 minutes before the threshold alert could fire, and ranked the fleet in
  the manifest's blast-radius order (db 1.02% > web 0.76% > cache 0.43%).
- The **real health engine** raised `disk_space_usage` WARNING at 93.5% on
  exactly the mount the manifest names as root cause, and stayed CLEAR on the
  two other mounts of the same node.
- The **logs** showed that node's Postgres reporting
  `No space left on device ... on /var/lib/pgsql`, with the nine other nodes
  and the two untargeted mounts silent.

Nothing was faked at any step: the scenario moved generator inputs and the real
product did the rest.
