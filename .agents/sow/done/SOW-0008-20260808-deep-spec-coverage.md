# SOW-0008 - Deep specs cover less than their generated equivalents

## Status

Status: completed

Sub-state: Delivered. User chose option B. A hand-authored spec now takes the
generated spec's breadth and overrides the contexts it models more carefully.

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

1. A hand-authored spec emits its own contexts plus everything the generated one
   adds. MET: Postgres 15 -> 71, Redis 11 -> 29, nginx 8 -> 10 on a live agent.
2. Where both define a context id, the hand-authored one wins, so scenarios keep
   targeting the signals they name. MET.
3. The picker still shows one entry per integration. MET.
4. Every committed template lints. MET.

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


## Implications And Decisions

**User chose option B** (2026-08-08): teach composition that a hand-authored
context overrides a generated one, rather than extending the deep specs by hand
(A) or documenting the gap (C).

1. **`extends:` is a list on the spec, not a rule in the loader.** One
   collector's metrics do not always map to one spec - Kubernetes reports
   through both a kubelet and a cluster-state collector - so `kubernetes.yaml`
   extends two.
2. **`overlay` replaces in place**, keeping the generated spec's ordering and
   the priorities that follow from it.
3. **One level only.** A chain would be a spec hierarchy, which is more
   machinery than "take the generated breadth" needs, and the loader says so
   rather than recursing.
4. **The generated alias spec stays on disk but out of the catalogue.** The
   picker shows one PostgreSQL; the file exists because `postgres.yaml` extends
   it.

## Plan

Delivered as above.

## Execution Log

### 2026-08-08

- `GeneratorSpec::extends` and `GeneratorSpec::overlay` in `sim-spec`.
- `load_service_spec` in `sim-plugin` resolves `extends:` before composition.
- `extends:` wired into `nginx`, `postgres`, `redis`, `containers`, `kubernetes`.
  `otel-collector` has no generated equivalent and is left alone.
- `scripts/sync-integrations.py` keeps alias specs on disk, hiding them only
  from the catalogue.
- `install()` copies spec dependencies, and the console lints the installed copy.

### Findings during implementation

- **The install did not carry spec dependencies.** The fleet installed cleanly,
  the repo lint passed, and the plugin died on its first tick looking for
  `generated/postgresql.yaml`. The repo lint cannot catch this because paths
  resolve differently in the install directory.
- **That failure poisoned the next install.** netdata disables a plugin that
  exits with an error before collecting anything
  (`plugins_d.c:94-98`), until the agent restarts - the same class as
  `SOW-0011`, reached by a different route. The console now lints the installed
  copy and refuses to leave a fleet that does not load.
- **Two unrelated bugs surfaced in `environments/initech.yaml`.** Re-skin wrote
  the installed YAML verbatim into `environments/`, so the committed template
  carried install-directory paths and could not be linted from the checkout. It
  also killed the plugin to force a reload, which is exactly the disable trap
  from `SOW-0011`. Both fixed: the repo copy gets repo-relative paths, and the
  plugin now notices its environment changed and exits cleanly so the agent
  restarts it.

## Validation

Context counts on live agent `v2.10.0-1030-nightly`, composed specs installed
through the console:

| Service | Before | After |
|---|---|---|
| Postgres | 15 | **71** |
| Redis | 11 | **29** |
| nginx | 8 | **10** |

`deep-db-01` carries 174 charts in total, `deep-cache-01` 109.

Every committed template lints at 6h: `web-stack`, `k8s-microservices`,
`otel-fleet`, `robotics-edge`, `acme`, `initech`. `initech` did not before this
SOW, for reasons unrelated to composition.

Tests: 189 passed, including the `extends:` parser across both YAML forms and
the case where a top-level `- ` is a node list rather than a continuation.
Clippy and fmt clean.

Same-failure search: `stop_plugin` was called from two places, teardown and
re-skin. `SOW-0011` fixed teardown; re-skin was the other and is fixed here. No
other caller signals the plugin.

Sensitive data gate: none involved.

Artifact maintenance gate: `README.md` loses the known-issue entry and states
what composition does; `.agents/sow/specs/generator-and-engine.md` gains
`extends:`/`overlay`. `docs/QUICKSTART.md` unchanged.

## Outcome

A simulated Postgres now reports what Netdata's own collector reports, while the
15 contexts the hero scenarios target stay causally coupled. The trade the SOW
was written to resolve is gone: breadth and depth compose instead of competing.

## Lessons Extracted

- **A lint that runs somewhere other than where the code runs proves less than
  it appears to.** The repo lint passed on a fleet that could not start, because
  the thing that was wrong was the install layout. Validate the artifact you
  actually ship.
- **A failure that disables the next attempt is worse than a failure.** One
  broken install cost an agent restart, and the cost was invisible until the
  following create also did nothing.

## Followup

- `otel-collector` has no generated equivalent to extend. If Netdata ships a
  collector for it, wiring is one line.
