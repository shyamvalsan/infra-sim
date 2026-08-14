# SOW-0017 - Rich CloudWatch service profiles

## Status

Status: in-progress

Sub-state: Generator delivered and validated live; `systematica` tailored and
serving 95 AWS charts. **Two chunks remain**: synthetic AWS inventories in
`describe.rs` (so new fleets get resources without hand-editing), and label
diversity. Decisions all resolved.

## Requirements

### Purpose

Make simulated AWS monitoring real. A node claiming `amazon-cloudwatch` currently
reports three charts of the collector's own API-call accounting and nothing about
AWS, which is worse than not claiming AWS at all under the "no disqualifying
artifacts" bar.

### User Request

"the AWS collector (the new one with all the profiles) doesn't seem to be working,
i just see AWS Cloudwatch -> collector -> some random charts instead of those new
AWS profiles being excercised", then: "i want RICH profiles to be used for
simulations, do this.. and update the existing systematica sim so i can see these
new AWS metrics (specifically for the entities they mentioned) so it is tailored".

### Assistant Understanding

Facts:

- `specs/generated/amazon-cloudwatch.yaml` has **3 contexts**, all
  `cloudwatch.collector_*`: SDK invocations, metric requests, query items. Those
  are the collector's self-monitoring, which is exactly what the operator saw.
- The cause is structural, not a bad sync run. The collector declares **no static
  metrics**: `grep -c "context:"` on
  `netdata/netdata @ 91c8b3741f09
  src/go/plugin/go.d/collector/cloudwatch/metadata.yaml` returns **0**. Its metrics
  section is prose: "Charts are generated at runtime from the **active service
  profiles**... All contexts live under the `cloudwatch.` namespace."
- `scripts/sync-integrations.py` derives specs from `metadata.yaml`'s metrics
  section, so for this collector there was nothing to derive beyond the four
  collector-activity charts named in that prose.
- The real definitions live where the sync never looks:
  `src/go/plugin/go.d/config/go.d/cloudwatch.profiles/default/*.yaml` - **47 files**
  covering ~27 AWS services.
- Those profiles are fully machine-readable and contain everything a generator
  spec needs. From `ec2.yaml`:
  - `namespace: AWS/EC2`, `display_name: AWS EC2`
  - `instance.dimensions[]` -> `{name: InstanceId, label: instance_id}`
  - `metrics[]` -> `{id, metric_name, statistics[], rate?, disabled?}`
  - `template.context_namespace` -> `ec2`
  - `template.chart_defaults.instances.by_labels` -> `[account_id, region, instance_id]`
  - `template.charts[]` -> `{id, context, title, family, units, algorithm,
    dimensions[{selector, name}]}`
- Units are present per chart (`percentage`, `bytes/s`, `ops/s`, `milliseconds`,
  `requests/s`, ...), so the earlier worry that units would have to be invented is
  closed: they are declared.
- Chart dimension `selector` values are `<metric id>_<statistic>` (e.g.
  `cpu_utilization_average`, `network_in_sum`), which ties charts back to `metrics[]`
  and identifies which are `disabled: true` opt-ins to skip.
- `scripts/sync-integrations.py:152` already has `profile(unit, dim)` returning
  plausible `(base, min, max, rate)` from a unit string via `UNIT_RULES` and
  `EXACT_UNITS`. That is the magnitude logic to reuse, not re-invent.
- The running `infra-sim-systematica` container (up, healthy, port 19989) has
  `/var/lib/infra-sim/systematica` bind-mounted **read-write** at
  `/etc/netdata/infra-sim`, so its specs can be replaced in place without
  recreating the fleet or disturbing node identity.
- `systematica` is 160 nodes: 90 `[nginx, amazon-cloudwatch, processes]`, 60
  `[nginx, processes]`, 5 `[kubernetes, containers, kubernetes-containers,
  processes]`, 3 `[kubernetes, containers, processes]`, 2 `[aws-ecs-containers,
  processes]`.
