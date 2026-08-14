# Operating Infra-Sim

Reference for running simulations. `README.md` covers what Infra-Sim is and why it
exists, `docs/QUICKSTART.md` is the shortest path to a demo, and this is the full
reference. Design rationale lives in `.agents/sow/specs/`.

## Two ways to run a simulation

**In a container, with its own agent** (`scripts/sim-docker.sh`). Use this for
anything you show someone, and for scale tests. Each simulation is isolated: its own agent, its
own Cloud claim, its own history, and `docker rm` removes all of it. Your own
agent is never touched.

**On the host agent** (the console). Use this for local iteration, where a build
step per change is not worth it.

Both run the same binary against the same environment file.

### Containerised

```bash
cargo build --release
./scripts/sim-docker.sh build                                  # once, and after changing the plugin

./target/release/infra-sim --llm netdata \
  --describe "6 web servers behind two haproxy load balancers, a 3-node postgres cluster, 4 access switches" \
  --nodes 50 --name customer-a --environment environments/customer-a.yaml

./scripts/sim-docker.sh create customer-a environments/customer-a.yaml
./scripts/sim-docker.sh status customer-a
```

Claim it into its own Space:

```bash
./scripts/sim-docker.sh create customer-a environments/customer-a.yaml --claim --rooms <room-id>
```

The token is read from `$NETDATA_CLAIM_TOKEN` or prompted for. It reaches the
container as an environment variable, which is visible in `docker inspect` for
the life of the container. That is the mechanism Netdata documents for
containers; baking it into an image or a mounted file is worse.

Scenarios, logs and teardown:

```bash
./scripts/sim-docker.sh scenario customer-a trigger disk-fill
./scripts/sim-docker.sh scenario customer-a resolve disk-fill
./scripts/sim-docker.sh logs customer-a start
./scripts/sim-docker.sh teardown customer-a
```

The container's own node is named `<name>-parent` and runs no host collectors,
so it does not appear in the Space as a stray machine reporting container CPU.

## Console

```bash
cargo build --release
sudo ./target/release/infra-sim-console --repo "$PWD"
# http://127.0.0.1:8080
```

Two tabs.

**Build** describes a fleet in plain English, turns it into an editable table,
and installs it:

1. Type what the fleet runs. "20 Java app servers behind a pair of HAProxy
   boxes, a 3-node Postgres cluster, Redis for sessions, Elasticsearch for logs."
2. Press *Build the fleet*. Optionally set a fleet size and the groups scale to
   it, keeping the ratio between tiers.
3. Edit the result. Search 260 integrations, change counts, add or remove groups.
4. Name it and press *Create & install*. The name fixes the seed and every node
   GUID, so it cannot change later without orphaning history.

Creation runs a fidelity lint over N simulated hours first and refuses to install
a fleet that fails it.

The same tab holds Cloud claiming (token plus optional room id), re-skin, and
teardown.

**Run** is the demo surface: preflight verdict, scenario triggers with escalate
and rewind, and the live node table.

Reading a description needs a model key in a gitignored `.env` beside the repo:

```bash
echo 'LLM_API_KEY=...' >> .env      # llm.netdata.cloud
```

`ANTHROPIC_API_KEY` and `OPENAI_API_KEY` work too. The console offers only
providers whose key it can find. `sudo` strips the environment, so `.env` is the
only place the key reliably survives.

### Model choice

The plan contract needs a strict `json_schema` response format and not every
model honours one. Measured on `llm.netdata.cloud` against a real request:

| Asked for | Answered as | Structured output | Time |
|---|---|---|---|
| `k3` (default) | `k3` | yes | 10-24s |
| `glm-5.2-max` | `glm-5.2-max` | no, prose | ~24s |
| `deepseek-v4-flash` | `MiniMax-M3` | no, prose | ~24s |

A gateway can answer as a different model than the one requested, so a
JSON-parse failure names the model that actually replied. Override with
`--llm-model`; set `INFRA_SIM_LLM_DEBUG=1` to see the exchange.

## Command line

```bash
# build an environment from a description
# --nodes scales the fleet to that size, keeping the ratio between tiers
./target/release/infra-sim --llm netdata \
  --describe "3 web servers behind an nginx load balancer, a postgres primary" \
  --nodes 50 --name customer-a --environment environments/customer-a.yaml

# check it before trusting it - 2h is what create runs, and catches the same defects
./target/release/infra-sim --environment environments/customer-a.yaml --lint 2

# install
sudo cp target/release/infra-sim /etc/netdata/custom-plugins.d/infra-sim.plugin
```

