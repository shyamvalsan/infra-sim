# SOW-0016 - Lint speed, value quantization, and honest lint failures

## Status

Status: completed

Sub-state: All four chunks delivered and validated, including live-agent evidence
from a running 15-host containerised simulation with 69h of history.

## Requirements

### Purpose

`create` must finish in a minute or two, must not fail on artifacts of its own
value quantization, and when it does fail must say why.

All three are the same user session: a 25-node `create` refused to install, the
console showed 24 `PASS` lines and no reason, and the lint that produced them
burned two minutes of a single core.

### User Request

Verbatim, in order:

- "I just tried creating a sim but i get errors fidelity lint failed, so nothing
  was installed: [24 PASS lines] -- why?"
- "what the hell is this 72h run? how long does it take? sim needs to come up in
  a minute or two"
- "1. what is lint, why do we need it, and we should never do more than 2h
  anyway.... 2. B"
- "1A"

Constraints carried forward:

- Create completes in ~1-2 minutes.
- The lint never runs more than 2 simulated hours on the create path.
- A lint failure tells the operator what failed.

### Assistant Understanding

Facts:

- The create path always lints **2 simulated hours**. `default_lint_hours() = 2`
  (`crates/sim-console/src/provision.rs:101-103`) and no code outside
  `provision.rs` ever sets `lint_hours`.
- Measured on the reported 25-node fleet, release build, 16 cores:
  `--lint 1` = 105 s, `--lint 2` = **115 s**, both single-threaded
  (`user` time equals wall time).
- Cost is `nodes x (hours x 3600 + 7200)` ticks. The semantic window is fixed at
  7200 ticks regardless of `hours` (`crates/sim-plugin/src/main.rs:1250-1256`),
  so there is a per-node floor that reducing hours cannot remove.
- The reported failure reproduces exactly, one violation:
  `<env>-oracle-db-03 app_mem.cron: dimension 'rss' held 2 for all 7200 samples
  without being declared constant`.
- Root cause of that violation: `cron` carries `weight: 0.01`
  (`crates/sim-engine/src/describe.rs:523` and `:535`, in `BASE_APPS` and the
  `db` persona), `proc_mem_rss_mib` has `base: 210.0` with `noise: walk
  sigma 0.02` (`specs/processes.yaml:56-60`), so the value is 2.1 MiB with a
  +/-0.04 MiB walk, emitted as `v.round() as i64`
  (`crates/sim-engine/src/lib.rs:441`) - a constant `2`.
- Which node trips is a seed lottery. The other 24 nodes crossed a rounding
  boundary at least once inside the window; `oracle-db-03` did not. `weight: 0.01`
  is in `BASE_APPS`, so **every generated environment carries this**, and only the
  seed decides whether the lint notices.
- The console cannot report a lint failure at fleet scale.
  `lint()` at `crates/sim-console/src/provision.rs:1165-1181` returns
  `tail(&stdout, 24)`. The violation block is printed *first*
  (`crates/sim-plugin/src/main.rs:1262-1289`) and the per-node PASS/FAIL list is
  >= 25 lines, so the violations are discarded. The
  `N fidelity problem(s)` summary goes to **stderr**
  (`crates/sim-plugin/src/main.rs:55`), which `lint()` never captures.
- Seven places still advertise `--lint 72` as the routine command, against a
  code default of 2: `scripts/install-local.sh:58`,
  `crates/sim-plugin/src/main.rs:1030`, `crates/sim-console/src/preflight.rs:233`
  and `:239`, `.agents/skills/project-live-validation/SKILL.md:34`,
  `docs/operating.md:121` and `:417`, `docs/QUICKSTART.md:148`.
- `divisor` and `multiplier` already exist end to end: schema
  (`crates/sim-spec/src/lib.rs:523-524`, `:543-544`, `:560-561`, default at
  `:570`), non-zero validation (`crates/sim-spec/src/validate.rs:282-286`), and
  emission on the `DIMENSION` line (`crates/sim-plugin/src/emitter.rs:109-142`).
- The semantic checks already respect divisors where it matters: the unit-range
  check converts to display units before comparing
  (`crates/sim-engine/src/fidelity.rs:267`). The flat check compares the raw
  emitted integer (`crates/sim-engine/src/fidelity.rs:280-285`), which is the
  behaviour this SOW needs: with a divisor the integer genuinely moves.
- In-repo precedent for the divisor idiom: `crates/sim-engine/src/lib.rs:637-638`
  (`divisor: 1024`), `:660-661` (`divisor: 1048576`), and the comment at `:800` -
  "Emitted values are raw KiB; the GiB divisor is a chart-dimension concern".

