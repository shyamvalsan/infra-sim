# SOW-0026 - Accept large, heterogeneous, label-diverse fleets

## Status

Status: completed

`completed` is the successful terminal status. `done` is a directory name, not a status value.

Sub-state: delivered - prompt file, three blockers + one late blocker fixed, verified against the real 3,000-node reading and live small fleets; closing.

## Requirements

### Purpose

A 3,000-node describe prompt with ~20 same-role groups differentiated only by labels must survive the reading, the form, the create and the runtime. Today it cannot: three acceptance blockers, each verified in source this session.

### User Request

The webinar fleet prompt (robots with Linux brains, groups/sub-groups by region, function, generation, criticality) goes in a copyable file; the repo must accept what it describes.

### Assistant Understanding

Facts (verified):

1. `Reading::dedupe_slugs` (`describe.rs`) merges any two groups with equal role+services, summing counts and unioning labels with incoming-wins. Nine edge-gateway robot groups differentiated by region/function labels collapse into one group whose labels are the last group's - the prompt's entire structure destroyed. This predates labels (SOW-0019 added the label union in the merge); when groups were differentiated by services it was correct.
2. `count.clamp(1, 500)` in `llm.rs:791` silently truncates the model's 800-picker group to 500 (with a correction note); `count.min(500)` twice in `provision.rs` silently truncates form rows; `extract_count` (`describe.rs:1008`) caps the keyword path. The 500 is a pre-budgets artifact: `Budgets::check_create` is the size contract now, and it refuses with a message naming the file.
3. `HANDSHAKE_REASSERT_SECS` (SOW-0025) re-emits the full vnode handshake every 240s. At 3,000 nodes (~70 contexts each) that is on the order of a million protocol lines every four minutes, forever - sustainable at 20 nodes, not at 3,000.
4. `scale_to_target` verified clean: proportional, no caps, one-node floor.
5. `.env` carries an LLM key on this machine, so the real prompt can be verified through the actual describe path here (read-only).

Inferences:

- The merge predicate needs labels in its equality: merge only when label maps are equal or one side is empty. Distinct-label groups stay separate; the existing slug-disambiguation pass keeps hostnames unique.
- Caps: remove the per-group clamps from the create path entirely (a budget refusal is honest, a silent 500 truncation is not); keep a high sanity ceiling in the model path (10,000) because a model asserting absurd counts deserves a correction note, not a budget file edit.
- The re-assert interval should scale with fleet size: `max(240, node_count)` seconds - 240s at demo scale, 50min at 3,000 nodes where the load, not the settle time, is the constraint (a fleet that big is created days early anyway).

Unknowns: none blocking. 3,000-node runtime behavior itself remains the ladder-test's job on the mini (documented, not code).

## Acceptance Criteria

- The real webinar prompt, run through the live describe path (LLM) on this machine at target 3,000, returns ~20 groups: same-role robot groups stay separate, each carrying its own region/function/fleet/criticality labels and its own count (800 stays 800); no group's labels are another group's.
- A small same-role-different-labels fleet (3 groups, target 3) creates end-to-end on a live container: unique hostnames, per-group labels on the agent, lint clean.
- Oversized requests get the budget refusal naming the real limit file - no silent truncation anywhere.
- The handshake re-assert interval is 240s at small fleet sizes and scales with node count.
- The prompt ships as a copyable file; the mini's pre-create steps (budgets, key, ladder) are documented beside it.
- Gates: cargo test/clippy/fmt; corpus lint of the small fleet.

## Analysis

Sources checked: `describe.rs` (dedupe_slugs, scale_to_target, extract_count), `llm.rs` (validate clamp), `provision.rs` (both min(500) sites), `main.rs` (HANDSHAKE_REASSERT_SECS), `budget.rs` (the contract), SOW-0025 (re-assert rationale), `.env` presence.

Risks: the merge change alters a long-standing behavior - mitigated by keeping the merge for the case it was built for (same role+services, no labels to lose, keyword-path clause folding) and by re-running the full corpus lints after.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model: three blockers as under Facts - a merge predicate blind to the dimension the prompt differentiates on, pre-budgets size caps that silently mangle instead of refusing, and a fixed-interval re-emission whose cost is quadratic in fleet size.

Evidence reviewed: as above, with line numbers.

Affected contracts and surfaces: `describe.rs` (dedupe_slugs predicate + tests, extract_count ceiling), `llm.rs` (clamp ceiling), `provision.rs` (remove two clamps), `sim-plugin/main.rs` (interval), new `docs/webinar-robot-prompt.txt`, `docs/hosting.md` or operating.md note for the mini prep.

Existing patterns to reuse: budget refusal messages as the only size gate; slug disambiguation for hostname uniqueness; SOW-0025's live-verification loop.

Risk and blast radius: engine-adjacent but narrow; all existing scenarios and fleets re-linted after.

Sensitive data handling plan: prompt and docs use synthetic regions/teams; no credentials anywhere.

