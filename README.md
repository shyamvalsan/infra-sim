# Infra-Sim

Synthetic infrastructure for Netdata. Generates realistic simulated fleets —
nodes, metrics, and injected problem scenarios — that the **real** Netdata
pipeline processes live.

## The one hard rule

**Synthetic world, live product.**

Only the raw data is simulated. Everything downstream is the real product: ML
actually trains and detects, the health engine actually raises alerts, Netdata
AI actually investigates. Nothing downstream of data injection is ever scripted
or mocked — no canned AI responses, no fake alert states, no mocked dashboards.

Every simulated environment is labelled as one. Each node carries
`simulated=true`, applied by the runtime and not overridable from an
environment file.

This is verifiable, not a claim. On a live agent running the example
environment, Netdata creates its own `anomaly_detection.*` charts per simulated
node and trains on them with no special casing:

```
sim-db-01   anomaly_detection.ml_running = 1
sim-db-01   netdata.training_status      = untrained 12.6, trained 10.4
```

## Status

Early. The first vertical slice is working end to end; most of the product in
`spec.md` is not built yet.

**Working:** generator spec format, deterministic seeded engine with invariants
enforced by construction, plugins.d emitter with virtual-node support,
per-instance cardinality (one chart per disk / interface / mount), a
70-context Linux baseline, a 5-node web-stack environment, and a fidelity lint.

Also working: a scenario engine with live trigger, ground-truth manifests, and
one hero scenario.

**Not built:** correlated logs, control console, the remaining four hero
scenarios, additional verticals, OTEL simulation, the eval gym. See
`.agents/sow/` for what is tracked and `spec.md` for the full product
definition.

## How it works

Infra-Sim is an **external plugin** speaking Netdata's plugins.d protocol. It
needs no agent fork, no monorepo PR, and no go.d module: `HOST_DEFINE` is
available to external plugins and creates real Netdata virtual nodes.

```
environment.yaml  ──┐
                    ├──►  infra-sim  ──plugins.d──►  netdata agent  ──►  Cloud
generator spec    ──┘     (Rust)                     (ML, health, AI)
   + seed
```

An `environment.yaml`, its generator spec, and the seed fully determine the
emitted stream. Archive those three and a past demo replays identically.

### A vnode's dashboard is exactly what you emit

There is no automatic "System Overview" section. Netdata builds dashboard menus
from the contexts present on a node, so a plugin emitting six contexts produces
a node with six charts. For reference, a real Linux host on the same agent
carries **188 OS-baseline contexts across 312 chart instances**.

This is the single biggest driver of effort in this project. Details and
measurements: `prototypes/vnode-probe/FINDINGS.md`.

## Quick start

Requires a running Netdata agent and a Rust toolchain.

```bash
cargo build --release

# Simulate 72 hours and fail on fidelity violations. No agent needed.
./target/release/infra-sim --environment environments/web-stack.yaml --lint 72

# Install into the local agent. Runs the lint first and refuses to install if
# it fails. The agent rescans for new plugins every 60s.
./scripts/install-local.sh

curl -s localhost:19999/api/v3/nodes | grep sim-
```

Remove it again:

```bash
sudo rm -rf /etc/netdata/custom-plugins.d/infra-sim.plugin /etc/netdata/infra-sim
sudo systemctl restart netdata
```

Simulated nodes persist in the agent's database after removal — a vnode GUID is
a durable identity, which is what makes the re-skin workflow possible.

## Building an environment from a description

Rather than hand-writing `environment.yaml`, describe the stack:

```bash
./target/release/infra-sim \
  --describe "3 web servers behind an nginx load balancer, a postgres primary and 2 redis caches" \
  --name acme --environment environments/acme.yaml
```

That runs offline against a keyword parser — no API key, no network. For a
description written in the prospect's vocabulary rather than ours, add `--llm`:

```bash
export ANTHROPIC_API_KEY=...   # or OPENAI_API_KEY for --llm openai

./target/release/infra-sim --llm anthropic \
  --describe "Our checkout tier runs on six boxes fronted by an ALB, with an \
Aurora writer, two ElastiCache nodes and an SQS queue" \
  --name acme --environment environments/acme.yaml
```

```
   2 x lb                 acme-alb-NN                nginx        <- "fronted by an ALB"
   6 x web                acme-checkout-NN           nginx        <- "checkout tier on six boxes"
   1 x db                 acme-aurora-NN             postgres     <- "an Aurora writer"
   2 x cache              acme-elasticache-NN        redis        <- "two ElastiCache nodes"

not modelled, so nothing was generated for these:
  "an SQS queue - no generator spec models a message broker"
```

