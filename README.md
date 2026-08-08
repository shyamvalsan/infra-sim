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

The first vertical slice is complete and validated end to end on a live agent.

**Working:**

- Declarative generator spec format; deterministic seeded engine (same
  environment + seed replays identically, bit-exact with `--replay-from`).
- plugins.d emitter with virtual nodes, per-instance cardinality (one chart per
  disk / interface / mount) and chart labels so stock health templates attach.
- A 70-context Linux baseline plus five service specs (nginx, postgres, redis,
  containers, kubernetes) composed per node.
- Four environment templates; five hero scenarios with ground-truth manifests,
  triggerable live.
- Two-layer fidelity harness (pinned-signal + semantic checks). It has found
  seven real bugs that were live.
- Control console covering the full lifecycle: create (role counts + collector
  checkboxes, lint-gated install), preflight, scenario controls, claim, and
  guided teardown with archive.
- Environment authoring by hand, from a text description (offline or
  LLM-backed), or by re-skinning a warm fleet without touching GUIDs.
- Correlated logs — one journal source per node, fault lines matched on signals.
- OpenTelemetry: an OTLP emitter showcase, and a collector-fleet generator spec.

**Not built:** the eval gym, additional verticals beyond the five service specs,
and packaging for someone who did not build it. See `.agents/sow/` for what is
tracked and `spec.md` for the full product definition.

**Proven, not claimed.** With `disk-fill` running, Netdata's own ML raised
`ml_1min_node_ar` about 18 minutes before any threshold alert and ranked the
fleet in the manifest's blast-radius order; the health engine then raised
`disk_space_usage` WARNING on exactly the mount the manifest names as root
cause; and that node's logs showed Postgres reporting no space left on that
same mount, while the other nodes stayed quiet. Nothing in that chain is
scripted.

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

**New here? Read [docs/QUICKSTART.md](docs/QUICKSTART.md)** — zero to a running
fleet with a live incident, including the warm-up timing that decides whether a
demo lands.

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

## The console

The full lifecycle — create, claim, warm up, demo, tear down — from one screen.

```bash
sudo ./target/release/infra-sim-console --repo "$PWD"
# then open http://127.0.0.1:8080
```

Root is required: create writes under `/etc/netdata`, manages the plugin
process, and claim reads the agent's local-proof file. Shelling out to `sudo`
per action would put a password prompt in the middle of a demo instead.

- **New simulation** — start from a committed template or blank, then set node
  count per role and tick a checkbox per collector (`nginx`, `postgres`,
  `redis`, `containers`, `kubernetes`, `otel-collector`), each role's defaults
  pre-ticked. Builds `environment.yaml`, runs the fidelity lint, and **refuses
  to install a fleet that fails it**. A template fills the form rather than
  being installed directly, so one code path builds every environment.
- **Preflight** — a green board it verifies against the live agent, not a
  checklist it trusts you to have done.
- **Scenarios** — trigger, resolve, and move the demo clock. **Escalate** pushes
  a running scenario forward through its own authored timeline rather than
  applying a separate intensity knob, so severity and the ground-truth manifest
  can never disagree. The same control rewinds.
- **Re-skin** — rename the running fleet for a new prospect. GUIDs are never
  touched, so it keeps its history, trained ML and alert log.
- **Claim** — one claim covers the whole fleet. The token goes straight to the
  agent and is never stored, logged, or written to any file. The Space name
  must end `(Simulated Demo)`; that is enforced, not suggested.
- **Teardown** — disarms scenarios, removes the plugin *and* stops its process
  (either alone is not enough), and archives the environment, seed and scenario
  manifests. Cloud-side steps stay manual and say so.

### Warm-up incidents

`warmup_incidents: true` (on by default in generated environments and the
committed templates) runs minor, auto-resolving faults on a deterministic
schedule — roughly one every six hours, twenty minutes long, drawn from the
scenarios the fleet can host.

A fleet whose alert log is empty reads as a fleet nothing has ever happened to,
which is the opposite of a real estate. These give the alert log and anomaly
history texture before a live session.

Nothing is stored and no timer runs: which incident is active at time *t* is a
pure function of `(seed, t)`. That keeps the replay promise, means the plugin
never writes `control.yaml` (the console owns it), and lets a restart resume
mid-incident with no state to lose. Each one runs only the opening stretch of a
scenario's timeline, so it never looks like the hero demo an SE will trigger
deliberately. A deliberately triggered scenario always suppresses them.

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

## OpenTelemetry

Two separate things, for two separate conversations.

### 1. Send OTLP into Netdata

A made-up application emitting **metrics, logs and traces** over OTLP/gRPC
straight into Netdata's receiver — no collector in between. For the prospect
who says "we already run OpenTelemetry".

```bash
./scripts/otlp.sh                      # builds a venv on first run
./scripts/otlp.sh --service checkout-api --rps 20
```

It emits a storefront service: request counters, a latency histogram, in-flight
gauge, cart value, plus parent/child spans and correlated INFO/WARN/ERROR logs.

What Netdata does with each signal — **probed against agent v2.10.0, not
assumed**:

| Signal | Ingested | Where it shows up |
|---|---|---|
| Metrics | yes | Charts, as `otel.*` contexts. Histograms arrive fully decomposed (`.bucket`, `.count`, `.sum`, `.minmax`) |
| Logs | yes | The `otel-logs` function, with `service.name`, severity and resource attributes as columns |
| Traces | yes | Persisted to `/var/log/netdata/otel/v2/traces/` — **but there is no trace viewer on the agent yet**, so spans are stored rather than rendered |

Two limitations worth knowing before you demo it:

- **OTLP does not create Netdata nodes.** Verified by probe: two resources with
  distinct `host.name` produced no new nodes — both landed on the ingesting
  host, attributes flattened into `resource.attributes.*` labels, one chart
  instance per attribute set. Distinct series, one node. So there is no node
  list, no per-node ML ranking, no node-scoped alerts and no re-skin story on
  this path. For a fleet, use the plugins.d path (the rest of this README).
- `trace_id` / `span_id` are not exposed as columns by `otel-logs`, so
  trace↔log correlation is sent but not currently surfaced.

Observed and not diagnosed: OTLP log timestamps render as year 9999 in the
agent's function API — the stored value is nanoseconds while the column
declares a microsecond transform. `systemd-journal` renders correctly, so it is
specific to this path. Check how it looks in your Cloud dashboard before
demoing the logs pane.

### 2. Monitor an OTel Collector fleet

`specs/otel-collector.yaml` models a collector's **own** internal telemetry —
receiver accepted/refused, batch behaviour, dropped points, exporter queue
against capacity, enqueue failures, send retries, process RSS/CPU.
`environments/otel-fleet.yaml` places it in the topology people actually run:
agent collectors beside the application, forwarding to a gateway pair.

```bash
./target/release/infra-sim --environment environments/otel-fleet.yaml --lint 72
```

This runs on the plugins.d path, so it is a real multi-node fleet with per-node
dashboards and ML. The story is a genuine pain point: a collector that silently
drops telemetry is the worst failure in an observability pipeline, because the
thing that would have told you is the thing that broke.

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
