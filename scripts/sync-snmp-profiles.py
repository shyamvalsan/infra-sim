#!/usr/bin/env python3
"""Turn Netdata's SNMP device profiles into generator specs, one per device.

`specs/network-device.yaml` models a generic switch: the standard IF-MIB plus
CPU, memory and uptime. That is right for "some switches" and wrong the moment a
prospect says "Cisco Catalyst" - a real Catalyst reports per-supervisor CPU,
chassis temperature sensors, power-supply state and FRU status, none of which a
generic profile has.

Netdata already knows all of this. It ships 176 device profiles under
go.d/snmp.profiles/default, each naming its vendor and device type and inheriting
from shared MIB files. Resolving one profile's `extends` chain gives the metric
set that device actually reports.

## What this produces

    integrations/snmp-devices.json          the picker's vendor/model catalogue
    specs/generated/snmp/<profile>.yaml     one generator spec per device

## Fidelity: what is taken from the profile, and what is invented

Taken from the profile, so it matches what a real device would stream. Cited
against netdata/netdata src/go/plugin/go.d/collector/snmp:

  * the context id, `snmp.device_prof_<name>` (metric_ids.go:56);
  * the chart's units, verbatim from `chart_meta.unit` - UCUM strings like
    `By/s`, `{packet}/s`, `Cel`, and `1` when a profile declares none
    (charts.go:201,209);
  * the title from `chart_meta.description` and the family from
    `chart_meta.family`, so charts land in `Network/Interface/Traffic/Total`
    rather than a family this project made up;
  * area chart type for `bit/s`, as the collector draws it (charts.go:215);
  * one dimension per mapped state for an enum metric, because the collector
    expands a `mapping:` into a MultiValue metric with a 0/1 dimension per state
    (ddsnmp/transform.go:107-114);
  * `virtual_metrics`, the composed in/out charts. These carry interface traffic,
    packets, errors and discards for 94 of the profiles, and reading only
    `metrics:` missed all of them - a simulated switch with no traffic chart.

Invented: the values. A profile declares an OID and a unit, never a plausible
magnitude, so magnitudes come from the unit (a `%` rests near 35, a `Cel` near
41, an `{error}/s` at 0 until a scenario moves it). These specs are
**structurally** faithful and their absolute values are a plausible fiction -
the same contract as the collector-metadata specs.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import re

import yaml

# metric_ids.go:56 - the context is "snmp.device_prof_" plus the metric name with
# dots and spaces turned into underscores.
CONTEXT_PREFIX = "snmp.device_prof_"


def clean(name: str) -> str:
    return name.replace(".", "_").replace(" ", "_")


def ident(s: str) -> str:
    return re.sub(r"[^a-z0-9]+", "_", (s or "").lower()).strip("_")


# A device's standard-MIB metrics keep the signal names the generic switch spec
# uses, so one scenario degrades an uplink on a Catalyst, a Juniper MX and an F5
# alike. Without this every hero scenario would need a variant per vendor and the
# device specs would be unscriptable - most of their value gone.
#
# Keys are lowercased symbol names with any leading underscore stripped: the
# profiles collect `_ifHCInOctets` as a composition input and chart it through a
# virtual metric.
CANONICAL: dict[str, str] = {
    "ifhcinoctets": "if_in_octets_rate",
    "ifinoctets": "if_in_octets_rate",
    "ifhcoutoctets": "if_out_octets_rate",
    "ifoutoctets": "if_out_octets_rate",
    "ifhcinucastpkts": "if_in_ucast_rate",
    "ifinucastpkts": "if_in_ucast_rate",
    "ifhcoutucastpkts": "if_out_ucast_rate",
    "ifoutucastpkts": "if_out_ucast_rate",
    "ifhcinmulticastpkts": "if_in_mcast_rate",
    "ifhcinbroadcastpkts": "if_in_bcast_rate",
    "ifinerrors": "if_in_errors_rate",
    "ifouterrors": "if_out_errors_rate",
    "ifindiscards": "if_in_discards_rate",
    "ifoutdiscards": "if_out_discards_rate",
    "ifoperstatus": "if_oper_status",
    "ifadminstatus": "if_admin_status",
    "ifhighspeed": "if_speed",
    "ifspeed": "if_speed",
    "cpu.usage": "device_cpu_usage",
    "memory.usage": "device_memory_usage",
    "memory.total": "device_memory_total_bytes",
    "sysuptime": "uptime_tick",
    "sysuptimeinstance": "uptime_tick",
}

# UCUM unit -> (base, min, max, size_like). Rate-ness is decided separately, from
# the unit's `/s` suffix.
#
# `size_like` marks a quantity that describes the device rather than flowing
# through it - a port count, a fan count, an operational state. Those are held
# constant and never weight-scaled, so a low-weight instance does not report a
# scaled-down version of a fact about the hardware.
#
# Bytes are deliberately *not* size_like: a memory pool's free and used bytes
# move constantly, and a smaller pool genuinely holds fewer of them, so weight
# applies. Only a name that says "total" or "capacity" is fixed, and that is
# decided below by name.
UNIT_PROFILE: dict[str, tuple[float, float, float, bool]] = {
    "%": (35.0, 0.0, 100.0, False),
    "1": (12.0, 0.0, 1_000_000.0, False),
    "By": (4_294_967_296.0, 0.0, 137_438_953_472.0, False),
    "By/s": (3_100_000.0, 0.0, 1_250_000_000.0, False),
    "bit/s": (24_800_000.0, 0.0, 10_000_000_000.0, False),
    "Cel": (41.0, 0.0, 120.0, False),
    "degF": (106.0, 0.0, 250.0, False),
    "A": (3.2, 0.0, 200.0, False),
    "V": (12.0, 0.0, 480.0, False),
    "W": (145.0, 0.0, 4000.0, False),
    "daW": (14.5, 0.0, 400.0, False),
    "VA": (168.0, 0.0, 5000.0, False),
    "Hz": (60.0, 0.0, 120.0, False),
    "ms": (24.0, 0.0, 600_000.0, False),
    "s": (86_400.0, 0.0, 315_360_000.0, False),
    "us": (2400.0, 0.0, 60_000_000.0, False),
    "ns": (240_000.0, 0.0, 1_000_000_000.0, False),
    "{status}": (1.0, 1.0, 1.0, True),
    "{packet}": (0.0, 0.0, 1_000_000_000.0, False),
    "{packet}/s": (2800.0, 0.0, 15_000_000.0, False),
    "{error}/s": (0.0, 0.0, 500_000.0, False),
    "{discard}/s": (0.0, 0.0, 500_000.0, False),
    "{failure}/s": (0.0, 0.0, 500_000.0, False),
    "{frame}/s": (2600.0, 0.0, 15_000_000.0, False),
    "{request}/s": (340.0, 0.0, 2_000_000.0, False),
    "{connection}": (240.0, 0.0, 5_000_000.0, False),
    "{connection}/s": (46.0, 0.0, 500_000.0, False),
    "{session}": (180.0, 0.0, 5_000_000.0, False),
    "{session}/s": (24.0, 0.0, 500_000.0, False),
    "{user}": (34.0, 0.0, 100_000.0, False),
    "{client}": (52.0, 0.0, 500_000.0, False),
    "{interface}": (26.0, 1.0, 4096.0, True),
    "{entry}": (140.0, 0.0, 10_000_000.0, False),
    "{tunnel}": (18.0, 0.0, 65_536.0, False),
    "{cpu}": (2.0, 1.0, 1024.0, True),
    "{fan}": (4.0, 1.0, 64.0, True),
    "{disk}": (2.0, 1.0, 1024.0, True),
    "{process}": (180.0, 0.0, 100_000.0, False),
    "{thread}": (740.0, 0.0, 1_000_000.0, False),
    "{event}/s": (0.0, 0.0, 100_000.0, False),
    "{operation}/s": (240.0, 0.0, 5_000_000.0, False),
    "{alarm}": (0.0, 0.0, 10_000.0, False),
    "{rule}": (240.0, 0.0, 1_000_000.0, True),
    "{route}": (1840.0, 0.0, 10_000_000.0, False),
    "{peer}": (6.0, 0.0, 100_000.0, False),
    "{neighbor}": (6.0, 0.0, 100_000.0, False),
    "{license}": (4.0, 0.0, 10_000.0, True),
    "{byte}": (4_294_967_296.0, 0.0, 137_438_953_472.0, False),
    "%{battery}": (100.0, 0.0, 100.0, False),
    "rpm": (4200.0, 0.0, 30_000.0, False),
    "{revolution}/min": (4200.0, 0.0, 30_000.0, False),
}

# A count of something bad is zero on a healthy device, whatever its unit. The
# unit table cannot know this - `{entry}` is a routing table on one device and a
# discard counter on the next - so the name decides.
ZERO_BASE = re.compile(
    r"error|discard|drop|fail|crc|collision|reject|abort|oom|overflow|underrun|"
    r"retransmit|retrans|timeout|denied|deny|violation|fault|alarm|lost|loss"
)

# Words that mean "how big is it", not "how much is flowing through it".
FIXED_NAME = re.compile(r"total|size|capacity|max|limit|configured|installed|speed")


def unit_profile(name: str, unit: str) -> tuple[float, float, float, bool, bool]:
    """(base, min, max, is_rate, fixed) for a metric.

    `fixed` means the value is a fact about the device, not a measurement of it:
    installed memory, a port's rated speed, an operational state. A fixed signal
    is emitted as a declared constant - min equal to max - which is both truthful
    and what keeps the flat-signal check from reporting every capacity metric as
    a stuck driver. A scenario can still drive it.
    """
    is_rate = unit.endswith("/s")
    base, lo, hi, size_like = UNIT_PROFILE.get(unit, (120.0, 0.0, 1_000_000.0, False))
    low = name.lower()
    fixed = size_like or (not is_rate and bool(FIXED_NAME.search(low)))
    if ZERO_BASE.search(low) and not fixed:
        base, lo = 0.0, 0.0
    if fixed:
        lo = hi = base
    return base, lo, hi, is_rate, fixed


def signal_body(name: str, unit: str) -> dict:
    base, lo, hi, _, fixed = unit_profile(name, unit)
    sig: dict = {"base": base, "min": lo, "max": hi}
    if lo == hi:
        # A declared constant - a state, or a tick. The flat-signal check exempts
        # it and a scenario can still move it.
        sig.update({"min_is_floor": True, "max_is_ceiling": True})
    else:
        if hi == 100.0:
            sig["max_is_ceiling"] = True
        if base > 0 and not fixed:
            sig["seasonality"] = {
                "daily_amplitude": 0.45,
                "peak_hour": 14.0,
                "weekend_factor": 0.55,
            }
            sig["noise"] = {"kind": "gauss", "sigma": 0.16}
    if fixed:
        sig["ignore_weight"] = True
    return sig


def resolve(
    profile_dir: str, filename: str, seen: set[str] | None = None
) -> tuple[list, list]:
    """Every metric entry and virtual metric a profile reports, following `extends`.

    A device profile is mostly `extends`: cisco-catalyst.yaml declares nothing of
    its own and inherits its whole metric set from six base files. The virtual
    metrics live only in those shared files, so they have to be collected up the
    chain too.
    """
    seen = seen if seen is not None else set()
    if filename in seen:
        return [], []
    seen.add(filename)
    path = os.path.join(profile_dir, filename)
    if not os.path.exists(path):
        return [], []
    try:
        doc = yaml.safe_load(open(path)) or {}
    except yaml.YAMLError:
        return [], []
    metrics = list(doc.get("metrics") or [])
    virtual = list(doc.get("virtual_metrics") or [])
    for parent in doc.get("extends") or []:
        m, v = resolve(profile_dir, parent, seen)
        metrics += m
        virtual += v
    return metrics, virtual


def instance_group(tags: list[dict]) -> str | None:
    """Which instance list a table's rows expand over.

    A profile tags each table row - by `interface`, `cpu_index`, `temp_index`.
    Tags beginning with an underscore are metadata the collector attaches as
    labels rather than identity, so they do not create instances.
    """
    for tag in tags:
        name = tag.get("tag") or (tag.get("column") or {}).get("name") or ""
        if not name or name.startswith("_"):
            continue
        if "interface" in name or name in ("ifIndex", "if_index"):
            return "interface"
        return ident(name) or None
    return None


def signal_key(pid: str, raw: str, suffix: str = "") -> str:
    """Signal name for a symbol: canonical where one exists, else namespaced.

    The leading underscore of a composition input is dropped first, so
    `_ifHCInOctets` and `ifHCInOctets` name the same flow.
    """
    bare = raw.lstrip("_")
    key = CANONICAL.get(bare.lower()) or f"{pid}_{ident(bare)}"
    return f"{key}_{ident(suffix)}" if suffix else key


# Signals whose shape the generic switch spec already settled, and where reading
# it back off the unit would be wrong.
#
#   * uptime is a tick accumulated once per second, because a device reporting a
#     flat uptime is one of the cheapest tells that a fleet is synthetic;
#   * a port's rated speed comes from the port itself, so one spec serves a 1G
#     access port and a 10G uplink, and a port named TenGigabitEthernet never
#     reports 1000.
CANONICAL_BODY: dict[str, dict] = {
    "uptime_tick": {
        "base": 1.0,
        "min": 1.0,
        "max": 1.0,
        "min_is_floor": True,
        "max_is_ceiling": True,
    },
    "if_speed": {
        "from_attr": "if_speed_mbps",
        "base": 1000.0,
        "min": 0.0,
        "max": 400_000.0,
    },
}

# Signals that must accumulate whatever their declared unit says. An uptime in
# seconds is not a per-second rate, but it only advances if the engine adds to it
# every tick.
FORCE_RATE = {"uptime_tick"}

# States a healthy device sits in. Everything else starts at zero.
HEALTHY_STATES = {
    "up", "ok", "normal", "online", "active", "enabled", "good", "running",
    "operational", "present", "true", "yes", "available", "ready", "healthy",
    "connected", "established", "master", "primary", "inservice", "on",
}


class Device:
    """One device profile, being turned into a generator spec."""

    def __init__(self, pid: str):
        self.pid = pid
        self.signals: dict[str, dict] = {}
        self.contexts: list[dict] = []
        self.groups: set[str] = set()
        self.seen_ctx: set[str] = set()
        self.seen_prefix: set[str] = set()
        self.priority = 60000

    def prefix(self, raw: str) -> str:
        # The full name, not a truncation. Truncating collided
        # ciscoEnvMonTemperatureState with ciscoEnvMonTemperatureStatusValue on
        # one chart id, which makes the agent see a chart declared twice.
        p = ident(raw)
        while p in self.seen_prefix:
            p += "_x"
        self.seen_prefix.add(p)
        return p

    def add_signal(self, key: str, name: str, unit: str) -> str:
        if key not in self.signals:
            self.signals[key] = dict(CANONICAL_BODY.get(key) or signal_body(name, unit))
        return key

    def add_context(
        self,
        raw: str,
        meta: dict,
        dims: list[dict],
        is_rate: bool,
        group: str | None,
        table: str | None,
    ) -> None:
        ctx_id = CONTEXT_PREFIX + clean(raw)
        if ctx_id in self.seen_ctx or not dims:
            return
        self.seen_ctx.add(ctx_id)
        units = meta.get("unit") or "1"
        family = meta.get("family") or ident(table or raw)
        ctx: dict = {
            "id": ctx_id,
            # The collector's own fallbacks: the metric name in the title, the
            # metric name as a family (charts.go:198-212).
            "title": meta.get("description") or f"SNMP metric {raw}",
            "units": units,
            "family": family,
            # bit/s is drawn as an area, as the collector draws it.
            "chart_type": "area" if units == "bit/s" else "line",
            "priority": self.priority,
            "shape": "counters" if is_rate else "independent",
            "dimensions": dims,
        }
        if group:
            self.groups.add(group)
            ctx["instances"] = {
                "group": group,
                "chart_prefix": self.prefix(raw),
                "family": family,
                "labels": {group: "{instance}"},
            }
        self.contexts.append(ctx)
        self.priority += 1

    def add_symbol(self, sym: dict, table: str | None, group: str | None) -> None:
        """A charted symbol: one dimension, or one per mapped state."""
        raw = sym["name"]
        meta = sym.get("chart_meta") or {}
        unit = meta.get("unit") or "1"
        mapping = sym.get("mapping") or {}

        if mapping:
            self.add_context(
                raw, meta, self.state_dims(raw, mapping), False, group, table
            )
            return
        key = self.add_signal(signal_key(self.pid, raw), raw, unit)
        _, _, _, is_rate, _ = unit_profile(raw, unit)
        is_rate = is_rate or key in FORCE_RATE
        dim = {"id": ident(raw)[:28] or "value"}
        dim["rate_signal" if is_rate else "signal"] = key
        self.add_context(raw, meta, [dim], is_rate, group, table)

    def state_dims(self, raw: str, mapping: dict) -> list[dict]:
        """One 0/1 dimension per mapped state, as the collector emits.

        A profile's `mapping:` becomes a MultiValue metric - a dimension per state
        name, exactly one of them 1 (ddsnmp/transform.go:107-114). The healthy
        state is the one that reads 1 on a device nobody is paging about; every
        other state is a declared zero, which the flat-signal check exempts and a
        scenario can raise to simulate a fault.
        """
        states = [str(v) for _, v in sorted(mapping.items(), key=lambda kv: str(kv[0]))]
        if not states:
            return []
        healthy = next((s for s in states if s.lower() in HEALTHY_STATES), states[0])
        dims = []
        seen: set[str] = set()
        for state in states:
            dim_id = ident(state)[:28] or "state"
            if dim_id in seen:
                continue
            seen.add(dim_id)
            key = signal_key(self.pid, raw, "" if state == healthy else state)
            live = state == healthy
            self.signals.setdefault(
                key,
                {
                    "base": 1.0 if live else 0.0,
                    "min": 1.0 if live else 0.0,
                    "max": 1.0 if live else 0.0,
                    "min_is_floor": True,
                    "max_is_ceiling": True,
                    "ignore_weight": True,
                },
            )
            dims.append({"id": dim_id, "signal": key})
        return dims

    def add_virtual(
        self, vm: dict, symbols: dict[str, dict]
    ) -> None:
        """A composed chart: one dimension per source, in/out on one chart.

        This is where interface traffic, packets, errors and discards live for
        most profiles - the raw per-direction OIDs are collected as `_`-prefixed
        inputs and only ever charted through here.
        """
        raw = vm.get("name")
        sources = [s for s in (vm.get("sources") or []) if s.get("metric")]
        if not raw or not sources:
            return
        meta = vm.get("chart_meta") or {}
        unit = meta.get("unit") or "1"
        _, _, _, is_rate, _ = unit_profile(raw, unit)
        group = None
        if vm.get("per_row"):
            for by in vm.get("group_by") or []:
                group = "interface" if "interface" in by else ident(by)
                break

        # A single mapped source charts as its states aggregated - a count of
        # interfaces per operational status, not a flow.
        first = symbols.get(sources[0]["metric"].lstrip("_")) or {}
        if len(sources) == 1 and (first.get("mapping") or {}):
            self.add_context(
                raw, meta, self.state_dims(raw, first["mapping"]), False, group, None
            )
            return

        dims = []
        seen: set[str] = set()
        for src in sources:
            metric = src["metric"]
            as_name = src.get("as") or ident(metric.lstrip("_"))
            dim_id = ident(as_name)[:28] or "value"
            if dim_id in seen:
                continue
            seen.add(dim_id)
            key = self.add_signal(signal_key(self.pid, metric), metric, unit)
            dim: dict = {"id": dim_id}
            dim["rate_signal" if is_rate else "signal"] = key
            # Netdata draws egress below the axis when a chart carries both
            # directions, which is what makes a traffic chart readable.
            if len(sources) > 1 and as_name in ("out", "sent", "tx", "egress"):
                dim["multiplier"] = -1
            dims.append(dim)
        self.add_context(raw, meta, dims, is_rate, group, None)


def spec_for(profile_dir: str, filename: str) -> tuple[dict, dict] | None:
    doc = yaml.safe_load(open(os.path.join(profile_dir, filename))) or {}
    fields = ((doc.get("metadata") or {}).get("device") or {}).get("fields") or {}
    vendor = (fields.get("vendor") or {}).get("value")
    if not vendor:
        # No vendor metadata means a shared MIB fragment, not a device.
        return None
    device_type = (fields.get("type") or {}).get("value") or "Network device"
    pid = ident(filename[:-5])

    entries, virtual = resolve(profile_dir, filename)
    dev = Device(pid)

    # Index every symbol first: a virtual metric's sources name symbols that may
    # be declared in a different file of the same extends chain.
    symbols: dict[str, dict] = {}
    charted: list[tuple[dict, str | None, str | None]] = []
    for entry in entries:
        table = (entry.get("table") or {}).get("name")
        syms = [
            s
            for s in (entry.get("symbols") or [])
            if isinstance(s, dict) and s.get("name")
        ]
        if not syms and isinstance(entry.get("symbol"), dict):
            if entry["symbol"].get("name"):
                syms = [entry["symbol"]]
        group = instance_group(entry.get("metric_tags") or []) if table else None
        for sym in syms:
            symbols.setdefault(sym["name"].lstrip("_"), sym)
            # A leading underscore marks a composition or tagging input, charted
            # only through a virtual metric.
            if not sym["name"].startswith("_"):
                charted.append((sym, table, group))

    for sym, table, group in charted:
        dev.add_symbol(sym, table, group)
    for vm in virtual:
        dev.add_virtual(vm, symbols)

    if not dev.contexts:
        return None

    spec = {
        "version": 1,
        "name": pid,
        "description": (
            f"{vendor} {device_type}, as Netdata's SNMP collector reports it. "
            f"Generated from go.d/snmp.profiles/default/{filename} with its "
            f"`extends` chain resolved. Contexts, units, families and metric "
            f"names are the device's own; values are a plausible fiction."
        ),
        "signals": dev.signals,
        "contexts": dev.contexts,
    }
    entry = {
        "id": pid,
        "vendor": vendor,
        "device_type": device_type,
        "profile": filename,
        "charts": len(dev.contexts),
        "instance_groups": sorted(dev.groups),
    }
    return spec, entry


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--netdata", required=True, help="path to a netdata checkout")
    ap.add_argument("--out", default=".")
    args = ap.parse_args()

    profile_dir = os.path.join(
        args.netdata, "src/go/plugin/go.d/config/go.d/snmp.profiles/default"
    )
    if not os.path.isdir(profile_dir):
        print(f"no SNMP profiles at {profile_dir}")
        return 1

    spec_dir = os.path.join(args.out, "specs", "generated", "snmp")
    os.makedirs(spec_dir, exist_ok=True)
    os.makedirs(os.path.join(args.out, "integrations"), exist_ok=True)

    catalogue = []
    for path in sorted(glob.glob(os.path.join(profile_dir, "*.yaml"))):
        filename = os.path.basename(path)
        if filename.startswith("_"):
            continue
        try:
            built = spec_for(profile_dir, filename)
        except Exception as e:  # a malformed profile is skipped, not fatal
            print(f"  skipped {filename}: {e}")
            continue
        if not built:
            continue
        spec, entry = built
        with open(os.path.join(spec_dir, f"{entry['id']}.yaml"), "w") as fh:
            yaml.safe_dump(spec, fh, sort_keys=False, width=120)
        catalogue.append(entry)

    catalogue.sort(key=lambda c: (c["vendor"].lower(), c["device_type"], c["id"]))
    out = os.path.join(args.out, "integrations", "snmp-devices.json")
    with open(out, "w") as fh:
        json.dump({"devices": catalogue}, fh, indent=1)
        fh.write("\n")

    vendors = sorted({c["vendor"] for c in catalogue})
    print(f"wrote {len(catalogue)} device specs to {spec_dir}")
    print(f"catalogue: {len(catalogue)} devices, {len(vendors)} vendors -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
