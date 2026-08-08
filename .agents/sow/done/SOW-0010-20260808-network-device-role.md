# SOW-0010 - Network devices as a first-class node class

## Status

Status: completed

Sub-state: Delivered and validated on a live agent. Switches are a first-class
node class with their own base spec, a mixed fleet works, and the hero scenario
behaves exactly as its manifest claims.

## Requirements

### Purpose

Switches, routers and firewalls are a large part of most prospects' estates and
this simulator cannot represent them at all. The catalogue advertises
`snmp-devices` with 6 charts, all of them licensing metrics - an SE who ticks it
gets no interfaces, no traffic and no errors, which is worse than not offering
it.

Every role this project has is Linux-shaped: cores, RAM, disks, mounts, and the
Linux baseline composed underneath. A switch has none of that. It has ports.

### User Request

"also what about network devices (snmp)" followed by "option 1 then.."
(2026-08-08), selecting a `network-device` role with a hand-authored SNMP spec
over two cheaper alternatives.

### Assistant Understanding

A network device is a **new node class**, not another service spec. It needs:

1. A base generator spec emitting only SNMP contexts.
2. A role whose shape is ports rather than disks.
3. A way for one fleet to hold both Linux nodes and network devices.
4. A hero scenario, so the class is demo-capable rather than decorative.

### Acceptance Criteria

1. A `network-device` node emits per-port SNMP contexts and **no** Linux
   contexts.
2. One environment can hold Linux nodes and network devices together.
3. The spec passes the 6-hour fidelity lint, including a monotonic uptime.
4. A hero scenario targets the class and carries a ground-truth manifest.
5. The console offers the role, and `--describe` resolves "switches" onto it.
6. Charts land on a live agent with the labels Netdata's own SNMP collector
   would produce.

## Analysis

### What Netdata's SNMP collector actually does

`netdata/netdata @ c23face0bd94`:

- `src/go/plugin/go.d/collector/snmp/collector.go:218` and
  `device_state.go:22` - the collector **creates one vnode per device**. A
  switch is already its own node in Netdata, which is exactly the shape this
  project emits, so the class fits the existing model rather than fighting it.
- `src/go/plugin/go.d/collector/snmp/metric_ids.go:53` - contexts are
  `snmp.device_prof_<cleaned metric name>`, built **at runtime** from device
  profiles, not declared in `metadata.yaml`.
- `src/go/plugin/go.d/config/go.d/snmp.profiles/default/` - 272 profiles.
  `generic-device.yaml` extends `_std-if-mib.yaml`, `_system-base.yaml` and the
  std IP/TCP/UDP MIBs.
- `_std-if-mib.yaml` is the universal set every device reports:
  - `ifTable`: `ifInErrors`, `ifOutErrors`, `ifInDiscards`, `ifOutDiscards`,
    `ifAdminStatus`, `ifOperStatus`, tagged by `interface` and `_if_type`
  - `ifXTable`: `ifHCInOctets`, `ifHCOutOctets`, uni/multi/broadcast packet
    counters both directions, `ifHighSpeed`
  - plus `ifNumber` at device level
- Across all 272 profiles the most universal device-level metrics are
  `cpu.usage` (49 profiles), `memory.usage` (19), `memory.total` (15),
  `memory.free`/`memory.used` (10 each), `systemUptime` (3).

This is why the synced `snmp-devices` spec is useless: `metadata.yaml` declares
only a licensing scope, so the sync structurally cannot see the interface
charts. The generated spec is not merely thin, it is **misleading**, and must be
removed rather than left beside the real one.

### Probe: does the engine tolerate a node with no disks or mounts?

Run before designing, per this project's own rule. A four-context SNMP-only spec
and a one-node environment with an `interface` instance group and **no** disk,
mount or net groups:

```text
infra-sim: environment 'probe-snmp' - 1 node(s), base spec 'probe-snmp' (4 contexts)
infra-sim lint: 2h simulated, 7200 samples per node
  semantic checks: 1 violation(s)
    perfectly flat: snmp.device_prof_system_uptime held 4200000 for all 7200 samples
  PASS  sim-probe-sw-01
```

