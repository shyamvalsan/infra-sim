# Infra-Sim

Infra-Sim builds infrastructure that does not exist, in enough detail that
Netdata cannot tell the difference.

You describe an estate — "twelve web servers behind two HAProxy boxes, a 3-node
Postgres cluster, Redis for sessions, four Cisco Catalyst access switches" — and
you get that estate: nodes with hostnames, hardware, processes, users, disks,
interfaces, logs and metrics, streaming into a real Netdata agent. Then you break
it on cue, and watch the real product find the problem.

## Why this exists

**Generic demos do not sell.** A prospect watching charts from a fleet that looks
nothing like theirs is doing translation work instead of evaluating a product.
The demo that lands is the one where they recognise their own stack.

**Running the real thing does not scale.** Netdata integrates with hundreds of
technologies. Standing up a real instance of each — in every combination a
prospect might run, at the size they run it — is not work anyone can do before a
call.

**Interesting failures do not happen on schedule.** A monitoring product is
judged on how it behaves when something is wrong. Waiting for something to go
wrong is not a demo strategy, and neither is a screenshot.

So: simulate the infrastructure, keep everything else real.

## The one hard rule

**Synthetic world, live product.**

Only the raw data is simulated. Everything downstream is the actual shipping
product: ML really trains on the data and detects anomalies in it, the health
engine really evaluates its alarms and raises alerts, Netdata AI really
investigates and reaches its own conclusions.

Nothing downstream of data injection is ever scripted, canned or mocked. No
prepared AI answers, no fake alert states, no dashboards drawn to look busy. If a
demo shows Netdata finding a root cause, Netdata found it.

The corollary matters as much: **every simulated environment says so.** Nodes
carry a `simulated=true` host label, hostnames are prefixed, and the console shows
a SIMULATED badge. Nobody should ever have to wonder whether a node is real.

## Who it is for

1. **Sales engineers** — a demo environment shaped like the prospect's own
   infrastructure, built in an afternoon rather than a quarter.
2. **Public demo spaces** — always-on, clearly-labelled environments anyone can
   click into.
3. **The Netdata AI team** — a supply of incidents whose true cause is known in
   advance, which is what makes it possible to grade an answer.

The bar it is built to: an experienced SRE zooming into individual charts finds
nothing that gives the game away.

## How it works

```
  a description in plain English
            │
            ▼
     environment.yaml          which nodes, which software, which hardware
            │
            ▼
   a Netdata plugin  ──────▶   a real Netdata agent
            │                    ├── stores it
   scenarios move the            ├── trains ML on it
   underlying signals            ├── raises alerts on it
                                 └── lets Netdata AI investigate it
```

Faults are applied to the *signals* underneath the charts, not to the charts. A
disk filling up moves the same quantity every chart derived from it reads, so the
consequences propagate the way they would on a real host — which is what makes a
correlation genuinely discoverable rather than decorative.

## Getting started

```bash
cargo build --release
sudo ./target/release/infra-sim-console --repo "$PWD"
# open http://127.0.0.1:8080
```

Describe the estate, adjust the fleet, name it, create it. Each simulation runs
in its own container with its own agent, so it claims into whatever Netdata Cloud
Space you point it at and `docker rm` removes every trace of it. Your own agent is
never touched.

There is a command-line path for everything the console does; see
`--help` on the two binaries.

## What can be simulated

**Software.** 260 integrations, from Netdata's own collector metadata: correct
contexts, units, chart types and dimension names. Six are hand-authored in depth,
where the signals are causally coupled and scenarios target them by name.

**Network devices.** 105 device models from 71 vendors, generated from Netdata's
own SNMP device profiles. A Catalyst reports what a Catalyst reports — per-
supervisor CPU, chassis sensors, power supplies, PoE — because the vendor profile
says so. Pick a vendor and model, or take a generic managed switch.

**Node shapes.** Load balancers, web servers, databases, caches, Kubernetes
control planes and workers, edge gateways, network devices. A role is a shape —
cores, RAM, disks, interfaces — not a category of software.

**The details an SRE checks first.** Processes, users and groups that match what
a host of that kind actually runs. Mounts that fill at plausible rates. Ports with
the right rated speeds. Cloud provider and instance-type labels.

**Logs.** Each node becomes its own log source, with lines that follow whatever
fault is running. Healthy nodes stay quiet, because a node logging steadily while
nothing is wrong is its own tell.

**Location.** Fleets can be placed on the map, per site, so a multi-region estate
looks like one.

**Other paths in.** Prometheus endpoints that Netdata's own collector scrapes,
and OpenTelemetry.

## Incidents

Each scenario carries a ground-truth manifest: the root cause, the causal chain,
the blast radius, and the finding a competent investigator should reach. It is
written *with* the scenario, never reconstructed afterwards from what the product
happened to say — otherwise it grades nothing.

| Scenario | What it demonstrates |
|---|---|
| `disk-fill` | A database volume filling, and time-to-exhaustion |
| `db-replication-lag` | A replica falling behind until the application sees stale data |
| `memory-leak-oom` | A slow leak that ends in an OOM kill |
| `noisy-neighbour` | One tenant starving the others on shared hardware |
| `flapping-edge-links` | Intermittent physical-layer trouble on a remote uplink |
| `switch-uplink-degrading` | A dirty optic. The link never drops, so link-state monitoring reports the switch healthy while errors climb and the servers behind it slow down |

Faults escalate and resolve on demand, and they unwind gradually rather than
snapping back between two samples. Minor, self-resolving incidents can run on a
schedule, so an alert log has history before anyone looks at it.

## What is honest about this, and what is not

**Structurally faithful, and only sometimes causally faithful.** A generated
integration emits the right charts with the right units and the right dimension
names, but its signals move independently — the way a real system's do not. The
hand-authored ones are coupled deliberately. The console shows which is which
rather than implying they are equal.

**Values are plausible, not measured.** Nothing here was sampled from a real
system. Where a source declares a unit, that unit is used; magnitudes are chosen
to be believable for it.

**A fleet starts at zero.** There is no history to backfill — Netdata's plugin
protocol has no mechanism for it — so a new simulation begins now and accumulates
from there. A fleet that has been running a while has genuinely been running a
while, with real trained models and a real alert log, and it can be renamed for
the next prospect without losing any of that.

**Every fleet is checked before it ships.** A fidelity pass simulates hours of
data and refuses anything that gives itself away: a signal pinned to a bound, an
impossible percentage, a total that does not equal its parts, a value that never
moves when it should. Changes are validated against a live agent, because "it
compiles" has never been evidence here.

## Documentation

- `docs/operating.md` — how to drive it: containers, the console, the command
  line, scenarios, logs, exporters, and how to check a fleet before it ships.
- `spec.md` — the product definition: what this is for and what counts as done.
- `.agents/sow/specs/` — what is actually built, and why it works the way it does.

## Repository layout

```
crates/sim-spec      the generator and scenario formats
crates/sim-engine    signal evaluation, scenarios, logs, description parsing
crates/sim-plugin    the Netdata plugin, the logs writer, the exporter server
crates/sim-console   the control console
specs/               generator specs: what a node of some kind reports
scenarios/           fault timelines with their ground-truth manifests
environments/        fleet templates
integrations/        the catalogues the console offers
scripts/             sync tooling and the container lifecycle
```

## Requirements

Rust (stable), Docker for containerised simulations, and a Netdata agent. Root is
needed only for the paths that write under `/etc/netdata`, and
`systemd-journal-remote` only for correlated logs.