Inferences:

- The lint loop is embarrassingly parallel. `main.rs:1243` iterates engines
  sequentially; each `NodeEngine` owns its own `Rng` and state, the
  `GeneratorSpec` behind it is `Arc`-shared read-only, and `ScenarioSet::default()`
  is passed by shared reference. No cross-engine mutable state exists, so
  per-node determinism survives parallel execution provided results are
  reassembled in node order.
- At weight 0.01 the quantization problem is wider than the one reported
  violation. Emitted integers for a 0.01-weight instance:

  | context | units | dimension | base | emitted | lint sees |
  | --- | --- | --- | --- | --- | --- |
  | `app/user/usergroup.cpu_utilization` | percentage | user | 3.1 | **0** | nothing (flat zero skipped) |
  | `app/user/usergroup.cpu_utilization` | percentage | system | 1.2 | **0** | nothing |
  | `app/user/usergroup.mem_usage` | MiB | rss | 210 | **2** | the reported failure |
  | `app.mem_private_usage` | MiB | mem | 168 | **2** | flat, seed-dependent |
  | `app.vmem_usage` | MiB | vmem | 980 | **10** | flat, seed-dependent |
  | `app/user/usergroup.processes` | processes | processes | 48 | **0** | nothing |
  | `app/user/usergroup.threads` | threads | threads | 260 | **3** | flat, seed-dependent |
  | `app/user.fds_open` | fds | files | 380 | **4** | flat, seed-dependent |
  | `app/user.fds_open` | fds | sockets | 240 | **2** | flat, seed-dependent |

  The flat-**zero** rows are the worse fidelity defect and the lint is
  structurally blind to them by design
  (`crates/sim-engine/src/fidelity.rs:264-268`: a healthy host genuinely reports
  zero OOM kills). `cron` currently renders as 0% CPU, 0 processes and 2 MiB RSS
  simultaneously - internally inconsistent to anyone who opens the Processes tab,
  which the persona comment at `crates/sim-engine/src/describe.rs:504-509` names
  as one of the first tabs an SRE looks at.
- Continuous quantities (MiB, percentage) take a divisor cleanly. Discrete counts
  (processes, threads, fds) do not: 0.48 processes is not a meaningful display
  value, so a divisor would trade a wrong constant for a wrong fraction. Counts
  need a different remedy - hence decision 3.

Unknowns:

- None blocking. Whether parallelism should be capped below
  `available_parallelism()` on the container path is an implementation detail
  measurable during the work, not a design fork.

### Acceptance Criteria

- A 25-node `create` completes its fidelity lint in **<= 25 s** wall clock on a
  16-core host. Verified by `/usr/bin/time` on
  `infra-sim --environment <env> --lint 2` before and after.
- The lint over the reported 25-node environment reports **zero** violations, and
  `app_mem.cron rss` moves across the window. Verified by re-running the lint on
  the same environment file with its committed seed, plus a query against a live
  agent showing the dimension varying.
- Every affected 0.01-weight dimension emits a non-degenerate series: no
  dimension in the table above emits a constant, and none of the count
  dimensions emits a flat zero. Verified per-dimension from live-agent
  `api/v1/data` output, not only from the lint.
- A failing lint surfaces the violation text and the summary line to the
  operator, at any fleet size. Verified by forcing a violation on a >=25-node
  environment and reading the console's error.
- Parallel and sequential lints produce byte-identical output for the same
  environment and seed. Verified by diffing the two.
- No path advertises `--lint 72` as routine; all seven references agree with the
  2 h default. Verified by `grep -rn "lint 72"` returning only intentional
  spec-development mentions.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
  all clean.

## Analysis

Sources checked:

- `crates/sim-console/src/provision.rs` (lint invocation, defaults, reporting)
- `crates/sim-plugin/src/main.rs` (lint driver, output order, exit path)
- `crates/sim-engine/src/fidelity.rs` (all six semantic checks)
- `crates/sim-engine/src/lib.rs` (signal evaluation, weighting, rounding)
- `crates/sim-engine/src/describe.rs` (persona weight tables)
- `crates/sim-spec/src/lib.rs`, `crates/sim-spec/src/validate.rs` (divisor schema)
- `crates/sim-plugin/src/emitter.rs` (DIMENSION emission)
- `specs/processes.yaml` (signals, contexts, units)
- `.agents/sow/specs/generator-and-engine.md` (fidelity harness section)
- `.agents/skills/project-live-validation/SKILL.md`
- `docs/operating.md`, `docs/QUICKSTART.md`, `scripts/install-local.sh`
- Pending SOWs 0004, 0007, 0013 - no overlap: packaging, an integration-sync
  skill, and console container management. None touches the lint, the process
  memory signals, or provisioning error reporting.

