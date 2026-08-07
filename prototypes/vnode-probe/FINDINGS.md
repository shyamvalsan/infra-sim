# Vnode probe — verification results

Answers the load-bearing engineering questions in `spec.md` **Open questions**
before architecture is committed. Everything below was executed against a live
agent, not reasoned about.

- **Agent:** netdata `v2.10.0-1022-nightly`, standalone, `localhost:19999`
- **Probe:** `infra-sim-probe.plugin` — plain Python, no Go, no go.d, installed
  to `/etc/netdata/custom-plugins.d/` and picked up by the normal
  `check for new plugins every = 1m` scan
- **Source tree consulted:** `/home/shyam/netdata/netdata`

---

## Q2 — Does plugins.d expose vnode assignment to external plugins? **YES**

Resolved, and it removes a fallback from the spec.

**Source evidence**
- `src/plugins.d/gperf-hashtable.h:139-161` — `HOST`, `HOST_DEFINE`,
  `HOST_DEFINE_END`, `HOST_LABEL` are all registered with the
  `PARSER_INIT_PLUGINSD` flag. That flag *is* the external-plugin parser.
  These are not go.d-internal.
- `src/plugins.d/pluginsd_parser.c:147-215` — `HOST_DEFINE` validates the GUID
  as a UUID and `HOST_DEFINE_END` calls `rrdhost_find_or_create(...)` with
  `NETDATA_VIRTUAL_HOST`.
- `src/plugins.d/README.md:191-260` — the protocol is publicly documented.

**Empirical evidence** — three vnodes registered from the Python probe:

```
laptop       | reachable | 4d8b0595
sim-web-01   | reachable | 9c6b1e42
sim-db-01    | reachable | 3d81f570
sim-cache-01 | reachable | b57e2a09
```

Per-vnode data queries return correctly scoped data:
`/host/sim-db-01/api/v1/data?chart=system.cpu` →
`user=31.97, system=13.32, iowait=4.26, idle=46.71`.

**Consequence for the spec:** the standalone-repo / external-plugin
architecture (§Repository) is viable as written. The stated **fallback**
("contribute a thin go.d module upstream") is **not needed** and can be struck.

---

## Q1 — Do vnodes render complete node-level dashboard sections? **NO — not for free**

Resolved, and this is the **largest correction to P0 sizing in the spec**.

A vnode carries *exactly* the charts its plugin emits, plus ML charts Netdata
adds on its own. There is no automatic "System Overview". Dashboard menus and
sections are constructed **from the contexts present on the node**.

`/host/sim-db-01/api/v1/charts` → 14 charts total:

```
system.cpu  system.io  system.load  system.net  system.processes  system.ram   <- ours (6)
anomaly_detection.anomaly_detection / anomaly_rate / context_anomaly_rate
anomaly_detection.dimensions / ml_running
netdata.machine_learning_status / metric_types / training_status              <- Netdata's (8)
```

Measured against the same agent's **real** Linux node:

| | contexts | chart instances |
|---|---|---|
| localhost, all | 445 | 3506 |
| localhost, OS baseline only (`system/mem/disk/net/ip/ipv4/ipv6/cpu/...`) | **188** | **312** |
| a probe vnode | 6 | 6 |

**Consequence for the spec:** Goal 2 ("an experienced SRE zooming into
individual charts finds no disqualifying artifacts") means the *Linux system
baseline generator alone* is on the order of **~190 contexts / ~310 chart
instances**, not one spec file. The P0 line "~20 hero collector generators"
undercounts by roughly an order of magnitude on its first item. An SRE opening
a simulated node and finding 6 charts where they expect 300 is itself the
disqualifying artifact.

The spec's **scale target is unaffected and internally consistent**: 50–200
vnodes × ~310 charts ≈ 15k–60k charts, matching the spec's own "≈10k–60k
metrics on one agent" estimate.

---

## The hard rule holds — **PROVEN, not assumed**

"Synthetic world, live product" verified end-to-end. Netdata's real ML engine
trains on the simulated nodes with no special casing:

```
sim-web-01  anomaly_detection.ml_running = 1
sim-db-01   netdata.training_status      = untrained 12.6, trained 10.4
```

Netdata created those `anomaly_detection.*` charts per vnode by itself. Nothing
downstream of the plugin's stdout was mocked.

---

## Q3 — Journal attribution for multi-node logs

Mechanically resolved. **The spec's framing needs correcting.**

Netdata's logs pipeline does **not** scope journals to Netdata nodes. It
classifies journal *files* into sources:

- `src/collectors/systemd-journal.plugin/systemd-journal-files.c:391-421` —
  a path containing `/remote/` yields source `remote-<host>` (reverse-resolving
  an IP to a hostname where it can).
- `:432-433` — a `*.<namespace>` directory yields source `namespace-<ns>`.
- `:881-893` — scanned roots are `/run/log/journal` and `/var/log/journal`.
- `src/collectors/systemd-journal.plugin/systemd-journal.c:166` — `_HOSTNAME`
  is registered as a **facet** (filterable field), not a node binding.

**Both candidate mechanisms in the spec work**, and per-host journal files are
the better fit — writing `/var/log/journal/remote/remote-<hostname>.journal`
produces one selectable source per simulated node.

**But:** logs will be reachable via the logs UI's source/`_HOSTNAME` facets,
**not** nested inside each vnode's node view. The P0 requirement "Correlated
logs for hero services, attributed to correct vnodes in logs UI" is
satisfiable only in the facet sense. This should be reworded before it becomes
an acceptance test someone fails.

---

## ML warm-up math — confirmed, but the spec quotes **obsolete config keys**

The 72h conclusion is sound. The runbook wording is not.

`src/ml/ml_config.cc:81-84` explicitly *moves* the keys the spec names into
`obsolete ...` entries. Live v2.10 `[ml]` section uses new names:

| spec (§3 History) | v2.10 actual | value |
|---|---|---|
| `maximum num samples to train = 21600` | `training window` | `6h` |
| `minimum num samples to train = 900` | `min training window` | `15m` |
| `train every = 3h` | `train every` | `3h` (unchanged) |
| `number of models per dimension = 18` | same | `18` (unchanged) |
| — | `delete models older than` | `7d` |

18 models × 3h = **54h ≈ 2.25 days** to a full ensemble, so **≥72h warm-up
holds with margin**. Only the key names must change, or a warm-up runbook will
silently no-op on v2.10+.

---

## Fidelity artifact found by the probe itself

`sim-cache-01` reported `system.ram free = 0`:
`used_frac 0.80 + cached 0.15 + buffers 0.03 + diurnal 0.05 > 1.0`, so `free`
clamped to zero. A real host does not sit at exactly zero free memory.

This is a **conservation-invariant violation** — `free + used + cached +
buffers == total` must hold by construction, not by clamping. It is precisely
the artifact class §8 "Semantic lints" must catch, and it took ~4 minutes of
runtime to surface. Good evidence the harness earns its place.

---

## Still unresolved

- **Q4 — Cloud API coverage for teardown.** Not tested; needs Cloud API
  credentials and docs. Determines how much of §6.5 teardown is automated vs.
  checklist-manual.
- **Q5 — vnode scaling ceiling (target 200).** Not tested. Requires loading a
  live agent with ~200 vnodes × ~310 charts; not run unattended on the user's
  working laptop.

---

## Removing the probe

```bash
sudo rm /etc/netdata/custom-plugins.d/infra-sim-probe.plugin
sudo systemctl restart netdata     # or wait for the plugin scan to drop it
```

The three vnodes will go offline but persist in the local agent's DB, since a
vnode GUID is a durable identity (spec §2, "Move, don't clone").
