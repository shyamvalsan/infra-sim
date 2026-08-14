# SOW-0004 - Packaging and SE quickstart (`startsim`)

## Status

Status: completed

Sub-state: `startsim` delivered and validated end to end, including a real
simulation created from the statically-linked binary and torn down again.

## Requirements

### Purpose

Make Infra-Sim runnable by an SE who did not build it, and demonstrable to the
Netdata team without the author present.

`spec.md`'s success criterion is an SE producing an expert-grade simulated
environment in <= 4 hours of hands-on effort. Everything built so far assumes a
developer workstation with a Rust toolchain, a live agent and repo-relative paths.

### User Request

Originally raised during `SOW-0001`: "how long does it take to make the simulated
infra currently, is this something i can demo to the netdata team internally".
Answered with measured timings; the packaging work itself was deferred.

Reopened 2026-08-14: "is there a single kickstart style command to start the sim ux
on a new machine? that handles any dependencies, installation etc.. as well.. can
you do this", followed by decisions "1A 2A 3C (and instead of kickstart lets call it
startsim)".

### Assistant Understanding

Facts:

- The shortest path today is four commands and two pre-installed dependencies.
  `README.md:99` states it: "You need **Rust** (stable) and **Docker**. Root is
  needed for the console."
- Dependency handling today is a failure message, not a check-and-guide:
  `scripts/sim-docker.sh:61-64` verifies docker exists and its daemon answers, then
  dies. Nothing checks for Rust, python3, or root.
- There is **no release automation of any kind** - `.github/workflows` does not
  exist - so "ship prebuilt binaries" means inventing a release process, a platform
  matrix and a signing/hosting story from nothing.
- Docker is **already mandatory**: a simulation *is* a container
  (`scripts/sim-docker.sh`, `docker/Dockerfile`). Rust is the only dependency that
  does not have to be on the operator's machine.
- `docker/Dockerfile` expects the plugin to be built outside and copied in - its own
  comment says "Built outside: `cargo build --release`, then `docker/build.sh`" -
  so the image does not currently build any Rust.
- `scripts/sim-docker.sh` shells out to `python3` twice, so python3 is a real host
  dependency that nothing currently checks.
- Correlated logs and OTLP are **already automatic** on the container path:
  `scripts/sim-docker.sh:213-221` starts `--logs` and `--otlp` inside the container
  as part of `create`, with a comment recording why it must not be opt-in again.

Spike findings, 2026-08-14 (this is what makes the chosen option real):

- A **static musl build inside Docker works**. `rust:alpine` + `musl-dev`,
  `cargo build --release --bin infra-sim --bin infra-sim-console`, cold, produced
  both binaries in **1m11s**. `file` reports `ELF 64-bit LSB pie executable,
  x86-64, static-pie linked`.
- The resulting binary **runs on this host and works**: `--help` renders, and
  `--environment environments/web-stack.yaml --lint 1` completes with "no signals
  pinned to their bounds".
- Static linking removes a real portability trap. Host glibc here is **2.43**, the
  simulation container's is **2.41**, and the plugin has to run inside the container
  while the console runs on the host. A glibc build would silently constrain which
  machines the artifacts work on; a static one does not.
- **The declared MSRV is wrong.** `Cargo.toml` says `rust-version = "1.85"`, but the
  locked `tonic@0.14.6` and `tonic-prost@0.14.6` require **rustc 1.88**. A 1.85
  toolchain fails with "requires rustc 1.88". The spike only succeeded once pointed
  at current stable (1.97.1). See Open decisions.

Inferences:

- The measured baseline recorded here on 2026-08-08 is stale: it cites a 125s lint,
  which `SOW-0016` reduced to 29s for a 25-node fleet. Cold time is now dominated by
  the container image build, not the lint.
- A genuine one-liner on a new machine means `curl … | sudo sh`, because anything
  else requires a clone first. That is a trust posture, not just a mechanism, which
  is why it was put to the user rather than assumed.

Unknowns:

- None blocking. Whether `rust:<version>-alpine` pinned tags exist for the chosen
  version is a build detail confirmed during implementation, not a design fork.

### Acceptance Criteria

