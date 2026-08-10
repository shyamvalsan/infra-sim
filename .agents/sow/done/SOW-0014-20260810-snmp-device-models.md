# SOW-0014 - SNMP device models, fleet placement, and labelling every node

## Status

Status: completed

Sub-state: Delivered and validated on a live containerised agent. All 105 device
specs pass the fidelity lint; a Catalyst fleet reports 1426 charts across 126
contexts with the vendor's own units and families; the hero scenario degrades the
targeted uplink on it. Every node including the simulation's own agent carries
`simulated=true` and its coordinates.

## Requirements

### Purpose

`SOW-0010` made network devices a first-class node class with one hand-authored
generic switch spec. That is right for "some switches" and wrong the moment a
prospect names their hardware. A real Cisco Catalyst reports per-supervisor CPU,
chassis temperature and voltage sensors, power-supply state, FRU status and PoE;
the generic spec has none of them. An SE demoing to a network-heavy prospect can
say "switches" but not "your switches".

Netdata already ships the answer: 176 SNMP device profiles under
`go.d/snmp.profiles/default`, each naming its vendor and device type and
inheriting from shared MIB files.

### User Request

Three requests, one continuous session on 2026-08-10, all touching the same
files, so they are tracked as one SOW rather than split into three:

1. "also when we simulate snmp network devices we should be able to use the snmp
   profile info and let the user choose which vendor/device-type and populate
   based on this" (2026-08-09), followed by "yeah keep going with the fixes and
   improvements.."
2. "all simulated nodes should have a \"simulated\" hostlabel set to true"
3. "also \"latitude\" and \"longitude\" host labels should be populated as well,
   based on location input by the user in the ux"

A fourth item came out of validating the third: the console's teardown removed the
wrong simulation. Recorded below.

### Assistant Understanding

Device models:

1. A sync script turning each device profile into a generator spec, with the
   profile's `extends` chain resolved.
2. A console picker where a network-device group chooses vendor and model.
3. The chosen spec travelling with the fleet through lint and install.

Constraint carried from `SOW-0010`: a device's own `generator:` replaces the
Linux baseline rather than composing on top of it.

Labelling: every vnode already carried `simulated=true`, applied unconditionally
in `profiles()`. The gap was the simulation's **own agent** - the container's node
was the one node in the Space with no marking at all.

Placement: a fleet location and a per-group override, entered as coordinates,
written as `latitude`/`longitude` host labels on every node and on the
simulation's agent.

### Acceptance Criteria

1. Every generated device spec passes the fidelity lint.
2. A device's contexts, units and families match what the real collector would
   emit, not values this project invented.
3. `switch-uplink-degrading` works on any model without a per-vendor variant.
4. A network device's hostname stays `<name>-sw-NN`, because the hero scenario
   targets that suffix.
5. The console offers vendor and model, one model per group.
6. A fleet with a model installs and runs; the plugin does not die looking for
   the spec.
7. Every node on a live agent, including the simulation's own, carries
   `simulated=true`.
8. A placed fleet writes `latitude`/`longitude` on every node; an unplaced one
   writes neither, rather than defaulting to 0,0.
9. A group override places only that group; the simulation's own agent takes the
   fleet's location, not a group's.
10. Teardown removes the simulation it was asked to remove.

## Analysis

The generic spec covers the standard IF-MIB and nothing vendor-specific. The
profiles carry far more than expected, and in a newer schema than a first pass
assumed:

- a profile declares `chart_meta.unit` for 2928 of 3043 symbols, so units are
  **declared**, not inferrable-only;
- `chart_meta.family` and `chart_meta.description` give the real chart family and
  title;
- an enum symbol carries a `mapping:`, which the collector expands into one 0/1
  dimension per state;
- interface traffic, packets, errors and discards do not live in `metrics:` at
  all. They live in `virtual_metrics:`, which compose `_`-prefixed collection
  inputs into in/out charts. Only 9 shared files declare them; 94 device profiles
  inherit them through `extends`.

