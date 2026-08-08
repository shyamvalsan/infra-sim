# Infra-Sim

## Goals

Infra-Sim generates realistic simulated infrastructures — nodes, metrics, logs, and injected problem scenarios — that the **real** Netdata pipeline (ML, health engine, Netdata AI) processes live.

It exists because generic demos do not sell, and it is impractical to run a real instance of every service Netdata integrates with. Sales engineers need an environment that looks like the prospect's own infrastructure, and Netdata AI needs interesting incidents on cue.

Users, in priority order:

1. **P0 — Sales engineers:** prospect-shaped demos.
2. **P1 — Public demo spaces:** always-on labeled simulated environments on the demo parent.
3. **P2 — Netdata AI team:** the eval gym.

Success means: an SE produces an expert-grade simulated environment matching a prospect's stack in ≤ 4 hours of hands-on effort, and an experienced SRE zooming into individual charts finds no disqualifying artifacts.

The authoritative product definition is `spec.md`. Specs under `.agents/sow/specs/` describe what is actually built.

### The one hard rule

**Synthetic world, live product.** Only the raw data is simulated. Everything downstream is the real product: ML actually trains and detects, the health engine actually raises alerts, Netdata AI actually investigates.

Nothing downstream of data injection is ever scripted or mocked. No canned AI responses, no fake alert states, no mocked dashboards — ever. All simulated environments are visibly labeled as simulations (`simulated=true` host labels, `<Prospect> (Simulated Demo)` Space naming).

Any change that would violate this rule requires explicit user approval and must be recorded in the active SOW.

## SOW System

This project uses a local Statement of Work system.

The SOW system is self-contained in this repository. Normal SOW work must not depend on `~/.agents`, `~/.AGENTS.md`, global skills, global templates, or global scripts. Use this `AGENTS.md`, project-local SOW files, project-local specs, project-local skills, and the active SOW.

### Roles

- **User responsibilities:** purpose, scope decisions, design forks, risk acceptance, destructive approvals, and final product judgment.
- **Assistant responsibilities:** investigation, evidence, implementation, tests or equivalent validation, reviews, documentation, memory updates, and concise reporting.

The user designs; the assistant codes. The assistant may propose designs but must not decide them. Design decisions are recorded in the SOW before implementation.

### Required First Checks

Before creating a SOW or starting non-trivial implementation:

1. Confirm the user has requested implementation.
2. Inspect code/docs/data to establish whether a change is needed.
3. Read pending/current SOWs for overlap, contradictions, and existing decisions.
4. Read relevant specs under `.agents/sow/specs/`.
5. Inspect `.agents/skills/project-*/SKILL.md` and load every runtime project skill whose trigger matches the work.
6. Ask the user only for irreducible product/design/risk decisions.

### Git Worktrees

Assistants must not create git worktrees on their own. Create a git worktree only when the user explicitly asks for it or approves it.

### Sensitive Data In Durable Artifacts

SOWs, specs, documentation, project skills, agent instructions, and code comments are commit-ready artifacts. Treat them as public unless a repository-specific policy explicitly says otherwise.

CRITICAL: Never write raw sensitive data to durable artifacts. This includes passwords, API keys, bearer tokens, SNMP communities, private keys, connection strings with embedded credentials, session cookies, community member names, customer names, customer identifiers, personal data, non-private IP addresses that can identify customers, private endpoints, account IDs, and proprietary incident details.

Project-specific hazards:

- **Netdata Cloud claim tokens and room IDs** are credentials. They never enter `environment.yaml`, SOWs, specs, docs, test fixtures, or commit history. They are supplied at runtime via env vars or console input only.
- **LLM provider API keys** (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or whatever `--llm-key-env` names) follow the same rule, and additionally must never reach a child process's argv — argv is world-readable via `ps` for the life of the process. `crates/sim-plugin/src/llm.rs` passes the key to `curl` on stdin for exactly this reason; keep it there.
- **Prospect and customer names** never appear in committed environment templates, scenario fixtures, hostnames, or SOW text. Use `customer-a`, `[PROSPECT]`, or a generic vertical name.
- Simulated hostnames, IPs, and labels committed to this repo must be obviously synthetic (RFC 5737 / RFC 3849 documentation ranges, `sim-*` prefixes) so they can never be mistaken for a real customer estate.

