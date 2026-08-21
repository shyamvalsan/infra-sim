# SOW-0013 - The console manages containerised simulations

## Status

Status: closed

Sub-state: Closed as superseded (2026-08-19, user decision "3A" under SOW-0021).

The substance shipped through SOW-0012's follow-through and later work: the
console creates, claims, drives and tears down containerised simulations
(`container::create`/`teardown`, `sim-docker.sh create`), and every simulation
runs its own agent in its own container. What this SOW asked for is the
default path today; what remained unasked - multi-simulation targeting on a
shared host - is SOW-0021's scope, which records the supersession.

## Requirements

### Purpose

`SOW-0012` made a simulation a container with its own agent, which is how
prospect demos should run. The console still manages only a host install, so the
good path is the command line and the convenient path is the wrong one.

### User Request

"when i do this again i don't want to get already claimed errors.. remember this
should be a new agent in a new docker, so it should NOT Be pre-claimed"
(2026-08-08), and, on being shown that torn-down nodes were left stale rather
than removed: "stale issue is because its using my netdata agent as parent and
not a temporary one in a container i guess" - which is correct.

### Assistant Understanding

The console would create, claim, drive and tear down containers instead of (or
as well as) the host agent. `scripts/sim-docker.sh` already defines the
lifecycle; the question is what the console becomes.

**Option A, chosen.** The console creates and manages containers. The host path
remains available on the command line for local iteration, but is no longer what
the console does.

Two symptoms made the case, both of them consequences of the agent outliving the
simulation:

- **Claiming cannot work.** The operator's agent is already claimed, so the
  console's Connect button can only refuse. A container gets a fresh, unclaimed
  agent and claims into whatever Space and room the operator names.
- **Teardown leaves corpses.** Stopping the collector leaves every vnode *stale*
  - retention but no collection - so it stays in the agent's database and in the
  prospect's Space. A 50-node demo leaves 50. `SOW-0012`'s teardown is
  `docker rm -f`, which takes the agent, its database and every vnode together.

A `remove-stale-node` step was added to the host teardown as a stopgap. It is a
band-aid on an architecture this SOW replaces, and should be judged as such
rather than kept as the answer.

Still to decide during implementation: whether the console talks to Docker
through the socket or shells out to `scripts/sim-docker.sh` (one implementation
of the lifecycle, and the script stays usable alone), and how the console's
status, preflight and scenario controls reach an agent that now lives on a
per-simulation port rather than a fixed one.

### Acceptance Criteria

To be written once the option is chosen.

## Analysis

To be written when the SOW starts. `crates/sim-console/src/provision.rs` is the
surface that changes; `scripts/sim-docker.sh` is the behaviour to reproduce.

## Pre-Implementation Gate

To be filled before implementation.

## Implications And Decisions

Blocked on the user choosing A, B or C.

## Plan

To be written.

## Execution Log

None yet.

## Validation

Not started.

## Outcome

Not started.

## Lessons Extracted

None yet.

## Followup

None yet.

## Regression Log

None yet.