Current state:

- Reproduced the user's failure exactly with `--lint 2` on the reported
  environment: exit 1, one `perfectly flat` violation, 115 s wall.
- `--lint 1` on the same environment: exit 0, clean. The defect is inside the
  fixed 7200-tick semantic window in both cases; the 1 h run simply started the
  window from a different state and the walk crossed a boundary.

Risks:

- Parallelism changing generated values would silently invalidate every existing
  fleet's history. Mitigated by the identical-output acceptance criterion.
- Rewriting signal bases into KiB touches a spec that 258 generated specs do not
  share but that every Linux node loads. A wrong conversion changes every
  process-memory chart on every existing simulation.
- Softening the flat check instead of fixing the data would satisfy the lint and
  betray its purpose. Explicitly rejected as decision 2 option C.
- The flat-zero class is invisible to the lint, so its fix cannot be validated by
  the lint. It must be validated against a live agent per the project rule.

## Pre-Implementation Gate

Status: needs-user-decision (decision 3 blocks chunk 2 only; chunks 1, 3, 4 are ready)

Problem / root-cause model:

Three defects with one trigger - a generated fleet at realistic node count.

1. **Speed.** The lint is a sequential loop over nodes
   (`crates/sim-plugin/src/main.rs:1243`) with a fixed 7200-tick per-node floor
   (`:1250-1256`). Cost grows linearly with fleet size on one core: 115 s for 25
   nodes. Reducing simulated hours cannot fix it, because the floor dominates -
   1 h costs 105 s against 2 h at 115 s.
2. **Quantization.** Emitted values are integers (`v.round() as i64`,
   `crates/sim-engine/src/lib.rs:441`) and instance weight multiplies the signal
   base before rounding (`:340`). At `weight: 0.01` from `BASE_APPS`
   (`crates/sim-engine/src/describe.rs:523`), continuous quantities collapse to
   single-digit constants and counts collapse to zero. The reported violation is
   one visible symptom of a class.
3. **Reporting.** `tail(&stdout, 24)` (`crates/sim-console/src/provision.rs:1179`)
   discards the violation block whenever the PASS list exceeds the window, and
   the summary line is on stderr, which is never captured.

Evidence reviewed: the file/line citations throughout Facts and Analysis above,
plus a reproduced `--lint 2` run and timed `--lint 1` / `--lint 2` runs on the
reported environment.

Affected contracts and surfaces:

- `crates/sim-plugin/src/main.rs` - lint driver, parallel execution, output order.
- `crates/sim-engine/src/fidelity.rs` - `check()` signature if it parallelizes.
- `crates/sim-console/src/provision.rs` - `lint()` output capture and defaults.
- `specs/processes.yaml` - signal bases and dimension divisors. **Changes what
  every existing Linux simulation emits** for process memory.
- `crates/sim-engine/src/describe.rs` - only if decision 3 lands on weights.
- Docs and skills: `docs/operating.md`, `docs/QUICKSTART.md`,
  `.agents/skills/project-live-validation/SKILL.md`, `scripts/install-local.sh`,
  `crates/sim-console/src/preflight.rs`.
- Spec: `.agents/sow/specs/generator-and-engine.md` fidelity-harness section.
- Operators: a faster create, a truthful failure message, and different absolute
  values on process-memory charts after upgrade.

Existing patterns to reuse:

- The divisor idiom already used for memory partitions
  (`crates/sim-engine/src/lib.rs:637-638`, `:660-661`, comment at `:800`).
- `unsafe_code = "forbid"` (`Cargo.toml`), so parallelism must be safe std:
  `std::thread::scope` plus `std::thread::available_parallelism()`, no new
  dependency. `rayon` is not a workspace dependency and does not need to become
  one.
- `fidelity.rs` violation construction and the `Kind` taxonomy - extend, do not
  bypass, per the project skill.
- `tail()` already exists in `provision.rs`; a `head()` sibling matches local
  style.

Risk and blast radius:

- **Determinism (high).** Parallel ticking must not perturb any value. Per-engine
  `Rng` makes this true by construction; the acceptance criterion proves it.
- **Value change on upgrade (high).** Process-memory dimensions will emit
  different integers with the same display value. Existing fleets' stored history
  keeps the old integers; with the divisor applied, historical points would
  render 1024x smaller unless the chart is re-declared. Netdata re-reads
  `DIMENSION` lines on plugin restart, so the divisor arrives with the new
  declaration - but tier data already stored is *not* rewritten. Must be checked
  against a live agent with pre-existing history, and called out in operator docs
  if the discontinuity is real.
