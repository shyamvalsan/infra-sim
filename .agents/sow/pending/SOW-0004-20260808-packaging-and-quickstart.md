# SOW-0004 - Packaging and SE quickstart

## Status

Status: open

Sub-state: Not started. Raised as a follow-up by `SOW-0001`, which delivered the runtime but nothing that lets a second person run it.

## Requirements

### Purpose

Make Infra-Sim runnable by an SE who did not build it, and demonstrable to the Netdata team without the author present.

`spec.md`'s success criterion is an SE producing an expert-grade simulated environment in <= 4 hours of hands-on effort. Everything built so far assumes a developer workstation with a Rust toolchain, a live agent and repo-relative paths.

### User Request

The user asked during `SOW-0001`: "how long does it take to make the simulated infra currently, is this something i can demo to the netdata team internally". Answered with measured timings; the packaging work itself was deferred.

### Assistant Understanding

What exists: `scripts/install-local.sh` (installs into a local agent), `scripts/scenario.sh`, `scripts/logs.sh`, the console binary, and a README.

What is missing for a second person:

- A container image or compose stack (agent + runtime + console) so no Rust toolchain is needed.
- A quickstart that goes from clone to a live fleet with a triggered scenario, without reading the README end to end.
- A warm-up runbook: ML needs ~15 minutes to start contributing and ~72 hours to be fully credible, so a demo booked for tomorrow has to be started today. This is the single most likely way a first demo disappoints.
- Correlated logs add a host dependency (`systemd-journal-remote`) and require root; the packaging story has to account for both.

Measured today: description -> live fleet is ~4m11s cold (125s lint + 124s release build + ~2s to node visibility), ~30s warm.

### Acceptance Criteria

To be written once the decisions below are taken.

## Analysis

Risks:

- A container running the Netdata agent plus the plugin needs the agent's own privileges; the logs path additionally needs to write `/var/log/journal/remote` and have the agent read it, which is awkward across a container boundary.
- Shipping a prebuilt binary avoids the toolchain but adds a release process this project does not have.

## Pre-Implementation Gate

Status: needs-user-decision

Open decisions:

**Decision 1 — what is the deliverable?**

- **Option A — docker compose stack** (agent + infra-sim + console) that someone runs with one command.
- **Option B — prebuilt binaries plus the existing install script**, no container.
- **Option C — both.**

**Decision 2 — does the packaged deliverable include correlated logs?**

Logs need `systemd-journal-remote` and root, and the journal directory must be shared with whatever process runs the agent. Including them raises the packaging cost noticeably; excluding them means the packaged demo is missing a P0 feature.

**Decision 3 — is a real `--llm` run part of this?**

`--llm` was validated against a local mock of each provider's wire format, but never against a real provider (no key available). Model-quality behaviour on an unfamiliar description is unverified. This is cheap to close with one key and belongs with any "hand it to someone else" milestone.

No recommendation offered yet: the right answer depends on who the audience is (internal Netdata team vs SEs in the field), which is the user's call.

## Plan

To be written once decisions are taken.

## Execution Log

### 2026-08-08

- Opened as a follow-up from `SOW-0001`.

## Validation

Pending.

## Outcome

Pending.

## Lessons Extracted

Pending.

## Followup

None yet.

## Regression Log

None yet.
