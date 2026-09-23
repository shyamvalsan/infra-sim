#!/usr/bin/env python3
"""Opt-in one-node probe of base/service role composition through real Netdata."""
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import tempfile
from urllib.request import urlopen
import uuid

from live_agent_smoke import ROOT, run, wait_for


def freeze_signal(path, signal):
    """Remove random/time variation in the disposable fixture, retaining roles."""
    text = path.read_text()
    pattern = rf"(?ms)^  {re.escape(signal)}:\n.*?(?=^  \w+:|^\S|\Z)"

    def replace(match):
        block = re.sub(r"(?m)^    (noise|seasonality):.*\n", "", match.group())
        return block + "    noise: {kind: none}\n    seasonality: {daily_amplitude: 0, weekend_factor: 1}\n"

    text, count = re.subn(pattern, replace, text)
    if count != 1:
        raise RuntimeError(f"expected exactly one definition of {signal}")
    path.write_text(text)


def main():
    if os.geteuid() != 0:
        raise SystemExit("run with sudo: this probe owns a disposable container payload")
    name = "sim-compose-" + uuid.uuid4().hex[:10]
    container = "infra-sim-" + name
    host = name + "-01"
    work = Path(tempfile.mkdtemp(prefix="infra-sim-composition-"))
    environment = work / "environment.yaml"
    environment.write_text(f'''version: 1
name: {name}
seed: 421
update_every: 1
warmup_incidents: false
generator: specs/linux-system.yaml
specs: specs
scenarios: scenarios
nodes:
  - hostname: {host}
    guid: {uuid.uuid4()}
    role: db
    services: [postgres]
    attrs: {{cores: 4, ram_total_kb: 67108864, swap_total_kb: 4194304, inodes_total: 8000000, disk_total_kb: 1572864000}}
    instances:
      net: [{{name: eth0, weight: 1.0}}]
      disk: [{{name: sda, weight: 1.0}}]
      mount: [{{name: /, weight: 1.0, attrs: {{disk_total_kb: 1572864000}}}}]
''')
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, INFRA_SIM_STATE_DIR=str(work / "state"))
    for key in ("NETDATA_CLAIM_TOKEN", "NETDATA_CLAIM_ROOMS", "INFRA_SIM_CLAIM_ROOMS"):
        env.pop(key, None)
    script = str(ROOT / "scripts/sim-docker.sh")
    try:
        run("bash", script, "create", name, str(environment), "--port", str(port), "--no-exporters", env=env)
        payload = work / "state" / name
        # Stop only this probe's container before modifying its private input copies.
        run("docker", "stop", container)
        freeze_signal(payload / "specs/linux-system.yaml", "tcp_sockets_inuse")
        freeze_signal(payload / "specs/postgres.yaml", "pg_connections_used")
        run("docker", "start", container)

        def reading(chart, dimension, expected):
            path = f"http://127.0.0.1:{port}/host/{host}/api/v1/data?chart={chart}&after=-5&points=5&group=average"
            with urlopen(path, timeout=5) as response:
                data = json.load(response)
            index = data["labels"].index(dimension)
            values = [row[index] for row in data.get("data", []) if row[index] is not None]
            return len(values) >= 3 and all(abs(value - expected) < 0.01 for value in values)

        for chart, dimension, expected in (
            ("ip.tcpsock", "connections", 320.0),
            ("postgres.connections_usage", "used", 132.0),
            ("postgres.connections_utilization", "used", 33.0),
        ):
            wait_for(f"{chart}/{dimension} = {expected} through Netdata", lambda c=chart, d=dimension, e=expected: reading(c, d, e))
        print("PASS: base and service role settings both reach Netdata; 132/400 connections displays as 33%", flush=True)
    finally:
        exists = subprocess.run(["docker", "container", "inspect", "--format", "{{.Id}}", container], capture_output=True).returncode == 0
        if exists:
            run("bash", script, "teardown", name, env=env)
        print(f"Probe inputs retained at {work}", flush=True)


if __name__ == "__main__":
    main()
