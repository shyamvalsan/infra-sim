# Infra-Sim

Generates simulated infrastructure (nodes, metrics, logs, injected faults) that a
real Netdata agent collects and processes. ML trains on it, the health engine
alerts on it, Netdata AI investigates it.

Built for sales engineers who need a demo shaped like the prospect's own estate,
and for producing incidents on cue.

## The rule

Only the raw data is simulated. Nothing downstream is mocked: no canned AI
responses, no fake alert states, no scripted dashboards.

Every simulated environment is labelled as one. Nodes carry `simulated=true`,
hostnames are prefixed, and the console shows a SIMULATED badge.

## Status

Working: 261 integrations, 7 node roles, 6 hero scenarios, correlated logs,
Prometheus exporters, OTLP, a control console covering create/claim/demo/reskin/
teardown.

Not done: 50+ node scale test, the eval gym (P2).

Product definition is `spec.md`. What is actually built is described in
`.agents/sow/specs/`.

## Requirements

- Rust (stable), `cargo build --release`
- A local Netdata agent
- Root, for anything that writes under `/etc/netdata`
- `systemd-journal-remote`, only for correlated logs

## Two ways to run a simulation

**In a container, with its own agent** (`scripts/sim-docker.sh`). Use this for
prospect demos and scale tests. Each simulation is isolated: its own agent, its
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
  --name customer-a --environment environments/customer-a.yaml

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

**Build** describes an estate in plain English, turns it into an editable fleet,
and installs it:

1. Type what the prospect runs. "20 Java app servers behind a pair of HAProxy
   boxes, a 3-node Postgres cluster, Redis for sessions, Elasticsearch for logs."
2. Press *Build the fleet*. Optionally set a fleet size and the groups scale to
   it, keeping the ratio between tiers.
3. Edit the result. Search 261 integrations, change counts, add or remove groups.
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
./target/release/infra-sim --llm netdata \
  --describe "3 web servers behind an nginx load balancer, a postgres primary" \
  --name customer-a --environment environments/customer-a.yaml

# check it before trusting it
./target/release/infra-sim --environment environments/customer-a.yaml --lint 72

# install
sudo cp target/release/infra-sim /etc/netdata/custom-plugins.d/infra-sim.plugin
```

The agent picks up a new plugin within 60s.

Without `--llm` the description is read by an offline keyword parser. It resolves
any integration named in the text but understands less phrasing.

## Node classes

Most nodes are Linux servers: the fleet's `generator:` is the Linux baseline and
service specs compose on top.

Network devices are not. A `network-device` node carries
`specs/network-device.yaml` as its own base via a per-node `generator:` override:
26 ports, per-port traffic, packets, errors, discards, status and speed, plus
device CPU, memory and uptime. No Linux contexts, because a switch has no `/var`
to fill. Contexts follow the naming Netdata's SNMP collector produces
(`snmp.device_prof_*`).

One fleet can hold both.

## Integrations

`integrations/catalogue.json` lists 261, synced from Netdata's own collector
metadata by `scripts/sync-integrations.py`.

Six are hand-authored (`nginx`, `postgres`, `redis`, `containers`, `kubernetes`,
`otel-collector`), badged DEEP in the picker. Their signals are causally coupled
and hero scenarios target them by name.

Five of them `extends:` the generated spec for the same software, so they emit
its full context set with their own carefully modelled contexts layered on top.
A simulated Postgres reports 71 contexts, of which 15 are causally coupled.

The rest are generated: correct contexts, units, chart types and dimension names,
with a value profile derived from the unit. Signals move independently and no
scenario targets them. All 258 pass the 6-hour fidelity lint.

A labelled scope (per database, per index) is modelled as one representative
instance.

To resync after a Netdata update:

```bash
python3 scripts/sync-integrations.py --netdata /path/to/netdata
```

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

## Correlated logs

A separate process, not part of the metrics plugin.

```bash
sudo apt-get install systemd-journal-remote
./scripts/logs.sh start|status|stop
```

Each node becomes its own log source in Netdata. Fault lines are matched on
signals rather than scenario names, so any scenario moving a modelled signal
produces matching logs.

Healthy nodes log nothing above `notice`. There are no access logs: a node
reporting 1,200 req/s while its logs show three lines a second is a contradiction
an SRE notices, and journald would not hold them anyway.

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

## OpenTelemetry

Two separate things:

- `otlp/emit.py` sends metrics, logs and traces for a made-up service over OTLP
  to Netdata's ingestion endpoint (`./scripts/otlp.sh`). OTLP cannot create
  virtual nodes, so this lands on the ingesting host.
- The `otel-collector` integration models a collector fleet as monitored
  services on the plugins.d path, which keeps per-node dashboards and ML.

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
# units, broken conservation, stuck values and unresolvable scenario targets
./target/release/infra-sim --environment environments/web-stack.yaml --lint 72
```

Changes to a generator, scenario or the runtime are validated against a live
agent, not only unit tests.

## Repository layout

```
crates/sim-spec      generator and scenario formats
crates/sim-engine    signal evaluation, scenarios, logs, describe, LLM
crates/sim-plugin    the Netdata plugin, logs writer, exporter server
crates/sim-console   control console (HTTP + UI)
specs/               hand-authored generator specs
specs/generated/     synced from Netdata collector metadata
scenarios/           fault timelines with ground-truth manifests
environments/        committed fleet templates
integrations/        the picker's catalogue
.agents/sow/specs/   what the project currently does, and why
```


