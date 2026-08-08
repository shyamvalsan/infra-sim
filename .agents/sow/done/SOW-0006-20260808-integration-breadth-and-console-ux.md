# SOW-0006 - Integration breadth, free-form input and console UX

## Status

Status: completed

Sub-state: Delivered. 261 integrations selectable, free-form text as the primary
input, simulated Prometheus exporters scraped by Netdata's own collector onto the
right virtual node, and the console rebuilt as two tabs.

## Requirements

### Purpose

The console could offer seven collectors. A prospect's estate is never seven
collectors, so every demo built with it was visibly a demo. The create form also
asked an SE to translate a conversation ("they run HAProxy in front of a Postgres
cluster") into checkboxes, which is work the tool should do.

### User Request

"ok so the sim UX needs a LOT of work to make the ux intuitive and interactive..
we need free form text to input requirements and the check boxes should have
options for ALL netdata integrations (you can find the list in netdata repo i
guess with icons etc.. ), as well as options to sim prometheus exporters etc..
claim details just needs claim ID and optinonal Room ID (no need of space
name)... also reduce amount of noise/crud in the ux.." (2026-08-08)

### Assistant Understanding

Five separate pieces of work:

1. Every Netdata integration selectable, with its own icon, not seven.
2. Free-form text as the primary way to describe an estate.
3. Simulated Prometheus exporters as an option.
4. Claim reduced to token plus optional room.
5. Less noise on screen.

### Acceptance Criteria

1. The picker offers every Netdata integration that can be simulated, with
   Netdata's own icon and a visible deep/generated distinction. MET - 261
   integrations, 6 deeply modelled.
2. Every generated spec passes the 6-hour fidelity lint. MET - 259/259.
3. Free-form text produces an editable fleet, and names software the seven-role
   defaults could not reach. MET - "two haproxy load balancers" yields HAProxy,
   not nginx.
4. A simulated Prometheus exporter is scraped by Netdata's own collector and
   charted on the correct virtual node. MET - verified on a live agent.
5. Claim asks for a token and an optional room, nothing else. MET.
6. The console fits two tabs with the preflight detail collapsed by default. MET.

## Analysis

Netdata ships a `metadata.yaml` per collector describing its
`monitored_instance` (name, icon, categories) and every context it emits with
units, chart types and dimension names. That is exactly the input a generator
spec needs, so integration breadth is a sync problem, not an authoring problem.

The gap between the two is fidelity: metadata says *what* a collector emits, not
what plausible values look like. A generated spec therefore carries the right
shape with a unit-derived value profile, and the catalogue labels it `generated`
so nobody mistakes it for the six hand-authored specs whose signals are causally
coupled and which the hero scenarios target by name.

## Pre-Implementation Gate

**Problem / root-cause model.** The create form was built when seven specs
existed and its data model (one integration set per role) encoded that
assumption. Free-form input existed only as a CLI flag. Prometheus exporters had
never been scoped.

**Evidence reviewed.**

- `netdata/netdata @ c23face0bd94`
  - `src/go/plugin/framework/confgroup/config.go:23` - job-level `vnode` key.
  - `src/go/plugin/agent/setup.go:179` - the vnode registry is loaded once, at
    plugin startup.
  - `src/go/plugin/framework/vnoderegistry/registry.go` - go.d emits
    `HOST_DEFINE` for vnodes, the same mechanism the plugins.d path uses, so a
    scrape can land on a virtual node this project already owns.
  - `src/go/plugin/go.d/collector/prometheus/writer_schema.go:114-126` - a
    summary with no `_sum` / `_count` is skipped entirely.
  - 163 collector `metadata.yaml` files as the integration source.
- `crates/sim-console/src/provision.rs`, `ui.html` - the role-keyed create form.
- `crates/sim-engine/src/describe.rs` - the keyword reader's fixed role table.

**Affected contracts and surfaces.** Console API (`/api/describe` added,
`/api/catalogue` extended, `ClaimRequest.space_name` removed), console UI,
`sim-engine` (`parse_with_services`, `NodeEngine::signal_values`, `llm` moved in
from `sim-plugin`), `sim-plugin` (`--exporters`), the generated spec corpus,
`integrations/catalogue.json`, Netdata's own `go.d/prometheus.conf` and
`vnodes/` directory, README, quickstart, specs.

**Existing patterns to reuse.** The atomic stage-and-rename from the plugin
install; `/proc`-based process matching from `stop_plugin`; the fidelity lint as
the gate on every generated spec; the control file as the single source of
scenario state.

**Risk and blast radius.** The exporter path writes into Netdata's own
configuration directory and restarts `go.d.plugin`. Both are guarded: every file
carries a marker line and is never overwritten without it, and the plugin is
matched by executable path from `/proc`. A failed exporter setup degrades to a
note rather than failing the create.

**Sensitive data handling plan.** No new credentials. Removing `space_name`
removes a field, not a secret. The generated corpus contains only Netdata's own
public collector metadata. Evidence in this SOW cites `owner/repo @ commit` and
repository-relative paths.

**Implementation plan.** Sync script and catalogue → console catalogue and
describe endpoint → UI rewrite → exporter runtime → exporter provisioning →
live validation.

**Validation plan.** Lint every generated spec at 6h. Drive describe, create,
claim and teardown through the console's own API as root against the live agent.
Confirm scraped charts land on the right vnode and carry data. Render the UI in
a browser and exercise the picker.

**Artifact impact plan.** `README.md`, `docs/QUICKSTART.md`,
`.agents/sow/specs/runtime-and-scenarios.md`,
`.agents/sow/specs/generator-and-engine.md`.

**Open decisions.** None outstanding. `space_name` removal was the user's
explicit instruction; the labelling convention it enforced is recorded below as
a consequence.

## Implications And Decisions

1. **Generated specs are labelled, not disguised.** 255 of 261 integrations come
   from Netdata's metadata: right contexts, units, chart types and dimension
   names, with a unit-derived value profile. They are not causally coupled and no
   scenario targets them. The picker says so rather than implying every
   integration is equal.
2. **Labelled scopes become one representative instance.** A real Elasticsearch
   with twelve indices shows twelve chart instances; this shows one, carrying the
   scope's chart labels so health templates still attach. Collecting instance
   lists for 259 collectors in the create form is a worse trade for an SE with
   four hours.
3. **The fleet builder is a list of groups, not a table of roles.** "6 nginx web
   servers and 3 Elasticsearch nodes" is two groups of the same role with
   different software. The old role-keyed model collapsed them into nine nodes
   running both.
4. **The exporter publishes application metrics only.** Orders, carts, queues,
   worker pools - nothing a Netdata collector already provides. Emitting CPU
   from the exporter would put the same series on a node twice, which an SRE
   reads as a broken agent.
5. **`space_name` is gone from the claim form** (user instruction). Simulated
   environments are still labelled at every other layer - `simulated=true` host
   labels, `sim-*` hostnames, the console's own SIMULATED badge, and
   `simulated="true"` on every exported Prometheus series. The Space naming
   convention from `spec.md` §6 is now a convention the SE follows, not
   something the console enforces.

## Plan

Delivered as described in the gate.

## Execution Log

### 2026-08-08

- `scripts/sync-integrations.py`: read every scope rather than only unlabelled
  ones; exact-match unit table; dedupe contexts and dimensions; namespace bare
  context ids; drop generated aliases of hand-authored specs.
- `integrations/catalogue.json`: 261 integrations with icon, category, chart
  count and `modelled`.
- `crates/sim-engine/src/describe.rs`: `parse_with_services` resolves software
  named in the text against the installable catalogue.
- `crates/sim-engine/src/llm.rs`: moved from `sim-plugin` so the console can use
  it.
- `crates/sim-console/src/provision.rs`: `/api/describe`, integrations in the
  catalogue, `exporters` module, scoped generated-spec install, `space_name`
  removed.
- `crates/sim-plugin/src/exporters.rs` + `specs/prometheus-app.yaml`: the
  exporter runtime.
- `crates/sim-console/src/ui.html`: rewritten as Build / Run tabs with a
  searchable picker.

### Findings during implementation

- **`%` was not a recognised unit.** The unit table matched on substrings, so
  `percentage` matched and `%` did not - 60 metrics fell through to a generic
  0-1,000,000 profile and ZFS pools reported 108% fragmentation. Short unit
  strings now match exactly, before any substring rule.
- **A counter computed as `rate x uptime` is not monotonic.** The rate has a
  daily cycle, so the expression falls every evening and go.d reads the drop as
  a counter reset. Counters now integrate scrape by scrape, which is also what a
  client library does. Caught by a test walking a full day; a 30-minute walk
  passed.
- **A Prometheus summary without `_sum` and `_count` is silently dropped.** go.d
  skips it (`writer_schema.go:124`), so the latency chart - the most valuable
  thing the exporter publishes - simply never appeared. No error anywhere.
- **go.d loads its vnode registry only at startup.** Writing a job that
  references a vnode declared afterwards attributes nowhere, with no error. The
  console now restarts `go.d.plugin`, which the daemon respawns; there is no
  netdatacli command for this.
- **Merging description clauses on role alone discarded software.** "6 nginx web
  servers ... and an elasticsearch cluster of 3" became nine nginx nodes.
  Clauses now merge only when role *and* software match.
- **`ETXTBSY` again, on the exporter binary.** The same stage-and-rename fix the
  plugin install needed; stopping the previous process first only narrows the
  race.
- **The exporter published an `instance` label.** In a real deployment Prometheus
  adds that at scrape time from the target address - an exporter does not emit
  it. It also put the hostname into every auto-generated chart id, which the
  vnode already carries.
- **Two PostgreSQLs in the picker.** The hand-authored `postgres` and the
  generated `postgresql` sat side by side with different chart counts and no way
  to tell which was which. The generated alias is now dropped, spec file
  included, so `--describe` cannot resolve to the shallow copy and lose every
  Postgres scenario.

## Validation

**Generated corpus.** 259/259 generated specs pass the 6-hour fidelity lint - no
semantic violations, no signals pinned to a bound. Failures found and fixed
along the way: bare context ids (3), duplicate contexts and dimensions (2), unit
misclassification (20), perfectly flat dBm signals (3).

**Free-form input** (live console API): "6 nginx web servers behind two haproxy
load balancers, a 3-node postgres cluster, 2 redis caches and an elasticsearch
cluster of 3" produced 6 web/nginx, 2 lb/haproxy, 3 db/postgres, 2 cache/redis,
3 web/elasticsearch, with nothing unrecognised.

**Prometheus exporters** (live agent `v2.10.0-1030-nightly`): a fleet created
with `exporters: true` produced `/etc/netdata/go.d/prometheus.conf` and
`/etc/netdata/vnodes/infra-sim.conf`, and Netdata's own go.d prometheus
collector scraped `127.0.0.1:19998` and auto-charted onto the **virtual node**,
not the host:

- `prometheus_infra_sim_app_<host>.app_http_requests_total-code=2xx-...` -
  201.7 requests/s, differentiated by Netdata from our cumulative counter.
- `...app_queue_depth-queue=default-...` - 42.5 items.
- `...app_request_duration_seconds-...` - `quantile_0.5` 0.055s,
  `quantile_0.95` 0.258s, `quantile_0.99` 0.883s.

**Console UI** (rendered in a browser): both tabs render, the only console error
is a missing favicon. Typing a description and pressing Read this filled five
group rows with the correct icons fetched from netdata.cloud. The picker
searched 261 integrations, filtered "sql" to 11 results, and showed the DEEP
badge on the hand-authored PostgreSQL.

**Teardown**: disarmed scenarios, removed the plugin and stopped it, stopped the
exporters and removed both files it had written to Netdata's config, archived
the environment, and reported the two Cloud-side steps as MANUAL.

**Tests**: 177 passed, 0 failed. `cargo clippy --all-targets -- -D warnings`
clean. `cargo fmt --check` clean.

**Same-failure search**: the `ETXTBSY` class was checked across every path that
writes an executable - the plugin install and the exporter install are the only
two, and both now stage-and-rename. The "status is not an acknowledgement" class
from `SOW-0005` was re-checked against the new exporter path: `enable()` reports
what it did and names the failure mode when it could not restart go.d, rather
than reporting success. The unit-misclassification class was checked across the
whole corpus by re-running the 6-hour lint on all 259 specs, not on a sample.

**Sensitive data gate**: no credentials in any committed artifact. The claim
token remains memory-only and is cleared from the DOM after use. The generated
corpus contains only Netdata's public collector metadata. Committed hostnames
remain `sim-*` or prefixed by a synthetic environment name.

**Artifact maintenance gate**:

- `AGENTS.md` - no change needed; no new project-wide guardrail. The exporter's
  writes into Netdata's own config are guarded in code and documented in the
  runtime spec.
- Runtime project skills - none exist yet; `SOW-0001` deferred creation until
  the generator-authoring loop stabilised. The sync script plus this SOW is that
  loop's first durable record; a `project-integration-sync` skill is the natural
  next artifact and is tracked in Followup.
- Specs - `runtime-and-scenarios.md` gains Prometheus exporters;
  `generator-and-engine.md` gains the generated corpus and its limits.
- End-user docs - `README.md` and `docs/QUICKSTART.md` updated.
- SOW lifecycle - this file, moving to `done/` with the commit.

## Outcome

An SE can now describe a prospect's estate in the words the prospect used, edit
the result, tick anything from 261 integrations, optionally publish simulated
Prometheus exporters, and install a linted fleet - from one screen, without
knowing this repository's role vocabulary.

The gap this closes is the one that made every demo look like a demo: seven
collectors is not an estate. 258 generated specs are structurally faithful to
what Netdata's own collectors emit, and all of them pass the same fidelity lint
the hand-authored ones do.

Against `spec.md`, the P0 checklist is unchanged except that the integration
coverage item is now met. What remains is the 50+ node scale test, deferred by
the user to a non-laptop machine, and the P2 eval gym.

## Lessons Extracted

- **Breadth is a sync problem; fidelity is not.** Netdata's metadata gave 258
  integrations in an afternoon. Every defect found afterwards was a *value*
  defect - 108% fragmentation, flat dBm, counters that ran backwards - because
  metadata says what a collector emits, never what plausible looks like. The
  lint found all of them; reading the YAML found none.
- **Substring matching on a short token is a silent misclassification.** `%`
  never matched `percentage`, so 60 metrics quietly took a generic profile. The
  failure surfaced as a plausible-looking chart with an impossible number, which
  is the worst way for it to surface.
- **An incomplete Prometheus summary produces no chart and no error.** go.d
  skips a summary with no `_sum`/`_count`. Nothing logged it. The only way to
  find it was to look for a chart that should have existed and did not - which
  is an argument for validating what *appeared*, not only that the pipeline ran.
- **A UI's data model encodes an assumption about scale.** Keying the create
  form by role was correct for seven collectors and wrong for 261: it silently
  merged two groups of the same role and threw away one's software. The
  describe endpoint returned the right answer and the form destroyed it.

## Followup

- **`project-integration-sync` skill.** The sync script now carries real,
  hard-won knowledge (exact-vs-substring unit matching, scope handling, the
  alias problem, the lint-as-gate loop). That is the first genuinely reusable
  workflow this project has produced, and it is what `SOW-0001` was waiting for
  before creating project skills. Tracked as `SOW-0007`.
- **Deep specs cover less than their generated equivalents.** The hand-authored
  Postgres emits 15 contexts where Netdata's collector emits 70. Composing the
  two would collide on context ids. Worth a decision - extend the hand-authored
  specs, or teach composition to let a deep context override a generated one.
  Tracked as `SOW-0008`.
- `SOW-0003` (graceful scenario recovery) unchanged.
- `SOW-0004` (packaging, SE quickstart, a real `--llm` provider run) unchanged;
  the console's `--describe` path now has a second consumer, which makes the
  provider run more valuable.
- 50+ vnode scale test remains deferred to a non-laptop machine by user
  decision.

## Regression Log

None yet.
