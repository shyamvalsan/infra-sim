# SOW-0003 - Graceful scenario recovery

## Status

Status: open

Sub-state: Not started. Raised as a follow-up by `SOW-0001`, which shipped the scenario engine and the `recover` effect but left `resolve` as an instant snap-back.

## Requirements

### Purpose

Make clearing a fault look like a system healing rather than a value being reset.

### User Request

Raised by the assistant during `SOW-0001` and recorded in its follow-up list; not yet put to the user as a decision.

### Assistant Understanding

Removing a scenario from `control.yaml` drops its multiplier straight to 1.0 on the next tick, so a fault vanishes within one collection interval.

`spec.md` explicitly calls out that showing recovery is as persuasive as showing failure. On screen an instant snap-back reads as artificial — the opposite of what the fidelity work exists to protect.

The engine already has the machinery: the `recover` effect scales an accumulated fault back toward neutral, but only within a scenario's own timeline (`ScenarioSet::perturbation`, the `recovery` term).

### Acceptance Criteria

To be written once the design decision below is taken.

## Analysis

Evidence:

- `crates/sim-engine/src/scenario_runtime.rs` — `perturbation()` blends fault and additive contributions through a `recovery` factor sourced only from timeline steps.
- `crates/sim-engine/src/control_file.rs` / `crates/sim-plugin/src/control.rs` — removing an entry removes the `ActiveScenario` outright; there is no unwinding state.

Risk: whichever option is chosen changes what an operator sees when they press "resolve" mid-demo, so it is a product-visible behaviour change, not an internal refactor.

## Pre-Implementation Gate

Status: needs-user-decision

Open decisions:

**Decision 1 — what should `resolve` do?**

- **Option A — `resolve` synthesises a recovery ramp.** Keep the scenario active internally with a generated `recover` step, and drop it once the ramp completes.
  - Pro: every scenario gets graceful recovery with no authoring burden; one code path. Con: "resolve" no longer means "stop now", so an SE who wants an immediate stop has no way to get one.
- **Option B — hero scenarios must end with a `recover` step; `resolve` becomes an abort.** Normal demo flow is to let the timeline finish; `resolve` stays the emergency stop.
  - Pro: recovery is authored and therefore realistic per scenario; `resolve` keeps a clear meaning. Con: every scenario must be updated, and an SE who resolves early still sees a snap.
- **Option C — both: `resolve` ramps, `resolve --now` aborts.**
  - Pro: covers both intents. Con: two behaviours to explain in the console and the script.

**Recommendation: C** — classified **long-term-best**. The instant stop is genuinely needed when a demo goes wrong, and graceful recovery is the common case; conflating them into one verb is what forces the bad trade.

## Plan

To be written once Decision 1 is taken.

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
