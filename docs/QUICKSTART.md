# Quickstart

Zero to a running simulated fleet with a live incident. `README.md` is what
Infra-Sim is and why; `docs/operating.md` is the full reference; this is the
shortest path that works.

Needs Docker and root. A Netdata agent on this machine is optional — each
simulation runs its own. No Rust toolchain: `startsim` builds in a container.

## 1. Start

```bash
sudo ./startsim.sh
```

or, on a machine with nothing checked out:

```bash
curl -fsSL https://raw.githubusercontent.com/shyamvalsan/infra-sim/main/startsim.sh | sudo bash
```

Open <http://127.0.0.1:8080>. The first run takes a couple of minutes to compile;
after that it starts in seconds. Missing dependencies stop it with the fix, and it
installs nothing.

`startsim` stops there: creating a fleet, claiming it and tearing it down are
yours to do in the console. If simulations already exist it lists them and prints
the flag to drive one, because the Run tab's scenarios come from `--environment`:

```bash
sudo ./startsim.sh --environment /var/lib/infra-sim/<name>/environment.yaml
```

To have a model read a free-form description, put a key in a gitignored `.env`
beside the repo — `sudo` strips the environment, so this is the reliable place:

```bash
echo 'LLM_API_KEY=...' >> .env      # Netdata's own gateway, llm.netdata.cloud
```

`ANTHROPIC_API_KEY` and `OPENAI_API_KEY` work the same way; the console offers
only providers whose key it can find. Without one, build the fleet from the
picker instead.

## 2. Build the fleet

On the **Build** tab:

1. Type what the fleet runs — "6 nginx web servers behind two haproxy load
   balancers, a 3-node postgres cluster, 4 Cisco Catalyst access switches" — and
   press **Build the fleet**. It fills the table below.
2. Adjust. Change counts, swap roles, search 260 integrations and tick what they
   actually run. The six badged `DEEP` are the ones hero scenarios target by
   name.
3. A `network-device` row picks a **vendor and model** — 105 of them, generated
   from Netdata's own SNMP device profiles, so a Catalyst reports what a Catalyst
   reports.
4. Optionally set a **fleet latitude and longitude**, and override it per group
   for a fleet spanning sites.
5. Name it and press **Create simulation**.

The name fixes the seed and every node identity, so it cannot change later
without orphaning the fleet's history. A fidelity check runs first and refuses to
install a fleet that would give itself away. The first create also builds the
container image, which takes a minute or two.

Each simulation runs in its own container with its own fresh, unclaimed agent —
your own agent is never touched, and correlated logs and OpenTelemetry start with
it.

## 3. Start it warming up

**Do this at least 72 hours before the demo**, and ideally the moment you know it
is happening.

Netdata's ML trains on what it sees: anomaly detection starts contributing after
roughly 15 minutes and is fully credible at about 72 hours. A fleet started an
hour before a call has no anomaly history, no alert log and no texture — which is
exactly what makes a demo feel real. This is the single most common way a first
demo disappoints.

There is no way around it. The plugin protocol cannot backfill history, so a
fleet always starts at zero and accumulates.

## 4. Claim it into Netdata Cloud

Paste a token from *Cloud → Connect Nodes* into the create form, with a room id
if you want one. The token is never written to disk, logged, or passed on a
command line, and it is cleared from the page once used.

The agent is fresh and unclaimed, so it joins whatever Space you name. Use a
Space per simulation and delete it afterwards; a reused Space accumulates dead
nodes from fleets that no longer exist.

Cloud will not put virtual nodes in a specific room from the agent side, so use a
**room membership rule** on `infra_sim_name = <your simulation>` — the whole
fleet joins, including nodes added later.

## 5. Run the demo

The **Run** tab is the demo surface: preflight verdict, scenario triggers with
escalate and rewind, and the live node table.

Trigger a scenario and let it develop. Resolving one unwinds it over three
minutes rather than snapping back, so the recovery looks like a recovery.

Each scenario declares the roles it needs (`requires_roles` in
`scenarios/*.yaml`), because a fault nobody can see is worse than no fault:

| Scenario | Needs |
|---|---|
| `disk-fill` | `db` + `web` |
| `db-replication-lag` | `db` + `web` |
| `memory-leak-oom` | `web` |
| `noisy-neighbour` | `cache` + `web` |
| `flapping-edge-links` | `lb` |
| `switch-uplink-degrading` | `network-device` |

A fleet missing a required role does not offer that scenario at all. Individual
steps that target a tier the fleet does not have are skipped, so the rest of the
timeline still runs — `switch-uplink-degrading` degrades the uplink on a
switch-only fleet and simply does not slow a web tier that is not there.

## 6. Tear it down

One button on the Build tab, or:

```bash
sudo ./scripts/sim-docker.sh teardown <name>
```

The container carries the agent, its database and every simulated node, so
removing it removes all of them — nothing is left stale. The environment and
scenario manifests are archived first, so the fleet can be replayed.

The Cloud Space or room is yours to delete; the teardown says so rather than
pretending it did it.

---

## Retargeting a warm fleet

Reshaping a fleet for a different audience? Do **not** regenerate the
environment — that orphans days of ML training. Re-skin it instead, from the Build
tab or:

```bash
./target/release/infra-sim --reskin --from-prefix sim- --to-prefix acme- \
  --new-name acme --environment environments/web-stack.yaml \
  --output environments/acme.yaml
```

GUIDs are preserved, so the fleet keeps its history, trained models and alert
log. Only one environment carrying a given set of GUIDs may be claimed at a time.

## Building an environment from the command line

```bash
./target/release/infra-sim --llm netdata \
  --describe "3 web servers behind an nginx load balancer, a postgres primary and 2 redis caches" \
  --name acme --environment environments/acme.yaml

./target/release/infra-sim --environment environments/acme.yaml --lint 2
sudo ./scripts/sim-docker.sh create acme environments/acme.yaml
```

Without `--llm` an offline keyword parser reads the description; it resolves any
integration named in the text but understands less phrasing. Always lint the
result before trusting it.

Not every model can do this: the plan contract needs a strict `json_schema`
response format. Override with `--llm-model`.

## Things that will bite you

- **Warm up early.** Nothing else on this list costs a demo as often.
- **A green lint is not a working demo.** The lint does not run scenarios. After
  changing anything that sizes a mount or bounds a signal, trigger the scenario
  that targets it and watch the value.
- **The GUID is the identity.** Changing a node's `guid` orphans its history;
  changing its `hostname` renames it in place.
- **Name it once.** The simulation's name fixes the seed and every GUID. Re-skin
  to rename; regenerating starts the history over.
- **Traces are write-only for now.** The application tier emits OpenTelemetry
  spans and the nightly agent stores them, but no Netdata build can display them
  yet. Do not plan a demo beat on traces.