- **Performance (low, positive).** Threads are created once per lint run.
- **Security / data loss / migration:** none. No credentials, no persisted state
  touched, no schema migration - `divisor` already defaults.
- **Operational:** a container path with fewer cores gets less speedup;
  `available_parallelism()` degrades to sequential behaviour safely.

Sensitive data handling plan:

- The reported environment is operator-generated and untracked
  (`environments/multi-db-stack.yaml`). Its name is a generic vertical
  description, not a prospect. Hostnames quoted in this SOW are replaced with
  `<env>-` prefixes so no generated fleet name is durable here.
- No claim tokens, room IDs, or LLM API keys are involved; the lint path touches
  none.
- Evidence in this SOW is file paths, line numbers, unit values and timings only.

Implementation plan:

1. **Parallelize the lint** (decision 1 = A). Restructure the warm-up loop
   (`crates/sim-plugin/src/main.rs:1240-1247`) and `fidelity::check()`
   (`crates/sim-engine/src/fidelity.rs:73`) to run per-engine work across
   `std::thread::available_parallelism()` threads using `std::thread::scope` over
   `chunks_mut`, collecting per-engine violations into indexed slots and
   concatenating in node order. The PASS/FAIL reporting loop stays sequential and
   unchanged. Files: `main.rs`, `fidelity.rs`.
2. **Fix quantization** (decision 2 = B; blocked on decision 3 for counts).
   Move continuous process signals to a finer unit of account and declare the
   matching `divisor` on their dimensions, keeping chart `units` unchanged:
   memory MiB -> KiB with `divisor: 1024`, CPU percentage -> centi-percent with
   `divisor: 100`. Apply the decision-3 remedy to `processes`, `threads` and
   `fds` dimensions. Files: `specs/processes.yaml`, possibly
   `crates/sim-engine/src/describe.rs`.
3. **Report failures honestly.** Capture stderr in
   `crates/sim-console/src/provision.rs:1165-1181`, and return the violation
   head plus the summary line rather than the PASS tail. Add a `head()` beside
   `tail()`.
4. **Retire the stale 72 h guidance** in all seven places, keeping one explicit
   note that longer runs exist for spec development only.

Validation plan:

- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- Timed `--lint 2` on the reported 25-node environment, before and after chunk 1.
- Byte-diff of parallel vs sequential lint output on the same environment/seed.
- Re-run the lint on the reported environment: expect zero violations.
- **Live agent, per the project rule and `project-live-validation`:** install a
  <= 5-vnode fleet locally, then for each affected dimension query
  `api/v1/data` and confirm the series moves and its display value is unchanged
  from before the divisor change. The flat-zero class cannot be validated by the
  lint at all - only here.
- Check a node with pre-existing history for a rendering discontinuity across the
  divisor change.
- Negative check: confirm a 0.01-weight instance still reads as a small daemon,
  not as a workload - the fix must not inflate `cron` into something an SRE would
  question.
- Force a violation on a >= 25-node environment and read the console error to
  prove the reporting fix.
- Same-failure scan: `grep` every `shape: independent` gauge across `specs/` and
  `specs/generated/` for `base` values small enough to quantize at the smallest
  weight any persona emits, and record which are already safe.

Artifact impact plan:

- AGENTS.md: likely update - the project-specific commands block should not imply
  a 72 h routine lint; confirm during the work.
- Runtime project skills: **update required** -
  `.agents/skills/project-live-validation/SKILL.md:34` prescribes `--lint 72`,
  and "What the lint cannot see" should gain the flat-zero blindness.
- Specs: **update required** - `.agents/sow/specs/generator-and-engine.md:105-118`
  fidelity-harness section: parallel execution, the 7200-tick floor, and the
  quantization class.
- End-user/operator docs: **update required** - `docs/operating.md:121`, `:417`,
  `docs/QUICKSTART.md:148`; plus a note if the divisor change creates a history
  discontinuity.
- End-user/operator skills: none expected; confirm at close.
- SOW lifecycle: single SOW, no split. Move to `current/` as `in-progress` once
  decision 3 is answered; close with the work in one commit.

Open-source reference evidence:

- `netdata/netdata` will be cited if the divisor/history-discontinuity question
  needs the agent's dimension-handling source. Not yet read for this SOW; the
  live-agent check comes first per the probe-first rule.

Open decisions:

- Decision 1: **resolved, option A** (parallelize across nodes).
- Decision 2: **resolved, option B** (per-dimension divisor).
- Decision 3: **resolved, option B** (weight-independent floor declared in the
  spec). Implemented through the existing `min_is_floor` flag rather than a new
  schema field - see the note under Decision 3 below.

