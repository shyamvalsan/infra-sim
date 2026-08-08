# Generator specs and the execution engine

What a generator spec is, and how the engine turns one into samples. Describes
current reality as of `SOW-0001`.

## Shape of a spec

A spec (`specs/*.yaml`, parsed by `sim-spec`) declares **signals** and
**contexts**. Signals are the modelled quantities; contexts are the Netdata
charts that read them.

```yaml
signals:
  cpu_busy:
    base: 32.0
    daily_amplitude: 0.35
    noise: 0.06
    min: 0.0
    max: 100.0
    max_is_ceiling: true
contexts:
  system.cpu:
    title: Total CPU utilization
    units: percentage
    shape: partition          # independent | partition | counters
    driver: cpu_busy
    dimensions: [user, system, iowait, ...]
```

Three shapes:

| Shape | Meaning |
|---|---|
| `independent` | Each dimension reads its own signal. |
| `partition` | Dimensions split one driver signal and **must** sum to it. |
| `counters` | Monotonic accumulators; the agent computes rates. |

## Bounds are safety rails, not modelling tools

`min`/`max` exist to catch a broken model, not to shape output. A signal that
rests against a bound has stopped being modelled and started being clamped,
which flattens the metric — the artifact class an SRE spots instantly.

Because zero is a legitimate value for most modelled quantities, it is never
treated as a violation. Any other floor must be declared with
`min_is_floor: true`, and every ceiling with `max_is_ceiling: true`. Without
that distinction the first lint run reported 175 false violations.

**Declared ceilings are never widened, including by a scenario.** Granting
headroom to every `max` under an active scenario once pushed
`10min_disk_utilization` to 101.5% — impossible for one device.

## Determinism

Output is a pure function of `(spec, profile, seed, tick, scenarios)`.

- PRNG is SplitMix64, implemented in-repo (`sim-engine/src/rng.rs`) so
  reproducibility survives dependency upgrades.
- Streams are name-addressed via FNV-1a: each `(node, signal)` pair draws from
  its own stream, so adding a signal does not shift any other signal's values.
- Seasonality is a function of absolute time, so `--replay-from TS` pins the
  clock and makes replay bit-exact rather than merely same-shaped.

This property is load-bearing beyond replay: the correlated-logs process
recomputes the same values the metrics plugin emitted without the two
coordinating.

## Noise scales with the current level, not the base

Noise proportional to `base` gives a 3 a.m. trough the same absolute jitter as
an afternoon peak, driving troughs into their floor. Noise is proportional to
the current seasonal level.

## Composition

A node's spec is the Linux baseline merged with one spec per declared service
(`GeneratorSpec::merge`). Nodes sharing a service set share one composed spec,
so a 50-node fleet holds one copy per distinct set, not 50.

## Per-instance cardinality

Per-instance cardinality — not context count — was the real fidelity gap. A
node showing one unnamed disk chart reads as fake regardless of how many
contexts exist.

Instances are declared per node in groups (`disk`, `mount`, `net`,
`container`), each with a `weight` that scales every signal for that instance
and optional `attrs` overriding node attributes (e.g. per-mount
`disk_total_kb`).

Netdata's own chart-id and family conventions are **not uniform** — `disk.io`
becomes chart `disk.<dev>` in family `io`, while `net.packets` becomes
`net_packets.<iface>` in family `<iface>`. The spec states these explicitly
rather than inferring them; they were read off a real node.

## Chart planning happens once

`plan_charts()` produces the chart list, and both declaration and per-tick
emission read that one plan. Deriving them separately would let `SET` lines
reach charts that were never declared, which surfaces as silently missing data
rather than an error.

## Fidelity harness

Two layers, both offline (`--lint HOURS`):

1. **Pinned-signal check** — fails a signal resting on a *rail* for more than
   0.1% of samples.
2. **Semantic checks** (`sim-engine/src/fidelity.rs`) — `UnitOutOfRange`,
   `CounterWentBackwards`, `ConservationBroken`, `PerfectlyFlat`, `NotFinite`,
   `MissingTotal`.

The second layer exists because the first is structurally blind to a bound that
is itself wrong: the 101.5% utilisation passed the pinned check cleanly.

**The lint does not run scenarios.** A generated environment can pass the lint
and still saturate under a hero scenario — this is how `--describe` shipped
mounts sized so `disk-fill` clamped them at 100%.

## Known gaps

- No declarative log spec: log rules live in code (`sim-engine/src/logs.rs`),
  not YAML, unlike every other generator concern.
- The lint's coverage of scenario-active behaviour is manual.


## The generated integration corpus

`specs/*.yaml` holds six hand-authored specs. `specs/generated/*.yaml` holds 258
more, synced from Netdata's own collector metadata by
`scripts/sync-integrations.py`, and `integrations/catalogue.json` is what the
console's picker reads.

A collector's `metadata.yaml` gives the right contexts, units, chart types and
dimension names. It does not give plausible *values*, so the sync derives a
value profile from the unit and the dimension name:

- Short unit strings (`%`, `ms`, `B`, `pps`) match **exactly**, before any
  substring rule. Matching `%` as a substring of `percentage` is what let 60
  metrics fall through to a generic profile and produce 108% disk fragmentation.
- A dimension naming a failure (`error`, `drop`, `refused`, `timeout`, ...)
  starts at zero. A fleet idling with a steady error rate reads as broken, and a
  scenario needs headroom to move it off the floor.
- A 0/1 dimension is declared constant rather than modelled as a noisy gauge
  that rests on its ceiling.
- A negative baseline (dBm) drifts rather than following a working day.

### What generated specs are not

They are **structurally** faithful and **not causally coupled**: signals move
independently, and no scenario targets them. The catalogue labels each entry
`deep` or `generated` so the console can show the difference rather than imply
every integration is equal.

A labelled scope (per database, per index, per queue) becomes **one**
representative instance carrying that scope's chart labels. A real Elasticsearch
with twelve indices shows twelve chart instances; this shows one.

### The gate

Every generated spec must pass the 6-hour fidelity lint before it ships - all
258 do. The lint is what caught every unit misclassification above; reading the
YAML would not have.
