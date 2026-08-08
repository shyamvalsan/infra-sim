#!/usr/bin/env python3
"""Trigger or resolve a scenario in a simulation's control file.

Used by scripts/sim-docker.sh, which manages containerised simulations. The
console does the same job for host installs through sim-engine's ControlFile;
this is the container path's equivalent, kept deliberately small.

The file is rewritten **in place** rather than replaced. A containerised
simulation has its payload directory bind-mounted, and a replace-and-rename
would leave the container reading a stale inode - which is exactly how a
triggered scenario came to be visible on the host and invisible inside the
container.
"""

from __future__ import annotations

import sys
import time


def parse(text: str) -> list[dict[str, str]]:
    """Read the entries out of a control file.

    Deliberately not a YAML parser: this file has one shape, written only by
    this script and by the console, and a dependency for it would be silly.
    """
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("- scenario:"):
            current = {"scenario": stripped.split(":", 1)[1].strip()}
            entries.append(current)
        elif current is not None and ":" in stripped and not stripped.startswith("-"):
            key, value = stripped.split(":", 1)
            current[key.strip()] = value.strip()
    return entries


def render(entries: list[dict[str, str]]) -> str:
    if not entries:
        return "active: []\n"
    out = ["active:"]
    for entry in entries:
        out.append(f"- scenario: {entry['scenario']}")
        for key, value in entry.items():
            if key != "scenario":
                out.append(f"  {key}: {value}")
    return "\n".join(out) + "\n"


def main() -> int:
    if len(sys.argv) != 4:
        print("usage: control_file.py <control.yaml> trigger|resolve <scenario>", file=sys.stderr)
        return 2
    path, action, name = sys.argv[1], sys.argv[2], sys.argv[3]

    try:
        text = open(path).read()
    except OSError as e:
        print(f"cannot read {path}: {e}", file=sys.stderr)
        return 1

    entries = parse(text)
    now = int(time.time())

    if action == "trigger":
        existing = next((e for e in entries if e["scenario"] == name), None)
        if existing is not None:
            # Re-triggering cancels an unwind and keeps the original start time,
            # so the fault resumes where it was rather than replaying.
            existing.pop("recovering_since", None)
        else:
            # started_at is written explicitly. Without it the plugin assigns
            # "now" on first read, so a restart rewinds a running scenario -
            # indistinguishable on screen from the fault resolving itself.
            entries.append({"scenario": name, "started_at": str(now)})
    elif action == "resolve":
        if not any(e["scenario"] == name for e in entries):
            print(f"'{name}' is not running", file=sys.stderr)
            return 1
        for entry in entries:
            if entry["scenario"] == name:
                # Marked rather than removed, so the fault unwinds over the
                # engine's recovery window instead of clearing between two
                # samples.
                entry.setdefault("recovering_since", str(now))
    else:
        print("action must be trigger or resolve", file=sys.stderr)
        return 2

    with open(path, "r+") as fh:
        fh.write(render(entries))
        fh.truncate()
    return 0


if __name__ == "__main__":
    sys.exit(main())