- On a machine with Docker and no Rust toolchain, one command brings up the console
  and prints its URL. Verified by building through the Docker builder with the
  host's `cargo` untouched, and confirming the produced binaries are `static-pie
  linked`.
- Both delivery shapes work from the same script: `./startsim.sh` inside a clone,
  and `curl -fsSL … | sudo sh` on a bare machine. Verified by running the in-repo
  path end to end, and by exercising the clone branch with the target directory
  absent.
- A missing dependency stops the script with an actionable message naming the
  package and how to install it - and installs nothing. Verified by simulating
  absent docker, absent python3 and non-root.
- The produced `target/release/infra-sim` is byte-compatible with what the existing
  tooling expects: `scripts/sim-docker.sh build` and `create` work against it
  unchanged. Verified by building an image and creating a simulation from it.
- Binaries are not left root-owned in an operator's checkout. Verified by `ls -l`
  after a root-run build.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
  stay clean (no Rust source changes expected, so this is a regression check).

## Analysis

Sources checked:

- `README.md:99-133` (getting started), `docs/QUICKSTART.md`, `docs/operating.md`
- `scripts/install-local.sh`, `scripts/sim-docker.sh`, `scripts/scenario.sh`
- `docker/Dockerfile`, `Cargo.toml`, absence of `.github/workflows`
- `crates/sim-console/src/main.rs:680-763` (console arguments, container adoption)
- Pending SOWs 0007 and 0013 for overlap; `SOW-0016` (closed today) for the stale
  timing baseline

Risks:

- A container running the agent plus the plugin needs the agent's own privileges;
  the logs path additionally writes `/var/log/journal/remote`. Already solved on the
  container path, but `startsim` must not regress it.
- Shipping a prebuilt binary avoids the toolchain but adds a release process this
  project does not have - which is why option 1B was rejected.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

Nothing about the current entry path is broken; it is simply developer-shaped. Four
commands, an undeclared python3 dependency, an unchecked Rust dependency, and a
stale MSRV that makes the documented toolchain fail. An SE on a fresh machine hits
whichever of those breaks first, with no guidance. The fix is one entry point that
checks what it needs, builds without a toolchain, and starts the UI.

Evidence reviewed: every citation under Facts and Spike findings above, plus a
working end-to-end build spike whose output was executed on this host.

Affected contracts and surfaces:

- New: `startsim.sh` at the repository root, `docker/builder.Dockerfile`.
- `target/release/{infra-sim,infra-sim-console}` - now produced by two routes
  (cargo, or the Docker builder). Everything downstream consumes the same paths, so
  `scripts/sim-docker.sh`, `scripts/install-local.sh` and the console are unchanged.
- Docs: `README.md` getting-started, `docs/QUICKSTART.md`, `docs/operating.md`.
- Operators: a new supported entry point; the existing cargo path keeps working.
- No Rust source changes, no schema changes, no protocol changes.

Existing patterns to reuse:

- The `run()` transparency wrapper with colour output, already in
  `scripts/install-local.sh` and `scripts/sim-docker.sh` - same shape, same
  stderr discipline.
- `require_docker()` (`sim-docker.sh:61-64`) as the model for a check that names the
  remedy.
- `inherit_owner()` (`crates/sim-console/src/provision.rs`) as the precedent for
  giving generated files the owner of the directory they land in, rather than root.
- `docker/Dockerfile`'s pinned-tag-with-a-reason comment style.

Risk and blast radius:

- **Low for existing users**: additive script plus a new Dockerfile. The cargo path
  is untouched, and the artifact paths do not move.
- **Medium for the curl path**: `curl | sudo sh` executes remote code as root. It is
  the user's explicit choice (decision 3C); the script must therefore be readable,
  refuse to install anything, and clone to a predictable location.
- **Build reproducibility**: pinning the builder image matters. An unpinned
  `rust:alpine` would change compiler under operators silently.
- Security: no credentials handled. The LLM key path is unchanged and stays in a
  gitignored `.env`.
- No data loss, no migration: the script creates nothing an operator owns except a
  clone directory and build artifacts.

Sensitive data handling plan:

- `startsim` touches no tokens, keys or customer data. It must not echo the
  environment, and must not print a `.env` if one exists.
- Evidence in this SOW is paths, versions, timings and linkage - nothing sensitive.

Implementation plan:

1. `docker/builder.Dockerfile` - multi-stage: `rust:<pinned>-alpine` + `musl-dev`,
   build both release binaries, then a `scratch` stage holding just the two
   artifacts so they can be copied out without a bind mount.
2. `startsim.sh` - preflight checks (docker binary, docker daemon, python3, root,
   git only when cloning) that name remedies and install nothing; locate or clone
   the repo; build via the builder and `docker cp` the binaries into
   `target/release/`, chowned to the checkout's owner; exec the console and print
   the URL. Flags for `--bind`, `--rebuild`, `--repo`, `--no-build`.
3. Docs - README getting-started leads with `startsim`, keeps the cargo path as the
   developer route; QUICKSTART and operating.md updated to match.
4. Validation per the plan below.

Validation plan:

- Build through the builder with the host toolchain unused; confirm `static-pie
  linked` and that the binaries run and lint a real fleet.
- Confirm `scripts/sim-docker.sh build` then `create` succeed against the produced
  binary, i.e. an actual simulation comes up from builder output.
- Negative tests: docker absent (PATH-shadowed), python3 absent, non-root - each
  must stop with a remedy and change nothing.
- Ownership check after a root build.
- Idempotence: a second run with binaries present must not rebuild unless asked.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- Same-failure scan for other undeclared host dependencies in the scripts.

Artifact impact plan:

- AGENTS.md: likely update - "Project-specific commands" documents the cargo build
  as the way in; it should mention the toolchain-free route.
- Runtime project skills: no update expected; `project-live-validation` covers
  validating simulations, not packaging. Confirm at close.
- Specs: `.agents/sow/specs/` describes generator/runtime behaviour, not packaging.
  Expect no update; record the reason at close.
- End-user/operator docs: README, QUICKSTART, operating.md - all expected to change.
- End-user/operator skills: none expected.
- SOW lifecycle: this SOW moves to `current/` as `in-progress`, then `done/` as
  `completed` in one commit with the work.

Open-source reference evidence:

- `netdata/netdata @ 91c8b3741f09` was read for the plugin protocol in earlier SOWs;
  nothing in this SOW depends on agent internals. The netdata *image* is consumed as
  a published artifact (`netdata/netdata:latest`), not as source.

Open decisions:

- Decision 1: **resolved, option A** - build in a Docker builder stage.
- Decision 2: **resolved, option A** - check and stop with exact remediation;
  install nothing.
- Decision 3: **resolved, option C** - both `./startsim.sh` and
  `curl … | sudo sh`, documented both ways.
- Decision 4 (correlated logs): **resolved by investigation, not by the user.** Moot
  on the container path - `sim-docker.sh:213-221` already starts logs and OTLP as
  part of `create`, so there is nothing for `startsim` to include or exclude.
- Decision 5 (was SOW-0004 decision 3, a real `--llm` run against a live provider):
  **carried, not resolved.** Out of scope for `startsim`, which must work without a
  key. Tracked at close rather than silently dropped.
- Decision 6: **OPEN, non-blocking.** `Cargo.toml` declares `rust-version = "1.85"`
  while the lockfile requires rustc 1.88. Implementation pins the builder to a
  working toolchain either way, so this does not block; but the declared MSRV is
  false and would mislead anyone building with cargo. Put to the user at close
  rather than changed unilaterally.

## Implications And Decisions

**Decision 1 - how does the binary get built?** User chose **A** (2026-08-14).

- A. Docker builder stage. Only Docker needed, no release process, adds ~1-2 min to
  a cold first run. *long-term-best, and it removes a dependency using one already
  required.*
- B. Prebuilt binaries from GitHub Releases. Rejected: invents CI, a platform
  matrix and a hosting story this repo has never had.
- C. Keep requiring Rust and just script the four steps. Rejected: does not solve
  the stated problem.

**Decision 2 - install dependencies, or check and stop?** User chose **A**.

- A. Check and stop with exact remediation. *surgical; predictable on someone
  else's machine.*
- B. Install docker too. Rejected: invasive, OS-specific, and untestable here - no
  CI exists to exercise those branches on any distro.

**Decision 3 - delivery shape?** User chose **C**: both `./startsim.sh` in a clone
and `curl -fsSL … | sudo sh`, same script, documented both ways.

**Naming.** The user renamed the deliverable from `kickstart` to **`startsim`**.

**Decision 4 - correlated logs.** Not put to the user in the end: investigation
showed it is already automatic on the container path, so there was nothing to
decide.

## Plan

1. `docker/builder.Dockerfile` (chunk 1).
2. `startsim.sh` with preflight, locate-or-clone, build, launch (chunk 2).
3. Docs: README, QUICKSTART, operating.md, AGENTS.md if warranted (chunk 3).
4. Validation, including a real simulation created from builder output (chunk 4).

## Execution Log

### 2026-08-08

- Opened as a follow-up from `SOW-0001`.

### 2026-08-14

- Reopened on user request. Decisions 1A, 2A, 3C recorded; deliverable renamed
  `startsim`.
- Confirmed no overlap with pending SOWs 0007 (integration-sync skill) and 0013
  (console manages containers). `SOW-0016` closed first, to honour one-SOW-at-a-time.
- Build spike: static musl build in Docker succeeded (1m11s cold, `static-pie
  linked`), binaries executed on the host and linted a real fleet. Found the
  declared MSRV to be false (1.85 declared, 1.88 required by the lockfile).
- Gate filled and moved to `current/`.
- Implemented `docker/builder.Dockerfile` and `startsim.sh`; docs updated in
  `README.md`, `docs/QUICKSTART.md`, `docs/operating.md`, `AGENTS.md`.
- Two defects found by testing and fixed: `docker create` refuses a `FROM scratch`
  image without a command; the non-root hint lost the operator's flags.
- Scope corrected on user instruction: `startsim` no longer chooses a simulation for
  the console. It reports what exists and forwards `--environment`.

## Validation

Acceptance criteria evidence:

- **One command, no Rust toolchain.** `sudo ./startsim.sh --rebuild` built both
  binaries in Docker and started the console. `file` on the output:
  `ELF 64-bit LSB pie executable, x86-64, static-pie linked`. The host's cargo was
  not invoked.
- **Both delivery shapes.** In-repo: `sudo ./startsim.sh` located the checkout from
  `BASH_SOURCE` and started the UI. Piped: the script fed to `bash -s` from `/tmp`
  with `INFRA_SIM_SRC`/`INFRA_SIM_REPO_URL` overridden took the clone branch,
  cloned, and then correctly refused `--no-build` because the fresh clone had no
  binaries - the branch is exercised, not assumed. Documented as `| sudo bash`, not
  `| sudo sh`: piping ignores the shebang and the script uses `BASH_SOURCE`.
- **Dependency checks stop and guide, install nothing.** Three negative tests:
  non-root (error names `sudo` and repeats the operator's own flags); docker daemon
  unreachable via `DOCKER_HOST=unix:///nonexistent.sock` (error names
  `systemctl start docker` and the docker group); python3 absent via a minimal PATH
  of symlinks (error names the package, falling back to the generic hint when no
  package manager is on PATH). Each ended with "preflight failed - nothing was
  installed or changed."