Write only sanitized evidence:

- use placeholders such as `[REDACTED_SECRET]`, `[CUSTOMER]`, `[ACCOUNT]`, `[PRIVATE_ENDPOINT]`;
- use stable aliases such as `customer-a` only when the real mapping is not stored in the repository;
- cite file paths, line numbers, command names, schema fields, or error classes instead of copying sensitive values;
- summarize logs and traces; include only minimal redacted snippets.

If sensitive data is required to continue, stop and ask the user for a secure handling path. If sensitive data is found in a durable artifact, sanitize it before any commit. If sensitive data was already committed, tell the user and do not rewrite history without explicit approval.

### Open-Source Reference Evidence

When a SOW uses external open-source repositories as evidence, record the upstream repository identity and checked commit, not the workstation mirror path.

For local mirrored or cloned open-source repositories, cite evidence in this form:

```text
owner/repo @ commit
relative/path/inside/repo:line
```

Rules:

- Never use workstation absolute paths for external open-source evidence in SOWs.
- Resolve `owner/repo` from the repository remote, not only from the local directory name.
- Record the commit with `git -C <repo> rev-parse --short=12 HEAD` or the full hash when precision matters.
- Use paths relative to the upstream repository root after the `owner/repo @ commit` line.
- If multiple repositories were checked, list each repository and commit separately.

This project reads the Netdata agent source constantly. Cite it as `netdata/netdata @ <commit>` plus repository-relative paths such as `src/plugins.d/pluginsd_parser.c:147`.

### Pre-Implementation Gate

Implementation covered by a SOW must not begin until the SOW contains a concrete `## Pre-Implementation Gate` section. Before moving a SOW from `pending/open` to `current/in-progress`, or before continuing implementation in an existing current SOW that lacks this section, fill the gate.

The gate must record:

- Problem / root-cause model: what is happening, why it is happening, and what evidence supports that model.
- Evidence reviewed: specs, code, docs, tests, logs, traces, prior SOWs, issues, or external references checked. Open-source references from local mirrors or clones must be cited as `owner/repo @ commit` plus repository-relative paths, never as workstation absolute paths.
- Affected contracts and surfaces: APIs, schemas, files, commands, UI, docs, specs, skills, tests, integrations, operators, users.
- Existing patterns to reuse: local modules, helpers, conventions, tests, and docs that should shape the implementation.
- Risk and blast radius: regressions, compatibility, performance, security, data loss, migration, rollout, and operational risks.
- Sensitive data handling plan: whether the work may expose secrets, credentials, bearer tokens, SNMP communities, community/customer data, personal data, non-private customer-identifying IPs, private endpoints, or proprietary incident details; how evidence will be redacted.
- Implementation plan: ordered chunks with scope, dependencies, and files or modules likely to change.
- Validation plan: tests, fixtures, manual checks, real-use evidence, review passes, and same-failure searches.
- Artifact impact plan: expected updates to `AGENTS.md`, runtime project skills, specs, end-user/operator docs, end-user/operator skills, and SOW lifecycle.
- Open decisions: resolved decisions or numbered options for the user; unresolved decisions block implementation.

Generic placeholders such as `TBD`, `N/A`, or "to be checked later" are invalid unless the SOW explains why the item truly does not apply. If the gate exposes an unknown that cannot be resolved by investigation, stop and ask the user before implementation.

### When A SOW Is Required

Create or reuse a SOW only after the user requests implementation and preliminary analysis confirms a non-trivial change is needed.

Questions, discussions, reviews, status reports, and read-only investigation do not need a SOW. Trivial implementation such as typo or formatting-only fixes does not need one.

When unsure whether a change is needed, investigate first. When an authorized change has unclear risk, treat it as non-trivial.

### SOW Locations