Reading only `metrics:` therefore produced switches with no traffic chart - the
single most important chart a network device has.

## Pre-Implementation Gate

Status: ready

Problem / root-cause model:

- The `network-device` role has exactly one shape, so every simulated switch is
  the same generic device regardless of the prospect's hardware. Evidence:
  `specs/network-device.yaml` is the only network base spec, and
  `crates/sim-engine/src/describe.rs` hardcoded it for the role.
- The vendor detail needed to fix this is already in the Netdata tree and was
  simply not being read.

Evidence reviewed:

- `netdata/netdata @ c23face0bd94`
  - `src/go/plugin/go.d/collector/snmp/metric_ids.go:56` - context id is
    `snmp.device_prof_` plus the name with dots and spaces underscored.
  - `src/go/plugin/go.d/collector/snmp/charts.go:201,209,215` - chart units are
    the profile's `chart_meta.unit` verbatim, `1` when absent, and `bit/s` is
    drawn as an area.
  - `src/go/plugin/go.d/collector/snmp/charts.go:198-212` - title and family
    fall back to the metric name.
  - `src/go/plugin/go.d/collector/snmp/charts.go:362-371` - dimension algorithm
    from `metric_type`.
  - `src/go/plugin/go.d/collector/snmp/collect_snmp.go:143-165` - a table
    metric's chart key is the name plus its non-`_` tag values, sorted.
  - `src/go/plugin/go.d/collector/snmp/ddsnmp/transform.go:107-114` - a
    `mapping:` becomes a MultiValue metric, one 0/1 entry per state.
  - `src/go/plugin/go.d/config/go.d/snmp.profiles/default/_std-if-mib.yaml` -
    interface counters are collected as `_`-prefixed inputs and charted through
    `virtual_metrics`.
  - `src/go/plugin/go.d/config/go.d/snmp.profiles/default/cisco-catalyst.yaml` -
    declares no metrics of its own; inherits everything from six base files.
- Local: `specs/network-device.yaml`, `crates/sim-engine/src/describe.rs`,
  `crates/sim-engine/src/fidelity.rs:111-187`, `crates/sim-console/src/provision.rs`,
  `crates/sim-console/src/ui.html`, `scripts/sync-integrations.py`,
  `scenarios/switch-uplink-degrading.yaml`.
- Prior SOWs: `SOW-0010` (network-device role), `SOW-0008` (generated spec
  contract), `SOW-0006` (integration picker).

Affected contracts and surfaces:

- New: `scripts/sync-snmp-profiles.py`, `integrations/snmp-devices.json`,
  `specs/generated/snmp/*.yaml` (105 files).
- Changed: `describe.rs` (`base_spec`, `device_instances`, `effective_slug`),
  `provision.rs` (`Catalogue`, group validation, `copy_generators`),
  `ui.html` (dual-mode picker), `README.md`.
- Contract for operators: a network-device group's one "service" is its model.

Existing patterns to reuse:

- `scripts/sync-integrations.py` - the same generate-from-Netdata-metadata
  pattern, including the honesty contract about generated value profiles.
- `Signal.ignore_weight` and `Signal.from_attr`, both added by `SOW-0010`
  precisely for device properties.
- The picker overlay, search and category filter already in `ui.html`.
- `copy_extends` in `provision.rs`, the existing precedent for a spec dependency
  travelling with an install.

Risk and blast radius:

- A device spec is loaded instead of the Linux baseline, so a broken one is a
  node with no charts. Mitigated by linting every device.
- Chart-id collisions: truncating names to 20 characters collided
  `ciscoEnvMonTemperatureState` with `ciscoEnvMonTemperatureStatusValue`, which
  makes the agent see one chart declared twice. Fixed by using full names.
- Renaming risk: treating the model as a distinguishing service renamed hosts to
  `<name>-cisco-catalyst-NN` and broke `switch-uplink-degrading`'s
  `hostname_suffix: -sw-01`.
