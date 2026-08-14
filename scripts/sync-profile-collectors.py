#!/usr/bin/env python3
"""Turn Netdata's profile-based collectors into generator specs, one per profile.

Some collectors do not declare their metrics in `metadata.yaml` at all. They build
charts at runtime from *service profiles*, and their metadata says so in prose:

    Charts are generated at runtime from the **active service profiles**...
    All contexts live under the `cloudwatch.` namespace.

`sync-integrations.py` reads the metrics section of `metadata.yaml`, so for those
collectors it can only produce the handful of collector-activity charts named in
that prose. That is how a simulated CloudWatch node came to report three charts of
the collector's own API-call accounting and nothing about AWS - which is worse than
not claiming AWS at all, because an SRE opens it expecting EC2.

The profiles themselves are fully machine-readable, and they carry everything a
generator spec needs: contexts, titles, families, units, algorithms, dimension
names, and which labels identify one resource. This script reads them.

## Families

Four profile directories ship with the agent. Three share one `template:` schema
and are handled here; SNMP has its own shape and its own script already
(`sync-snmp-profiles.py`), and SNMP *traps* are events rather than metrics.

    cloudwatch.profiles      47 files   AWS, keyed by `namespace`
    azure_monitor.profiles   38 files   Azure, keyed by `resource_type`
    prometheus.profiles       7 files   apps, keyed by `app`

The shared contract is `template.charts[]`; the identity key differs per family,
which is the only thing the adapters below encode. Adding a fourth family that
follows the same schema means adding one FAMILIES entry.

## What this produces

    specs/generated/<prefix>-<profile>.yaml     one generator spec per profile

Flat names, deliberately: a service resolves as `specs/generated/<service>.yaml`
(sim-plugin/src/main.rs), and only the top level of that directory is enumerated
for `--describe` matching, so a nested path would resolve but never be discovered.

## Fidelity limits, stated plainly

Structurally faithful: right contexts, units, families, dimension names, and one
chart instance per simulated resource. Not causally coupled - a profile describes
what AWS publishes, not how one metric drives another, so signals move
independently. Opt-in metrics (`disabled: true`) are skipped, matching a default
install.
"""

import argparse
import importlib.util
import os
import re
import sys

import yaml

# Reuse the magnitude logic rather than growing a second opinion about how big a
# "bytes/s" is. The filename has a hyphen, so it cannot be imported normally.
_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "sync_integrations", os.path.join(_HERE, "sync-integrations.py")
)
_si = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_si)
magnitudes = _si.profile
clean = _si.clean


FAMILIES = {
    "cloudwatch": {
        "dir": "cloudwatch.profiles/default",
        # What one profile monitors, for the spec description.
        "identity": ("namespace", "resource_type", "app"),
        "prefix": "aws",
        "vendor": "AWS",
    },
    "azure_monitor": {
        "dir": "azure_monitor.profiles/default",
        "identity": ("resource_type", "namespace", "app"),
        "prefix": "azure",
        "vendor": "Azure",
    },
    "prometheus": {
        "dir": "prometheus.profiles/default",
        "identity": ("app", "namespace", "resource_type"),
        "prefix": "prom",
        "vendor": "Prometheus",
    },
}

# Labels that identify the account or region rather than the resource. They belong
# on every instance of a profile, but they do not distinguish one resource from
# another, so they must not become the instance name.
ACCOUNT_LABELS = {
    "account_id", "region", "subscription_id", "resource_group", "location",
    "cloud", "profile", "resource_type", "namespace",
}


def ident(name: str) -> str:
    """A signal-name-safe identifier."""
    return re.sub(r"[^a-z0-9]+", "_", (name or "").lower()).strip("_")


def enabled_metric_ids(prof: dict) -> set[str]:
    """Metric ids a default install actually collects."""
    out = set()
    for m in prof.get("metrics") or []:
        if not isinstance(m, dict) or m.get("disabled"):
            continue
        if m.get("id"):
            out.add(str(m["id"]))
    return out


def selector_metric(selector: str, metric_ids: set[str]) -> str | None:
    """Resolve `cpu_utilization_average` back to the metric id it came from.

    Selectors are `<metric id>_<statistic>`, and a metric id may itself contain
    underscores, so the longest matching prefix wins rather than a naive split.
    """
    best = None
    for mid in metric_ids:
        if selector == mid or selector.startswith(mid + "_"):
            if best is None or len(mid) > len(best):
                best = mid
    return best


def instance_group(prof: dict, family_prefix: str, profile_name: str) -> tuple[str, list[str]] | None:
    """The instance group for a profile, and the labels identifying one resource.

    `by_labels` names every label a chart instance is keyed by, including account
    and region. What distinguishes one resource is whatever remains once those are
    removed - `instance_id` for EC2, `function_name` for Lambda, `cluster_name` and
    `service_name` for ECS (which is how Fargate reports).
    """
    tmpl = prof.get("template") or {}
    defaults = tmpl.get("chart_defaults") or {}
    inst = defaults.get("instances") or {}
    by_labels = inst.get("by_labels") or []
    if not isinstance(by_labels, list):
        return None
    identifying = [str(l) for l in by_labels if str(l) not in ACCOUNT_LABELS]
    if not identifying:
        # Some profiles describe a single account-wide resource (a Log Analytics
        # workspace, a billing total). One chart per node is correct there.
        return None
    return f"{family_prefix}_{ident(profile_name)}", identifying


