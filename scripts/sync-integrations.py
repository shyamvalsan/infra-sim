#!/usr/bin/env python3
"""Generate infra-sim generator specs from Netdata's own collector metadata.

Every Netdata collector ships a `metadata.yaml` describing exactly what a
generator spec needs: context ids, titles, units, chart types and named
dimensions. Reading that beats inventing metrics — the chart names, units and
dimension names come from the source of truth, so a simulated MySQL node has
the charts a real one has, spelled the way Netdata spells them.

    scripts/sync-integrations.py --netdata /path/to/netdata --out .

Writes:
  integrations/catalogue.json   name, id, icon, category, chart count
  specs/generated/<id>.yaml     one generator spec per integration

## What generated specs are and are not

They are structurally faithful: right contexts, right dimensions, right units,
plausible values with daily seasonality. That is what makes a dashboard read as
real at a glance, and it is what an SE needs when a prospect runs MySQL.

They are not deeply modelled. The six hand-authored specs couple their signals
causally — queries move with connections, latency follows disk service time —
and the hero scenarios target their signal names directly. A generated spec has
independent signals and no scenario targets it. The console labels the
difference rather than pretending it away.

## Scope handling

Every scope is generated. An unlabelled scope becomes plain node-level
contexts. A labelled scope (per database, per index, per queue) becomes contexts
carrying one representative instance's chart labels, with a value derived from
the scope's label name.

That single instance is the honest limitation to know about: a real
Elasticsearch with twelve indices shows twelve chart instances and this shows
one. The alternative - asking the create form to collect instance lists for 150
collectors - is a worse trade for an SE with four hours. `instances_modelled` in
the catalogue records which integrations are affected.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import re
import sys

try:
    import yaml
except ImportError:
    sys.exit("needs PyYAML:  pip install pyyaml")

# Netdata serves integration icons from its own CDN.
ICON_BASE = "https://www.netdata.cloud/img/"


def slug(name: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")
    return s or "integration"


def ident(name: str) -> str:
    """A signal name safe to use as a YAML key."""
    return re.sub(r"[^a-z0-9]+", "_", name.lower()).strip("_") or "value"


# Plausible magnitudes by unit. Netdata units are free text, so this matches on
# substrings, most specific first. Getting the order of magnitude right is what
# stops a chart reading as obviously synthetic.
UNIT_RULES = [
    # (substring, base, min, max, is_rate)
    ("percentage", 34.0, 0.0, 100.0, False),
    ("percent", 34.0, 0.0, 100.0, False),
    ("celsius", 41.0, 0.0, 120.0, False),
    ("kilobits/s", 18000.0, 0.0, 10_000_000.0, True),
    ("megabits/s", 220.0, 0.0, 100_000.0, True),
    ("bytes/s", 2_400_000.0, 0.0, 5_000_000_000.0, True),
    ("kilobytes/s", 2400.0, 0.0, 5_000_000.0, True),
    ("bytes", 620_000_000.0, 0.0, 900_000_000_000.0, False),
    ("kib", 480_000.0, 0.0, 900_000_000.0, False),
    ("mib", 620.0, 0.0, 900_000.0, False),
    ("gib", 12.0, 0.0, 4096.0, False),
    ("milliseconds", 24.0, 0.0, 600_000.0, False),
    ("microseconds", 850.0, 0.0, 60_000_000.0, False),
    ("nanoseconds", 240_000.0, 0.0, 900_000_000.0, False),
    ("seconds", 0.9, 0.0, 86400.0, False),
    ("operations/s", 1450.0, 0.0, 5_000_000.0, True),
    ("queries/s", 920.0, 0.0, 5_000_000.0, True),
    ("requests/s", 1100.0, 0.0, 5_000_000.0, True),
    ("connections/s", 45.0, 0.0, 500_000.0, True),
    ("events/s", 260.0, 0.0, 1_000_000.0, True),
    ("errors/s", 0.4, 0.0, 100_000.0, True),
    ("packets/s", 9800.0, 0.0, 20_000_000.0, True),
    ("messages/s", 340.0, 0.0, 2_000_000.0, True),
    ("files/s", 22.0, 0.0, 500_000.0, True),
    ("/s", 130.0, 0.0, 1_000_000.0, True),
    ("connections", 180.0, 0.0, 200_000.0, False),
    ("threads", 46.0, 0.0, 100_000.0, False),
    ("processes", 210.0, 0.0, 100_000.0, False),
    ("sessions", 64.0, 0.0, 100_000.0, False),
    ("files", 1200.0, 0.0, 10_000_000.0, False),
    ("inodes", 240_000.0, 0.0, 500_000_000.0, False),
    ("boolean", 1.0, 0.0, 1.0, False),
    ("status", 1.0, 0.0, 1.0, False),
    ("events", 90.0, 0.0, 1_000_000.0, False),
    ("errors", 0.0, 0.0, 1_000_000.0, False),
    ("count", 320.0, 0.0, 10_000_000.0, False),
]


# Short unit strings must match exactly, never as substrings: `b` would match
# "bytes" and "bits", `ms` would match "messages/s". Checked before UNIT_RULES.
#
# These are not cosmetic. `%` fell through to the generic profile and produced
# ZFS pools reporting 108% fragmentation - a number an SRE spots instantly and
# the semantic lint rightly refuses.
EXACT_UNITS = {
    "%": (34.0, 0.0, 100.0, False),
    "percent": (34.0, 0.0, 100.0, False),
    "ratio": (34.0, 0.0, 100.0, False),
    "state": (1.0, 0.0, 1.0, False),
    "status": (1.0, 0.0, 1.0, False),
    "bool": (1.0, 0.0, 1.0, False),
    "ms": (24.0, 0.0, 600_000.0, False),
    "us": (850.0, 0.0, 60_000_000.0, False),
    "ns": (240_000.0, 0.0, 900_000_000.0, False),
    "s": (0.9, 0.0, 86400.0, False),
    "b": (620_000_000.0, 0.0, 900_000_000_000.0, False),
    "kb": (480_000.0, 0.0, 900_000_000.0, False),
    "mb": (620.0, 0.0, 900_000.0, False),
    "gb": (12.0, 0.0, 4096.0, False),
    "pps": (9800.0, 0.0, 20_000_000.0, True),
    "bps": (18_000_000.0, 0.0, 10_000_000_000.0, True),
    "rps": (1100.0, 0.0, 5_000_000.0, True),
    "qps": (920.0, 0.0, 5_000_000.0, True),
    "iops": (1450.0, 0.0, 5_000_000.0, True),
    "mhz": (2400.0, 0.0, 6000.0, False),
    "ghz": (2.4, 0.0, 6.0, False),
    "watts": (145.0, 0.0, 2000.0, False),
    "volts": (12.0, 0.0, 480.0, False),
    "amps": (3.2, 0.0, 200.0, False),
    "rpm": (2400.0, 0.0, 30_000.0, False),
    "dbm": (-62.0, -120.0, 30.0, False),
    "num": (320.0, 0.0, 10_000_000.0, False),
    "value": (320.0, 0.0, 10_000_000.0, False),
}


def profile(unit: str, dim: str) -> tuple[float, float, float, bool]:
    u = (unit or "").lower().strip()
    if u in EXACT_UNITS:
        b, mn, mx, r = EXACT_UNITS[u]
    else:
        for token, base, lo, hi, rate in UNIT_RULES:
            if token in u:
                b, mn, mx, r = base, lo, hi, rate
                break
        else:
            b, mn, mx, r = 120.0, 0.0, 1_000_000.0, False

    # Dimensions that name a failure should sit at zero: a fleet idling with a
    # steady error rate reads as broken, and a scenario needs headroom to move
    # them off the floor.
    d = (dim or "").lower()
    if any(k in d for k in ("error", "fail", "drop", "refus", "reject", "denied",
                            "timeout", "lost", "abort", "dead", "stale")):
        b = 0.0
    return b, mn, mx, r


def clean(text: str, limit: int = 110) -> str:
    t = " ".join((text or "").split())
    return t[:limit]


def spec_for(module: dict, meta: dict, mi: dict, base_contexts: set[str]) -> tuple[str, dict] | None:
    """Build one generator spec from a collector module. None if unusable."""
    name = mi["name"]
    sid = slug(name)

    # Operating-system collectors are not services you add to a node - they are
    # what the node already is. Their contexts (system.ram, system.cpu) collide
    # with the Linux baseline, and spec composition rightly refuses that.
    cats = mi.get("categories") or []
    if any("operating-systems" in c for c in cats):
        return None
    scopes = ((module.get("metrics") or {}).get("scopes")) or []
    if not isinstance(scopes, list):
        return None

    global_scopes = [s for s in scopes if isinstance(s, dict)]
    # Charts that a real deployment would show once per database / index / queue
    # and this shows once, full stop.
    single_instance = sum(len(s.get("metrics") or []) for s in scopes
                          if isinstance(s, dict) and (s.get("labels") or []))

    signals: dict[str, dict] = {}
    contexts: list[dict] = []
    priority = 60000
    # Metadata files may repeat a metric across scopes, and a metric may repeat
    # a dimension name. Both are fatal to the plugin, so collapse them here.
    seen_ctx: set[str] = set()

    for scope in global_scopes:
        # One representative instance per labelled scope, labelled so Netdata
        # health templates that filter on these labels still attach.
        scope_labels = {}
        for lab in scope.get("labels") or []:
            if isinstance(lab, dict) and lab.get("name"):
                scope_labels[str(lab["name"])] = f"sim-{ident(str(lab['name']))}-1"
        for metric in scope.get("metrics") or []:
            if not isinstance(metric, dict):
                continue
            ctx_id = metric.get("name")
            dims = [d for d in (metric.get("dimensions") or [])
                    if isinstance(d, dict) and d.get("name")]
            if not ctx_id or not dims:
                continue
            # A handful of collectors declare bare metric names. The plugins.d
            # protocol requires `<type>.<id>`, so namespace them by module.
            if "." not in ctx_id:
                ctx_id = f"{sid}.{ctx_id}"
            # Safety net for anything the category check missed: the baseline
            # owns these contexts, and composing a duplicate is a hard error.
            if ctx_id in base_contexts or ctx_id in seen_ctx:
                continue
            seen_ctx.add(ctx_id)
            seen_dim: set[str] = set()

            unit = metric.get("unit") or "value"
            family = ctx_id.split(".", 1)[1].split("_")[0] if "." in ctx_id else "metrics"
            rate = False
            dim_entries = []
            for d in dims:
                if d["name"] in seen_dim:
                    continue
                seen_dim.add(d["name"])
                dn = d["name"]
                key = f"{ident(sid)}_{ident(ctx_id.split('.',1)[-1])}_{ident(dn)}"
                base, lo, hi, is_rate = profile(unit, dn)
                rate = is_rate
                sig = {"base": base, "min": lo, "max": hi}
                # A boolean or status dimension is a state, not a quantity.
                # Modelled as a noisy gauge it rests on its ceiling and the
                # fidelity lint fails it - correctly, because a value that is
                # always clamped has stopped being modelled. Declared as a
                # constant instead, which the flat-signal check exempts.
                if hi == 1.0 and lo == 0.0:
                    signals[key] = {
                        "base": 1.0, "min": 1.0, "max": 1.0,
                        "min_is_floor": True, "max_is_ceiling": True,
                    }
                    dim_entries.append((dn, key))
                    continue
                # A percentage that could pin at 100 is a rail, not a value.
                if hi == 100.0:
                    sig["max_is_ceiling"] = True
                if base > 0:
                    sig["seasonality"] = {"daily_amplitude": 0.45,
                                          "peak_hour": 14.0,
                                          "weekend_factor": 0.55}
                    sig["noise"] = {"kind": "gauss", "sigma": 0.16}
                elif base < 0:
                    # Negative-baseline units are signal strengths (dBm), which
                    # drift rather than follow a working day. Without this they
                    # sat perfectly flat, which the fidelity lint refuses -
                    # correctly, since a real optical module never does.
                    sig["noise"] = {"kind": "walk", "sigma": 0.04, "reversion": 0.1}
                signals[key] = sig
                dim_entries.append((dn, key))

            priority += 1
            if rate:
                contexts.append({
                    "id": ctx_id,
                    "title": clean(metric.get("description") or ctx_id),
                    "units": unit,
                    "family": family,
                    "chart_type": {"stacked": "stacked", "area": "area"}.get(
                        metric.get("chart_type"), "line"),
                    "priority": priority,
                    "shape": "counters",
                    "labels": dict(scope_labels),
                    "dimensions": [{"id": dn, "rate_signal": k} for dn, k in dim_entries],
                })
            else:
                contexts.append({
                    "id": ctx_id,
                    "title": clean(metric.get("description") or ctx_id),
                    "units": unit,
                    "family": family,
                    "chart_type": {"stacked": "stacked", "area": "area"}.get(
                        metric.get("chart_type"), "line"),
                    "priority": priority,
                    "shape": "independent",
                    "labels": dict(scope_labels),
                    "dimensions": [{"id": dn, "signal": k} for dn, k in dim_entries],
                })

    if not contexts:
        return None

    spec = {
        "version": 1,
        "name": sid,
        "description": (
            f"{name} metrics, generated from Netdata's own collector metadata so the "
            f"contexts, units and dimension names match what a real {name} node reports. "
            f"Signals are independent and plausible rather than causally coupled - see "
            f"scripts/sync-integrations.py."
        ),
        "signals": signals,
        "contexts": contexts,
    }
    return sid, {
        "spec": spec,
        "entry": {
            "id": sid,
            "name": name,
            "icon": ICON_BASE + mi["icon_filename"] if mi.get("icon_filename") else "",
            "category": (mi.get("categories") or [""])[0].replace("data-collection.", ""),
            "charts": len(contexts),
            "instances_modelled": single_instance,
            "modelled": "generated",
        },
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--netdata", required=True, help="path to a netdata checkout")
    ap.add_argument("--out", default=".", help="infra-sim repo root")
    ap.add_argument("--limit", type=int, default=0, help="stop after N (for testing)")
    args = ap.parse_args()

    spec_dir = os.path.join(args.out, "specs", "generated")
    os.makedirs(spec_dir, exist_ok=True)
    os.makedirs(os.path.join(args.out, "integrations"), exist_ok=True)

    # Contexts the Linux baseline already defines; a generated spec must not
    # redefine them or composition fails at load time.
    base_contexts: set[str] = set()
    try:
        base = yaml.safe_load(open(os.path.join(args.out, "specs", "linux-system.yaml")))
        base_contexts = {c["id"] for c in (base or {}).get("contexts", []) if c.get("id")}
    except Exception:
        pass

    seen: set[str] = set()
    catalogue: list[dict] = []
    written = 0

    files = sorted(glob.glob(os.path.join(args.netdata, "src", "**", "metadata.yaml"),
                             recursive=True))
    for path in files:
        try:
            doc = yaml.safe_load(open(path))
        except Exception:
            continue
        if not isinstance(doc, dict):
            continue
        for module in doc.get("modules") or []:
            if not isinstance(module, dict):
                continue
            meta = module.get("meta") if isinstance(module.get("meta"), dict) else {}
            mi = meta.get("monitored_instance")
            if not isinstance(mi, dict) or not mi.get("name"):
                continue
            built = spec_for(module, meta, mi, base_contexts)
            if not built:
                continue
            sid, payload = built
            if sid in seen:
                continue
            seen.add(sid)

            with open(os.path.join(spec_dir, f"{sid}.yaml"), "w") as fh:
                yaml.safe_dump(payload["spec"], fh, sort_keys=False, width=100)
            catalogue.append(payload["entry"])
            written += 1
            if args.limit and written >= args.limit:
                break
        if args.limit and written >= args.limit:
            break

    # Hand-authored specs are the deeply modelled ones; mark them so the console
    # can tell an SE which integrations scenarios can actually target.
    hand = {
        "nginx": "NGINX", "postgres": "PostgreSQL", "redis": "Redis",
        "containers": "Containers", "kubernetes": "Kubernetes",
        "otel-collector": "OpenTelemetry Collector",
    }
    icons = {"nginx": "nginx.svg", "postgres": "postgresql.svg", "redis": "redis.svg",
             "containers": "container.svg", "kubernetes": "kubernetes.svg",
             "otel-collector": "opentelemetry.svg"}
    # Category comes from Netdata's own metadata where the collector exists, so a
    # deeply modelled integration still appears under the category an SE would
    # filter by. Falling back to "modelled" would hide Postgres from "databases".
    cats = {c["id"]: c["category"] for c in catalogue}
    fallback = {"nginx": "web-servers-and-web-proxies", "postgres": "databases",
                "redis": "databases", "containers": "containers-and-vms",
                "kubernetes": "kubernetes", "otel-collector": "observability"}
    # The generated entry for the same software under a different id would sit
    # beside the hand-authored one as a second, differently-sized "PostgreSQL".
    # An SE picking between them has no way to know which is which.
    # snmp-devices is generated from a metadata.yaml that declares only a
    # licensing scope, so it advertises 6 licence charts and no interfaces at
    # all - the real per-port charts are built at runtime from device profiles
    # and are invisible to this sync. Six licence charts sold as SNMP support is
    # worse than nothing. Network devices are a node class of their own
    # (specs/network-device.yaml), not a service composed onto Linux.
    misleading = {"snmp-devices"}
    catalogue = [c for c in catalogue if c["id"] not in misleading]
    for gone in misleading:
        path = os.path.join(spec_dir, f"{gone}.yaml")
        if os.path.exists(path):
            os.remove(path)

    # A generated spec under a different id for the same software is hidden from
    # the picker, so an SE never chooses between two differently-sized
    # "PostgreSQL" entries. The file stays on disk: the hand-authored spec
    # `extends:` it, taking its breadth and overriding the contexts it models
    # more carefully.
    aliases = {"postgres": ["postgresql"]}
    for sid, label in hand.items():
        drop = {sid, *aliases.get(sid, [])}
        catalogue = [c for c in catalogue if c["id"] not in drop]
        # Count what the hand-authored spec actually emits rather than
        # publishing 0 charts for the six best-modelled integrations.
        spec_path = os.path.join(args.out, "specs", f"{sid}.yaml")
        try:
            with open(spec_path) as fh:
                charts = sum(1 for line in fh if line.lstrip().startswith("- id: "))
        except OSError:
            charts = 0
        catalogue.append({
            "id": sid, "name": label,
            "icon": ICON_BASE + icons[sid],
            "category": cats.get(sid) or fallback[sid],
            "charts": charts, "instances_modelled": 0,
            "modelled": "deep",
        })

    catalogue.sort(key=lambda c: (c["modelled"] != "deep", c["name"].lower()))
    out = os.path.join(args.out, "integrations", "catalogue.json")
    with open(out, "w") as fh:
        json.dump({"integrations": catalogue}, fh, indent=1)

    deep = sum(1 for c in catalogue if c["modelled"] == "deep")
    print(f"wrote {written} generated specs to {spec_dir}")
    print(f"catalogue: {len(catalogue)} integrations ({deep} deeply modelled) -> {out}")


if __name__ == "__main__":
    main()