The agent picks up a new plugin within 60s.

Without `--llm` the description is read by an offline keyword parser. It resolves
any integration named in the text but understands less phrasing.

### Putting a simulation in its own Cloud room

`NETDATA_CLAIM_ROOMS` applies only to the claiming agent. The message that
registers a virtual node with Cloud has no rooms field at all
(`netdata/netdata @ c23face0bd94`
`src/aclk/schema-wrappers/node_creation.h:10-16`), so a simulation's nodes
always land in the Space's "All nodes" room and no agent-side setting changes
that.

Use a **room membership rule** instead. Every simulated node carries:

| Label | Value |
|---|---|
| `infra_sim_name` | the simulation's name |
| `infra_sim_role` | `web`, `db`, `network-device`, … |
| `infra_sim_env` | `production` |
| `simulated` | `true` |

Create the room with a rule matching `infra_sim_name = <your simulation>` and
the whole fleet joins it, including nodes added later. `infra_sim_role` gives a
room per tier if you want one.

The simulation's own agent carries the same labels, with
`infra_sim_role = sim-parent` so a rule selecting `web` or `db` does not pick it
up while a rule on `infra_sim_name` does.

## Placing a fleet on the map

The console takes a fleet latitude and longitude in decimal degrees, and any
group can override them - switches in the DC, edge gateways in the field. Every
node gets `latitude` and `longitude` host labels, which is what Cloud reads to
place it.

Leave both blank and no coordinates are written at all. There is no default,
because a default would put the whole fleet in the Gulf of Guinea and look
deliberate.

Nodes at one site are scattered by up to roughly 500m, derived from the hostname
so a node never moves between runs or re-skins. Machines in one rack really do
share a coordinate, but 27 nodes on one pin is a map that hides 26 of them.

The fleet's location is also recorded once at the top of `environment.yaml`, and
the container's own agent is labelled from it - so no node in the Space is left
unplaced.

## Node classes

Most nodes are Linux servers: the fleet's `generator:` is the Linux baseline and
service specs compose on top.

Network devices are not. A `network-device` node carries its own base spec via a
per-node `generator:` override, and no Linux contexts, because a switch has no
`/var` to fill. Contexts follow the naming Netdata's SNMP collector produces
(`snmp.device_prof_*`).

Pick nothing and the group gets `specs/network-device.yaml`, a generic managed
switch: 26 ports, per-port traffic, packets, errors, discards, status and speed,
plus device CPU, memory and uptime.

Pick a **model** and it gets that device. `integrations/snmp-devices.json` lists
105 models from 71 vendors, generated by `scripts/sync-snmp-profiles.py` from
Netdata's own SNMP device profiles with each profile's `extends` chain resolved.
A Cisco Nexus reports 202 contexts - per-supervisor CPU, chassis temperature and
voltage sensors, power supplies, FRU state, PoE - because that is what the vendor
profile says a Nexus reports.

Taken from the profile: context ids, units (the collector's own UCUM strings,
`By/s`, `{packet}/s`, `Cel`), chart families, one 0/1 dimension per state for enum
metrics, and the composed in/out charts. Invented: the magnitudes, derived from
the unit.

The standard interface counters keep the same signal names across every model, so
`switch-uplink-degrading` degrades an uplink on any of them without a per-vendor
variant.

A device only charts the hardware its environment gives it - ports, CPUs, memory
pools, sensors, fans, power supplies. The topology tables a profile also describes
(OSPF neighbours, CDP peers, MAC tables) are left empty, because inventing them
would mean inventing peers.

One fleet can hold both node classes.

```bash
python3 scripts/sync-snmp-profiles.py --netdata /path/to/netdata
```

## Integrations

`integrations/catalogue.json` lists 260, synced from Netdata's own collector
metadata by `scripts/sync-integrations.py`.

Six are hand-authored (`nginx`, `postgres`, `redis`, `containers`, `kubernetes`,
`otel-collector`), badged DEEP in the picker. Their signals are causally coupled
and hero scenarios target them by name.

Five of them `extends:` the generated spec for the same software, so they emit
its full context set with their own carefully modelled contexts layered on top.
A simulated Postgres reports 71 contexts, of which 15 are causally coupled.