- The environment records only "Generated from a text description." - the
  description prose is **not stored**, so which AWS entities were originally asked
  for cannot be recovered from the repository.

Inferences:

- Real CloudWatch puts every AWS resource on **one** node as labelled chart
  instances, not as separate Netdata nodes - the metadata says so explicitly. So a
  faithful simulation needs per-node inventories of synthetic AWS resources
  (instance ids, bucket names, function names) feeding instanced contexts.
- The same "charts built at runtime" limitation may affect other entries in the
  258-spec generated corpus. CloudWatch was inspected because it was reported;
  the corpus has not been audited for the pattern.

Unknowns:

- **Which AWS entities to tailor `systematica` to.** Not recoverable from the repo
  and not inferable without guessing a prospect's estate. Blocking for that chunk
  only; see Open decisions.

### Acceptance Criteria

- `specs/generated/amazon-cloudwatch.yaml` (or a profile-derived successor) carries
  contexts for the enabled charts of every default profile, under
  `cloudwatch.<profile>.*`, with the profile's own titles, families, units,
  algorithms and dimension names. Verified by counting contexts and diffing a
  sample against the source profile.
- Opt-in (`disabled: true`) metrics produce no contexts. Verified against `ec2.yaml`,
  which has both enabled and opt-in metrics.
- The fleet lints clean: `--lint 2` reports no semantic violations and no pinned
  signals. Verified on a fleet carrying the new spec.
- On a live agent, an AWS-carrying node shows the profile charts with correct units
  and moving values, and per-resource labelled instances. Verified by querying the
  running simulation's agent, not the generator.
