# SOW-0018 - macOS host support

## Status

Status: open

Sub-state: Container mode delivered and working as far as it can be verified on
Linux. One identified blocker remains, needing a Rust change and a user decision.

## Requirements

### Purpose

Let an operator run Infra-Sim on a Mac. "Many folks will be running this on their
macbook/mac mini" - the tool is Linux-only today, and refuses on Darwin.

### User Request

"Come on do macos support, we need it... many folks will be running this on their
macbook/mac mini etc", after a report from a real attempt on macOS: several minutes
of Docker build ending in `exec format error`.

### Assistant Understanding

Facts:

- The binaries are built in an Alpine container (`docker/builder.Dockerfile`) and
  are therefore static Linux ELF. `startsim.sh` exec'd one on the host, which Darwin
  cannot do.
- **No Rust change was needed to package the console for a container.** It already
  shells out to `bash scripts/sim-docker.sh` for build and create
  (`crates/sim-console/src/container.rs:65-95`) and already honours
  `INFRA_SIM_STATE_DIR` (`container.rs:217`).
- Delivered this session and verified by forcing container mode on Linux with a
  shimmed `uname`: the console image builds, the container starts, and the UI serves
  on the published port.
- **Blocker.** `crates/sim-console/src/main.rs:757` resolves an adopted simulation's
  agent as `Agent::new("127.0.0.1", a.port)`. From inside the console container,
  `127.0.0.1` is the container itself, so a running simulation's dashboard - published
  on the *host's* loopback - is unreachable. Measured: 0 nodes, 0 scenarios, where
  the same console on the host sees 160 and 6.
- `--network host` is not a fix on Docker Desktop, where host networking is limited
  and not equivalent to Linux.
- Portability defects found and fixed on the way, all of which also affected Linux
  or would have on a Mac:
  - `startsim.sh` read `INFRA_SIM_STATE` while the console and `sim-docker.sh` read
    `INFRA_SIM_STATE_DIR` - overriding it did nothing. A live bug on Linux.
  - `stat -c` is GNU-only; BSD needs `stat -f`.
  - `sed -i` takes no argument on GNU and requires one on BSD (three sites in
    `scripts/sim-docker.sh`).
  - `target/release` cannot be trusted as the source of a container binary: a later
    `cargo build --release` replaces the static musl build with a glibc one, which
    cannot run on Alpine, and on macOS it would hold a Mach-O binary. The console
    image now copies from the builder image instead. Observed as
    `exec ... no such file or directory`, not theorised.

Inferences:

- Simulation containers themselves need nothing: they are Linux containers either
  way, and `systemd-journal-remote` runs inside them.
- The state directory default must move on macOS - `/var/lib` is neither shared with
  Docker Desktop by default nor where a Mac keeps user state. `$HOME/.infra-sim` is
  used, and `$HOME` is shared by default.
- Identical-path mounting of the repo and state directory is load-bearing:
  `sim-docker.sh` hands `-v <path>:...` to the daemon, which resolves it against the
  host.

Unknowns:

- Everything Docker-Desktop-specific is unverified: the socket path, default file
  sharing, and whether `host.docker.internal` resolves as expected. There is no Mac
  in this environment.

### Acceptance Criteria

- On a Mac with Docker Desktop, `./startsim.sh` brings up the UI, creates a fleet,
  claims it, shows its nodes and scenarios, and tears it down.
- No Rust toolchain required on the Mac.
- Linux behaviour unchanged.
- Verified by an operator on an actual Mac; this cannot be self-certified.

## Analysis

Sources checked: `startsim.sh`, `docker/builder.Dockerfile`,
`docker/console.Dockerfile`, `scripts/sim-docker.sh`,
`crates/sim-console/src/{main.rs,container.rs}`, and a forced container-mode run on
Linux.

Risks:

- Shipping something that half-works is worse than refusing, if it is not labelled.
  Container mode therefore prints an explicit experimental warning naming the gap.
- `host.docker.internal` exists on Docker Desktop but not on plain Linux Docker,
  so any fix must not regress the Linux path.

## Pre-Implementation Gate

Status: needs-user-decision

Problem / root-cause model:

The console assumes it shares a network namespace with the simulations it watches.
On Linux that holds. Containerised, it does not: the simulation's dashboard is
published on the host's loopback, which the console container cannot reach at
`127.0.0.1`.

Evidence reviewed: the citations above, plus a measured 0-nodes/0-scenarios result
against a console that reports 160/6 when run on the host.

Affected contracts and surfaces:

- `crates/sim-console/src/main.rs:757` and `container.rs` - how an adopted
  simulation's agent host is resolved.
- `startsim.sh` container mode - would pass the host alias and `--add-host`.
- Docs: README, QUICKSTART, operating.md.

Existing patterns to reuse:

- `--agent HOST:PORT` already exists as a console argument, and `INFRA_SIM_STATE_DIR`
  already exists as an environment override. The fix should extend that convention
  rather than invent a new one.

Risk and blast radius:

- Low for Linux if the host alias is opt-in and defaults to today's `127.0.0.1`.
- The change is small; the risk is that it cannot be verified here.

Sensitive data handling plan:

- No credentials involved. Claim tokens continue to reach containers by environment
  variable, never argv.

Implementation plan:

1. Give the console an override for the host it reaches adopted simulations on -
   an environment variable read where `Agent::new("127.0.0.1", ...)` is built,
   defaulting to today's behaviour.
2. In container mode, pass `--add-host=host.docker.internal:host-gateway` and set
   that override to `host.docker.internal`.
3. Docs: macOS as supported, with its own requirements.
4. Have an operator verify on a Mac, including create, claim, watch and teardown.

Validation plan:

- Forced container mode on Linux must show the adopted simulation's nodes, proving
  the override works before a Mac is involved.
- Linux host mode must be byte-identical in behaviour.
- An operator run on a Mac. This SOW cannot close without it.

Artifact impact plan:

- AGENTS.md: add the platform expectation.
- Specs: `.agents/sow/specs/` has no platform section; may need one.
- Docs: README, QUICKSTART, operating.md.

Open decisions:

**Decision 1 - how the console reaches an adopted simulation when containerised.**

- **A. An environment override** (`INFRA_SIM_AGENT_HOST`), defaulting to
  `127.0.0.1`, set to `host.docker.internal` in container mode. Smallest change,
  Linux untouched, uses the convention already there. *Recommended, long-term-best.*
- B. Detect containerisation in the console and switch automatically. No
  configuration, but magic, and wrong the moment someone runs the container on
  Linux with `--network host`.
- C. Publish simulation ports on `0.0.0.0` and reach them by the host's LAN address.
  Works, and exposes every simulation dashboard to the network - a downgrade in
  posture for a demo tool that currently binds loopback only.

**Decision 2 - does teardown of a simulation created on a Mac need anything extra?**
Unknown until tested; raised so it is not discovered by an operator mid-demo.

## Plan

1. Decision 1, then the override and container-mode wiring.
2. Docs.
3. Operator verification on a Mac.

## Execution Log

### 2026-08-14

- Opened after macOS support was requested. Container mode, the console image, the
  macOS state directory default, and four portability fixes delivered and pushed.
- Verified as far as Linux allows: UI serves from the container; adopted-simulation
  visibility is the remaining blocker, with the cause identified at
  `main.rs:757`.

## Validation

Pending decision 1.

## Outcome

Pending.

## Lessons Extracted

Pending.

## Followup

None yet.

## Regression Log

None yet.
