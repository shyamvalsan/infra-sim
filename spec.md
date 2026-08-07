# Infra-Sim — Synthetic Infrastructure for Netdata

**Spec v1.1** · **Status:** Draft for review · **Owner:** Shyam Sreevalsan
**Name note:** "Infra-Sim" — chosen for immediate legibility: it simulates infrastructure. Naming the simulated *object* in the name also removes the "simulates Netdata" misreading. (A long-archived EMC project called InfraSIM existed for bare-metal/BMC simulation; no active collision.)

---

## Problem statement

Generic demos don't sell, and it is impractical (cost, licenses, trial periods, ops effort) to run a real instance of every service Netdata integrates with. SEs cannot show prospects an environment that looks like the prospect's own infrastructure, and Netdata AI cannot be showcased or evaluated on demand because interesting incidents don't happen on cue. Infra-Sim generates realistic simulated infrastructures — nodes, metrics, logs, and injected problem scenarios — that the real Netdata pipeline (ML, health, Netdata AI) processes live.

## The one hard rule

**Synthetic world, live product.** Only the raw data is simulated. Everything downstream is the real product: ML actually trains and detects, the health engine actually raises alerts, Netdata AI actually investigates. Nothing downstream of data injection is ever scripted or mocked. All simulated environments are visibly labeled as simulations.

## Goals

1. An SE can produce an expert-grade simulated environment matching a prospect's stack in ≤ 4 hours of hands-on effort (overnight warm-up allowed), running in a VM on their laptop, claimed to Netdata Cloud.
2. Simulated environments withstand expert scrutiny: an experienced SRE zooming into individual charts and cross-checking correlations finds no disqualifying artifacts.
3. Incidents can be triggered on cue during a live demo, and Netdata's ML/alerts/AI respond genuinely within the demo window.
4. Public demo spaces run on the demo parent using the same engine, clearly labeled as simulated.
5. The scenario format supports the AI-eval gym from day one (seeded, reproducible, ground-truth manifests) even though the gym harness itself ships later.
6. The full lifecycle — create, claim, warm up, demo, tear down — is driven from one control console; the safe path is the easy path.

## Non-goals

- **Mid-call environment generation (< 2 min).** Prep budget is hours; hot pools and on-demand pre-warmed snapshots are out of scope for v1.
- **Ingesting customer documents** (RFIs, discovery notes) into the builder. v1 input is a structured picker/config. Avoids confidentiality questions entirely.
- **Traces.** Future phase, same principles.
- **Simulating the product itself.** No mocked dashboards, no canned AI responses, no fake alert states — ever.
- **Per-datapoint LLM generation.** AI authors generators offline; runtime is cheap deterministic code with no inference in the data path.
- **Replacing real POCs.** Infra-Sim sells the meeting; the POC on customer infra still closes the deal.

## Users and priority

1. **P0 — Sales engineers:** prospect-shaped demos.
2. **P1 — Public demo spaces:** always-on labeled simulated environments on the demo parent.
3. **P2 — Netdata AI team:** the eval gym.

---

## Repository, packaging, licensing

- **Standalone repo:** start at `shyamvalsan/infra-sim`, transfer to `netdata/infra-sim` when ready for OSS. GitHub repo transfers preserve stars, issues, and watchers and create permanent redirects, so the early home costs nothing.
- **Why standalone, not in netdata/netdata:** the runtime is an **external plugin speaking the plugins.d protocol** (the same mechanism go.d uses), installable alongside any stock agent — no monorepo PR, no agent release train, no fork. And most of the repo is not Go: it is generator specs, log grammars, scenario definitions, and environment templates (YAML), which a far wider contributor pool can PR against. CI runs the fidelity harness, not agent builds. This is the shape of repo that thrives as OSS.
- **Fallback:** if external-plugin vnode support proves gnarly, contribute a thin go.d module upstream and keep everything else (the actual product: content + console + harness) in the standalone repo.
- **Open-core line:** engine, generator library, hero scenarios, fidelity harness, console → public repo. Vertical scenario packs and advanced builder features → private companion repo.
- **License:** GPL-3.0, matching the agent. Nothing technically forces it (separate process), but house consistency avoids an unnecessary conversation.
- **Disclosure posture:** README leads with the hard rule. Simulated environments are labeled everywhere (host labels, Space naming). Framing is confident, not apologetic: "a living model of your infrastructure — everything the product does with it is live."

