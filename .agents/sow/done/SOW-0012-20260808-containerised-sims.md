# SOW-0012 - One container per simulation, with its own agent

## Status

Status: completed

Sub-state: Delivered and validated. Two simulations ran side by side, isolated from each other and from the host agent.

## Requirements

### Purpose

A simulation currently installs into the operator's own Netdata agent. That
makes the agent's identity the simulation's identity, and the two cannot be
separated:

- **Claiming is impossible to do correctly.** The agent can be in one Space.
  Claiming a prospect's fleet would move the operator's own node into that Space
  with it, and `spec.md` asks for a fresh Space per prospect.
- **Teardown can only be approximate.** Stale vnodes stay in the agent's
  database, config is written under `/etc/netdata`, and a plugin that exits
  badly disables itself for the next fleet. Several defects fixed today were
  variants of "the host agent remembers the last simulation".

### User Request

"i think the way it should work is that each sim should live inside a docker
container, and a netdata agent that lives inside that container, and not try to
use the base netdata running on the system if any" (2026-08-08), then "yes go
forth and complete the build".

Prompted by hitting the claim conflict: the console reported the agent already
claimed, to a Space claimed on 2026-07-23 for the operator's own laptop.

### Assistant Understanding

Each simulation becomes a container running its own agent, its own vnodes and
nothing of the host's. The host agent is left alone.

### Acceptance Criteria

1. A simulation runs entirely inside a container, with the host agent untouched.
2. The container's own node does not look like a stray machine in the Space.
3. Claiming is per-simulation.
4. Correlated logs work inside the container.
5. Teardown removes the container and its history.
6. Two simulations run side by side without interfering.

## Analysis

### Probe 1: does a vnode-only agent work in a container?

Run before designing. `netdata/netdata:stable` with **no** `/proc`, `/sys` or
`/host` mounts, and the plugin plus environment mounted read-only:

```
nodes: ['33e3d3b8ac6d', 'probe-web-01', 'probe-web-02', 'probe-db-01']
```

All three vnodes appeared and were reachable. **No engine or plugin change is
needed.**

The container's own node is the problem: named after the container id, carrying
**527 charts, 136 of them host-style**. Even without host mounts the agent sees
its own namespaced `/proc`, so it reports the container's CPU, memory and
filesystem. In a prospect's Space that is a stray machine called `33e3d3b8ac6d`,
which is exactly the artifact this project exists to avoid.

Fixed by a mounted `netdata.conf` that names the node and turns off the internal
collectors:

```
nodes: ['probe-sim-parent', 'probe-web-01', 'probe-web-02', 'probe-db-01']
parent node: 181 charts total, 1 host-style
```

The 181 remaining are netdata's own internal metrics, which any agent has.

### Probe 2: what does the image cost us?

- Base is **Debian 13 (trixie)** with `apt-get`.
- `systemd-journal-remote` is **absent**. The image builds with
  `--internal-systemd-journal` (`netdata/netdata @ c23face0bd94`
  `packaging/docker/Dockerfile:42`) and ships `systemd-journal.plugin` and
  `systemd-cat-native`, but not the remote receiver correlated logs depend on.
  One `apt-get install` in a derived image.
- `netdata-claim.sh` is **absent**, but the daemon reads `NETDATA_CLAIM_TOKEN`
  and `NETDATA_CLAIM_ROOMS` from the environment - "a good choice for docker
  container users" in its own words (`src/claim/claim-with-api.c:603`). So
  claiming is `docker run -e`.

## Pre-Implementation Gate

**Problem / root-cause model.** The simulation has no identity of its own; it
borrows the host agent's. Every consequence above follows from that one fact.

