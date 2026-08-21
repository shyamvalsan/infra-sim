---
name: project-integration-sync
description: How to add or repair a generated integration spec - which source feeds which sync script, the loop that turns Netdata's own metadata/profiles into specs, and the value-defect rules the lint exists to catch. Load before running any scripts/sync-*.py, when you add an integration, regenerate specs, or a generated spec fails the lint.
---

# Integration sync

Breadth comes from syncing Netdata's own declarations; fidelity is never free.
Netdata's metadata says what a collector *emits*, never what *plausible* looks
like — every defect found after the first sync was a value defect (108%
fragmentation, flat dBm, backwards counters) that reading the YAML never
showed and the lint always did (SOW-0006).

## Step 0: which source owns this collector

The failure this table prevents: a simulated CloudWatch node shipped with
three charts of the collector's own API-call accounting and nothing about AWS
— because the metadata was read as the metric contract when the collector
builds charts at runtime from profiles (SOW-0017). Check which row you are in
before touching a script:

| The collector's metrics… | Source of truth | Script |
|---|---|---|
| are declared in `metadata.yaml` | that `metadata.yaml` | `sync-integrations.py` |
| are built at runtime from service profiles (the metadata *says so in prose*) | the profile files under `go.d/<name>.profiles` | `sync-profile-collectors.py` |
| are an SNMP device | `go.d/snmp.profiles/default/<model>` | `sync-snmp-profiles.py` |

Reading a collector's metadata and finding a handful of collector-activity
charts where infrastructure should be is not a thin collector — it is row 2, and
SOW-0017 is the whole story.

## The loop

```bash
# 1. Regenerate (a netdata checkout as --netdata; output lands in the repo)
scripts/sync-integrations.py         --netdata <netdata-checkout> --out .
scripts/sync-snmp-profiles.py        --netdata <netdata-checkout> --out .
scripts/sync-profile-collectors.py   --netdata <netdata-checkout> --out .

# 2. Lint the WHOLE corpus, not the changed file. Every unit defect in
#    SOW-0006 was invisible in review and immediate in the lint — and several
#    only failed on particular seeds (SOW-0016: one clean environment proves
#    nothing).
./target/release/infra-sim --environment environments/<env-using-the-spec>.yaml --lint 6

# 3. Semantic spot-check against a live agent — see project-live-validation.
#    Two SOW-0014 defects passed lint, tests and review and were obvious in
#    one api/v3/nodes call.
```

## Rules, each earned by a shipped defect

**Match short unit strings exactly, never by substring.** `%` failed to match
`percentage`, 60 metrics quietly took a generic profile, and disk
fragmentation charted at 108% — a plausible-looking chart with an impossible
number, the worst way for a defect to surface (SOW-0006).

**Failure-named dimensions start at zero; `0/1` dimensions are constant;
negative-quantity baselines must not drift.** They were generated like healthy
gauges, producing error rates that followed the working day (SOW-0006).

**A labelled scope becomes ONE representative instance carrying that scope's
chart labels.** A real Elasticsearch with twelve indices shows twelve chart
instances; a generated one shows one. This is the accepted limitation —
`instances_modelled` in the catalogue records which integrations are affected
(sync-integrations docstring). Do not "fix" it by inventing instance lists.

**Read the source format completely before inferring anything from names.**
The first SNMP pass inferred units from metric names and produced 165 charts
for a Catalyst with no traffic chart — because interface traffic lives in
`virtual_metrics`, a section the first pass never read (SOW-0014). Profile
units are UCUM strings (`By/s`, `{packet}/s`, `Cel`, `1`), taken verbatim.

**A generated alias of a hand-authored spec must be dropped from BOTH the
catalogue and disk.** Left alive, `--describe` resolves the description to the
shallow generated copy and every scenario targeting that software silently
stops applying (SOW-0006). After any sync, diff the catalogue against
`specs/` and delete shadow copies of the six deep specs.

**Generated specs live at the TOP level of `specs/generated/` only.** A nested
path resolves when named explicitly but is never discovered by the describe
matcher, which enumerates one level (sync-profile-collectors docstring). SNMP
devices are the one exception — they are picked from `snmp-devices.json`, not
described.

**Generalise a schema from two examples, never one.** `sync-profile-collectors.py`
handles cloudwatch + azure_monitor + prometheus because the first two
independently share the `template:` schema; a fourth family that follows it is
one FAMILIES entry, and one that doesn't is not (SOW-0017).

**Reusing a helper inherits its assumptions.** The unit-magnitude rule
imported from one family mis-scaled another family's units silently
(SOW-0017). When you reuse mapping code across families, re-check its table
against the new family's unit strings.

**An incomplete Prometheus summary produces no chart and no error.** go.d
skips a summary missing `_sum`/`_count` and logs nothing (SOW-0020's exporter
work). If you emit one, emit it whole, and check for the chart's *absence* on
a live agent rather than for an error.

## What generated specs are — say so, don't blur it

Structurally faithful (right contexts, dimensions, units, plausible
seasonality), not deeply modelled: signals are independent, no scenario
targets them, unlike the six hand-authored specs. The console labels the
difference. Keep that distinction honest in anything user-facing
(sync-integrations docstring).
