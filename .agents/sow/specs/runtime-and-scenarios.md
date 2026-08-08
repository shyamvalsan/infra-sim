# Runtime, vnodes, scenarios and logs

How a simulated fleet reaches Netdata, and how faults are injected. Describes
current reality as of `SOW-0001`.

## The plugins.d path

`infra-sim` is a Netdata external plugin (`/etc/netdata/custom-plugins.d/`).
The agent rescans for new plugins every 60s; no restart is needed.

Per node it emits `HOST_DEFINE` / `HOST_LABEL` / `HOST_DEFINE_END`, then
`CHART` / `DIMENSION` / `CLABEL` / `CLABEL_COMMIT` declarations, then `BEGIN` /
`SET` / `END` per tick.

**Verified against a live agent, not assumed:** a vnode's dashboard is exactly
what the plugin emits — the agent adds only its own ML charts (8 per node).
This changed the P0 estimate by roughly an order of magnitude and is why this
project runs probes before designing.

### The GUID is the identity

Changing a node's `guid` orphans its history. Changing its `hostname` renames
it in place, preserving history, trained ML models and alert log. The entire
re-skin workflow rests on that distinction, and `reskin.rs` refuses to emit an
environment where any GUID changed.

Two environments sharing a GUID cannot both be claimed — the second claim takes
over the first node's identity — so `check_guid_uniqueness` refuses to write a
new environment carrying GUIDs already used by a sibling file.

### Chart labels are load-bearing

Netdata health templates filter on `chart labels:`. Without `CLABEL` /
`CLABEL_COMMIT`, stock templates silently never attach — 41 alarms looked
perfectly healthy while `disk_space_usage` was simply not evaluated. Emitting
labels took it to 51.

### Removing a plugin file does not stop the plugin

A removed collector kept running for over an hour, writing to the same vnode
GUIDs as its replacement and corrupting values with interleaved writes.
**Teardown must kill the process**, not just delete the file. The install
script kills previous PIDs matched on exact path.

## Scenarios

A scenario (`scenarios/*.yaml`) is a timeline of effects over generator
signals, plus a **ground-truth manifest**.

- Effects: `step`, `ramp`, `drift`, `oscillate`, `recover`, `add`, `add_ramp`.
- `add`/`add_ramp` exist because a multiplier cannot lift a zero-baseline
  signal — drop rates need an absolute term.
- Targets select by `signal` plus optional `hostname`, `role`, `instance`.
  Targeting by **role** keeps a scenario correct after a re-skin.
- Effects compound across scenarios; `recover` scales the accumulated fault
  back toward neutral rather than adding another multiplier.
- `requires_roles` means an environment lacking those roles is not offered the
  scenario, rather than offered a button that cannot work.
- `hostname_suffix` pins a fault to one node of a role. Role alone is right for
  a fault that really is fleet-wide, and wrong for a physical one: a dirty optic
  is in one switch, not all of them. A suffix rather than a hostname because the
  prefix is the prospect's name and changes on every re-skin, while `-sw-01`
  does not. Targeting by role alone put the "dirty optic" on every switch at
  once, contradicting a manifest that claims one port of one switch - and the
  manifest is what Netdata AI is scored against.

The manifest (root cause, causal chain, blast radius, expected finding) is
authored with the scenario and never reconstructed from what the product did —
that is the point of scoring against it.

### Control channel

`control.yaml` beside the environment lists active scenarios and their
`started_at`. The plugin polls it (one `stat` in the common case).

`started_at` **must** be written explicitly. Without it the plugin assigns
"now" on first read, so a plugin restart rewinds a running scenario to its
opening state — indistinguishable on screen from the fault resolving itself
mid-sentence.

### Scenario targets are checked, not trusted

`check_scenarios()` verifies every scenario's signal, hostname, role and
instance resolve against the environment. A step naming something absent
produces no effect at all: the trigger appears to work and nothing happens,
in front of a prospect, with nothing in any log.

### Warm-up incidents

`warmup_incidents: true` in an environment runs minor, auto-resolving faults on
a deterministic schedule (one per 6h slot, 20 minutes, jittered within the
slot), so the alert log has texture before a demo — `spec.md` §3.

- Which incident is active at `t` is a pure function of `(seed, t)`. No state,
  no timer, no writes to `control.yaml` (the console owns that file), and a
  restart resumes mid-incident.
- Each incident keeps only the scenario steps beginning in the first half of the
  window, so it reaches a first-order symptom and stops short of the crisis the
  same scenario produces when triggered deliberately.
- A deliberately triggered scenario suppresses warm-up entirely: an SE mid-demo
  must not have warm-up noise layered on top.

## Correlated logs

A **separate process** (`infra-sim --logs`), not part of the metrics plugin.

```text
infra-sim --logs
  -> Journal Export Format
  -> systemd-journal-remote --output=/var/log/journal/remote/remote-<host>.journal
  -> Netdata systemd-journal.plugin (reads root-owned files via cap_dac_read_search)
  -> logs UI, one source per node
```

