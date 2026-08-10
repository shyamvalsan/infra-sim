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

## The generated SNMP device corpus

`specs/generated/snmp/*.yaml` holds 105 device specs, one per SNMP device profile
that names a vendor, generated by `scripts/sync-snmp-profiles.py`.
`integrations/snmp-devices.json` is the picker's model list; 7029 contexts across
71 vendors.

These are generated from a *device profile*, not collector metadata, and a
profile is a better source: it declares units. Taken verbatim, cited against
`netdata/netdata @ c23face0bd94` `src/go/plugin/go.d/collector/snmp`:

- the context id `snmp.device_prof_<name>` (`metric_ids.go:56`);
- the chart's units, the profile's own UCUM string - `By/s`, `{packet}/s`, `Cel`
  - and `1` where a profile declares none (`charts.go:201,209`);
- the title from `chart_meta.description`, the family from `chart_meta.family`;
- `area` for `bit/s` (`charts.go:215`);
- one 0/1 dimension per state for a metric with a `mapping:`, because that is how
  the collector renders an enum (`ddsnmp/transform.go:107-114`);
- `virtual_metrics`, the composed in/out charts.

That last one is load-bearing. Interface traffic, packets, errors and discards
are **not** in a profile's `metrics:` - the per-direction OIDs are collected as
`_`-prefixed inputs and charted only through `virtual_metrics`, which just nine
shared files declare and 94 device profiles inherit. A first pass that read only
`metrics:` produced a Catalyst with 165 charts and no traffic chart at all.

Invented: the magnitudes, derived from the unit (`%` rests near 35, `Cel` near
41, an `{error}/s` at 0 until a scenario moves it). Names matching
total/size/capacity/limit are emitted as declared constants, since a capacity is
a fact rather than a measurement.

### Standard counters keep one name

`ifHCInOctets` becomes the signal `if_in_octets_rate` on every model, and the
same for the other IF-MIB counters, statuses, speed, CPU, memory and uptime. One
scenario therefore degrades an uplink on a Catalyst, a Juniper MX and an F5
without a per-vendor variant. Without this the 105 device specs would be
unscriptable, which is most of their value gone.

Two overrides ignore the unit and take the shape the hand-authored switch spec
already settled: uptime accumulates a tick per second rather than resting on a
noisy 86400, and port speed comes `from_attr` the port's own `if_speed_mbps` so a
10G uplink never reports 1000.


## Node classes

Most nodes are Linux boxes: the fleet's `generator:` is the Linux baseline and
services compose on top. A **network device** is not, so it carries its own base
spec through a per-node `generator:` override.

```yaml
generator: ../specs/linux-system.yaml   # the fleet's default
nodes:
  - hostname: acme-sw-01
    generator: ../specs/network-device.yaml   # this node only
    instances:
      interface:
        - { name: TenGigabitEthernet1/1/1, weight: 4.0, attrs: { if_speed_mbps: 10000 } }
```

Absent means the fleet's, so every environment written before this keeps
working. The composed-spec cache is keyed on `(base spec, services)` - keying on
services alone gave a mixed fleet whichever class was composed first, which is a
wrong-data bug rather than a crash.

An instanced context whose group is absent is skipped, so a node with no disks
or mounts simply has no disk charts. That was verified by probe before the class
was designed; no engine change was needed for it.

### Two signal properties this required

- **`ignore_weight`** - weight expresses how much flows *through* an instance,
  which is right for throughput and wrong for a property *of* it. A link-up
  signal of 1.0 on a 0.35-weight access port emitted 0, i.e. 24 dead ports on
  every simulated switch.
- **`from_attr`** - takes the value verbatim from a node or instance attribute,
  bypassing seasonality, noise, weight and bounds. A port's rated speed is a
  specification, not a modelled quantity, and one context has to serve a 1G
  access port and a 10G uplink. The fidelity lint treats an attribute-sourced
  signal as a declared constant, or all 24 ports report a stuck signal.


## Composing depth with breadth

A hand-authored spec may declare `extends:`, listing generated specs by path
relative to the specs directory:

```yaml
name: postgres
extends:
  - generated/postgresql
```

The named specs load first, in order, and the hand-authored one is layered over
the result with `GeneratorSpec::overlay`. Colliding contexts and signals are
replaced by the hand-authored definitions, in place, so the generated ordering
and priorities survive.

The point is that breadth and depth stopped competing. A simulated Postgres
emitted 15 contexts where Netdata's own collector emits 70; it now emits 71, of
which the 15 are causally coupled and are what the hero scenarios target.

One level only. A chain would be a spec hierarchy, which is more machinery than
this needs, and the loader rejects it rather than recursing.

`otel-collector` has no generated equivalent and extends nothing.

### Dependencies travel with the install

`install()` copies every spec named by an `extends:` in the specs it installed,
and the console then lints the **installed** copy. The repo lint cannot catch an
install-layout fault, because paths resolve differently there - and a fleet that
installs cleanly then dies on its first tick does not merely fail. netdata
disables a plugin that exits with an error before collecting anything
(`netdata/netdata @ c23face0bd94` `src/plugins.d/plugins_d.c:94-98`) until the
agent restarts, so one broken install silently breaks the next one too.
