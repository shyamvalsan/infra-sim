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

You need **Rust** (stable) and **Docker**. Root is needed for the console, which
writes under `/etc/netdata`. A Netdata agent on this machine is optional — each
simulation runs its own inside its container.

```bash
git clone https://github.com/shyamvalsan/infra-sim
cd infra-sim
cargo build --release
sudo ./target/release/infra-sim-console --repo "$PWD"
```

Open <http://127.0.0.1:8080>. Then:

1. **Describe the estate** in plain English, or skip it and build the fleet from
   the picker — counts, roles, and which software runs on each tier.
2. **Name it and create it.** The name fixes the seed and every node identity, so
   it cannot change later without orphaning the fleet's history. The first create
   builds the container image, which takes a minute or two; after that it is
   seconds. A fidelity check runs before anything is installed and refuses a fleet
   that would give itself away.
3. **Watch it come up.** The console prints the simulation's own dashboard URL, and
   nodes appear there within about a minute.
4. **Claim it into Netdata Cloud** (optional) with a token from
   *Cloud → Connect Nodes*, and a room id if you want one. The agent is fresh and
   unclaimed, so it joins whatever Space you point it at; your own agent is never
   touched.
5. **Run the demo** on the Run tab: trigger a scenario, escalate it, resolve it.
6. **Tear it down** when you are finished. One button removes the container and
   everything inside it — agent, database, every simulated node.

### Describing an estate in plain English

That step reads your description with a model, which needs a key in a gitignored
`.env` beside the repo:

```bash
echo 'LLM_API_KEY=...' >> .env      # Netdata's own gateway
```

`ANTHROPIC_API_KEY` and `OPENAI_API_KEY` work too; the console offers only
providers whose key it can find. `sudo` strips the environment, so `.env` is the
one place a key reliably survives.

Without a key nothing breaks — the description box says so, and you build the
fleet from the picker instead.

Running this for a prospect? **[docs/QUICKSTART.md](docs/QUICKSTART.md)** is the
demo-day path, including the one thing that costs demos most often: a fleet needs
about 72 hours of running before its ML has anything to say.

Every step also has a command-line path — see
**[docs/operating.md](docs/operating.md)**.

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

**Logs, two ways.** Each node becomes its own log source in Netdata, with lines
that follow whatever fault is running — healthy nodes stay quiet, because a node
logging steadily while nothing is wrong is its own tell. On top of that, the
application tier ships OpenTelemetry logs: request failures, orders, declined
payments, saturated pools, carrying the simulated host as an attribute. Both start
with the simulation; neither is a second command to remember.

**Traces.** The application tier emits OpenTelemetry spans — a request, the
queries it made, how long each took — with durations taken from the same latency
the charts draw. Simulations run Netdata's nightly image, which accepts and stores
them. Worth knowing before you plan a demo around them: no Netdata build can
*display* traces yet. They are sent so the pipeline is right the day that changes.

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

- **[docs/QUICKSTART.md](docs/QUICKSTART.md)** — the demo-day path, start to
  teardown, and the mistakes that cost demos.
- **[docs/operating.md](docs/operating.md)** — the reference: containers, the
  console, the command line, scenarios, logs, exporters, and how to check a fleet
  before it ships.
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