The rest are generated from Netdata's collector metadata: correct contexts, units, chart types and dimension names,
with a value profile derived from the unit. Signals move independently and no
scenario targets them. All 258 pass the 6-hour fidelity lint.

A labelled scope (per database, per index) is modelled as one representative
instance.

To resync after a Netdata update:

```bash
python3 scripts/sync-integrations.py --netdata /path/to/netdata
```

## Processes, users and groups

Every Linux node composes `specs/processes.yaml`, so the Processes tab shows
what a real host of that kind runs rather than `root` and `netdata`. A database
node reports postgres, pgbouncer, barman, systemd, sshd and the agent, under a
postgres user and group; a web node reports nginx, php-fpm, node and filebeat
under `www-data`.

Per-role rosters live in `persona()` in `crates/sim-engine/src/describe.rs`, and
the weights are relative load, so the workload dominates its own node while the
agents beside it barely register. Metric names, units and dimensions follow
`apps.plugin`'s own metadata: `app.*` by `app_group`, `user.*` by `user`,
`usergroup.*` by `user_group`.

The roster is chosen by **role**, not by the software a node actually runs. A
MongoDB or Oracle node therefore reports the database role's postgres roster.
Worth knowing before you open the per-application charts on a node whose engine
is not Postgres.

Those rosters reach down to weight 0.01, and emitted values are integers, so
these signals carry a finer unit of account than they display: memory in KiB with
`divisor: 1024`, percentages in hundredths with `divisor: 100`. Without that, a
0.01-weight group's memory rounded to a constant and its CPU rounded to zero.
Counts cannot take a divisor, so `proc_processes` and `proc_threads` carry
`min_is_floor` instead - a floor is weight-independent, because a group that
exists runs at least one process however little load flows through it.

**Upgrading a fleet across that change.** A simulation created before
2026-08-11 stored process-memory points in whole MiB. The divisor changes the
emitted integer, and Netdata does not rewrite stored tier data when a
`DIMENSION` divisor changes, so historical points on those charts can render
1024x smaller than the live ones. Recreate such a fleet rather than upgrading it
in place; a fleet created after that date is unaffected.

## Scenarios

A scenario is a timeline of effects over generator signals plus a ground-truth
manifest naming the root cause, causal chain, blast radius and expected finding.
The manifest is authored with the scenario, never reconstructed from what the
product did.

| Scenario | What it demonstrates |
|---|---|
| `disk-fill` | A database volume filling, projected exhaustion |
| `db-replication-lag` | Replica falling behind, application-visible staleness |
| `memory-leak-oom` | Slow leak to OOM kill |
| `noisy-neighbour` | One tenant starving others on shared hardware |
| `flapping-edge-links` | Intermittent physical-layer trouble on an uplink |
| `switch-uplink-degrading` | A dirty optic. The link never goes down, so link-state monitoring sees nothing while errors climb and the tier behind it slows |

Faults move signals, not charts, so a fault propagates into every context that
signal feeds.

Targets select by signal plus optional hostname, hostname suffix, role or
instance. `hostname_suffix` pins a physical fault to one node of a role and
survives a re-skin.

Resolving a scenario unwinds it over three minutes rather than clearing it
between two samples.

`warmup_incidents: true` runs minor auto-resolving faults on a deterministic
schedule so the alert log has texture before a session.

## Logs and OpenTelemetry

Both start with the simulation. Nothing to run by hand — that was the bug: every
simulation built before this shipped with empty logs because `logs start` was a
step someone had to remember.

```bash
./scripts/sim-docker.sh telemetry <name> status     # what both are doing
./scripts/sim-docker.sh telemetry <name> stop|start # if you need to
```

Two separate processes, neither of them the metrics plugin: Netdata owns the
plugin's lifecycle, and a collector that outlives its own removal has already cost
this project real debugging time.

### Correlated logs (journald)

Each node becomes its own log source in Netdata's `systemd-journal` function.
Fault lines are matched on signals rather than scenario names, so any scenario
moving a modelled signal produces matching logs.

Healthy nodes log nothing above `notice`. There are no access logs: a node
reporting 1,200 req/s while its logs show three lines a second is a contradiction
an SRE notices, and journald would not hold them anyway.

Needs `systemd-journal-remote`, which the container image installs. On a host,
`sudo apt-get install systemd-journal-remote` and `./scripts/logs.sh start`.

### OpenTelemetry logs and traces

