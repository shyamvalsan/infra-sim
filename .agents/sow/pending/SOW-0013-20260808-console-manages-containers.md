# SOW-0013 - The console manages containerised simulations

## Status

Status: open

Sub-state: Not started. Raised as a follow-up by `SOW-0012`, which delivered the
container path as an operator script.

## Requirements

### Purpose

`SOW-0012` made a simulation a container with its own agent, which is how
prospect demos should run. The console still manages only a host install, so the
good path is the command line and the convenient path is the wrong one.

### User Request

Raised by the assistant in `SOW-0012`'s follow-up list; not yet put to the user.

### Assistant Understanding

The console would create, claim, drive and tear down containers instead of (or
as well as) the host agent. `scripts/sim-docker.sh` already defines the
lifecycle; the question is what the console becomes.

Decisions to put to the user:

- **A.** Console manages containers only. Simplest model, one way to do things;
  loses the host path's fast iteration.
- **B.** Console gains a target selector: host or container. Keeps both, doubles
  the surface it has to be correct about.
- **C.** Console per simulation, running inside the container. Self-contained
  and matches the isolation story, but needs a port each and gives no
  cross-simulation view.

Also unresolved: whether the console talks to Docker through the socket or by
shelling out to the CLI, and how it reaches a scenario trigger inside a running
container.

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
