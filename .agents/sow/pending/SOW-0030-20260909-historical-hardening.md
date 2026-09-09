# SOW-0030 - Historical hardening follow-through

## Status

Status: open

Sub-state: authorized follow-through, split from release automation so changes remain independently reviewable.

## Requirements

### Purpose

Adjudicate and implement the still-relevant maintenance items recorded by SOW-0027. The user approved the complete review programme on 2026-09-09.

### Acceptance Criteria

Each item is implemented with evidence, explicitly rejected with evidence, or tracked under its actual product dependency. No list-only deferral at close.

## Analysis

Inventory from SOW-0027: claim-token visibility to Docker administrators; create-validation duplication; per-simulation full specs copy; agent HTTP failures versus unreachable status; disk-walk symlink cycles; poisoned-mutex reporting; signal_exists_somewhere robustness; reskin structural-field protection; port-allocation race; XSS via inline handlers; dirty-checkout startup diagnostics; local-install fleet visibility. Fidelity lookup, lint-readiness and replay belong to SOW-0028. VM/rootless launcher compatibility belongs to SOW-0018. User approved all recommended improvements, but new behavior forks still require prior design discussion.

## Pre-Implementation Gate

Status: blocked

This pending scope has not started. Investigate each current causal path and record a concrete risk/implementation/validation plan before moving to current; prior review labels are not proof an item remains defective.

Sensitive data handling plan:

Use synthetic simulation names and credentials; do not expose Docker environment values or user service data.

## Plan

1. Complete current delivery.
2. Verify and classify each item, with explicit user decisions for any new product fork.
3. Implement the minimal-complete fixes, test applicable live paths and update specs/docs.

## Validation

No implementation or acceptance claimed yet.

## Outcome

Pending work, not complete.

## Followup

This file is the concrete follow-through for the listed historic items, not evidence that they are fixed.