- **The journal-remote hop is not optional.** journald refuses trusted fields
  (`_HOSTNAME`) from a local client, so anything written to the local journal
  attributes to the demo machine. `systemd-journal-remote` accepts them because
  ingesting entries formed elsewhere is its purpose.
- Netdata derives the logs source name from the **filename**
  (`remote-<host>.journal` → source `<host>`).
- `--split-mode=host` is rejected for stdin sources, so per-node files mean one
  `systemd-journal-remote` child per node. A fleet in the hundreds should share
  one file and filter on the `_HOSTNAME` facet instead.
- Children exit on EOF when the parent's pipes close, however the parent dies —
  so no signal handling is needed and nothing outlives its parent.

**Fault rules match on signals, not scenario names.** A rule fires when
`ScenarioSet::perturbation` reports a signal driven past a threshold — the same
question the metrics engine asks. Nothing in the log generator knows `disk-fill`
exists, so a future scenario moving the same signal gets matching logs for free.

**No access logs.** A node reporting 1,200 req/s while its logs show three lines
a second is the contradiction an SRE notices. Real deployments send access logs
to a file and only errors to journald; this emits what journald would hold. A
healthy node logs nothing above `notice`.

Requires `systemd-journal-remote` installed and root.

## Simulated Prometheus exporters

Optional, and a **separate process** (`infra-sim --exporters`).

```text
infra-sim --exporters
  -> GET http://127.0.0.1:19998/metrics/<hostname>   (Prometheus text format)
  -> Netdata's own go.d prometheus collector
  -> charts auto-generated on the matching virtual node
```

The point is that Netdata charts an exporter it has never seen. Nothing about
the resulting charts is authored here.

- **One listener, one path per node.** A port per node would mean 50 listeners
  for a 50-node fleet, and the go.d job config carries the node identity anyway.
- **`vnode: <hostname>` in the job attributes the scrape to the fleet's own
  virtual node**, so a node carries both its plugins.d charts and its scraped
  ones. `netdata/netdata @ c23face0bd94`
  `src/go/plugin/framework/confgroup/config.go:23`.
- **go.d reads its vnode registry once, at startup**
  (`src/go/plugin/agent/setup.go:179`). A job referencing a vnode declared
  afterwards attributes nowhere, with no error, so the console restarts
  `go.d.plugin` - the daemon respawns it, and no netdatacli command exists for
  this.
- **The metrics are application-level only** (`specs/prometheus-app.yaml`):
  orders, carts, queues, worker pools. Emitting CPU here would put the same
  series on a node twice, once from plugins.d and once from the scrape.
- **Counters integrate scrape by scrape.** `rate * uptime` is not monotonic,
  because the rate has a daily cycle - it falls every evening and go.d reads the
  drop as a counter reset.
- **A summary must publish `_sum` and `_count`.** go.d skips one that does not
  (`src/go/plugin/go.d/collector/prometheus/writer_schema.go:124`), so quantiles
  alone produce no latency chart and no error anywhere.
- **No `instance` label.** Prometheus adds that at scrape time from the target
  address; an exporter does not publish it.
- Scenario aware: the exporter reads the same `control.yaml` as the metrics
  plugin, so a fault moves application metrics on the same timeline.

The console writes `/etc/netdata/go.d/prometheus.conf` and
`/etc/netdata/vnodes/infra-sim.conf`. Both carry a marker line and are never
overwritten without it; teardown removes only files carrying that marker.

## Teardown

One button, and it has to leave nothing behind - the next prospect's demo runs
on the same machine.

1. Disarm scenarios, so nothing is mid-fault when the fleet stops.
2. Remove the plugin **and** stop its process. Either alone is insufficient: the
   agent rescans every 60s and relaunches a file it still finds, and a running
   collector keeps writing from a deleted file.
3. Stop `infra-sim --logs` and delete **this fleet's** journal files. Only files
   named for the environment's own hostnames - `systemd-journal-remote` output
   from anything else on the machine lives in the same directory. Left behind,
   Netdata keeps offering a log source per node for a fleet that no longer
   exists, and an SE opening logs mid-demo finds the last prospect's hostnames.
4. Stop the exporters and remove the config they added to Netdata, matching on
   the marker line so an operator's own `prometheus.conf` is never touched.
5. Archive the environment, seed and scenario manifests.
6. Remove the install directory - **only if the archive succeeded**, because
   that is the copy being removed.

Processes are matched on executable path plus argument from `/proc`, never on a
process name. Cloud-side removal stays manual and says so.

## Proven end to end

With `disk-fill` running on a live agent:

- Netdata's **own ML** raised `ml_1min_node_ar` at a 1.02% node anomaly rate
  ~18 minutes before the threshold alert could fire, and ranked the fleet in
  the manifest's blast-radius order (db 1.02% > web 0.76% > cache 0.43%).
- The **real health engine** raised `disk_space_usage` WARNING at 93.5% on
  exactly the mount the manifest names as root cause, and stayed CLEAR on the
  two other mounts of the same node.
- The **logs** showed that node's Postgres reporting
  `No space left on device ... on /var/lib/pgsql`, with the nine other nodes
  and the two untargeted mounts silent.

Nothing was faked at any step: the scenario moved generator inputs and the real
product did the rest.
