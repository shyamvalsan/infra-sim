# Infra-Sim

Infra-Sim builds infrastructure that does not exist, in enough detail that
Netdata cannot tell the difference.

You describe a fleet — "twelve web servers behind two HAProxy boxes, a 3-node
Postgres cluster, Redis for sessions, four Cisco Catalyst access switches" — and
you get that fleet: nodes with hostnames, hardware, processes, users, disks,
interfaces, logs and metrics, streaming into a real Netdata agent. Then you break
it on cue, and watch the real product find the problem.

<img width="469" height="426" alt="image" src="https://github.com/user-attachments/assets/dbeeec54-2b9a-4238-81fb-36f708180533" />


## Why simulate infrastructure

Monitoring is unusual among software: you cannot exercise it on its own. A
database can be tested with a test database. An observability platform needs
infrastructure — a lot of it, behaving badly, when you want it to.

**Healthy infrastructure proves nothing.** What matters about a monitoring system
is how it behaves when something is wrong, and your production is, hopefully, not
wrong. Pointed at a healthy fleet, any tool can draw a chart.

**Failures do not arrive on schedule.** If you want to watch what happens during a
slow memory leak, or a dirty fibre optic that never quite drops the link, you can
wait for one — or you can make one, now, and again after lunch.

**Real incidents have no answer key.** You learn what happened eventually and
imperfectly. An authored incident knows its own root cause, which is the only way
to grade an answer: did the alert fire for the right reason, did the runbook lead
anywhere, did the investigation find the cause or a coincidence?

**Breadth is otherwise unobtainable.** Nobody stands up real instances of hundreds
of technologies, in every combination, at the size others run them. Not to test an
integration, not to show someone, not to practise on.

**Repeatable and safe.** Break a simulation as hard as you like, as often as you
like, identically every time — same seed, same world. You cannot break production
to find out what the dashboard does.

## What it is used for

- **Showing what monitoring looks like** on infrastructure that resembles what the
  people watching actually run, instead of charts they have to mentally translate.
- **Evaluating** alerts, dashboards and integrations against a fleet the size of
  yours, before committing to anything.
- **Practising incident response** against faults you can trigger on demand, with
  a known cause to check your conclusion against.
- **Testing your own** alert rules, notification paths and runbooks — the parts
  that are only ever exercised at the worst possible moment.
- **Measuring detection.** Anomaly detection and automated investigation can only
  be scored against incidents whose cause is known in advance. This is a supply of
  them, on demand.

None of that works if the fleet is unconvincing, so the bar is set there: an
experienced SRE zooming into individual charts should find nothing that gives the
game away.

## The one hard rule

**Synthetic world, live product.**

Only the raw data is simulated. Everything downstream is the actual shipping
product: ML really trains on the data and detects anomalies in it, the health
engine really evaluates its alarms and raises alerts, Netdata AI really
investigates and reaches its own conclusions.

Nothing downstream of data injection is ever scripted, canned or mocked. No
prepared AI answers, no fake alert states, no dashboards drawn to look busy. When
you see Netdata find a root cause here, Netdata found it — which is the only reason
watching it is worth anything.

The corollary matters as much: **every simulated environment says so.** Nodes
carry a `simulated=true` host label, hostnames are prefixed, and the console shows
a SIMULATED badge. Nobody should ever have to wonder whether a node is real.

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

You need a **Linux host**, **Docker**, and root — the console writes under
`/etc/netdata` and drives Docker. A Netdata agent on this machine is optional: each
simulation runs its own inside its container. No Rust toolchain is needed;
`startsim` builds the binaries in a container, because Docker is required anyway.

Linux is a hard requirement, not a preference: the binaries are built as Linux ELF
and run on the host, and the runtime writes `/etc/netdata` and `/var/lib/infra-sim`.
On macOS or Windows, Docker Desktop will happily run the simulation containers but
the console itself cannot run — use a VM (Multipass, UTM, or any Linux server).
`startsim` checks this first and refuses immediately rather than after a build.

On a Linux machine with nothing but Docker:

```bash
curl -fsSL https://raw.githubusercontent.com/shyamvalsan/infra-sim/main/startsim.sh | sudo bash
```

On macOS — **no `sudo`**, because the console runs unprivileged so it can reach
Docker Desktop's user-scoped socket, and because it runs in a container rather than
on the host:

```bash
curl -fsSL https://raw.githubusercontent.com/shyamvalsan/infra-sim/main/startsim.sh | bash
```