**The model returns a plan, never YAML.** It chooses among the roles and service
specs that exist in this repository, says how many of each and what to call
them; the same deterministic renderer used by the offline path writes the file.
So a model that misreads produces a wrong-but-visible fleet the SE corrects,
rather than an environment naming a signal no generator defines — which fails
silently, mid-demo, with nothing in any log to explain it. Anything it cannot
map is reported instead of substituted, and GUIDs stay derived from hostnames,
so regenerating never orphans a running fleet's history.

Whichever path produced it, lint before trusting it:

```bash
./target/release/infra-sim --environment environments/acme.yaml --lint 72
```

Options: `--llm anthropic|openai`, `--llm-model MODEL`, `--llm-key-env VAR`.
`$ANTHROPIC_BASE_URL` / `$OPENAI_BASE_URL` point at an internal gateway. The key
is read from the environment and handed to `curl` over stdin, so it never
appears in the process table — and it is never written to the environment file.

This is authoring-time only. `spec.md`'s non-goal is per-datapoint LLM
generation; the runtime is deterministic code with no inference in the data
path.

## Correlated logs

Each simulated node gets its own log source in Netdata, and the log lines
follow whatever scenario is running.

```bash
sudo apt-get install systemd-journal-remote   # one-time

./scripts/logs.sh start
./scripts/logs.sh status
./scripts/logs.sh stop
```

The pipeline:

```text
infra-sim --logs
  -> Journal Export Format
  -> systemd-journal-remote --output=/var/log/journal/remote/remote-<host>.journal
  -> Netdata's systemd-journal.plugin
  -> logs UI, one source per node
```

The journal-remote hop is not optional. journald refuses to let a local client
set `_HOSTNAME` — it is a *trusted* field — so anything written to the local
journal is attributed to the machine running the demo. `systemd-journal-remote`
accepts trusted fields because its whole purpose is ingesting entries formed on
another host, and that is what gives each simulated node its own identity.
Netdata reads the files with `cap_dac_read_search`, so they stay root-owned.

### Faults are matched on signals, not scenario names

A log rule fires when a *signal* is perturbed past a threshold — the same
question the metrics engine asks. Nothing in the log generator knows that
`disk-fill` exists. Any scenario that drives `disk_space_used_kb` up produces
disk-full logs, including one written next year, and a scenario that gets
renamed or retuned cannot drift away from its own logs.

So triggering `disk-fill` makes the database node log this, naming the same
mount the alert fired on:

```text
postgres  ERROR:  could not extend file "base/16384/400000": No space left on device
          HINT:  Check free disk space; relation "orders" on /var/lib/pgsql
kernel    EXT4-fs warning (device var-lib-pgsql): ext4_has_free_clusters:379: ...
```

### Two deliberate choices

**No access logs.** A web node reporting 1,200 req/s on its chart while its
logs show three lines a second is exactly the contradiction an SRE notices, and
emitting the real volume would be absurd for a demo. Real deployments split it
the same way — nginx access logs go to a file, only errors reach journald — so
this emits what journald would actually hold. A healthy node logs nothing above
`notice`.

**A separate process.** The logs writer shares nothing with the metrics plugin
but the environment file, the seed and `control.yaml`. Because the engine is a
pure function of those, both compute the same values for the same tick, so logs
and metrics correlate by construction rather than by coordination — and the
writer can be stopped without touching the plugin.

At fleet sizes in the hundreds this should share one journal file and filter on
the `_HOSTNAME` facet instead; `--split-mode=host` is rejected for stdin
sources, so per-node files mean one `systemd-journal-remote` per node.

## Writing generator specs

A spec describes contexts and the signals behind them. The format exists to
make fidelity artifacts *unrepresentable* rather than merely unlikely.

**Signals are shared.** One `cpu_busy` drives `system.cpu`, `system.load` and
the process counts, so those charts correlate the way a real host's do. That
correlation is a property of sharing the signal, not of tuning each context.

**Conserved quantities use `partition`.** Exactly one dimension is the
`remainder` and absorbs `total - sum(others)`:

```yaml
- id: system.ram
  shape: partition
  total: { from: node_attr, name: ram_total_kb }
  driver: mem_used_kb
  dimensions:
    - { id: used,    share: 0.86, divisor: 1024 }
    - { id: buffers, share: 0.03, divisor: 1024 }
    - { id: cached,  share: 0.11, divisor: 1024 }
    - { id: free,    remainder: true, divisor: 1024 }
```

Conservation is structural. The throwaway probe that preceded this code emitted
`free = 0` within four minutes by computing each dimension independently and
clamping the leftover; a `partition` cannot express that bug.

**Counters integrate a rate.** The spec states units per second and the engine
accumulates, so emitted counters are monotonic by construction.