---

## Architecture

### 1. Generator library (AI-authored, deterministically executed)

- Each collector gets a **generator spec**: a declarative file describing, per context/dimension — metric type (counter/gauge), base levels, daily + weekly seasonality, noise model, instance/cardinality model, label templates, and cross-metric invariants (e.g., network bytes ≈ packets × plausible packet size; disk busy time consistent with IOPS; DB connections bounded by max_connections).
- Generator specs are **authored by LLM batch jobs** reading collector metadata (contexts, dimensions, units, families) and iterated against the fidelity harness until they pass.
- The **runtime** (external plugin, plugins.d protocol, vnode-capable) executes generator specs and emits metrics through the normal collection path.
- **Record-and-replay** (the original #14051) is retained as: (a) a high-fidelity mode for specific collectors, and (b) the source of ground-truth corpora for the fidelity harness.

### 2. Node simulation via virtual nodes

Netdata **virtual nodes** already do most of the work: collectors can report metrics as if from separate logical nodes; vnodes are defined with hostname, GUID, and labels, assigned per collection job, and appear in Netdata Cloud as independent nodes with their own dashboards and alert states.

- v1 fleet = **one agent + N vnodes** inside the SE's laptop VM. No streaming-protocol impersonation needed.
- Identity semantics (verified in docs): the vnode **GUID is the identity**; changing it creates a new node and orphans history. **Changing only the hostname renames the node while preserving identity and history.** Labels are freely editable.
- **Re-skin workflow:** keep warm base environments running; for a new prospect, re-skin hostnames + labels + naming schemes to match their conventions. The environment keeps trained ML models, retention, and alert history. Per-prospect customization becomes a ≤ 30 min re-skin instead of a cold start.
- **Move, don't clone:** GUIDs must be unique across the infrastructure — only one instance of a given base may be claimed/active at a time. The base is re-skinned and re-claimed to the prospect Space, then re-claimed back to staging and neutralized. Two same-day demos from one base require either two warm bases per vertical, or a clone with regenerated GUIDs and a fresh 72h warm-up. The console enforces this.
- Scale target: 50–200 vnodes per laptop VM (≈ 10k–60k metrics on one agent — well within agent capacity).

### 3. History (warm-up, not backfill)

- **ML warm-up math:** defaults are `maximum num samples to train = 21600` (6h), `minimum num samples to train = 900` (15 min), `train every = 3h`, `number of models per dimension = 18`. First models exist within ~15 minutes; the full 18-model ensemble (which suppresses false positives) spans roughly the last **~2.5 days**.
- **Replication is not a backfill mechanism:** it is append-only (fills missing samples only at the *end* of each series), tier0 only, capped by `[db].replication period` (default 1 day) and sender retention, and optimized for short durations.
- **Decision:** v1 history = **start early**. Runbook: spin up ≥ 72h before the demo; the environment config includes scheduled **warm-up incidents** (2–3 minor, auto-resolving) so the alert log and anomaly history have texture before the live session. Replication-based deep backfill is a P2 research item only.

### 4. Correlated logs (v1)

- Per-service **log grammars** (AI-authored, like generators): realistic formats for the hero services, emitted into systemd-journal so Netdata's logs pipeline picks them up natively.
- Logs are **keyed to generator state**: baseline chatter at baseline, error/warn patterns when a scenario effect is active on that service. When the simulated Postgres develops replication lag, its logs say so.
- Requirement: logs must attribute to the correct simulated node in the logs UI. Candidate mechanisms: journal namespaces or journal-remote-style per-host journal files. Implementation choice is an engineering task.
- v1 scope: hero services + the five hero scenarios. Full long-tail log coverage is P1+.

### 5. Scenario engine

- **Declarative scenarios:** a timeline of effects applied to generator parameters (ramp, step, drift, oscillate) targeting nodes/services, with propagation along the environment's dependency graph (LB → app → DB).
- **Controllable clock:** scenarios can be scheduled (warm-up), compressed, or **triggered live** mid-demo.
- **Seeded and reproducible:** every scenario run takes a seed; identical seed + config ⇒ identical data. Required for the gym and for demo rehearsal — and for exact replay of a past demo.
- **Ground-truth manifest** per scenario: root-cause component, onset timestamp, causal chain, expected blast radius. The gym scores **time-to-detect** and **root-cause accuracy** against this manifest.
- **v1 scenario library — five cross-vertical classics:**
  1. Slow disk fill (days-long ramp → threshold alert → predictable crisis)
  2. Memory leak → OOM kill cascade
  3. DB replication lag under load
  4. Noisy neighbor (one workload starving co-located services)
  5. Flapping network links on edge nodes (robotics/IoT flavor)

### 6. Control console (new in v1.1 — named P0 component)

A small web UI served by the Infra-Sim container. The SE lives here from kick-off to teardown; it is the product's face and the enforcement point for safe workflows.

**Lifecycle:**

1. **Create.** `docker compose up` brings up agent + runtime + console. In the console: pick template → node count and scale → naming scheme (hostname prefix, domains, DC names, label conventions) → arm a scenario pack → launch. Output: `environment.yaml` + seed. Every environment is reproducible bit-for-bit from these two artifacts.
2. **Claim.** One claim covers the whole fleet: the console takes a claim token + room ID (pasted from the Space's Connect Nodes screen, or via env vars); the single agent claims, and all vnodes appear as nodes in Cloud. Convention enforced by the console: **fresh Space per prospect, named `<Prospect> (Simulated Demo)`**, `simulated=true` host labels on everything.
3. **Warm-up → preflight.** Countdown plus a **green-board checklist** the console verifies before declaring demo-ready: all vnodes online in Cloud, ML fully trained (≥ 72h), warm-up incidents visible in the alert log, scenarios armed and seeds recorded, naming scheme applied. No demo starts off a red board.
4. **Demo.** Second-screen controls: **trigger / escalate / resolve** per scenario, clock controls, live status of injected effects. Resolve matters — showing recovery is as persuasive as showing failure.
5. **Teardown.** Guided flow: disarm scenarios → stop container → remove now-offline nodes from Cloud → delete the Space → **archive** `environment.yaml` + seed + scenario manifests, so "show that to my boss next week" replays the identical world. Console tracks which teardown steps are automated vs. checklist-manual (see open questions re: Cloud API coverage).
6. **Leave-behind mode (P1).** Instead of teardown: invite the prospect into the Space for a week of self-serve exploration. A laptop can't host this, so it is **promote-to-hosted** — migrate the container to the demo parent or a small team server. The demo stops being a meeting and becomes a persistent asset.

**Console also enforces:** move-don't-clone (re-claim flows, one-active-instance-per-base), labeling conventions, and archive-on-teardown.

### 7. Environment builder

- **v1:** environment templates (per vertical: web stack, k8s microservices, robotics edge fleet, …) + the console's picker. Output: `environment.yaml`.
- **P1:** richer builder in the console; LLM-assisted compose ("describe the stack" → environment.yaml) as an internal tool.
- **No customer-document ingestion** (non-goal).

### 8. Fidelity harness (what makes "expert grade" scalable)

Expert grade is a pipeline property, not a promise:
1. **Recording corpus:** record real data from services we can run (the 2022 replay recorder, repurposed as QA infrastructure).
2. **Statistical checks vs corpus:** distribution similarity, autocorrelation/seasonality structure, cross-correlation matrices, counter monotonicity, realistic cardinality, no loop seams.
3. **Semantic lints:** units/ranges sane, invariants hold, rates and their integrals consistent.
4. **Blind review:** two internal SREs review hero-collector dashboards; any disqualifying artifact fails the generator back to the LLM authoring loop.

Hero collectors must pass all four before v1. Long-tail collectors (P1) must pass 2–3 automatically before shipping.

---

## Requirements

### P0 (v1 cannot ship without)

- [ ] Generator spec format defined; runtime executes specs as an external plugin with vnode assignment
- [ ] ~20 hero collector generators at expert grade: Linux system (CPU/mem/disk/net), cgroups/containers, Docker, k8s (kubelet + kube-state), nginx, HAProxy, Postgres, MySQL, Redis, Kafka, RabbitMQ, Elasticsearch, MongoDB, systemd services, web-log, plus 4–5 chosen per first templates
- [ ] Fidelity harness operational; all hero generators pass
- [ ] Vnode fleet: 50+ simulated nodes on one laptop-VM agent, claimed to Cloud, each with own dashboards/alert states
- [ ] Environment templates (≥ 3 verticals) consumable by the console picker
- [ ] **Control console:** create flow (picker → environment.yaml + seed), claim flow (token + room → whole fleet), preflight green-board, demo controls (trigger/escalate/resolve + clock), guided teardown with archive
- [ ] Warm-up runbook wired into preflight: 72h early start, scheduled warm-up incidents, verified trained-ML + alert-history state
- [ ] Re-skin workflow: hostname/label re-skin of a warm base in ≤ 30 min, history preserved (GUIDs unchanged), move-don't-clone enforced by console
- [ ] Correlated logs for hero services, attributed to correct vnodes in logs UI
- [ ] Scenario engine: 5 hero scenarios, live trigger, seeds, ground-truth manifests
- [ ] Simulation labeling enforced by console: `simulated=true` host labels + `<Prospect> (Simulated Demo)` Space naming
- [ ] Archive-and-replay: environment.yaml + seed + manifests reproduce an identical world
- [ ] Packaged deliverable: container/VM image + SE quickstart doc

### P1 (fast follows)

- [ ] Long-tail generator batch job across all collectors, gated by automated fidelity checks
- [ ] Richer builder in console
- [ ] **Leave-behind mode / promote-to-hosted** (demo parent or team server)
- [ ] Public demo environment on the demo parent (always-on, labeled)
- [ ] Multi-base management (2+ warm bases per vertical for same-day demos)
- [ ] Log coverage beyond hero services; scenario library → 15+
- [ ] LLM-assisted environment compose (internal)

### P2 (design for, don't build yet)

- [ ] Gym harness: batch scenario runs scoring **time-to-detect** and **root-cause accuracy** for Netdata AI; regression tracking across releases
- [ ] Replication-based deep backfill research (append-only/tier0/period constraints noted)
- [ ] Streaming-protocol child impersonation for very large fleets (1000+ nodes, parent load testing)
- [ ] Traces

---

## Success metrics

- **Prep time:** new-prospect environment ≤ 4h hands-on (excl. warm-up); re-skin of warm base ≤ 30 min; teardown ≤ 15 min
- **Adoption:** ≥ 50% of SE demos use an Infra-Sim environment within one quarter of release
- **Fidelity:** 100% hero collectors pass harness + blind review; zero "is this fake?" incidents attributable to data artifacts in demos
- **Demo efficacy (lagging):** SE-reported impact on meeting-to-POC conversion
- **Gym (once live):** TTD and root-cause accuracy per scenario, tracked per Netdata AI release

## Open questions

- **[eng]** Do vnodes render complete node-level dashboard sections (system overview etc.) when all charts come from one external plugin? Verify first — this is the load-bearing assumption.
- **[eng]** Does the plugins.d protocol expose vnode assignment to external plugins, or is it go.d-internal? Determines standalone-plugin vs thin-upstream-module path.
- **[eng]** Journal attribution mechanism for multi-node logs: namespaces vs per-host journal files. Prototype both, pick one.
- **[eng]** Cloud API coverage for teardown automation: can node removal and Space deletion be automated, or do they remain checklist-manual steps in the console?
- **[eng]** Vnode-per-agent scaling ceiling on a laptop-class VM (target 200).
- **[product]** Final hero-collector list and first three vertical templates (propose: web stack, k8s microservices, robotics edge — aligned with ABM Wave 1).
- **[product]** Confirm name (infra-sim proposed) before repo creation.
- **[design]** Console stack: keep minimal (single-binary embedded UI preferred) — confirm.

## Phasing

1. **Phase 1 (P0):** engine + hero generators + vnode fleet + templates + console (create/claim/preflight/demo/teardown) + warm-up/re-skin + logs + 5 scenarios + labeling + packaged image. Target: first prospect demo.
2. **Phase 2 (P1):** long-tail generators, richer builder, leave-behind/promote-to-hosted, public demo environment, multi-base ops, expanded scenarios/logs.
3. **Phase 3 (P2):** gym harness, backfill research, streaming impersonation at scale, traces.