**Answer: yes, no engine change is needed.** `plan_charts`
(`crates/sim-engine/src/lib.rs:520`) skips an instanced context whose group is
absent, so a node simply has no disk charts. Per-port charts, labels and the
fidelity harness all worked on the first attempt.

The single violation was my own modelling error, not an engine limit: uptime
must increase. `specs/linux-system.yaml:369` already solves this with an
`uptime_tick` signal under the `counters` shape, and that pattern carries over.

### The actual blocker: one fleet, two node classes

`generator:` is fleet-wide (`crates/sim-plugin/src/environment.rs:81`). A mixed
fleet is impossible today - a switch would be composed on top of the Linux
baseline and report CPU, RAM and mounts it does not have, which is precisely the
disqualifying artifact this class exists to avoid.

Two call sites assume one spec:

- `crates/sim-plugin/src/main.rs:388` - the composed-spec cache keys on
  `node.services.join("+")`, with no notion of a differing base.
- `crates/sim-plugin/src/main.rs:368` / `:425` and
  `crates/sim-console/src/main.rs:265` - "the" generator spec and its context
  count, for reporting.

## Pre-Implementation Gate

**Problem / root-cause model.** Every role is Linux-shaped because the
environment format has exactly one generator spec for the whole fleet. That was
correct while every simulated node was a server; a network device is the first
node that is not, and it makes the single-spec assumption load-bearing in the
wrong direction.

**Evidence reviewed.** Listed in Analysis above: the SNMP collector's vnode
creation, context derivation and profile set from `netdata/netdata @
c23face0bd94`; the local probe result; the three call sites assuming one spec;
`specs/linux-system.yaml`'s existing monotonic-uptime pattern.

**Affected contracts and surfaces.** Environment format (per-node `generator:`),
spec composition and its cache key, `specs/network-device.yaml` (new),
`scenarios/` (new hero scenario), role table in
`crates/sim-engine/src/describe.rs`, the console's create form and status
reporting, `scripts/sync-integrations.py` (drop the misleading `snmp-devices`
spec), catalogue, README, quickstart, specs.

**Existing patterns to reuse.** The `counters` shape and `uptime_tick` for
monotonic uptime; `instances:` groups for per-port charts, exactly as `disk` and
`net` work today; the ground-truth manifest format; the fidelity lint as the
gate.

**Risk and blast radius.** The environment format changes, so every committed
template and every archived environment must still load - the new field is
optional and absent means today's behaviour. The composition cache key must
include the base spec or a mixed fleet silently gets one node class's charts on
the other, which is a wrong-data bug rather than a crash. Scenario targeting by
role keeps re-skin correct.

**Sensitive data handling plan.** No credentials involved. SNMP communities are
credentials and this simulates the *result* of polling, never the polling - no
community string, real or fake, appears anywhere. Device hostnames stay
obviously synthetic (`sim-*`).

**Implementation plan.**

1. Per-node `generator:` in the environment format, defaulting to the fleet's.
2. Composition cache keyed on `(base spec, services)`.
3. `specs/network-device.yaml` from `_std-if-mib.yaml` plus `cpu.usage`,
   `memory.usage`, `systemUptime`, with monotonic uptime.
4. `network-device` role: ports instead of disks, sensible port counts.
5. Hero scenario - an uplink degrading: errors and discards climbing on one
   port, throughput collapsing, blast radius into the servers behind it.
6. Console and `--describe` support; drop `snmp-devices` from the sync.
7. Live-agent validation.

**Validation plan.** 6-hour lint on the new spec; a mixed fleet linted and
installed; charts and labels verified against a live agent; the scenario
triggered and its manifest checked against what the health engine and ML
actually did; every committed template re-linted to prove the format change is
backwards compatible.

**Artifact impact plan.** `README.md`, `docs/QUICKSTART.md`,
`.agents/sow/specs/generator-and-engine.md`,
`.agents/sow/specs/runtime-and-scenarios.md`,
`.agents/sow/specs/authoring-environments.md`.

**Open decisions.** Resolved - the user chose option 1 (`network-device` role
plus hand-authored spec) over fixing the generated spec in place (option 2) or
dropping it (option 3). Recorded below.

## Implications And Decisions