- Install risk: the specs sit one directory deeper than service specs, so the
  existing shallow copy would leave the plugin looking for a missing file.
- Size: 5.7MB of generated YAML committed. Accepted - the collector-metadata
  specs already set this precedent and the alternative is generating at install
  time, which makes a fleet's content depend on the operator's Netdata checkout.
- No performance risk to the agent: a device node's chart count (18-203) is in
  the same range as a Linux node with services.

Sensitive data handling plan:

- SNMP communities are a credential class named in `AGENTS.md`. The profiles
  contain none, this work reads only metric definitions, and no generated spec or
  environment carries any SNMP auth material - simulated devices are not polled.
- Device vendor and model names are public product names, not customer data.
- No prospect or customer name enters any generated artifact.

Implementation plan:

1. `scripts/sync-snmp-profiles.py`: resolve `extends`, read `chart_meta`, expand
   mappings, emit `virtual_metrics`, write specs and the catalogue.
2. `describe.rs`: per-group base spec from the model; device instance groups;
   keep the role slug.
3. `provision.rs`: serve the device catalogue, validate model ids per role, copy
   environment-named generator specs on install.
4. `ui.html`: dual-mode picker, single-select for models.
5. Lint every device; docs; tests.

Validation plan:

- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
- Fidelity lint on one device at 6h, then a sweep over all 105 at 2h.
- Unit tests pinning the generator path, the hostname slug, and quoted instance
  names.
- Browser check of the picker against a running console.
- Live-agent check on a containerised fleet with a model selected.

Artifact impact plan:

- AGENTS.md: no update expected - no workflow or guardrail change.
- Runtime project skills: `project-live-validation` already covers "before
  changing a generator spec"; no new trigger.
- Specs: `.agents/sow/specs/generator-and-engine.md` needs the device-model path.
- End-user/operator docs: README node-classes section.
- End-user/operator skills: none exist yet.
- SOW lifecycle: new SOW, no split or merge; `SOW-0010` stays completed because
  its own claims still hold.

Open-source reference evidence:

- `netdata/netdata @ c23face0bd94`, paths listed above.

Open decisions:

- None blocking. Two judgement calls recorded below.

## Implications And Decisions

1. **The model is expressed as the group's one "service".** No new field in the
   request or environment schema; the existing per-group selection carries it,
   and the picker needed no new machinery. Cost: "service" now means two things
   depending on role, which the validation and the UI both special-case.
   Alternative was a `device:` field on the group and the environment - cleaner
   naming, wider blast radius across schema, console, describe and docs.
   Classified **surgical**.

2. **Topology tables are left empty.** A profile tables OSPF neighbours, CDP
   peers, MAC addresses and LLDP ports; a context whose instance group is absent
   is skipped, so those charts simply do not appear. Populating them would mean
   inventing peer devices that do not exist in the fleet, which is a fidelity
   claim this project should not make silently. Ports, CPUs, memory pools,
   sensors, fans, power supplies and FRUs are populated.

3. **Location is per group, defaulting to the fleet's** (user decision,
   2026-08-10). A real estate has sites - switches in the DC, edge gateways in the
   field - and one demo should be able to show that. Cost: an extra column in the
   fleet table.

4. **Coordinates, not a city search** (user decision, 2026-08-10). Two number
   fields. No embedded city table to license or keep current, and no network call
   that can fail or invent a place. Cost: the SE looks the coordinates up.

5. **Nodes at a site are scattered deterministically** (user decision,
   2026-08-10), up to roughly 500m, fixed by hostname. Machines in one rack do
   share a coordinate, but 27 nodes on one pin is a map that hides 26 of them.
   Whether Cloud clusters identical pins was not verified, and this removes the
   question.

6. **No default location.** Unplaced means no coordinate labels at all. A default
   would put a prospect's estate at 0,0 in the Gulf of Guinea and read as
   deliberate.

7. **Free-text device selection is not implemented.** "6 cisco catalyst
   switches" produces generic switches, and the SE picks the model in the fleet
   editor afterwards. Adding 105 ids to the keyword parser's vocabulary risks
   false matches on common vendor words ("dell", "server"). Tracked in Followup.