The application tier — `web`, `lb` and `k8s-worker` nodes — ships OTLP to the
agent's own receiver on `127.0.0.1:4317` inside the container.

Everything is derived from `specs/prometheus-app.yaml` through the same
`signal_values()` call the Prometheus exporters use, so a scenario that triples
request latency triples span durations. The metrics you scrape and the
telemetry they trace are one description of one service.

What lands where, and why it is not per node:

- An OTel log stream is identified by `(service.namespace, service.name)` and
  never by host, so OTLP records arrive on the **agent's own node** as one stream
  per service. The simulated host survives as the `host.name` resource attribute,
  which is queryable in the `otel-logs` function — a filter, not a log source.
  Per-node log sources are what journald is for, which is why both run.
- Traces depend on the agent build, which is why the image is pinned to nightly.
  `netdata/netdata:latest` (nightly) accepts and stores spans under
  `otel/v2/traces`; `netdata/netdata:stable` has no `traces:` section in its
  `otel.yaml` and rejects them outright, and stores OTLP logs in an older
  journal-based layout. **No build can display traces yet.** The emitter reports
  which happened and stops retrying a receiver that will never take them, rather
  than failing every second for the life of the simulation.

Reading OTLP logs without Cloud SSO — the `otel-logs` function returns 412
without it. On the nightly image the store is a WAL/SFST layout with an offline
inspector:

```bash
docker exec infra-sim-<name> sh -c '/usr/libexec/netdata/plugins.d/otel-plugin logs \
  --wal-dir /var/log/netdata/otel/v2/logs/wal \
  --sfst-dir /var/log/netdata/otel/v2/logs/index \
  --name <sim>-storefront --namespace <sim>'
```

On a stable-based image it is journal files instead:

```bash
docker exec infra-sim-<name> sh -c \
  'journalctl --file=/var/log/netdata/otel/v1/*/*.journal -o json --no-pager -n 5'
```

## Prometheus exporters

Optional. Each node publishes a real `/metrics` endpoint that Netdata's own go.d
prometheus collector scrapes and charts.

```bash
sudo ./target/release/infra-sim --exporters --environment /etc/netdata/infra-sim/environment.yaml
curl http://127.0.0.1:19998/metrics/<hostname>
```

Metrics are application-level (orders, carts, payment declines, queue depth,
worker pools, a latency summary) so nothing duplicates a chart the agent already
collects.

The console writes the scrape jobs to `/etc/netdata/go.d/prometheus.conf` and
vnode entries to `/etc/netdata/vnodes/infra-sim.conf`, with `vnode:` on each job
so scraped charts land on the same virtual node as the plugins.d ones. Both files
carry a marker line and are never overwritten without it.

## Two other OpenTelemetry paths

Besides the fleet's own OTLP emitter above:

- `otlp/emit.py` sends metrics, logs and traces for a made-up service, using the
  official Python SDK (`./scripts/otlp.sh`). It knows nothing about a fleet - it
  exists to show Netdata ingesting OpenTelemetry from an ordinary instrumented
  application, with no collector in between.
- The `otel-collector` integration models a collector fleet as monitored services
  on the plugins.d path, which keeps per-node dashboards and ML.

## Writing generator specs

A spec declares `signals` (base, bounds, seasonality, noise) and `contexts`
(Netdata context id, units, chart type, dimensions). Contexts reference signals,
so contexts sharing a signal correlate.

Shapes: `independent`, `partition` (a conserved total split across dimensions),
`counters` (monotonic).

Signal properties worth knowing:

- `min_is_floor` / `max_is_ceiling` mark a bound as physical rather than a
  safety rail
- `ignore_weight` exempts a signal from its instance's weight, for properties of
  an instance rather than quantities flowing through it
- `from_attr` takes the value from a node or instance attribute, for facts that
  do not vary with time

Details and rationale: `.agents/sow/specs/generator-and-engine.md`.

## Validating

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# fidelity lint: simulates N hours and checks for clamped signals, impossible
# units, broken conservation, stuck values and unresolvable scenario targets.
# Nodes are linted one per core. 2h is the create default; the semantic checks
# always cover a fixed 2h window, so a larger N only adds warm-up before them,
# which is worth it while developing a spec and not while installing a fleet.
./target/release/infra-sim --environment environments/web-stack.yaml --lint 2
```

Changes to a generator, scenario or the runtime are validated against a live
agent, not only unit tests.