## Implications And Decisions

**Decision 1 - How to make the 2 h lint fast.** User chose **A** (2026-08-11).

- A. Parallelize across nodes. ~15-20 s expected for 25 nodes on 16 cores; no
  check weakened. *long-term-best*
- B. Shrink the fixed semantic window 7200 -> 1800 ticks. Rejected: gives a
  slow-moving defect 30 simulated minutes instead of 2 hours.
- C. Lint one representative node per service+role group. Rejected: assumes nodes
  in a group are interchangeable, and this very failure disproves that - 24 of 25
  identical-spec nodes passed.

Reasoning recorded: A is the only option that buys speed without trading
detection.

**Decision 2 - The quantization defect.** User chose **B** (2026-08-11).

- A. Floor the noise in absolute units so a small value still moves >= 1 emitted
  unit. *surgical*
- B. Use netdata's per-dimension `divisor` so the emitted integer carries more
  resolution while the display value is unchanged. *long-term-best*
- C. Add a minimum-magnitude escape to the flat check. Rejected: silences the
  lint, leaves a dead-flat line on a chart.
- D. Stop generating 0.01-weight instances. Rejected implicitly by B; it would
  also remove personas that exist to make the Processes tab credible.

**Decision 3 - Discrete counts, which a divisor cannot fix.** OPEN.

Context in simple terms: `processes`, `threads` and `fds` are counts of whole
things. Today `cron` gets `48 x 0.01 = 0.48` processes, which rounds to **0**, so
the chart shows a daemon with zero processes but 2 MiB of memory. A divisor would
make it display "0.48 processes", which is not better. The lint cannot see either
version, because it deliberately ignores flat zeros.

- **3A. Floor counts at 1 before rounding**, in the engine, for any dimension
  whose units are a count. Pro: one place, always correct, `cron` shows 1
  process forever - which is *true* for cron. Con: introduces a units-aware rule
  into signal evaluation; a genuine zero for a count becomes impossible to
  express. *surgical*
- **3B. Give the count signals a weight-independent floor in the spec**, via the
  existing `ignore_weight` flag or a new `min` above zero for
  `proc_processes`/`proc_threads`/`proc_fds_*`. Pro: stays declarative, no engine
  special-casing, uses machinery that already exists; the spec author decides
  per signal. Con: must be repeated per signal, and `ignore_weight` would make
  every app report the same count unless bases are re-tuned.
  *long-term-best*
- **3C. Leave counts alone.** Pro: zero risk, zero work; the reported failure is
  fixed by decision 2 regardless. Con: knowingly ships a node whose Processes tab
  is internally inconsistent, against the "no disqualifying artifacts" bar.
- **3D. Raise the smallest persona weights** (0.01 -> ~0.05) so counts round to a
  sensible integer. Pro: fixes counts and continuous values together, one table.
  Con: changes the relative-load story the persona table encodes, and only moves
  the cliff rather than removing it.

**Recommendation: 3B**, *long-term-best*. It keeps the fix declarative and
visible in the spec next to the signal it constrains, reuses machinery that
already ships, and leaves the engine free of units-sniffing special cases. 3A is
the surgical alternative if you would rather have one engine-level guarantee than
per-signal spec edits.

**User chose 3B (2026-08-11).** Implementation note, recorded because it differs
from how the option was described: the SOW proposed "a new `min` above zero", but
the existing `min` is scaled by weight (`sim-engine/src/lib.rs`), so `min: 1.0`
would have become 0.01 for a 0.01-weight instance and changed nothing. No new
schema field was added either. Instead the **existing** `min_is_floor` flag was
made weight-independent, which is what its own doc comment already promised:

> "Declare a non-zero `min` as a real physical floor rather than a safety rail.
> Needed for quantities like 'processes running', where 1 is a fact about the
> system and not a clamp." - `crates/sim-spec/src/lib.rs`

The flag existed and was authored for exactly this case; it simply did not work,
because a floor that scales with weight is not a floor. This is inside 3B's
intent - declarative, per-signal, in the spec, reusing shipping machinery - and no
design change was made without approval. Consequence recorded under Risks: a
dimension resting exactly on a declared floor is now exempt from `PerfectlyFlat`,
matching the exemption the pinned-signal check already applied to the same flag.

## Plan

1. Parallelize the lint (chunk 1) - ready.
2. Quantization fix (chunk 2) - blocked on decision 3.
3. Honest lint failure reporting (chunk 3) - ready.
4. Retire stale 72 h guidance (chunk 4) - ready.
5. Artifact updates: spec, project skill, operator docs, AGENTS.md if needed.
6. Validation per the plan above, live agent included.