## Plan

1. Sync script. (done)
2. Renderer: base spec, instances, slug, device identity. (done)
3. Console: catalogue, validation, install copy. (done)
4. UI: dual-mode picker. (done)
5. Labelling: the container agent's own host labels. (done)
6. Placement: `Site`, fleet and per-group coordinates, UI, container agent. (done)
7. Teardown target selection. (done)
8. Lint sweep, tests, docs, specs. (done)
9. Live-agent validation on a container. (done)

## Execution Log

### 2026-08-10

- `scripts/sync-snmp-profiles.py`: written, then rewritten once the newer profile
  schema was understood. First version read only `metrics:` and inferred units
  from metric names; it produced 165 charts for a Catalyst with no traffic chart
  at all. Second version reads `chart_meta`, expands `mapping:` into per-state
  dimensions, and emits `virtual_metrics`: 182 contexts for a Catalyst, 202 for a
  Nexus, 7029 across 105 devices.
- `crates/sim-engine/src/describe.rs`: `base_spec(role, services)` resolves a
  model to `../specs/generated/snmp/<id>.yaml`; `device_instances()` populates
  the eleven hardware groups; `effective_slug()` returns the role slug for
  network devices; instance names are emitted quoted.
- `crates/sim-console/src/provision.rs`: `SnmpDevice`, `snmp_devices()`,
  `Catalogue.devices`, per-role validation in both `build_environment` and
  `create`, and `copy_generators()` copying every spec the environment names.
- `crates/sim-console/src/ui.html`: the picker switches catalogue by role, filters
  by vendor, single-selects a model, and resets the selection when a row crosses
  into or out of `network-device`.
- `README.md`: node classes and repository layout.
- Two tests added pinning the generator path, the `sw` hostname and the quoted
  index names.

Labelling and placement:

- `docker/netdata.conf.template`: a `[host labels]` section giving the container's
  own agent `simulated = true`, `infra_sim_name`, `infra_sim_role = sim-parent`
  and the fleet's coordinates. The agent reads that section into host labels
  (`netdata/netdata @ c23face0bd94 src/database/rrdhost-labels.c:152`).
  `infra_sim_role` is deliberately not a server role, so a room rule selecting
  `web` does not capture the simulation's agent while one on `infra_sim_name`
  does.
- `scripts/sim-docker.sh`: reads `site:` out of the environment and substitutes
  it, or strips the two label lines when a fleet is unplaced.
- `crates/sim-engine/src/describe.rs`: `Site` with validation and deterministic
  scatter, `Group.site`, `Reading.site`, `site_labels()`, and a top-level `site:`
  block in the rendered environment.
- `crates/sim-plugin/src/environment.rs`: `Environment.site`, because the schema
  is `deny_unknown_fields`.
- `crates/sim-console/src/provision.rs`: fleet and per-group coordinates on the
  request, validated as a pair and as points on Earth.
- `crates/sim-console/src/ui.html`: fleet latitude/longitude above the fleet
  table, and a Location column where a row shows what it inherits or its own
  override.

Faults found and fixed during implementation:

- `name: 1` for an SNMP index parsed as an integer, so the environment failed to
  deserialise. Instance names are now quoted.
- 20-character chart prefixes collided two Cisco temperature contexts onto one
  chart id; the lint reported them as stuck at 1 because the chart resolved to
  the wrong context. Full names now, with a uniqueness guard.
- Marking every `By` metric as a device property suppressed its noise without
  declaring it constant, so 41 memory-pool metrics read as stuck. Bytes now vary
  and scale with instance weight; only names that say total/size/capacity/limit
  are fixed, and a fixed signal is emitted as a declared constant.
- Uptime read off its `s` unit became a noisy gauge around 86400 - the exact
  artifact `specs/network-device.yaml` calls out. `CANONICAL_BODY` now pins
  uptime to an accumulating tick and port speed to the port's own attribute.