**Roles retune, they never restructure.** A role overrides signal parameters
only — it cannot add or remove contexts — so every node of a spec has an
identical context set and differs solely in behaviour.

**Per-device contexts declare instancing.** Real collectors emit one chart
instance per device sharing a context, so a node with a single unnamed disk
chart reads as wrong immediately:

```yaml
- id: disk.io
  instances: { group: disk, chart_prefix: disk, family: "io" }
```

The node then supplies its devices, with weights and per-device attributes:

```yaml
instances:
  disk:
    - { name: nvme0n1, weight: 1.0 }
    - { name: nvme1n1, weight: 0.35 }
  mount:
    - { name: "/var/lib/pgsql", weight: 1.0, attrs: { disk_total_kb: 1572864000 } }
```

Chart ids and families follow Netdata's own convention, which is not uniform:
`disk.io` yields `disk.<dev>` in family `io`, while `net.packets` yields
`net_packets.<iface>` in family `<iface>`. The spec states it rather than
guessing. A node with no matching instance group emits no charts for that
context — correct, since a host without a second disk should not have a chart
for one.

### Bounds are safety rails, not modelling tools

`min`/`max` should sit outside the range a signal actually reaches. A signal
pinned to a bound has stopped being modelled and is being clamped, which
flattens the metric visibly.

The lint enforces this, with one distinction: **zero is always a legitimate
value** for the non-negative quantities these specs model — a quiet interface
really does report zero errors — so a floor of `0.0` is never flagged. Any
other floor is a rail unless declared with `min_is_floor: true`, and every
ceiling is a rail unless declared with `max_is_ceiling: true`.

## Scenarios

A scenario perturbs generator **inputs** on a timeline. It never fabricates an
alert, an anomaly score, or a dashboard state — the real health engine and the
real ML see the perturbed data and reach their own conclusions.

If a scenario runs and Netdata stays quiet, the scenario was too weak. The
answer is never to fake the alert.

```bash
./scripts/scenario.sh list
./scripts/scenario.sh trigger disk-fill
./scripts/scenario.sh status
./scripts/scenario.sh resolve disk-fill
```

Effects begin within one collection interval. The plugin watches a control file
recording **state, not commands** — which scenarios should be running and since
when — so re-reading is idempotent, an unrelated edit cannot restart a running
scenario, and a plugin restart mid-demo resumes at the same offsets instead of
silently dropping the fault you are presenting.

### Faults move signals, not charts

A scenario targets a *signal*, so the fault propagates into every context that
signal feeds. That coupling is what produces a coherent blast radius rather than
one conspicuously anomalous chart:

```yaml
- at: 0s
  target: { signal: disk_space_used_kb, hostname: sim-db-01, instance: /var/lib/pgsql }
  effect: ramp
  multiplier: 8.2
  over: 25m
```

Selectors combine with AND and can name a host, a role, or a single device.
Targeting by role means a scenario keeps working after a fleet is re-skinned for
a different prospect.

Effects are `step`, `ramp`, `drift` (compounding, never levels off),
`oscillate` (flapping) and `recover`. They are multipliers, so a fault composes
with seasonality and noise instead of replacing them — the same fault looks
different at 3 a.m. than at peak, as it would in reality. Multiple scenarios on
one signal compound, so a noisy neighbour plus a slow disk is worse than either
alone without either scenario knowing about the other.

### Every scenario carries ground truth

```yaml
manifest:
  root_cause: sim-db-01 /var/lib/pgsql
  causal_chain: [...]
  blast_radius: [sim-db-01, sim-web-01, sim-web-02]
  expected_finding: ...
```

Written when the scenario is authored, never reconstructed from what the product
happened to do — that is the point of scoring against it. The eval gym measures
time-to-detect and root-cause accuracy from this.

## Repository layout

```
crates/sim-spec/      generator spec format, parsing, validation
crates/sim-engine/    deterministic execution, seeded RNG, invariants
crates/sim-plugin/    plugins.d emitter, environment.yaml, fidelity lint
specs/                generator specs
environments/         environment definitions
scenarios/            scenario definitions and their ground-truth manifests
scripts/              local install, scenario trigger/resolve
prototypes/           throwaway verification probes and their findings
.agents/sow/          statement-of-work ledger and specs
```

## Contributing

Most of this repository is content, not Rust: generator specs, environment
templates, and scenario definitions are YAML. Adding a context to a simulated
node means editing a spec file.

Before non-trivial work, read `AGENTS.md` — it carries the project's runtime
rules, including the requirement that generator and runtime changes are
validated against a live agent, not only unit tests.

## Licence

GPL-3.0-or-later, matching the Netdata agent.