- Pending: `.agents/sow/pending/`
- Current: `.agents/sow/current/`
- Done: `.agents/sow/done/`
- Specs: `.agents/sow/specs/`
- Template for new SOWs: `.agents/sow/SOW.template.md`
- Local audit: `.agents/sow/audit.sh`

Create new SOW files from `.agents/sow/SOW.template.md`. The template is project-local and may be customized for this repository.

Empty SOW directories must contain `.gitkeep` or `.keep` so the committed repository preserves the full SOW layout after clone/checkout.

Filename:

```text
SOW-NNNN-YYYYMMDD-{slug}.md
```

Status and directory must agree:

- `open` lives in `pending/`
- `in-progress` lives in `current/`
- `paused` lives in `current/`
- `completed` lives in `done/`
- `closed` lives in `done/`

### SOW Completion And Commit

The successful terminal SOW status is `completed`. `done` is a directory name, not a status value. Never write `Status: done` or `Status: complete`.

When a SOW's work is ready to close:

1. Finish implementation, docs, specs, skills, validation, and follow-up mapping.
2. Update the SOW to `Status: completed`.
3. Move the SOW file to `.agents/sow/done/`.
4. Commit the work, artifact updates, SOW status change, and SOW move together as one commit, unless the user explicitly requested a different commit split.

Do not create a separate commit just to mark or move the SOW.

### One SOW At A Time

Never execute multiple SOWs as one batch.

If work overlaps:

- merge or consolidate before implementation; or
- split into separate SOWs and complete one before starting the next.

Progress reports are not stop points. Once a SOW is in progress, continue until it is delivered, failed with evidence, blocked on a real user decision/approval, or superseded by newer user instructions.

### User Decisions

When user decisions are needed:

1. Present concrete evidence with files/lines or source references.
2. Provide numbered options.
3. Explain pros, cons, implications, and risks.
4. Recommend one option with reasoning, classified as **surgical** (minimize risk and blast radius) or **long-term-best** (best for the project long term).
5. Record the user's decision in the SOW before implementation.

Number decisions and label options so the user can reply `1. A  2. C  3. A`.

### Followup Discipline

"Deferred" is not a terminal outcome.

Before a SOW can close, every valid deferred item must be:

- implemented in the current SOW; or
- explicitly rejected as not worth doing, with evidence; or
- represented by a real pending/current SOW file.

Pre-close, search the SOW for:

```text
defer|later|follow-up|future|TODO|pending
```

Map every remaining item to implemented, rejected, or tracked.

### Regressions

A regression is discovered after a SOW was considered completed or closed, later testing or use finds broken behavior, and the original SOW's claimed outcome is no longer true.

When behavior that a completed SOW claimed working stops working:

1. Find the original SOW in `done/`.
2. Move it back to `current/`.
3. Mark it `in-progress` with a regression note in `## Status`.
4. Append a new dated `## Regression - YYYY-MM-DD` section at the end of the file, after the original outcome, lessons, and follow-up content.
5. In that appended section, record what broke, evidence, why previous validation missed it, the repair plan, validation, and updates needed to specs, skills, docs, audits, or follow-up SOWs.
6. Fix and validate there.

Never prepend regression content above the original SOW narrative. Do not create a new SOW for a true regression.

### Validation Gate

A SOW cannot be completed until Validation records:

- acceptance criteria evidence;
- tests or equivalent validation;
- real-use evidence when a runnable path exists;
- reviewer findings and how they were handled;
- same-failure search results;
- sensitive data gate;
- artifact maintenance gate for `AGENTS.md`, runtime project skills, specs, end-user/operator docs, end-user/operator skills, and SOW lifecycle;
- SOW status/directory consistency;
- spec update or specific reason no spec update was needed;
- project skill update or specific reason no skill update was needed;
- end-user/operator docs update or evidence-backed reason none were affected;
- lessons extracted or specific reason there were none;
- follow-up mapping.

Generic "N/A" is invalid.

**Project-specific validation rule:** any change to a generator, scenario, or the runtime must be validated against a **live agent**, not only unit tests. Fidelity claims require harness output or query evidence from a running Netdata instance. "It compiles" is not validation for this project.