- Scatter used two 16-bit windows of one FNV hash. `-web-01` and `-web-02` differ
  in one trailing byte, which barely moves FNV's upper bits, so every node at a
  site got an identical longitude and latitudes 7m apart. Each axis now comes
  from its own splitmix64-mixed stream. **Only visible by reading the rendered
  coordinates** - no test or lint would have caught it.
- The fleet's site was inferred from the largest placed group. On a tie it took a
  *group override*, so the simulation's own agent was labelled Amsterdam while
  the operator had typed Frankfurt. Found on a live agent. `Reading.site` now
  carries the fleet's own location explicitly.
- A node loading the Cisco Catalyst profile was still labelled
  `device_vendor: sim-networks, device_model: SIM-2960X-24`, a contradiction an
  SRE reads first. `Group.device` now carries the profile's vendor, model and
  type, read from the catalogue by the console. A group with no model keeps the
  visibly-synthetic generic labels.

### Incident: teardown removed the wrong simulation

While validating placement, a `POST /api/teardown` naming `devsnmp` removed the
**`ceph`** simulation instead - a fleet that had been running 14 hours.

Cause: `/api/teardown` accepted no body at all. It tore down whatever the console
held in `app.active`, which is rediscovered on every status poll; with two
containers running it had adopted `ceph`. The name in the request was silently
ignored. A single-simulation machine never exposed this, so `SOW-0012` validated
teardown without meeting it.

What was lost: the container's agent database - 14 hours of stored history and the
ML models trained on it - and the agent's Cloud claim.

What survived: `environments/ceph.yaml` in the repo and
`archive/ceph-1786355914/environment.yaml`. Node GUIDs derive from the
environment, so recreating it reproduces the same node identities; Cloud
re-attaches the same nodes rather than duplicating them.

Fix: `/api/teardown` now takes a `TeardownRequest`, tears down only a simulation
whose name matches one that is running, refuses an unknown name while listing what
is running, and refuses an empty name when more than one simulation exists. The UI
sends the name it just showed in its confirmation dialog.

This is not filed as a `SOW-0012` regression because that SOW's claim - a
containerised teardown removes the simulation whole, leaving nothing stale - is
still true; the defect is target selection with more than one simulation, which it
never covered. Reclassify on request.

## Validation

Acceptance criteria evidence:

1. Lint sweep over all **105** device specs: 105 PASS, 0 FAIL. `cisco_catalyst`
   additionally at 6h: "semantic checks: no violations", "no signals pinned to
   their bounds".
2. On a live agent, `devsnmp-sw-01` reports 1426 charts across 126 contexts.
   `snmp.device_prof_ifTraffic` is `bit/s` in `Network/Interface/Traffic/Total`
   with `in`/`out`; `snmp.device_prof_ifOperStatus` is `{status}` in
   `Network/Interface/Status/Operational` with a dimension per state
   (`up, down, testing, unknown, dormant, notpresent, lowerlayerdown`);
   `snmp.device_prof_cpu_usage` is `%` in `System/CPU/Usage`. Units, families and
   dimension names are the profile's, not this project's.
3. `switch-uplink-degrading` triggered on the live Catalyst fleet: inbound errors
   on `iferrors.TenGigabitEthernet1/1/1` ramped 0 → 7/s over 90s while `out`
   stayed 0, which is what the timeline says should happen at that point.
   Scenario check: "all required scenario targets resolve".
4. `a_device_model_sets_the_nodes_generator_without_renaming_it`; live hostnames
   are `devsnmp-sw-01`, `devsnmp-sw-02`.
5. Console at 127.0.0.1:19972: picker title "Device model for these 4 network
   devices", 105 cards, 72 vendor options, "cisco" narrows to 22; selecting
   `cisco_nexus` after `cisco_catalyst` leaves exactly `["cisco_nexus"]`.
6. A containerised fleet with `cisco_catalyst` created, installed and ran; the
   spec was copied to `specs/generated/snmp/` in the payload and the plugin
   collected from it.
