---
name: project-live-validation
description: How to validate Infra-Sim changes against a live Netdata agent. Load before changing a generator spec, the engine, a scenario, the plugins.d runtime, or the logs writer - and before claiming any fidelity result. Covers the probe-first rule, what the lint cannot see, and the teardown trap.
---

# Live validation

"It compiles" is not validation for this project, and neither is a green
`cargo test`. Fidelity claims need output from a running agent.

## Probe before you design

This project sits on undocumented-by-default agent behaviour. Twice now, an
assumption that looked settled by source-reading was wrong or incomplete in a
way that changed the design:

- **vnode dashboard completeness** — resolved only by running a throwaway
  plugin; the answer moved the P0 estimate by roughly an order of magnitude.
- **journald trusted fields** — `_HOSTNAME` cannot be set by a local client, so
  correlated logs needed a `systemd-journal-remote` hop. Reading
  `systemd-cat-native --help` said so; a two-entry probe proved the whole chain
  before any generator code existed.

When a change rests on how the agent behaves, write the smallest possible probe
and run it first. Source-reading is necessary and not sufficient. Cite agent
source as `netdata/netdata @ <commit>` with repository-relative paths.

## The loop

```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check

# offline fidelity, no agent needed
./target/release/infra-sim --environment environments/<env>.yaml --lint 72

# install; the agent rescans every 60s, no restart
./scripts/install-local.sh

curl -s localhost:19999/api/v3/nodes | grep sim-
curl -s "localhost:19999/host/<hostname>/api/v1/data?chart=system.cpu&after=-60"
```

Cap local runs at **5 vnodes**. Larger fleets run on a separate machine.

## What the lint cannot see

- **It does not run scenarios.** An environment can pass the lint cleanly and
  still saturate under a hero scenario. `--describe` shipped mounts sized so
  `disk-fill` clamped them at 100%; the lint was green throughout. After
  changing anything that sizes a mount or bounds a signal, trigger the scenario
  that targets it and watch the value.
- **It only asks whether a signal is pinned, not whether its bound is
  physically meaningful.** A disk utilisation of 101.5% passed it. The semantic
  checks in `sim-engine/src/fidelity.rs` exist for that class; extend them
  rather than relying on someone noticing.
- **An alert can catch what the lint cannot.** That 101.5% surfaced through the
  health engine, not a chart.

## Verify through the product, not just the file

A file on disk proves nothing about what an operator sees.

```bash
# scenario actually moved the metric
curl -s "localhost:19999/host/<host>/api/v1/data?chart=disk_space./var/lib/pgsql&after=-120"

# alerts really attached (missing chart labels = templates silently skip)
curl -s "localhost:19999/api/v1/alarms?all" | grep -c alarm

# logs: the systemd-journal function needs a __logs_sources selection;
# 'all-remote-systems' is the simulated fleet
sudo journalctl --file=/var/log/journal/remote/remote-<host>.journal -o short-iso
```

Check the **negative** cases too — they are what makes a demo credible. When a
fault fires, confirm untargeted nodes, mounts and interfaces stayed quiet, and
that a node without a service never emitted that service's logs.

## Teardown kills processes

Removing a plugin file does **not** stop the running plugin. A deleted collector
once ran for over an hour writing to the same vnode GUIDs as its replacement,
corrupting values with interleaved writes, and the symptom looked like a
conservation bug in new code.

Always identify and kill the specific PID you started. Never `pkill`/`killall`
on a name — the user runs other work, and other simulations, on this machine.

```bash
ps -eo pid,args --no-headers | grep "infra-sim --logs" | grep -v grep
sudo kill <that-pid>
```

`comm` truncates at 15 characters, so match on `args` when checking for
`systemd-journal-remote` children.

## Identity rules that bite

- The **GUID is the identity**. Changing it orphans history; changing the
  hostname renames in place. Never regenerate GUIDs to "clean up".
- Two environments sharing a GUID cannot both be claimed.
- Claim tokens, room IDs and LLM API keys are credentials: env vars or console
  input only, never a file, never argv.