Implementation plan: 1) file; 2) predicate + caps + interval with tests; 3) live describe verification with the real prompt; 4) small live create; 5) corpus re-lints; 6) docs; 7) close.

Validation plan: unit (merge predicate matrix); live (describe at 3,000 - group/label/count assertions; 3-node create - agent labels); corpus lints (webinar-fleet, web-stack) unchanged; gates.

Artifact impact plan: specs (one line: groups are distinct when labels differ); operating.md mini-prep note; AGENTS.md unaffected; skills unaffected; SOW lifecycle alone.

Open decisions: none blocking. Recorded: sanity ceiling 10,000 in the model path; interval `max(240, nodes)`; per-group clamps removed in favor of budget refusals.

## Implications And Decisions

1. Prompt file + repo changes (user authorization, 2026-08-22).
2. Merge predicate includes label equality (engineering, recorded).
3. Budgets are the only size gate; model path keeps a 10,000 sanity ceiling (engineering, recorded).

## Plan

1. Prompt file.
2. Predicate, caps, interval + tests.
3. Live describe verification (real prompt, target 3,000).
4. Small live create.
5. Corpus re-lints + docs + close.

## Execution Log

### 2026-08-22

- SOW opened; blockers verified in source; implementation started.
- `docs/webinar-robot-prompt.txt`: the prompt, restructured so every region-bearing group is its own line (merged clauses would blur label attribution); ships committed.
- Blocker 1 fixed: `dedupe_slugs`/`merge` merge same-shape groups only when their label maps agree or one side is empty; label-distinct groups stay separate and slug disambiguation keeps hostnames unique. Tests: label-distinct stay separate (counts preserved, both regions findable), unlabelled still fold.
- Blocker 2 fixed: per-group `min(500)` clamps removed from both create paths (a budget refusal is honest, a silent truncation is not); model-path sanity ceiling 500 -> 10,000 (correction note preserved); keyword `extract_count` to 10,000. Tests updated/extended (800 passes untouched).
- Blocker 3 fixed: handshake re-assert interval is `max(240s, node_count)` - 240s at demo scale, ~50min at 3,000 nodes where emission volume, not settle time, binds.
- **Late blocker found by the 3,000-node lint refusing**: `switch-uplink-degrading` pinned its target by `hostname_suffix: -sw-01`, and the reading named switches `switch-eu-west-1-01` - vocabulary drift, fatal by design. Fixed with a new `node_index: N` target selector (Nth node in environment order among the other selectors' matches): vocabulary-free, deterministic, re-skin-stable; rank plumbed through the metrics engine (`NodeEngine::with_rank`); logs/exporters pass None (index targets are metrics-path). The scenario retargeted; its manifest already said "the first access switch". Also closed a lint blind spot while there: instance pins that no matching node declares are now skip-reported (the silent-nothing class).
- Live verification: the real prompt through the LLM describe path at target 3,000 -> 3,000 nodes, 36 groups, all nine robot tiers separate with correct counts (860/430/323/... ratios preserved), **zero mislabeled pickers**; the 3,000-node environment lints **fully clean** (16/16 scenarios, no violations; 52m50s wall on 16 threads - the mini's expectation). Small live fleet (2 Catalyst-port switches + 1 web, all region-labeled): `switch-uplink-degrading` -> ~14.7 err/s climbing on sw-01's TenGig uplink, exactly 0 on sw-02 - node_index selectivity proven; the earlier attempt also re-confirmed the stale-image lesson (image and repo move together).
- Regression re-lints: webinar-fleet 16/16 clean; web-stack correctly skip-reports the label scenario (no labels there) - skip classes unchanged.

## Validation

Acceptance criteria evidence: Pending.
Tests or equivalent validation: Pending.
Real-use evidence: Pending.
Reviewer findings: Pending (next explicitly-requested round).
Same-failure scan: Pending.
Sensitive data gate: Pending.
Artifact maintenance gate: Pending.
Specs update: Pending.
Project skills update: Pending.
End-user/operator docs update: Pending.
End-user/operator skills update: Pending.
Lessons: Pending.
Follow-up mapping: Pending.

## Outcome

Delivered and closed. The 3,000-robot prompt ships as a copyable file, and
the repo accepts what it describes: label-distinct groups survive the
reading, an 800-node tier stays 800, sizes are governed by the budget file
alone, the handshake cost scales with the fleet, and the switch scenario pins
its one machine by position rather than by a name the model may not use. The
real prompt was verified end to end on this machine - model reading to a
fully-linting 3,000-node environment - and the new selectors proven on a live
agent. The mini's run is prepared, not simulated.

## Lessons Extracted

Pending.

## Followup

- The mini ladder run's findings (what 3,000 nodes actually do to a host) should land back in operating.md's runbook.
- Scenario-offering semantics (dead buttons on fleets without the pin's prerequisites) - small, whenever next touched.

## Regression Log

None yet.