- `systematica` shows the tailored profiles without being recreated - node identity
  and accumulated history preserved. Verified by comparing node GUIDs before and
  after, and confirming history depth did not reset.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`
  clean.

## Analysis

Sources checked:

- `netdata/netdata @ 91c8b3741f09`:
  `src/go/plugin/go.d/collector/cloudwatch/metadata.yaml`,
  `src/go/plugin/go.d/collector/cloudwatch/chart.go`,
  `src/go/plugin/go.d/config/go.d/cloudwatch.profiles/default/{ec2,api_gateway}.yaml`
- `scripts/sync-integrations.py`, `specs/generated/amazon-cloudwatch.yaml`
- `environments/systematica.yaml`, the running container's mounts and state dir
- `.agents/sow/specs/generator-and-engine.md` (generated-corpus section)

Risks:

- 47 profiles x several charts each is a large context count. Applied to 90 nodes it
  could add materially to lint and runtime cost - the same cost curve that just made
  a 160-node create take over 12 minutes.
- Instanced contexts need environment-side inventories; getting that wrong yields
  either empty charts or one unlabelled instance, both obvious tells.
- Replacing a spec under a running container is the good path here, but the plugin
  must be restarted to re-read it, and a plugin that outlives its own replacement
  has already cost this project real debugging time.

## Pre-Implementation Gate

Status: needs-user-decision (decision 3 blocks the tailoring chunk only)

Problem / root-cause model:

The generated corpus is derived from collector metadata. CloudWatch builds its
charts at runtime from profile files, so its metadata declares no metrics and the
derived spec is empty of AWS content. Fixing it means teaching the sync a second
source of truth - the profile files - rather than adjusting magnitudes or names.

Evidence reviewed: every citation under Facts, all read this session.

Affected contracts and surfaces:

- `scripts/sync-integrations.py` (or a new sibling) - new input source.
- `specs/generated/amazon-cloudwatch.yaml` - regenerated, much larger.
- `integrations/catalogue.json` - may need the profile list surfaced for the picker.
- Environment generation (`crates/sim-engine/src/describe.rs`) - synthetic AWS
  resource inventories for instanced profile contexts.
- `environments/systematica.yaml` and the running container's mounted specs.
- Operators: AWS nodes gain many charts; lint and runtime cost rise.

Existing patterns to reuse:

- `sync-integrations.py:152` `profile(unit, dim)` with `UNIT_RULES` / `EXACT_UNITS`
  for plausible magnitudes from a unit string.
- The generated-spec header convention that records provenance and states signals
  are independent rather than causally coupled.
- Instanced-context conventions already used by `specs/processes.yaml`
  (`instances: {group: ..., chart_prefix: ..., labels: {...}}`).
- `sim-docker.sh`'s in-place control-file rewrite as the precedent for updating a
  running simulation through its bind mount.

Risk and blast radius:

- Regenerating one file in `specs/generated/` affects only nodes that carry the
  service. No engine, protocol or schema change is required for the spec itself.
- Environment-side inventories are additive but touch `describe.rs`, which every
  fleet generation path uses - the higher-risk part.
- Cost: more contexts per node means more ticks of work per node. Must be measured,
  not assumed, given the 12-minute lint already observed.
- No credentials involved: profiles are static config, no AWS account is contacted.
  Synthetic account ids and resource names must be obviously synthetic.

Sensitive data handling plan:

- Synthetic AWS identifiers only: documentation-style account ids, `i-0sim...`
  instance ids, `sim-*` bucket names. No real account id, ARN or bucket name enters
  a committed spec or environment.
- The prospect name behind `systematica` is not to be characterised in this SOW; the
  fleet is referred to by its environment name only.

Implementation plan:

1. Profile reader: parse `cloudwatch.profiles/default/*.yaml`, resolve chart
   dimension selectors back to `metrics[]`, drop charts whose selectors are all
   `disabled: true`, and emit sim contexts under `cloudwatch.<namespace>.*` with the
   profile's titles, families, units, algorithms and dimension names. Magnitudes via
   the existing `profile(unit, dim)`.
2. Instancing: map `template.chart_defaults.instances.by_labels` onto an instanced
   context with a per-profile instance group, and generate synthetic inventories so
   each AWS-carrying node has plausible resources.
3. Regenerate `specs/generated/amazon-cloudwatch.yaml`; lint; measure cost delta.
4. Tailor `systematica` to the chosen entities and apply in place through the bind
   mount, restarting only the plugin inside that container.
5. Validate on the live agent; update spec/docs artifacts.

Validation plan:

- Context count and a field-by-field diff of one generated context against
  `ec2.yaml`.
- Confirm an opt-in metric (`disk_read_bytes`, `disabled: true`) produces nothing.
- `--lint 2` on a fleet carrying the spec; record the cost delta against the same
  fleet without it.
- Live agent: query the profile charts on an AWS node for units, movement and
  labelled instances.
- Confirm `systematica` node GUIDs and history are unchanged after the in-place
  update, and that the old plugin process is gone rather than orphaned.
- `cargo test`, clippy, fmt.
- Same-failure scan: audit the generated corpus for other collectors whose metadata
  declares no metrics because charts are built at runtime.

Artifact impact plan:

- AGENTS.md: no update expected; no workflow change.
- Runtime project skills: `project-live-validation` may gain a note about verifying
  profile-derived charts through the agent.
- Specs: `.agents/sow/specs/generator-and-engine.md` generated-corpus section needs
  the second source of truth recorded.
- End-user/operator docs: `docs/operating.md` integrations section.
- SOW lifecycle: close with the work in one commit.

Open-source reference evidence:

- `netdata/netdata @ 91c8b3741f09`
  - `src/go/plugin/go.d/collector/cloudwatch/metadata.yaml`
  - `src/go/plugin/go.d/collector/cloudwatch/chart.go`
  - `src/go/plugin/go.d/config/go.d/cloudwatch.profiles/default/ec2.yaml`
  - `src/go/plugin/go.d/config/go.d/cloudwatch.profiles/default/api_gateway.yaml`

Open decisions:

- Decision 1: **resolved by the user** - rich profiles, not a narrower fix.
- Decision 2: **proposed, see below** - instanced contexts with synthetic
  inventories, versus flat one-resource-per-node contexts.
- Decision 3: **OPEN and blocking for the tailoring chunk** - which AWS entities
  `systematica` should exercise. Not recoverable from the repo.

## Implications And Decisions

**Decision 1 - scope.** User chose rich profiles: "i want RICH profiles to be used
for simulations".

**Decision 2 - how AWS resources appear.** Recommendation, not yet confirmed:

- **A. Instanced, with synthetic inventories.** Each AWS node carries N EC2
  instances, M buckets and so on, as labelled chart instances - which is what the
  real collector does. Faithful, and more work: it touches `describe.rs`.
  *long-term-best.*
- B. Flat: one implicit resource per node, no labels. Cheaper, and visibly wrong to
  anyone who knows CloudWatch - a single unlabelled EC2 per node.

**Decision 3 - which entities for `systematica`.** OPEN. The description was not
stored, so the choice is the user's. The fleet's existing composition implies
CloudWatch, ECS and Kubernetes, but inferring a prospect's estate from 90 nginx
nodes would be guesswork.

## Plan

1. Profile reader and spec generation (independent of decision 3).
2. Instancing and synthetic inventories (decision 2).
3. Tailor and apply to `systematica` (decision 3).
4. Validation, artifacts, close.

## Execution Log

### 2026-08-14

- Diagnosed the reported gap: 3 collector-activity contexts because the collector
  declares no static metrics and builds charts from profiles at runtime.
- Confirmed the 47 profile files are machine-readable and carry units, titles,
  families, algorithms, dimension names and instance labels.
- Confirmed the running `systematica` container's spec directory is a read-write
  bind mount, so the fleet can be updated in place without losing identity.
- SOW opened; gate filled.
- Decisions resolved by the user: rich profiles; instanced with synthetic
  inventories (2A); a mix of AWS services including Fargate and Lambda. Fargate has
  no profile of its own - it reports through `AWS/ECS`, keyed by cluster and service
  name - so that is how it is wired.
- Scope widened on user instruction to any profile-based collector, grounded in two
  real examples rather than generalised from one: `azure_monitor.profiles` (38) uses
  the same `template:` schema as `cloudwatch.profiles` (47), differing only in the
  identity key (`resource_type` vs `namespace`). `prometheus.profiles` (7) also
  matches. SNMP has its own script already; SNMP traps are events, not metrics.
- `scripts/sync-profile-collectors.py` written; 86 specs generated (47 AWS, 38
  Azure, 1 Prometheus - profiles without `template.charts` are skipped).
- Lint found a real defect in the generator: `EXACT_UNITS['status']` is
  `(base 1.0, max 1.0)` and the failure-keyword rule inspects only the dimension
  name, which for `status_check_failed` is `system`/`attached_ebs`. Every simulated
  EC2 reported a permanently failing status check, pinned against its bound ~50% of
  samples. Fixed by passing `context_dimension` to the existing rule; status checks
  now rest at 0, which is a legitimate flat zero.
- `systematica` tailored on 3 of its 90 CloudWatch nodes, not all 90: a real estate
  polls CloudWatch from a handful of collectors per account, and replicating an AWS
  estate 90 times would be both a tell and a 30x chart multiplier. Applied through
  the container's read-write bind mount - no recreate, identity and history intact.
- Live evidence: 95 AWS charts across 10 services on
  `systematica-amazon-cloudwatch-01`, values flowing - EC2 CPU 32-38%, Lambda
  invocations 106-145/s, ECS Fargate cpu 41 / memory 24, RDS CPU 32-37%.
- Two self-inflicted incidents during the in-place update, both recovered: a
  `pkill` inside the container matched the `--logs` and `--otlp` writers, which
  netdata does not manage, so they stayed dead until restarted via
  `sim-docker.sh telemetry`; and copying the repo's environment over the
  container's broke its spec paths (`specs: ../specs` is relative to
  `environments/`, the container needs `specs: specs`).
- Validation shortcut, recorded honestly: the new surface was linted on a 5-node
  extract of the tailored nodes rather than all 160, which would have cost another
  ~12 minutes. Clean - no violations, no pinned signals.

## Validation

Pending.

## Outcome

Pending.

## Lessons Extracted

Pending.

## Followup

None yet.

## Regression Log

None yet.