### Artifact Maintenance Gate

Every SOW close must explicitly record whether each durable artifact class was updated or why no update was needed:

- `AGENTS.md` - workflow, responsibility, local framework, project-wide guardrails.
- Runtime project skills - `.agents/skills/project-*/SKILL.md` for HOW to work here.
- Specs - `.agents/sow/specs/` for WHAT the project does.
- End-user/operator docs - README, SE quickstart, runbooks, console help text.
- End-user/operator skills - output/reference skills copied or consumed outside normal repo work.
- SOW lifecycle - split, merge, status, directory, deferred work, regression reopening, and follow-up mapping.

### Specs

Specs are memory of WHAT this project does. They live at `.agents/sow/specs/`.

`spec.md` at the repository root is the **product definition** authored by the user. It is aspiration and scope. Specs under `.agents/sow/specs/` describe **current reality**. When the two disagree, that is expected during build-out; record the gap in the active SOW rather than editing `spec.md` to match the code.

Do not edit `spec.md` without explicit user approval. Corrections to it are proposed to the user, not applied unilaterally.

### Project Skills

Project skills are memory of HOW to work here.

Runtime input project skills should live under `.agents/skills/project-*/SKILL.md`. Before non-trivial implementation, inspect those skill descriptions and load every matching runtime skill.

Do not create generic `project-*` skills only to make the framework look complete.

### Project Skills Index

None yet.

Project skill creation is **deliberately deferred** until the first vertical slice produces concrete, reusable workflow knowledge — specifically the generator-authoring loop and the fidelity-harness workflow. Creating them now would produce generic filler with no project evidence behind it. This decision is recorded in `SOW-0001`.

### Project-specific commands

Rust toolchain is user-local (`~/.cargo/bin`). Shells must source `$HOME/.cargo/env` or use absolute paths.

```bash
# build
cargo build --release

# test
cargo test

# lint (both must be clean before a SOW closes)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Live-agent validation (see Validation Gate):

```bash
# install a plugin build for local verification
sudo cp target/release/<plugin> /etc/netdata/custom-plugins.d/<plugin>.plugin
# agent rescans every 60s; no restart needed for a new plugin

# verify nodes registered
curl -s localhost:19999/api/v3/nodes

# verify per-vnode charts and data
curl -s "localhost:19999/host/<hostname>/api/v1/charts"
curl -s "localhost:19999/host/<hostname>/api/v1/data?chart=system.cpu&after=-60"
```

Correlated logs run as a **separate process** from the metrics plugin and need
`systemd-journal-remote` plus root. Never fold them into the plugin: Netdata owns
the plugin's lifecycle, and a collector that outlives its own removal has already
cost this project real debugging time.

```bash
sudo apt-get install systemd-journal-remote   # one-time

./scripts/logs.sh start|status|stop           # tracks a specific PID, never a pattern

# verify a node became its own log source, and read its lines
sudo journalctl --file=/var/log/journal/remote/remote-<hostname>.journal -o short-iso
```

Verify logs through the agent, not only the file: the `systemd-journal` function
requires a `__logs_sources` selection, and `all-remote-systems` is the simulated
fleet.

### Project-specific overrides

**Verify against the agent before designing around an assumption.** This project sits on undocumented-by-default agent behavior. Two of `spec.md`'s load-bearing assumptions were only resolvable by running code against a live agent, and one of them (vnode dashboard completeness) changed the P0 estimate by roughly an order of magnitude. Source-reading is necessary but not sufficient; run the probe.

**Scale testing does not run on the user's working laptop.** Vnode-fleet scale tests (>5 vnodes) run on a separate machine. Local verification is capped at 5 vnodes.

**Netdata agent source** is available at a local checkout for reading. Cite it as `netdata/netdata @ <commit>` with repository-relative paths, never the workstation path.

### Preservation Notes

No prior `AGENTS.md`, specs, or project skills existed. `spec.md` (user-authored product definition) and `prototypes/vnode-probe/` (verification probe and findings) predate this bootstrap and are preserved unmodified.

Project SOW status: initialized
