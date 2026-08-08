# SOW-0007 - Project skill for the integration sync loop

## Status

Status: open

Sub-state: Not started. Raised as a follow-up by `SOW-0006`.

## Requirements

### Purpose

Capture the generator-authoring loop as a runtime project skill, so the next
person to add or repair a generated spec does not rediscover it.

`SOW-0001` deliberately deferred project-skill creation until the loop produced
concrete, reusable knowledge rather than generic filler. `SOW-0006` produced it.

### User Request

Raised by the assistant in `SOW-0006`'s follow-up list; not yet put to the user.

### Assistant Understanding

The knowledge worth writing down, all of it earned by a defect:

- Netdata's `metadata.yaml` is the source of truth for contexts, units, chart
  types, dimension names, icons and categories.
- Short unit strings must match **exactly**; substring matching let `%` fall
  through to a generic profile and produce 108% disk fragmentation.
- Failure-named dimensions start at zero, 0/1 dimensions are declared constant,
  negative baselines drift rather than following a working day.
- A labelled scope becomes one representative instance carrying that scope's
  chart labels.
- A generated alias of a hand-authored spec must be dropped from both the
  catalogue and disk, or `--describe` resolves to the shallow copy and loses
  every scenario targeting that software.
- The 6-hour fidelity lint is the gate, run across the whole corpus rather than
  a sample. Every unit defect above was invisible in review and immediate in the
  lint.

### Acceptance Criteria

1. `.agents/skills/project-integration-sync/SKILL.md` exists with a trigger
   description that matches "add an integration", "regenerate specs", "a
   generated spec fails the lint".
2. It is hook-based: what to check, in what order, with the failure each check
   exists to catch.
3. It is not generic filler - every rule cites the defect that produced it.

## Analysis

To be written when the SOW starts.

## Pre-Implementation Gate

To be filled before implementation.

## Implications And Decisions

None yet.

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