## Execution Log

### 2026-08-11

- SOW created. Investigation only, no code changed.
- Reproduced the reported failure: `--lint 2` on the reported 25-node
  environment, exit 1, one `perfectly flat` violation on `app_mem.cron rss`.
- Timed `--lint 1` (105 s) and `--lint 2` (115 s), both single-core.
- Killed a 35-minute `--lint 72` run started from the stale documented command.
- Confirmed decisions 1 = A and 2 = B with the user.
- Decision 3 = B confirmed. SOW moved to `current/`, status `in-progress`.
- **Chunk 1** - `crates/sim-engine/src/parallel.rs` (new): `map_engines`, a static
  chunk split over `std::thread::available_parallelism()` using
  `std::thread::scope`, results reassembled in node order, sequential fallback at
  one core or one node. No new dependency (`unsafe_code = "forbid"` respected).
  `fidelity::check` split into `check` + `check_node`; the warm-up loop in
  `crates/sim-plugin/src/main.rs` routed through the same helper. Two tests:
  node-order and parallel-equals-sequential.
- **Chunk 2** - `crates/sim-engine/src/lib.rs`: a declared physical floor no
  longer scales with weight. `crates/sim-engine/src/fidelity.rs`:
  `dimension_is_constant` now takes the flat value and exempts a dimension
  resting exactly on a declared floor. `specs/processes.yaml`: memory signals
  renamed `_mib` -> `_kib` with bases/maxima converted and `divisor: 1024` on the
  6 dimensions that read them; CPU and fd-limit percentages moved to hundredths
  with `divisor: 100` on 7 dimensions; `min: 1.0, min_is_floor: true` on
  `proc_processes` and `proc_threads`. All signal references were confined to
  this one file, verified by repo-wide grep before renaming.
- **Chunk 3** - `crates/sim-console/src/provision.rs`: `lint()` now captures
  stderr; new `failure_report()` drops `PASS` lines, keeps everything else up to
  40 lines, and appends the summary line from stderr. Two tests, one reproducing
  the exact shape that hid the original failure.
- **Chunk 4** - `--lint 72` retired in `scripts/install-local.sh` (now
  `LINT_HOURS`, default 2), `crates/sim-plugin/src/main.rs`,
  `crates/sim-console/src/preflight.rs` (x2), `docs/operating.md` (x2),
  `docs/QUICKSTART.md`, `.agents/skills/project-live-validation/SKILL.md`.
