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
enforced by construction, plugins.d emitter with virtual-node support, a
50-context Linux baseline, a 5-node web-stack environment, and a fidelity lint.

**Not built:** scenario engine, correlated logs, control console, additional
verticals, OTEL simulation, the eval gym. See `.agents/sow/` for what is
tracked and `spec.md` for the full product definition.

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
identical chart set and differs solely in behaviour.

### Bounds are safety rails, not modelling tools

`min`/`max` should sit outside the range a signal actually reaches. A signal
pinned to a bound has stopped being modelled and is being clamped, which
flattens the metric visibly.

The lint enforces this, with one distinction: **zero is always a legitimate
value** for the non-negative quantities these specs model — a quiet interface
really does report zero errors — so a floor of `0.0` is never flagged. Any
other floor is a rail unless declared with `min_is_floor: true`, and every
ceiling is a rail unless declared with `max_is_ceiling: true`.

## Repository layout

```
crates/sim-spec/      generator spec format, parsing, validation
crates/sim-engine/    deterministic execution, seeded RNG, invariants
crates/sim-plugin/    plugins.d emitter, environment.yaml, fidelity lint
specs/                generator specs
environments/         environment definitions
scripts/              local install
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
