# SOW-0008 - Deep specs cover less than their generated equivalents

## Status

Status: open

Sub-state: Not started. Raised as a follow-up by `SOW-0006`. Needs a user
decision before any implementation.

## Requirements

### Purpose

The six hand-authored specs are the ones hero scenarios target, and they are
also the *narrowest*. A prospect zooming into a simulated Postgres node sees
fewer charts than Netdata's own collector would produce against the real thing.

### User Request

Raised by the assistant in `SOW-0006`'s follow-up list; not yet put to the user.

### Assistant Understanding

Evidence, from `integrations/catalogue.json` after `SOW-0006`:

| Integration | hand-authored contexts | Netdata's collector |
|---|---|---|
| PostgreSQL  | 15 | 70 |
| NGINX       |  8 | (generated variant dropped as an alias) |
| Redis       | 11 | ~40 |

The two cannot simply be composed: both declare the same context ids, and
`GeneratorSpec::merge` refuses a duplicate context - correctly, because two
definitions of `postgres.connections` is a bug, not a merge.

Options to put to the user:

- **A.** Extend the hand-authored specs by hand to full collector coverage.
  Highest fidelity, most authoring effort, and the coupling work has to be
  redone for each new context.
- **B.** Teach composition that a hand-authored context *overrides* a generated
  one of the same id. The deep spec keeps its causal coupling; everything it does
  not define comes from the generated spec. Smaller effort, one new rule in an
  otherwise strict merge.
- **C.** Accept the gap and document it. Zero effort; the narrowness stays
  visible to anyone who looks at a real Postgres beside a simulated one.

### Acceptance Criteria

To be written once the option is chosen.

## Analysis

To be written when the SOW starts. `crates/sim-spec/src/lib.rs`
(`GeneratorSpec::merge`) is the surface that changes under option B.

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
