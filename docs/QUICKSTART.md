# Quickstart

Zero to a running simulated fleet with a live incident. Assumes a Netdata agent
on the same machine and a Rust toolchain.

## 1. Build and check

```bash
cargo build --release

# Simulate 72 hours offline and fail on fidelity violations. No agent needed.
./target/release/infra-sim --environment environments/web-stack.yaml --lint 72
```

If the lint fails, stop. It is catching something an SRE would notice on a
chart.

## 2. Install into the local agent

```bash
./scripts/install-local.sh          # runs the lint first and refuses if it fails
```

The agent rescans for plugins every 60s — no restart needed.

```bash
curl -s localhost:19999/api/v3/nodes | grep sim-
```

You should see five simulated nodes.

## 3. Start the demo warming up

**Do this at least 72 hours before the demo**, and ideally the moment you know
it is happening.

Netdata's ML trains on what it sees: anomaly detection starts contributing
after roughly 15 minutes, and is fully credible at about 72 hours. A fleet
started an hour before a call has no anomaly history, no alert log and no
texture — which is exactly the thing that makes a demo feel real.

Nothing else is needed to warm up. Leave it running.

## 4. Trigger an incident

```bash
./scripts/scenario.sh list
./scripts/scenario.sh trigger disk-fill
./scripts/scenario.sh status
./scripts/scenario.sh resolve disk-fill
```

`disk-fill` is demo-paced at about 30 minutes. Netdata's own ML typically flags
the affected node well before the threshold alert fires, which is the point
worth showing.

## 5. Correlated logs (optional)

```bash
sudo apt-get install systemd-journal-remote     # one-time
./scripts/logs.sh start
```

Each node becomes its own log source, and fault lines follow whatever scenario
is running. Stop with `./scripts/logs.sh stop`.

## 6. Console — or do all of the above from one screen

```bash
sudo ./target/release/infra-sim-console
```

To have a real model read the free-form description, put the key in a
gitignored `.env` beside the repo — `sudo` strips the environment, so this is
the reliable place for it:

```bash
echo 'LLM_API_KEY=...' >> .env      # Netdata's own gateway, llm.netdata.cloud
```

`ANTHROPIC_API_KEY` and `OPENAI_API_KEY` work the same way. The console offers
only the providers whose key it can actually find.

Then open <http://127.0.0.1:8080>.

Steps 1-4 above are the command line. The **Build** tab does the same work in
about a minute:

1. Type what the prospect runs — "6 nginx web servers behind two haproxy load
   balancers, a 3-node postgres cluster and an elasticsearch cluster of 3" —
   and press **Read this**. It fills the fleet below.
2. Adjust: change counts, swap roles, search **261 integrations** by name and
   tick what they actually run. The six with a `DEEP` badge are the ones hero
   scenarios can target.
3. Optionally tick **Simulate Prometheus exporters** — each node then publishes
   a real `/metrics` endpoint that Netdata's own prometheus collector scrapes
   and charts.
4. Name the prospect and press **Create & install**. It builds the environment,
   lints it, **refuses to install if the lint fails**, and swaps the running
   fleet.

The same tab holds **Connect to Netdata Cloud** (claim token plus an optional
room id — the token is never written anywhere and is cleared from the page once
used), **Re-skin**, and **Tear down**.

The **Run** tab is the demo surface: preflight verdict, scenario triggers with
escalate/rewind, and the live node table.

Root is required for create, claim and teardown.

---

## Building an environment for a specific prospect

```bash
./target/release/infra-sim \
  --describe "3 web servers behind an nginx load balancer, a postgres primary and 2 redis caches" \
  --name acme --environment environments/acme.yaml
```

Add `--llm netdata` (with `LLM_API_KEY` in `.env`) when the description is in
the prospect's vocabulary rather than ours — it resolves software the keyword
reader cannot, and says plainly what it could not model. `--llm anthropic` and
`--llm openai` work the same way. Always lint the result before trusting it.

Not every model can do this: the plan contract needs a strict `json_schema`
response format. On `llm.netdata.cloud`, `k3` honours it (and is the default);
`glm-5.2-max` and `deepseek-v4-flash` do not. Override with `--llm-model`.

## Retargeting a warm fleet

Do **not** regenerate an environment for a new prospect — that orphans 72 hours
of ML training. Re-skin it instead:

```bash
./target/release/infra-sim --reskin --from-prefix sim- --to-prefix acme- \
  --new-name acme --environment environments/web-stack.yaml \
  --output environments/acme.yaml
```

GUIDs are preserved, so the fleet keeps its history, trained models and alert
log. Only one environment carrying a given set of GUIDs may be claimed at a
time.

## Teardown

```bash
./scripts/logs.sh stop
sudo rm -rf /etc/netdata/custom-plugins.d/infra-sim.plugin /etc/netdata/infra-sim
sudo systemctl restart netdata
sudo rm -f /var/log/journal/remote/remote-*.journal
```

Removing the plugin file does **not** stop a running plugin — the install
script kills the previous process for you, but if you remove things by hand,
kill the process too.

## Things that will bite you

- **A green lint is not a working demo.** The lint does not run scenarios.
  After changing anything that sizes a mount or bounds a signal, trigger the
  scenario that targets it and watch the value.
- **The GUID is the identity.** Changing a node's `guid` orphans its history;
  changing its `hostname` renames it in place.
- **Warm up early.** This is the single most common way a first demo
  disappoints.