def spec_for(path: str, family: str, cfg: dict) -> tuple[str, dict] | None:
    with open(path) as fh:
        prof = yaml.safe_load(fh) or {}
    if not isinstance(prof, dict):
        return None

    tmpl = prof.get("template") or {}
    charts = tmpl.get("charts") or []
    if not charts:
        return None

    profile_name = os.path.splitext(os.path.basename(path))[0]
    ns = tmpl.get("context_namespace") or ident(profile_name)
    display = prof.get("display_name") or profile_name
    identity = ""
    for key in cfg["identity"]:
        if prof.get(key):
            identity = str(prof[key])
            break

    metric_ids = enabled_metric_ids(prof)
    group = instance_group(prof, cfg["prefix"], profile_name)

    signals: dict = {}
    contexts: list = []
    priority = 90000

    for chart in charts:
        if not isinstance(chart, dict):
            continue
        context = chart.get("context")
        dims = chart.get("dimensions") or []
        if not context or not dims:
            continue

        units = str(chart.get("units") or "").strip() or "count"

        kept = []
        for d in dims:
            if not isinstance(d, dict):
                continue
            selector = str(d.get("selector") or "")
            if not selector:
                continue
            # A chart whose every dimension comes from an opt-in metric is not
            # part of a default install, so it must not appear in the simulation.
            if metric_ids and selector_metric(selector, metric_ids) is None:
                continue
            kept.append((str(d.get("name") or selector), selector))
        if not kept:
            continue

        priority += 10
        dim_entries = []
        for dim_name, selector in kept:
            sig = f"{ns}_{ident(context)}_{ident(dim_name)}"
            # The semantics live in the *context* for these profiles, not the
            # dimension: `status_check_failed` has dimensions named `system` and
            # `attached_ebs`, and `errors` has `error_4xx`. Passing only the
            # dimension name let a failure metric take the "status up" baseline of
            # 1.0, which is also its ceiling - so every EC2 reported a permanently
            # failing status check, pinned against the bound half the time. The
            # lint caught it. Give the existing rule the whole name.
            base, mn, mx, _rate = magnitudes(units, f"{context}_{dim_name}")
            signal: dict = {"base": base, "min": mn, "max": mx}
            if units in ("percentage", "%"):
                signal["max"] = 100.0
                signal["max_is_ceiling"] = True
            signal["noise"] = {"kind": "gauss", "sigma": 0.18}
            signals[sig] = signal
            dim_entries.append({"id": dim_name, "signal": sig})

        ctx: dict = {
            "id": f"{family}.{ns}.{ident(context)}",
            "title": clean(chart.get("title") or f"{display} {context}"),
            "units": units,
            "family": str(chart.get("family") or display),
            "chart_type": str(chart.get("type") or "line"),
            "priority": priority,
        }
        if group:
            group_name, identifying = group
            ctx["instances"] = {
                "group": group_name,
                "chart_prefix": f"{ns}_{ident(context)}",
                "family": str(chart.get("family") or display),
                "labels": {lbl: "{instance}" for lbl in identifying[:1]},
            }
            ctx["family"] = str(chart.get("family") or display)
        ctx["shape"] = "independent"
        ctx["dimensions"] = dim_entries
        contexts.append(ctx)

    if not contexts:
        return None

    name = f"{cfg['prefix']}-{ident(profile_name).replace('_', '-')}"
    description = (
        f"{display} as reported through Netdata's {family} collector, generated from "
        f"its service profile ({identity or 'no namespace declared'}) so the contexts, "
        f"units, families and dimension names match a real deployment. Charts are "
        f"per-resource instances, as the collector emits them: resources are labels on "
        f"one node, never separate nodes. Signals move independently rather than being "
        f"causally coupled - see scripts/sync-profile-collectors.py."
    )
    doc = {
        "version": 1,
        "name": name,
        "description": description,
        "signals": signals,
        "contexts": contexts,
    }
    return name, doc


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--netdata", required=True, help="path to a netdata checkout")
    ap.add_argument("--out", default=".")
    ap.add_argument(
        "--families",
        default=",".join(FAMILIES),
        help="comma-separated subset of: " + ", ".join(FAMILIES),
    )
    args = ap.parse_args()

    out_dir = os.path.join(args.out, "specs", "generated")
    os.makedirs(out_dir, exist_ok=True)

    total = 0
    for family in [f.strip() for f in args.families.split(",") if f.strip()]:
        cfg = FAMILIES.get(family)
        if not cfg:
            print(f"unknown family '{family}'", file=sys.stderr)
            return 1
        prof_dir = os.path.join(
            args.netdata, "src/go/plugin/go.d/config/go.d", cfg["dir"]
        )
        if not os.path.isdir(prof_dir):
            print(f"no {family} profiles at {prof_dir}", file=sys.stderr)
            return 1

        written = 0
        for fn in sorted(os.listdir(prof_dir)):
            if not fn.endswith(".yaml"):
                continue
            result = spec_for(os.path.join(prof_dir, fn), family, cfg)
            if not result:
                continue
            name, doc = result
            with open(os.path.join(out_dir, f"{name}.yaml"), "w") as fh:
                yaml.safe_dump(doc, fh, sort_keys=False, width=100)
            written += 1
        print(f"{family}: {written} spec(s)")
        total += written

    print(f"wrote {total} spec(s) to {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
