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

# offline fidelity, no agent needed. 2h is what create runs; the semantic checks
# always cover a fixed 2h window, so a bigger number only adds warm-up before
# them. Nodes are linted one per core (`sim-engine/src/parallel.rs`).
./target/release/infra-sim --environment environments/<env>.yaml --lint 2

# install; the agent rescans every 60s, no restart
./scripts/install-local.sh

curl -s localhost:19999/api/v3/nodes | grep sim-
curl -s "localhost:19999/host/<hostname>/api/v1/data?chart=system.cpu&after=-60"
```

Cap local runs at **5 vnodes**. Larger fleets run on a separate machine.

## What the lint cannot see

- **Scenario lint is bounded and model-only.** The lint now evaluates applicable
  scenarios independently, including recovery, and checks physical invariants.
  It cannot prove live collector semantics, exporter/OTLP delivery, or what an
  operator sees. After changing a mount size or signal bound, also trigger the
  relevant scenario against a live agent and query the affected chart.
- **It only asks whether a signal is pinned, not whether its bound is
  physically meaningful.** A disk utilisation of 101.5% passed it. The semantic
  checks in `sim-engine/src/fidelity.rs` exist for that class; extend them
  rather than relying on someone noticing.
- **It cannot see a flat zero,** by design — a healthy host really does report
  zero OOM kills. So the worst rounding failures are invisible to it: an app
  group at `weight: 0.01` emitted `0` processes and `0%` CPU while reporting
  memory, and every check passed. Read the emitted protocol when you change
  anything that scales a value:
  `timeout 8 ./target/release/infra-sim --environment <env> 1 | grep -A 3 '^BEGIN <chart>$'`.
- **Whether a defect trips it is a seed lottery.** The stuck `app_mem.cron` RSS
  that failed a 25-node create was present on all 25 nodes; 24 of them happened
  to cross a rounding boundary inside the window. Never conclude from one clean
  environment that a class of defect is absent - lint several, or read the values.
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

### OpenTelemetry needs its own probe

The `otel-logs` function requires Cloud SSO, so a plain `curl` against the agent
returns 412 and tells you nothing. Read the store instead, and note that the
layout differs by agent build:

```bash
# stable image: OTLP logs are journal files
docker exec infra-sim-<sim> sh -c \
  'journalctl --file=/var/log/netdata/otel/v1/*/*.journal -o json --no-pager -n 5'

# newer builds: a WAL/SFST store, with an offline inspector
sudo /usr/libexec/netdata/plugins.d/otel-plugin logs \
  --wal-dir /var/log/netdata/otel/v2/logs/wal \
  --sfst-dir /var/log/netdata/otel/v2/logs/index \
  --name <sim>-storefront --namespace <sim>

# what the emitter thinks is happening, per signal
./scripts/sim-docker.sh telemetry <sim> status
```

Two traps found the hard way:

- **Traces are build-dependent.** Simulations run `netdata/netdata:latest`
  (nightly), which accepts and stores them; `stable` refuses them outright and
  keeps OTLP logs in a different layout. Nothing can display them on any build.
  Confirm which image the container was built from before concluding a bug:
  `docker exec infra-sim-<sim> netdata -v`.
- **A shared health flag lies.** Logs and traces fail independently. Reporting
  them together showed "export failed" forever on stable while logs were landing
  perfectly — check each signal separately.

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

And anchor the pattern. An unanchored `pkill -f '$plugin --mode'` inside
`sh -c` also matches the wrapper running the pkill itself; the wrapper dies
mid-kill, `docker exec` returns 143, and `set -e` aborts the script with the
remaining processes still running. `telemetry stop` shipped broken this way
for weeks — it stopped the logs writer and silently nothing else. Anchor on
the path: `pkill -f '^$plugin --mode'`.

## Identity rules that bite

- The **GUID is the identity**. Changing it orphans history; changing the
  hostname renames in place. Never regenerate GUIDs to "clean up".
- Two environments sharing a GUID cannot both be claimed.
- Claim tokens, room IDs and LLM API keys are credentials: env vars or console
  input only, never a file, never argv.

## Exercise operational failure paths

Run `python3 -m unittest discover -s tests -p 'test_*.py'` after building the console and `node --test tests/console_ui.test.cjs` for lifecycle/UI changes. These use isolated payloads and fake Docker rather than shared services. A shell helper must capture command status in an `else` branch: `$?` inside `if ! command` is the inverted status and once made failed archive copies look successful. Validate the actual standalone telemetry command as well as create, because shell dynamic scope previously hid an unset payload variable.

The opt-in `sudo python3 tests/live_agent_smoke.py` exercises one uniquely named vnode against the built `infra-sim:latest` image, including actual CPU data and telemetry survival across container restart. Build the image from the revision being validated first. The manual hosted-runner Live agent acceptance workflow builds from its checked-out revision; a local run against an older image proves only the unchanged runtime plus current lifecycle scripts.

## Check effective service roles

Composition must preserve both base and service patches for the same role.
Counting contexts misses this: the Linux `db` role once discarded PostgreSQL's
`db` tuning while every service chart still existed. Assert effective values,
then compare dependent charts against their real capacity and display scaling.

After building the simulation image from the current portable plugin,
`sudo python3 tests/live_composition_probe.py` checks this through one disposable
Netdata vnode. It freezes noise and seasonality only in private fixture copies,
retains the shipped role values, and checks 320 TCP connections, 132 PostgreSQL
connections and 33% utilization of 400 slots. It stops/restarts and tears down
only its uniquely named container. This proves composition and display scaling,
not realistic distributions, scenario acceptance or large-fleet performance.


### Raw recording validation

Use `tests/test_recording_runtime.py` for real process shutdown, byte comparison,
exporter route ordering, and explicit incomplete-prefix replay. Build the selected
binary first; set `INFRA_SIM_TEST_BINARY` to validate a specific build.
`tests/live_agent_smoke.py` also verifies actual producer-boundary capture and a
self-contained archive after telemetry and container restarts.

A status read of an unfinalized recording must hold the append lock. Otherwise
reading file length and committed length between another producer's writes can
falsely mark a healthy recording incomplete. A finalized recording is immutable
and is read without the lock, so archives replay from read-only media.

Test recording on real disk, not only tmpfs. An fsync costs about 0.03 ms on
tmpfs and about 6 ms on ext4 here; two concurrency tests passed for weeks on tmpfs
and failed on disk. Run the tests with `TMPDIR` pointing at a disk directory, and
give `live_agent_smoke.py` a disk `TMPDIR` too: its simulation state, including
the recording, lives under the temporary directory, and a full tmpfs once
produced an honest but useless incomplete archive.

`tests/live_replay_probe.py ARCHIVE` is the end-to-end replay acceptance: it
replays a smoke archive into a disposable receiver. Raise the OTLP inspector's
`--limit` (default 50) before comparing record counts; the default silently
truncates. Keep the committed-byte checkpoint durable;
an interrupted suffix stays untouched and replay reads only the committed prefix.
Normal stops must allow producers to flush before the agent/container exits.

On the nightly build checked during SOW-0028, the `systemd-journal` HTTP function
also returned HTTP 412 without Cloud SSO. Treat that as an authentication blocker,
not a transient readiness failure. Metrics queries and `journalctl` checks remain
useful evidence but do not satisfy authenticated function-query acceptance.