1. **Option 1 chosen by the user.** A `network-device` role with a hand-authored
   SNMP spec. Rejected: composing SNMP onto a Linux node (every "switch" would
   report `/var` filling up - the artifact an SRE spots instantly), and dropping
   the integration (leaves a common prospect shape undemoable).
2. **The generated `snmp-devices` spec is removed, not kept alongside.** Six
   licensing charts advertised as SNMP support is worse than nothing.
3. **Per-node `generator:` is optional.** Absent means the fleet's, so every
   committed template and archived environment keeps working untouched.

## Plan

As in the gate.

## Execution Log

### 2026-08-08

- Probe run and recorded above.
- Per-node `generator:` override; composed-spec cache keyed on
  `(base spec, services)`.
- `specs/network-device.yaml`: 9 contexts, 26 ports, device CPU/memory/uptime.
- `network-device` role in the role table, with its own renderer branch - no
  disks, no mounts, no Linux labels, an `interface` instance group.
- `scenarios/switch-uplink-degrading.yaml` with a ground-truth manifest.
- `Signal::ignore_weight` and `Signal::from_attr` in `sim-spec`, both applied in
  `eval_signal`/`value`; the fidelity lint treats `from_attr` as a declared
  constant.
- `Target::hostname_suffix` in `sim-spec`, validated by `check_scenarios`.
- `snmp-devices` dropped from the sync, the catalogue and disk.

### Findings during implementation

- **Interface status read 0 on every access port.** The instance weight was
  applied to a constant status signal, so 1.0 on a 0.35-weight port emitted 0 -
  a simulated switch with 24 dead ports. Weight belongs on quantities flowing
  *through* an instance, not on properties *of* it, hence `ignore_weight`.
  Invisible in the lint (a flat 0 is legitimate) and obvious in one query.
- **Every port then reported 1000 Mbps**, including ports named
  TenGigabitEthernet. Fixed by `from_attr`, which reads the port's own
  `if_speed_mbps` - the same idea mounts already use for their size. The lint
  then flagged all 26 ports as stuck signals, correctly, until it learned that
  an attribute-sourced signal is constant by construction.
- **The scenario faulted every switch at once.** Targeting by role is right for
  a fault that really is fleet-wide and wrong for a physical one; the manifest
  said "one port of one switch" while both switches showed it. Since the
  manifest is what Netdata AI is scored against, that is not a cosmetic
  mismatch. `hostname_suffix` pins it and survives a re-skin.
- **The lint caught a wrong signal name** in the scenario
  (`net_rx_packets_rate` for `net_rx_pkt_rate`) before it ever ran - exactly the
  silent-no-op class it exists for.
- **A fleet created after a teardown never starts.** Netdata does not relaunch a
  plugin it already knows when the file reappears; only an agent restart does.
  Unrelated to network devices but discovered here, and it breaks the core SE
  loop. Filed as `SOW-0011`, documented in the quickstart, not fixed here.

## Validation

All against live agent `v2.10.0-1030-nightly`.

**Mixed fleet.** `--describe "6 web servers, a postgres primary, and 4 access
switches"` produced 6 web + 1 db + 4 network-device, and the fleet passed the
**6-hour** fidelity lint with no semantic violations and no pinned signals. Both
node classes composed from their own base specs in one environment.

**Node class.** `netdemo-sw-01` on the agent: **167 charts - 159 SNMP across 26
ports plus 3 device charts**, and **zero Linux contexts** (checked for
`system.*`, `disk*`, `mem.*`, `cpu*`: none). Host labels carry
`device_vendor`/`device_model`/`device_type: switch` and none of
`_os_name`/`_kernel_version`/`_system_cores`.

**Per-port correctness**, queried live after the two fixes:

| | value |
|---|---|
| uplink `if_speed` | 10000 |
| access port `if_speed` | 1000 |
| access port `if_status` | oper 1, admin 1 |
| uplink traffic | 9.0 MB/s in, 7.4 MB/s out |

Chart ids carry the real port names (`if_traffic.TenGigabitEthernet1/1/1`) and
Netdata accepts the `/`; the `interface` chart label is set.

**Hero scenario**, triggered and advanced +10m:

| | errors/s |
|---|---|
| sw-01 faulted uplink 1/1/1 | **45.0 in, 12.0 out** |
| sw-01 other uplink 1/1/2 | 0 |
| sw-01 access port 1/0/7 | 0 |
| **sw-02 same uplink** | **0** |

Link status stayed `1, 1` throughout - the fault is invisible to a link-state
check, which is the scenario's whole premise. Device CPU rose 14% → 29.6%.
Blast radius reached the tier behind it: `web_log.request_processing_time` on
web-01 read 44 / 176 / 1058 ms (min/avg/max) against a much lower baseline.
Resolve returned the fleet to normal.

**Backwards compatibility.** The per-node `generator:` field is optional; every
committed template still loads and lints.

**Tests**: 186 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
clean. `cargo fmt --check` clean.

**Same-failure search**: the weight-on-a-property class was checked across every
spec with instanced contexts - `linux-system.yaml`'s per-disk and per-mount
signals are all genuine quantities, and the only constants (`disk_total_kb`,
`inodes_total`) already come through attributes, so `network-device.yaml` was
the sole instance. The role-targeting class was checked across all six
scenarios: the other five target roles whose faults genuinely are per-role or
whose environments hold one such node, so none of them mis-scope today - but the
risk is now named in the runtime spec.

**Sensitive data gate**: no credentials. SNMP community strings are credentials
and none appears anywhere - this simulates the result of polling, never the
polling. Hostnames and device labels are obviously synthetic (`sim-networks`,
`SIM-2960X-24`).

**Artifact maintenance gate**:

- `AGENTS.md` - no change needed; no new project-wide guardrail.
- Runtime project skills - none yet; `SOW-0007` tracks the first. This SOW adds
  a rule for it: probe the node shape before designing a new node class.
- Specs - `generator-and-engine.md` gains node classes, `ignore_weight` and
  `from_attr`; `runtime-and-scenarios.md` gains `hostname_suffix`.
- End-user docs - `README.md` gains network devices; `docs/QUICKSTART.md` gains
  the post-teardown restart trap.
- SOW lifecycle - completed, in `done/`, committed with the work. `SOW-0011`
  filed for the plugin-relaunch defect.

## Outcome

A switch is now a first-class simulated node: 26 ports, the metric set Netdata's
own SNMP collector reports, no operating system underneath, and a hero scenario
that demonstrates the failure a link-state check cannot see. One fleet holds
both node classes.

The misleading `snmp-devices` entry - six licensing charts sold as SNMP support
- is gone from the catalogue and from disk.

This is the first node class in the project that is not a Linux box, and the two
signal properties it needed (`ignore_weight`, `from_attr`) are general: UPS,
PDU and access-point classes can now be built from the same 272 device profiles
without further engine work.

## Lessons Extracted

- **The probe answered the question that would have driven the estimate.** The
  assumption going in was that a node without disks or mounts would need engine
  work; it needed none, and the real blocker turned out to be the fleet-wide
  generator field, which nobody had looked at.
- **A modelling primitive that is right for one axis can be silently wrong on
  another.** Instance weight had been correct for every signal in the project
  until a signal described the instance rather than what flowed through it. The
  failure mode was not an error: it was 24 ports reporting "down".
- **The lint cannot see a plausible zero.** A flat 0 is legitimate - a healthy
  port really does report zero errors - so the dead-port bug passed every
  automated check and took one live query to find. Automated fidelity checks
  bound the search; they do not replace looking.
- **A manifest that overstates specificity is worse than a vague one.** "One
  port of one switch" while every switch showed the fault would have scored
  Netdata AI against a claim that was not true. Ground truth has to be
  ground truth.

## Followup

- `SOW-0011` - a fleet created after a teardown does not start until the agent
  is restarted. Found here, breaks the core SE loop, needs root cause in the
  netdata source and then a user decision between three options.
- `SOW-0007` (integration-sync skill) gains a rule: probe the node shape before
  designing a node class, and query the live agent for properties the lint
  cannot judge.
- **More device classes are now cheap.** UPS, PDU and access points have
  profiles in the same set and need no further engine work - a spec and a role
  entry each. Not tracked as a SOW until a prospect shape calls for one.
- `SOW-0003`, `SOW-0004`, `SOW-0008` unchanged.

## Regression Log

None yet.