macOS is **experimental and not yet working**: the UI comes up, but a running
simulation's node table and scenario controls are still empty.

So on a Mac, use the VM route, which is the ordinary Linux path and is fully
exercised. One command, once Multipass is installed:

```bash
brew install --cask multipass          # once
./startsim-vm.sh                       # creates the VM, provisions it, starts the console
```

It launches an Ubuntu VM, installs Docker and git *inside it*, clones, starts the
console, and prints the URL to open. Your Mac is not modified — Multipass has to be
there already, and the script installs nothing on the host. `--cpus`, `--memory` and
`--disk` size the VM; 160 simulated nodes is not a 2G job. `multipass stop infra-sim`
when you are done, `multipass delete --purge infra-sim` to reclaim the space.

Or from a clone:

```bash
git clone https://github.com/shyamvalsan/infra-sim
cd infra-sim
sudo ./startsim.sh
```

It checks what it needs — Docker, its daemon, python3, root — and **installs
nothing**: anything missing stops the script with the command to fix it. The first
run compiles the dependency tree and takes a couple of minutes; later runs reuse
the binaries unless you pass `--rebuild`. `--bind HOST:PORT` moves the console off
`127.0.0.1:19995`.

Developing on it instead? `cargo build --release` still works and `startsim` will
use what it finds. Note that `Cargo.toml` declares `rust-version = "1.85"` but the
lockfile needs **1.88** or newer.

Open <http://127.0.0.1:19995>. Then:

1. **Describe the fleet** in plain English, or skip it and build it from the
   picker — counts, roles, and which software runs on each tier.
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
5. **Watch the product work on it.** ML trains on it, the health engine alerts on
   it, Netdata AI investigates it — see the rule above.
6. **Trigger an incident** on the Run tab: start a scenario, escalate it, resolve
   it.
7. **Tear it down** when you are finished. One button removes the container and
   everything inside it — agent, database, every simulated node.

### Describing a fleet in plain English

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

Showing this to someone on a particular day?
**[docs/QUICKSTART.md](docs/QUICKSTART.md)** walks the whole path, including the
thing most easily got wrong: a fleet needs about 72 hours of running before its ML
has anything to say.

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
the right rated speeds. Cloud provider and instance-type labels. Host labels the
way real fleets carry them — `environment`, `site`, `team` — authored per fleet
and per tier in the console (suggested from your description when one is given),
editable live on a running fleet, and validated against the agent's own label
rules so what you write is what the node reports.

**Logs, two ways.** Each node becomes its own log source in Netdata, with lines
that follow whatever fault is running — healthy nodes stay quiet, because a node
logging steadily while nothing is wrong is its own tell. On top of that, the
application tier ships OpenTelemetry logs: request failures, orders, declined
payments, saturated pools, carrying the simulated host as an attribute. Both start
with the simulation; neither is a second command to remember.

**Traces.** The application tier emits OpenTelemetry spans — a request, the
queries it made, how long each took — with durations taken from the same latency
the charts draw. Simulations run Netdata's nightly image, which accepts and stores
them. Worth knowing before you rely on them: no Netdata build can *display* traces yet.
They are sent so the pipeline is right the day that changes.

**Location.** Fleets can be placed on the map, per site, so a fleet spanning
regions looks like one.

**Other paths in.** Prometheus endpoints that Netdata's own collector scrapes —
application-tier metrics with one shared context, so fleet-level views combine
them — and OpenTelemetry.

## Incidents

Each scenario carries a ground-truth manifest: the root cause, the causal chain,
the blast radius, and the finding a competent investigator should reach. It is
written *with* the scenario, never reconstructed afterwards from what the product
happened to say — otherwise it grades nothing.

| Scenario | What it shows |
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

## So how good is the simulation? 

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
while, with real trained models and a real alert log, and it can be renamed and
reshaped for a different audience without losing any of that.

**Every fleet is checked before it ships.** A fidelity pass simulates hours of
data and refuses anything that gives itself away: a signal pinned to a bound, an
impossible percentage, a total that does not equal its parts, a value that never
moves when it should. Changes are validated against a live agent, because "it
compiles" has never been evidence here.

## Documentation

- **[docs/QUICKSTART.md](docs/QUICKSTART.md)** — the shortest path from nothing to
  a running fleet with a live incident, and the mistakes that spoil a first
  attempt.
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