**Evidence reviewed.** Both probes above; `packaging/docker/Dockerfile` and
`run.sh`; `src/claim/claim-with-api.c:600-618`; the claim state on this
workstation (claimed 2026-07-23, one Space, the operator's own node in it).

**Affected contracts and surfaces.** New `docker/` directory (image and agent
config), new operator script, `README.md`, `docs/QUICKSTART.md`, specs. The
plugin, engine and environment format are **unchanged** - probe 1 settled that.

**Existing patterns to reuse.** The install layout the console already produces
(`environment.yaml` beside `specs/` and `scenarios/`) is exactly what the
container needs mounted. The `run()` transparency pattern for operator scripts.

**Risk and blast radius.** Docker becomes a dependency of the container path
only; the host path keeps working, so an operator without Docker loses nothing.
A container per simulation costs one agent process each. The image must be
rebuilt when the plugin changes, which is a step the host path does not have.

**Sensitive data handling plan.** The claim token reaches the container as an
environment variable on `docker run`. That is visible in `docker inspect` for
the life of the container, which is a real exposure and worth stating plainly:
it is the mechanism netdata documents, and the alternative (baking it into an
image or a mounted file) is worse. No token is written to the repo, and the
script never echoes it.

**Implementation plan.** Derived image -> per-sim agent config -> operator
script covering the lifecycle -> logs inside the container -> docs.

**Validation plan.** Create a simulation in a container, confirm the vnodes and
the absence of host metrics, run a scenario, run correlated logs, run two
simulations side by side, tear one down and confirm nothing is left. Confirm the
host agent is untouched throughout.

**Artifact impact plan.** `README.md`, `docs/QUICKSTART.md`,
`.agents/sow/specs/runtime-and-scenarios.md`.

**Open decisions.** Resolved by the user's instruction to proceed. Recorded
below.

## Implications And Decisions

1. **The container path is for prospect demos and scale tests; the host path
   stays for local iteration.** Both run the same binary against the same
   environment file. Removing the host path would cost quick iteration and gain
   nothing.
2. **The console keeps managing the host agent.** Container lifecycle is an
   operator script. Teaching the console to manage containers is a larger
   surface and is tracked separately rather than half-done here.
3. **The container's node is named `<sim>-parent` and runs no host collectors.**
   A stray `33e3d3b8ac6d` reporting container CPU is the artifact this project
   exists to avoid.
4. **Claim by environment variable.** Visible in `docker inspect`; documented
   rather than hidden.
5. **The image carries the plugin binary** rather than mounting it, so an image
   tag identifies exactly what a demo ran.

## Plan

As in the gate.

## Execution Log

### 2026-08-08

- Probes run and recorded above.
- `docker/Dockerfile`: netdata + `systemd-journal-remote` + the plugin.
- `docker/netdata.conf.template`: names the parent node, disables the internal
  collectors.
- `scripts/sim-docker.sh`: build, create, list, status, scenario, logs, shell,
  teardown.
- `scripts/control_file.py`: trigger/resolve, writing in place.

### Findings during implementation

- **A bind-mounted file pins one inode.** `sed -i` writes a new file and renames
  it, so after triggering a scenario the host saw the change and the container
  kept reading the old inode - with a control file that was now invalid YAML.
  The symptom was a scenario that reported success and did nothing, which is the
  failure mode this project most wants to avoid. Fixed twice over: the payload
  directory is mounted whole, and `control_file.py` truncates in place rather
  than replacing.
- **The container's own node is not free.** Even with no host mounts it reports
  527 charts, 136 host-style, from its own namespaced `/proc`. In a prospect's
  Space that is a machine called `33e3d3b8ac6d`.

## Validation

All against `netdata/netdata:stable` in containers, with the host agent running
throughout.

**Isolation.** `customer-a` (17 nodes) on :19990 and `customer-b` (4 nodes) on
:19989 ran at the same time. Each agent listed only its own nodes. The host
agent listed `laptop` and nothing else for the whole exercise;
`/etc/netdata/custom-plugins.d` stayed empty and `/etc/netdata/infra-sim` was
never created.

**The parent node.** `customer-a-parent`: 203 charts, **1 host-style** - down
from 527 and 136 before the config. No node named after a container id.

**Node classes.** `customer-a-sw-01` carried 167 charts, 159 of them
`snmp.device_prof_*`, so the network-device class works unchanged in a
container.

**Correlated logs.** `logs customer-a start` produced **16 journal files**, one
per node, inside the container - the derived image's `systemd-journal-remote`
doing its job.

**Scenarios.** `disk-fill` triggered and advanced: `/var/lib/pgsql` on
`customer-a-db-01` went from 1334 GiB free to **146 GiB free**. Resolved, it
unwound to 894 GiB free over the recovery window rather than snapping back.

**Teardown.** `teardown customer-b` archived its environment and scenarios to
`archive/customer-b-<stamp>`, removed the container, and left `customer-a`
running. No stray containers.

Tests: 189 passed (unchanged - this SOW adds no Rust). `cargo clippy
--all-targets -- -D warnings` and `cargo fmt --check` clean. `bash -n` clean on
the script.

Same-failure search: the bind-mounted-file class was checked across every mount
the script makes. `netdata.conf` is written before the container starts and
never again; the payload directory is mounted whole; the journal directory is a
directory. No other file mount exists.

Sensitive data gate: the claim token is read from the environment or prompted
for with `read -s`, never echoed, never written to the repo. Its exposure via
`docker inspect` is documented in the script, the README and this SOW rather
than left for someone to discover. No token appears in any committed artifact.

Artifact maintenance gate:

- `AGENTS.md` - no change needed; the container path is documented in the specs
  and README, and adds no project-wide guardrail.
- Runtime project skills - none yet; `SOW-0007` still tracks the first.
- Specs - `.agents/sow/specs/runtime-and-scenarios.md` gains the container path.
- End-user docs - `README.md` gains "Two ways to run a simulation".
- SOW lifecycle - completed, in `done/`, committed with the work.

## Outcome

A simulation is now a container: its own agent, its own Cloud claim, its own
history, removed completely by `docker rm`. The claim conflict that prompted
this - one agent, one Space, the operator's own node in it - is gone, and so is
the class of defect where the host agent remembers the last simulation.

The host path is unchanged and still the fastest way to iterate locally.

## Lessons Extracted

- **Sharing an identity is the root of a family of bugs.** The claim conflict,
  the stale vnodes, the disabled plugin and the config left in `/etc/netdata`
  were all one problem: the simulation had no identity of its own. Giving it one
  removed them together, and needed no change to the engine or the plugin.
- **A bind-mounted file is not a shared file.** It is a shared *inode*, and the
  ordinary way to edit a file - write beside it, rename over it - silently
  breaks the sharing. Everything that must be visible across the boundary is
  either mounted as a directory or written in place.
- **The probe paid for itself again.** Twenty minutes established that vnodes
  need no change to work in a container, and that the container's own node does
  need handling. Both would have been expensive to discover after building the
  lifecycle around them.

## Followup

- **The console still manages only the host agent.** Teaching it to create and
  manage containers is the natural next step and is a larger surface than
  belongs in this SOW - the script is the honest interim. Tracked as `SOW-0013`.
- **`SOW-0004` (packaging) is now partly answered**: the image is a deliverable
  an SE can be handed. What remains there is the quickstart and a released
  artifact.
- The 50+ node scale test now has somewhere to run that is not the operator's
  own agent.

## Regression Log

None yet.
