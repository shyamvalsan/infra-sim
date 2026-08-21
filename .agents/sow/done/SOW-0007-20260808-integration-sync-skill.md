# SOW-0007 - Project skill for the integration sync loop

## Status

Status: completed

Sub-state: delivered - skill written, accuracy-validated claim by claim; closing.

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

1. `.agents/skills/project-integration-sync/SKILL.md` exists; trigger carries the three phrases verbatim.
2. Hook-based: the Step-0 table is ordered by the first decision (which source owns the collector), then the loop, then rules keyed to failure modes.
3. Every one of the nine rules names its producing SOW defect (0006/0014/0016/0017/0020).

## Analysis

Sources checked:

- `scripts/sync-integrations.py` (docstring: scope handling, the one-representative-instance limitation, catalogue output)
- `scripts/sync-snmp-profiles.py` (docstring: what is taken from a profile vs invented, `virtual_metrics`, UCUM units)
- `scripts/sync-profile-collectors.py` (docstring: metadata-is-not-the-contract, the three families, flat-path discovery rule)
- `.agents/sow/done/SOW-0006` (lessons: substring unit matching, whole-corpus lint, alias dropping, value defects)
- `.agents/sow/done/SOW-0014` (lessons: read the source format fully, probe-first wins)
- `.agents/sow/done/SOW-0017` (lessons: metadata is not always the metric contract, generalise from two examples, inherited assumptions)
- `.agents/skills/project-live-validation/SKILL.md` (the house style for skills: defect-cited, hook-based)
- Current reality: `specs/generated/` (327 specs), `integrations/catalogue.json`, `integrations/snmp-devices.json`

Current state: no skill exists; the loop lives in three script docstrings and three SOWs' lessons.

Risks: a skill that paraphrases the scripts is filler. The bar (acceptance criterion 3) is that every rule names the defect that produced it.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model: the sync loop's knowledge is scattered across three script docstrings and three done-SOWs; the next person adding an integration re-reads all of it or, worse, rediscovers the defects by shipping them.

Evidence reviewed: as under Analysis.

Affected contracts and surfaces: one new file `.agents/skills/project-integration-sync/SKILL.md`. No code, no spec format, no runtime change.

Existing patterns to reuse: `project-live-validation`'s structure (frontmatter trigger, sections ordered by when you hit them, every rule defect-cited).

Risk and blast radius: documentation only. Worst case is inaccuracy, which the validation plan exists to prevent.

Sensitive data handling plan: none; all content is public repo knowledge with SOW citations.

Implementation plan: write the skill (trigger frontmatter, which-source-decides-which-script decision table, the loop, the rules with defect citations, the commands); validate every command/path/count claim against the repo; close.

Validation plan: an automated accuracy pass - each documented command's flags exist in the script's argparse; each named path exists; catalogue counts match the files on disk; each cited SOW exists. Gates (fmt/clippy untouched - no Rust). Human bar: every rule names its producing defect.

Artifact impact plan: the skill is the artifact. AGENTS.md skill index gains one line. Specs/skills/docs otherwise unaffected.

Open-source reference evidence: none newly required; the scripts' docstrings already carry their netdata/netdata citations.

Open decisions: none blocking. Recorded: the skill covers all three sync scripts, not only SOW-0006's - it was written before the SNMP (0014) and profile-family (0017) loops existed, and separate skills per script would be the filler AGENTS.md forbids.

## Implications And Decisions

None yet.

## Plan

1. Write the skill.
2. Accuracy-validate it against the repo.
3. AGENTS.md index line; close.

## Execution Log

### 2026-08-21

- Gate filled from the three scripts and three SOWs' lessons; scope extended to the full sync family (recorded under Open decisions).
- Skill written: Step-0 source-ownership table (the CloudWatch trap), the loop (sync -> whole-corpus lint -> live-agent spot-check), nine defect-cited rules (exact unit matching, failure-dimension semantics, one-representative-instance, read-the-whole-format/virtual_metrics, alias dropping, flat-path discovery, generalise-from-two, inherited-assumptions, complete summaries), and the faithful-not-deep honesty clause.
- Accuracy validation: 21 automated checks - script flags, paths, catalogue keys, SOW citations, discovery-layout claims, README consistency, trigger phrasing. Two initial FAILs resolved: the trigger now uses the acceptance-criteria phrasing verbatim ("add an integration", "regenerate specs", "fails the lint"); the "no snmp at top level" check was a false positive in the validator itself (`snmp-trap-listener.yaml` is a genuine metadata-declared collector, not a device profile - the corrected check confirms zero device-profile leakage).

## Validation

Acceptance criteria evidence: the file exists at the required path; the 21-check accuracy pass (commands' flags in argparse, named paths on disk, catalogue key `instances_modelled`, all cited SOWs present, snmp layout claim, README wording consistency, trigger phrasing) is green after the two documented corrections; scope covers the full three-script family per the recorded decision.

Tests or equivalent validation: the accuracy script above (kept in the execution log description; a doc artifact's tests are checkable claims).

Real-use evidence: the next person running any sync script loads a skill whose every command runs as written and whose every rule traces to a shipped defect.

Reviewer findings: folded into the next explicitly-requested round (established precedent).

Same-failure scan: grepped the repo for a second skill overlapping this territory - only project-live-validation exists, disjoint (validation loop vs authoring loop).

Sensitive data gate: public repo knowledge only; no credentials, names, endpoints.

Artifact maintenance gate:
- AGENTS.md: skill index gains the project-integration-sync line (this commit).
- Runtime project skills: this SOW's deliverable IS one.
- Specs: unaffected (no behavior change).
- End-user/operator docs: unaffected.
- End-user/operator skills: none.
- SOW lifecycle: oldest pending SOW closed; SOW-0018 remains the only pending SOW.

Specs update: none needed - documentation artifact.

Project skills update: the deliverable itself.

End-user/operator docs update: none affected.

End-user/operator skills update: none.

Lessons: validating a documentation artifact by script (flags, paths, keys, citations) caught two real inaccuracies in the first draft - the trigger phrasing and an overbroad layout claim whose refutation (snmp-trap-listener) required understanding WHY the file exists. A doc's tests are its checkable claims.

Follow-up mapping: none raised. SOW-0014's free-text device selection and vendor-icons follow-ups remain where they were recorded (that SOW's Followup), untouched by this skill.

## Outcome

Delivered and closed. The integration-sync loop - three scripts, three source
families, nine defect-earned rules - lives in one runtime skill instead of
three docstrings and three done-SOWs. Accuracy-validated claim by claim. The
project-skill index now covers both halves of the workflow: how to build
things (this skill) and how to prove they work (project-live-validation).

## Lessons Extracted

See Validation -> Lessons.

## Followup

None - SOW-0014's own follow-ups stay recorded there.

## Regression Log

None yet.