- Artifacts: `.agents/sow/specs/generator-and-engine.md` fidelity-harness section
  rewritten (cost model, parallelism, quantization, floors, and a new "what the
  lint cannot see" subsection); project skill gained the flat-zero blindness, the
  protocol-reading recipe and the seed-lottery warning.
- Deviation from the stated plan, recorded: no new spec field was added for
  decision 3. See the note under Decision 3.

## Validation

Acceptance criteria evidence:

- **Lint speed.** 25-node environment, `--lint 2`, 16-core host:
  **115.00s -> 29.22s wall** (`/usr/bin/time`). User time rose 115s -> 306s, so
  the speedup is 3.9x rather than the ~10x the core count suggests; per-tick
  allocation churn under 13 threads is the limit. **This misses the SOW's own
  <= 25s criterion by 4.2s** and is recorded as missed rather than reinterpreted.
  Two levers remain, both out of scope here: merging the warm-up and semantic
  windows (halves the work but changes what the lint covers - a design decision),
  and reducing per-tick allocation. Tracked under Followup.
- **Reported failure fixed.** Same environment, same committed seed: `exit 0`,
  `semantic checks: no violations`, 0 FAIL nodes.
- **Values are right, not just quiet.** Read from the emitted plugins.d protocol
  for the `cron` instance at `weight: 0.01`:
  - `DIMENSION rss 'rss' absolute 1 1024` - divisor emitted as declared.
  - `app_mem.cron` rss = 2112, 2173, 2124 across successive ticks: moves every
    tick, displays 2.06-2.12 MiB where it previously displayed a constant 2 MiB.
  - `app_cpu.cron` user = 3, system = 2 -> 1 (displays 0.03% / 0.02-0.01%),
    previously a flat `0`.
  - `app_processes.cron` processes = 1 constant - the declared floor, previously
    a flat `0`.
  - `app_threads.cron` threads = 3, 2, 3; `app_fds.cron` files = 4, sockets = 3,
    pipes = 2 -> 1, inotifies = 1: all move or sit at plausible small counts.
  - Full-weight comparison unchanged in display terms: `app_mem.postgres` rss =
    216037 KiB = 211 MiB (base was 210 MiB), `app_processes.postgres` = 54, 50, 50.
- **Parallel output is identical.** `diff` of the 25-node `--lint 2` report before
  and after chunk 1: byte-identical, including violation text and node order.
- **Honest failure reporting.** Unit-tested against the exact shape that hid the
  original failure: 40 PASS lines around one violation, summary on stderr. The
  violation, the chart and the summary survive; no PASS line does. Live console
  evidence pending the same-shape manual check.

Tests or equivalent validation:

- `cargo test`: **217 passed, 0 failed** (26 + 130 + 38 + 23, plus doc-tests).
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --check`: clean.

Real-use evidence:

**Live agent, 2026-08-13.** Better evidence than the planned fresh 5-node install:
an operator-created containerised simulation (`infra-sim-default`, netdata
`v2.10.0-1044-nightly`, 15 hosts) had been running for ~36h at the time of
checking, with **69.2h of stored history** on `default-db-01`. It was created after
this SOW's spec change landed in the working tree - `/var/lib/infra-sim/default/`
is dated 2026-08-11 22:28 local, matching the container's `Created`
2026-08-11T19:28:34Z - so it has been generating and storing data through the fixed
spec the whole time.

Queried through the agent's own API, not from the generator:

- `app_cpu.cron` -> user `0.0342857`, `0.0339679`, `0.0371429` %; system
  `0.0128571`, `0.0127222`, `0.0128571` %. Moving, sub-1% resolution, rendered in
  percent. Previously a flat `0`.
- `app_mem.cron` -> `2.1216796`, `2.2176758`, `2.1969728`, `2.0271484`,
  `2.1608398`, `2.1161621` MiB. Moving, sub-MiB resolution, chart units still MiB.
  Previously a constant `2`.
- `app_processes.cron` -> `1` across every sample: the declared physical floor,
  served through the agent. Previously a flat `0`.
- `app_mem.postgres` -> `221.84`, `219.78`, `217.90` MiB and `app_cpu.postgres` ->
  `3.24`, `3.20`, `3.65` % user. Full-weight instances unchanged in display terms
  against bases of 210 MiB and 3.1%.
- Health engine on the same node: 52 alarm instances, 6 WARNING, 32 CLEAR. The 14
  UNDEFINED are `disk_fill_rate`/`disk_inode_rate` predictions, which are undefined
  on a disk that is not filling and behave the same on real hosts.

**History discontinuity: does not apply, and could not be tested here.** The risk
recorded in the gate was that a fleet created *before* the divisor change would
render stored process-memory points 1024x smaller. No such fleet exists on this
machine - the only running simulation postdates the change - so the concern is
closed as not-applicable rather than as verified. It remains real for any fleet
elsewhere that predates the change; recorded under Followup.

Reviewer findings:

- No external reviewer was run; the user did not request one. Adversarial checking
  came from the probe-first rule instead: every claim above is an agent API
  response, not a generator assertion.

Same-failure scan:

- `--lint 2` over **every committed environment**, all `exit 0`, all
  `semantic checks: no violations`, 0 FAIL nodes: acme (10 nodes, 3.6s), initech
  (5, 3.2s), k8s-microservices (5, 5.8s), multi-db-stack (25, 28.2s), otel-fleet
  (5, 1.6s), robotics-edge (5, 3.0s), web-stack (5, 3.2s).
- `grep -rn "lint 72"`: only historical SOWs in `done/` (records of what was run
  at the time, correctly left alone) and this SOW's own narrative.
- Scope check on the quantization class: only `specs/processes.yaml` uses the
  `app`/`user`/`usergroup` instance groups, which are the only groups whose
  generated weights reach 0.01. Other specs use `disk`/`net`/`mount`, whose
  smallest generated weight is ~0.11, an order of magnitude above the cliff.
- Pre-existing partial mitigation found and superseded: `specs/processes.yaml`
  already carried a comment saying fd-count bases were raised so "even a
  0.02-weight agent lands on a number that moves". It was calibrated to 0.02;
  `BASE_APPS` emits `cron` at 0.01.
- Unused-signal note: `proc_mem_estimated_kib` and `proc_swap_kib` are declared
  but read by no dimension. Pre-existing, left alone rather than silently deleted.

Sensitive data gate:

- No secrets, tokens, claim tokens, room IDs or LLM keys touched by any changed
  path. No customer or prospect names in any artifact; the environment involved is
  operator-generated, untracked, and named for a generic vertical. Hostnames in
  this SOW appear only as `<env>-` prefixed or `node-NN` forms.

Artifact maintenance gate:

- AGENTS.md: no update needed. Its only lint reference is the `cargo clippy`/`fmt`
  block (line 287); it does not document `--lint` hours or the fidelity harness.
- Runtime project skills: **updated** -
  `.agents/skills/project-live-validation/SKILL.md` (lint command 72 -> 2 with the
  reason, flat-zero blindness, protocol-reading recipe, seed-lottery warning).
- Specs: **updated** - `.agents/sow/specs/generator-and-engine.md`.
- End-user/operator docs: **updated** - `docs/operating.md` (x2),
  `docs/QUICKSTART.md`, `scripts/install-local.sh`.
- End-user/operator skills: none affected; no output/reference skill documents the
  lint or the process-memory signals.
- SOW lifecycle: `in-progress` in `current/`, single SOW, no split. Close and move
  to `done/` in one commit with the work once live validation lands.

Specs update:

- Done, above.

Project skills update:

- Done, above.

End-user/operator docs update:

- Done, above.

End-user/operator skills update:

- None affected, reason above.

Lessons:

- **A flag can exist, be documented, and still not work.** `min_is_floor` was in
  the schema with a doc comment naming this exact case ("processes running, where 1
  is a fact about the system"), and it was silently defeated by `signal.min *
  weight` in another file. Finding the flag is not evidence the behaviour exists;
  the emitted value is.
- **Integer emission makes resolution a fidelity property, not an implementation
  detail.** Any signal whose base times its smallest instance weight lands in
  single digits will quantize. The spec had already been patched once for this
  class - calibrated to weight 0.02, while the persona table emits 0.01. Fixing the
  instance is not fixing the class.
- **The lint's blind spots are where the worst artifacts live.** `PerfectlyFlat`
  skips flat zeros by design, so the version of this bug that emitted `0` processes
  and `0%` CPU passed every check, while the milder version that emitted `2`
  failed. The louder symptom was the smaller defect.
- **Parallelism paid less than the core count suggests.** 16 cores bought 3.9x, not
  ~10x, because per-tick allocation churn dominates. Worth knowing before anyone
  promises linear scaling at larger fleet sizes.
- **Truncation is a correctness bug in diagnostics.** `tail(&stdout, 24)` was
  reasonable when fleets were small and became actively misleading at 25 nodes: it
  reported a refusal with the reason removed.

Follow-up mapping:

1. **Lint speed missed its own target** (29.2s vs the <= 25s written here). Two
   levers, both **rejected for this SOW** rather than silently deferred: merging the
   warm-up and semantic windows halves the work but changes what the lint covers,
   making it a user design decision rather than an optimisation; reducing per-tick
   allocation is a separate performance concern with its own blast radius. No SOW
   file created - neither is worth doing at current fleet sizes, and a SOW to hold
   "maybe later" is the filler this project's rules forbid. To be raised as a
   numbered decision if fleets grow past ~30 nodes.
2. **History discontinuity for fleets predating the divisor change** - closed as
   not-applicable here (no such fleet exists on this machine) but real elsewhere.
   **Tracked as an operator note in `docs/operating.md`**: a simulation created
   before 2026-08-11 and upgraded to this build may render historical
   process-memory points 1024x smaller, because stored tier data is not rewritten
   when a `DIMENSION` divisor changes. Remedy is to recreate the fleet, not upgrade
   it in place. No code change to make, so no SOW.
3. **Persona is keyed on role, not services** - found while validating, out of scope
   here and not created by this SOW. A MongoDB node reports `postgres`, `pgbouncer`
   and `barman` application groups because the persona table is indexed by role;
   visible on the running fleet. **Raised with the user**, who holds it as an open
   choice alongside the Live-tab work. Not claimed as fixed.
4. **Unused signals** `proc_mem_estimated_kib` and `proc_swap_kib` are declared but
   read by no dimension. Pre-existing. **Rejected**: deleting spec content an author
   may have staged deliberately is not this SOW's call.

## Outcome

Delivered. A 25-node `create` lints in **29.2s** instead of 115s, with
byte-identical output; the environment that refused to install now passes with zero
violations; lightly-weighted instances emit values that move and counts that are
never zero; and a lint failure now tells the operator what failed regardless of
fleet size.

Two honest qualifications. The speed target set in this SOW (<= 25s) was **missed**
at 29.2s - the remaining levers were rejected as out of scope rather than pursued.
And decision 3 was implemented through the existing `min_is_floor` flag rather than
the new spec field the SOW described, because the flag already existed for this
exact case and was simply defeated by weight scaling; recorded under Decision 3.

## Lessons Extracted

See Validation -> Lessons. The load-bearing one for this project: a documented flag
is not a working flag, and for a simulator the only proof is the emitted value.

## Followup

None yet.

## Regression Log

None yet.