- **Artifact is compatible with existing tooling.** `sim-docker.sh build` produced
  `infra-sim:latest` from the static binary, then `sim-docker.sh create startsimtest
  environments/web-stack.yaml` brought up a real fleet: 6 hosts
  (`startsimtest-parent`, `sim-lb-01`, `sim-web-01`, `sim-web-02`, `sim-db-01`,
  `sim-cache-01`) with 10/10 `system.cpu` points flowing. This is the portability
  claim proven where it matters - a musl-static binary running inside the
  Debian-based netdata image. Torn down afterwards; only the operator's
  pre-existing `infra-sim-default` remains.
- **No root-owned artifacts.** After a root build, `ls -l target/release` shows
  `shyam shyam` on both binaries.
- **Idempotence.** A second run without `--rebuild` reports "binaries already
  present" and starts immediately.

Tests or equivalent validation:

- `cargo test`: 217 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`:
  clean. `cargo fmt --check`: clean. No Rust source changed, so this is a regression
  check only.
- `bash -n startsim.sh` clean. `shellcheck` is not installed on this machine and was
  **not** run - recorded as a gap rather than implied.

Real-use evidence:

- The console came up on a non-default port, adopted the running container and
  served its 14 nodes; with `--environment` forwarded it served all 6 scenarios with
  durations. Both measured through `/api/status`.

Reviewer findings:

- No external reviewer; not requested. Two defects were found by testing rather than
  by reading, both fixed: `docker create` refuses a `FROM scratch` image with no
  command ("no command specified"), fixed with a `CMD` that is never executed rather
  than by requiring BuildKit's `docker build -o`; and the non-root hint printed no
  flags because `preflight "$@"` ran after the argument loop had shifted them away.
- Two of my own negative tests were invalid before they were valid: `sudo` resets
  PATH via `secure_path`, so the first "docker missing" and "python3 missing" runs
  silently tested nothing. Recorded because a passing negative test that does not
  exercise its branch is worse than no test.

Same-failure scan:

- Searched the scripts for other undeclared host dependencies. `python3` was the
  only one missing from any preflight and is now checked. `git` is checked, but only
  on the branch that needs it (the clone path). `docker` was already checked by
  `sim-docker.sh` and is now checked earlier, before anything else runs.

Sensitive data gate:

- `startsim` handles no tokens or keys, does not echo the environment, and never
  reads `.env`. Evidence recorded here is paths, versions, timings and linkage.

Artifact maintenance gate:

- AGENTS.md: **updated** - the project-commands block now names `startsim` as the
  operator entry point beside the cargo build.
- Runtime project skills: **no update needed.** `project-live-validation` covers
  validating simulations against a live agent; nothing about how the binaries were
  produced changes that loop, and the validation commands it documents are unchanged.
- Specs: **no update needed.** `.agents/sow/specs/` describes what the generator and
  runtime do; packaging changes neither. No spec statement became false.
- End-user/operator docs: **updated** - `README.md` getting-started now leads with
  `startsim` and states the real MSRV; `docs/QUICKSTART.md` step 1 replaced;
  `docs/operating.md` console section documents `startsim`, its flags, and which
  simulation the console drives.
- End-user/operator skills: none affected - no output/reference skill documents the
  build or entry path.
- SOW lifecycle: `completed`, moved to `.agents/sow/done/`, committed with the work.

Specs update:

- None needed, reason above.

Project skills update:

- None needed, reason above.

End-user/operator docs update:

- Done, above.

End-user/operator skills update:

- None affected, reason above.

Lessons:

- **Use the dependency you already require to delete the one you do not.** Docker
  was already mandatory because a simulation is a container; building in it removed
  the Rust toolchain from an operator's machine without inventing a release process.
- **Static linking is the cheap answer to a two-target problem.** The plugin runs
  inside the simulation container and the console on the host, at different glibc
  versions (2.41 and 2.43 here, older elsewhere). musl made the question disappear
  instead of being managed.
- **A negative test that cannot fail is not a test.** `sudo`'s `secure_path` quietly
  neutralised two dependency checks; both "passed" while exercising nothing.
- **Declared metadata drifts from reality.** `Cargo.toml` claims
  `rust-version = "1.85"` while the lockfile needs 1.88 - found only because the
  first builder image was pinned to the declared version and failed.

Follow-up mapping:

1. **`Cargo.toml` declares a false MSRV** (1.85 declared, 1.88 required by
   `tonic 0.14.6`). **Tracked, not fixed**: the builder pins a working toolchain so
   nothing here depends on it, and the README now states the real floor, but the
   manifest is still wrong. Raised with the user as decision 6; a one-line change
   once they say which value they want.
2. **The console's scenario list ignores the adopted container** - it comes from
   `--environment`'s parent, which defaults to the host install, so a containerised
   simulation shows 0 scenarios in the Run tab (measured: 0 by default, 6 when
   pointed at the simulation's own directory). **Tracked** against the existing
   pending `SOW-0013` (console manages containerised simulations), which owns this
   surface. `startsim` reports the flag instead of working around it, because
   choosing a fleet is the operator's decision.
3. **`shellcheck` was not run** on `startsim.sh` (not installed here). **Tracked**:
   worth running before the script is advertised as a `curl | bash` one-liner.
4. **A real `--llm` run against a live provider** (SOW-0004's original decision 3)
   remains unverified. **Carried, not resolved**: out of scope for `startsim`, which
   must work without a key. Still open for whoever hands this to an SE.

## Outcome

Delivered. `sudo ./startsim.sh`, or `curl -fsSL <raw>/startsim.sh | sudo bash` on a
bare machine, brings up the console on a host that has only Docker: preflight names
what is missing and installs nothing, both binaries are built statically inside a
container, and the UI starts. Validated end to end by creating and tearing down a
real 6-host simulation from the produced binary.

One scope correction during implementation, on the user's instruction: an earlier
revision pointed the console at an existing simulation automatically. The user's
scope for `startsim` is install dependencies, build, start the UI - creating,
claiming and tearing down fleets are decisions made in the console. It now reports
what it found and forwards `--environment`, choosing nothing.

## Lessons Extracted

See Validation -> Lessons.

## Followup

Four items, all mapped above: the false MSRV in `Cargo.toml` (decision 6, with the
user), the console's container scenario list (`SOW-0013`), an unrun `shellcheck`, and
`--llm` against a live provider.

## Regression Log

None yet.
