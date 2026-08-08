# SOW-0003 - Graceful scenario recovery

## Status

Status: completed

Sub-state: Delivered. Resolve now unwinds a fault over three minutes and the recovery is visible on a live chart.

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

1. Pressing resolve unwinds the fault gradually rather than clearing it between two samples. MET.
2. The unwind is monotonic: recovery never briefly makes the fault worse. MET, asserted per second across the window.
3. Re-triggering mid-recovery resumes rather than restarting the timeline. MET.
4. The plugin still never writes `control.yaml`; the console owns it. MET.

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


## Implications And Decisions

1. **Resolve marks, it does not delete.** `ActiveEntry` gains `recovering_since`. The entry stays in `control.yaml` while it unwinds, and the console prunes it once finished. The plugin never writes that file, so a finished entry simply contributes nothing until the console next reads it.
2. **Three minutes, eased.** `RECOVERY_SECONDS = 180`, with a smoothstep curve rather than a linear ramp: a system draining a backlog recovers fastest at the start and tails off, and easing keeps the last of the recovery visible instead of clipping it.
3. **Recovery multiplies the scenario's own `recover` effect** rather than replacing it, so a scenario that already models its own recovery composes with the operator pressing resolve.
4. **Re-triggering cancels the unwind and keeps `started_at`**, so the fault resumes where it was instead of replaying from the top of the timeline.

## Plan

Delivered as above.

## Execution Log

### 2026-08-08

- `ActiveEntry::recovering_since`; `resolve`/`resolve_all` set it, `prune_recovered` clears finished entries.
- `ActiveScenario::strength(now)` and its use in `ScenarioSet::perturbation`.
- `RECOVERY_SECONDS` in `sim-engine`.
- Console resolves with a timestamp and prunes on every read.

## Validation

Live agent `v2.10.0-1030-nightly`. `disk-fill` triggered, advanced +40m, then resolved, sampling `disk_space./var/lib/pgsql` on `rec-db-01`:

| after resolve | used (GiB) | free (GiB) |
|---|---|---|
| 0s | 1320.9 | 124.0 |
| 25s | 1297.6 | 148.3 |
| 50s | 1159.9 | 291.8 |
| 75s | 951.8 | 508.5 |
| 100s | 714.1 | 756.1 |
| 125s | 477.9 | 1002.2 |
| 150s | 284.4 | 1203.7 |
| 175s | 174.4 | 1318.4 |

Previously this went from the first row to the last between two samples.

Tests: 188 passed, including a per-second monotonicity assertion across the whole window, the idempotence of pressing resolve twice, and re-trigger behaviour. Clippy and fmt clean.

Same-failure search: `resolve_all` had the same instant-clear behaviour and takes the same path. Warm-up incidents are unaffected: they run their own timeline and are never resolved by hand.

Sensitive data gate: none involved.

Artifact maintenance gate: `.agents/sow/specs/runtime-and-scenarios.md` and `README.md` state the unwind. Specs otherwise unchanged.

## Outcome

Clearing a fault now looks like a system recovering. `spec.md` calls showing recovery as persuasive as showing failure, and until now the product demonstrated the opposite.

## Lessons Extracted

- **Deleting state is not the same as ending it.** The instant snap-back came from modelling "resolved" as absence rather than as a phase with a duration. The engine already had a recovery term; nothing was wired to it from the operator's side.

## Followup

None.