7. Live agent, all five nodes: `simulated: true`, including `devsnmp-parent`.
8. A placed fleet writes both labels on every node; `an_unplaced_fleet_writes_no_
   coordinates_at_all` asserts the empty case, and a fleet built via `--describe`
   has no `site:` block and no coordinate labels.
9. Live agent: web nodes near 50.11/8.68 (Frankfurt), switches near 52.36/4.90
   (Amsterdam), `devsnmp-parent` at the fleet's own 50.110900/8.682100.
   `the_fleets_own_location_beats_a_group_override` pins it.
10. `POST /api/teardown {"name":"nosuchsim"}` → "no simulation named 'nosuchsim'.
    Running: devsnmp". `{"name":"devsnmp"}` removed devsnmp and nothing else.

Tests or equivalent validation:

- `cargo test`: 204 passed, 0 failed (9 added by this SOW).
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --check`: clean.
- Lint sweep script: `105 PASS, 0 FAIL` at 2h each.

Real-use evidence:

- Two containerised simulations created through `POST /api/create`, unclaimed, on
  127.0.0.1:19989 and :19990: a mixed fleet of nginx web nodes and Cisco Catalyst
  switches at two sites. Charts, labels, coordinates and a triggered scenario all
  read back from the running agent's API. Both torn down; nothing left running.
- Browser checks of the picker and the fleet table via a driven console.

Reviewer findings:

- No external reviewer was run. The live agent caught what review would have been
  least likely to: the parent node in the wrong city, and the device labelled as a
  switch model that does not exist.

Same-failure scan:

- Other truncated identifiers that could collide across contexts: the dimension id
  is truncated to 28 characters, safe because a dimension is unique within its own
  chart; `sync-integrations.py` namespaces bare context ids by spec id.
- Other places a per-node `generator:` is resolved:
  `crates/sim-plugin/src/environment.rs:node_generator_path()` and
  `crates/sim-plugin/src/main.rs:load_service_spec()`; both take the path from the
  environment, so both are covered by `copy_generators`.
- Other endpoints that act on `app.active` rather than a named target: `claim`,
  `advance`, `reskin`, `logs`. None is destructive - the worst case is acting on
  the wrong simulation's scenario or claim, which is recoverable - but the same
  ambiguity exists. Tracked as follow-up 3 rather than fixed here, because
  widening this SOW to every lifecycle endpoint would leave it unvalidated.
- Other unconditional label injection: `profiles()` applies `simulated` and
  `infra_sim_name` to every node and has two tests; no other node source exists
  besides the container's own agent, now covered by its netdata.conf.

Sensitive data gate:

- No credentials, tokens, customer names or private endpoints in any artifact
  added or changed. Coordinates used throughout are Frankfurt and Amsterdam city
  centres, chosen as obviously generic.
- The SNMP profiles contain no community strings, and simulated devices are never
  polled, so no SNMP auth material exists to leak.
- Device vendor and model names are public product names.
- The scan for secrets over the changed files found nothing; the incident
  narrative names an archive directory and node names, all synthetic.

Artifact maintenance gate:

- AGENTS.md: no update needed - no workflow, guardrail or responsibility changed.
  The existing sensitive-data and open-source-evidence rules already covered this
  work and were followed.
- Runtime project skills: no update needed. `project-live-validation` already
  triggers on "before changing a generator spec, the engine, a scenario, the
  plugins.d runtime" and its probe-first rule is exactly what caught the parent's
  location and the device labels. The lesson below reinforces it rather than
  changing it.
- Specs: `.agents/sow/specs/generator-and-engine.md` (the SNMP device corpus) and
  `.agents/sow/specs/authoring-environments.md` (node classes, the `site:` block,
  location).
- End-user/operator docs: `README.md` rewritten as what-and-why at the user's
  request; the operational reference it held moved to `docs/operating.md` rather
  than being dropped, and gained the device-model, placement and labelling
  sections.
- End-user/operator skills: none exist.
- SOW lifecycle: `Status: completed`, moved to `.agents/sow/done/`, committed with
  the work in one commit.

Specs update:

- Both specs above updated.

Project skills update:

- No change needed, for the reason recorded in the gate above.

End-user/operator docs update:

- `README.md`, `docs/operating.md`.

Lessons:

- **A profile is a better source than a name.** The first version of the sync
  inferred units from metric names and got 165 charts for a Catalyst with no
  traffic chart. The profiles declare units for 96% of symbols and compose the
  traffic charts in a section the first pass never read. Read the source format
  fully before inferring anything from it.
- **The probe-first rule keeps paying.** Two defects were invisible to the type
  system, the tests, the lint and the YAML: the simulation's agent placed in the
  wrong city, and a Cisco device labelled as an invented switch model. Both were
  obvious in one `api/v3/nodes` call.
- **A destructive endpoint must name its target.** Teardown took no body and acted
  on ambient state. It behaved correctly for as long as exactly one simulation
  existed, then destroyed 14 hours of someone's work. Ambient state is acceptable
  for a read; never for a delete.
- **Weak hashes hide in plausible output.** The scatter looked fine until the
  numbers were read side by side: same longitude, latitudes 7m apart. FNV without
  an avalanche step does not spread a trailing-byte change.

Follow-up mapping:

1. Free-text device selection - **tracked**, follow-up 1 below.
2. Vendor icons in the device picker - **tracked**, follow-up 2 below.
3. Other lifecycle endpoints acting on ambient `active` - **tracked**, follow-up 3
   below.
4. Recreating the `ceph` simulation - **user decision**, follow-up 4 below. It
   needs their claim token, which never reaches this repo or an argv.
5. Nothing else deferred: the `defer|later|follow-up|future|TODO|pending` scan over
   this SOW returns only these items; every template placeholder is filled in.

## Outcome

Delivered:

- 105 SNMP device models from 71 vendors, 7029 contexts, generated from Netdata's
  own device profiles with units, families, titles, chart types, per-state status
  dimensions and composed in/out charts taken from the profile rather than
  invented. All 105 pass the fidelity lint.
- A console picker that chooses a vendor and model for a network-device group,
  with the chosen spec travelling through lint, install and the container payload.
- Standard interface counters named identically across every model, so one
  scenario degrades an uplink on any of them - demonstrated live.
- Every node carries `simulated=true`, including the simulation's own agent, which
  was previously the one unmarked node in a Space.
- Fleet and per-group placement written as `latitude`/`longitude` host labels,
  deterministically scattered, with the fleet's own location on its agent.
- Teardown that removes the simulation it was asked to remove.

Not delivered: free-text device selection, vendor icons, and name-checking on the
non-destructive lifecycle endpoints. All tracked below.

## Lessons Extracted

Recorded under Validation → Lessons.

## Followup

1. **Free-text device selection.** "6 cisco catalyst switches" should be able to
   pick the model. Not done here: adding 105 ids to the keyword parser's
   vocabulary risks false matches on common vendor words ("dell", "server").
   Needs a design decision about matching, so it is a SOW of its own.
2. **Vendor icons in the device picker.** Models show a two-letter initial box.
   Netdata ships vendor logos for some of these; worth wiring when the picker is
   next touched.
3. **Name the target on the other lifecycle endpoints.** `claim`, `advance`,
   `reskin` and `logs` all act on ambient `active`. None is destructive, but the
   same wrong-simulation ambiguity applies once a machine runs two.
4. **Recreating `ceph`.** The environment survives, so the fleet can be rebuilt
   with identical node identities; 14 hours of history and trained models cannot
   be recovered. Awaiting the user's decision.

## Regression Log

None yet.

Append regression entries here only after this SOW was completed or closed and
later testing or use found broken behavior. Use a dated
`## Regression - YYYY-MM-DD` heading at the end of the file. Never prepend
regression content above the original SOW narrative.
