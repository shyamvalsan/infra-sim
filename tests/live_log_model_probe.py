#!/usr/bin/env python3
"""Opt-in two-node additive-fault correlation probe through actual Netdata."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from urllib.parse import urlencode
from urllib.request import urlopen
from urllib.error import HTTPError
import uuid

from live_agent_smoke import ROOT, run, wait_for
from live_composition_probe import freeze_signal


def main():
    if os.geteuid() != 0:
        raise SystemExit("run with sudo: this probe owns disposable journal files")
    name = "sim-log-model-" + uuid.uuid4().hex[:8]
    container = "infra-sim-" + name
    hosts = [name + "-01", name + "-02"]
    work = Path(tempfile.mkdtemp(prefix="infra-sim-log-model-"))
    environment = work / "environment.yaml"
    nodes = "".join(f'''  - hostname: {host}
    guid: {uuid.uuid4()}
    role: db
    services: [postgres]
    attrs: {{cores: 4, ram_total_kb: 67108864, swap_total_kb: 4194304, inodes_total: 8000000, disk_total_kb: 1572864000}}
    instances:
      net: [{{name: eth0, weight: 1.0}}]
      disk: [{{name: sda, weight: 1.0}}]
      mount: [{{name: /, weight: 1.0, attrs: {{disk_total_kb: 1572864000}}}}]
''' for host in hosts)
    environment.write_text(f'''version: 1
name: {name}
seed: 421
update_every: 1
warmup_incidents: false
generator: specs/linux-system.yaml
specs: specs
scenarios: scenarios
nodes:
{nodes}''')
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, INFRA_SIM_STATE_DIR=str(work / "state"))
    for key in ("NETDATA_CLAIM_TOKEN", "NETDATA_CLAIM_ROOMS", "INFRA_SIM_CLAIM_ROOMS"):
        env.pop(key, None)
    script = str(ROOT / "scripts/sim-docker.sh")

    def query(path):
        with urlopen(f"http://127.0.0.1:{port}" + path, timeout=15) as response:
            return json.load(response)

    def metric(host, chart, expected):
        data = query(f"/host/{host}/api/v1/data?" + urlencode({"chart": chart, "after": -5, "points": 5}))
        values = [row[1] for row in data.get("data", []) if row[1] is not None]
        return len(values) >= 3 and all(abs(value - expected) < 0.01 for value in values)

    def journal(host):
        raw = run("docker", "exec", container, "journalctl", "--file=" + f"/var/log/journal/remote/remote-{host}.journal", "--no-pager", "-o", "json", "-n", "1000")
        return [json.loads(line) for line in raw.splitlines() if line.startswith("{")]

    def faults_visible():
        entries = journal(hosts[0])
        return all(any(phrase in row.get("MESSAGE", "") for row in entries) for phrase in ("OOM kills observed at 1.00", "eth0: interface errors 40.00/s"))

    try:
        run("bash", script, "create", name, str(environment), "--port", str(port), "--no-exporters", env=env)
        run("docker", "stop", container)
        payload = work / "state" / name
        for signal in ("oom_kill_rate", "net_err_rate"):
            freeze_signal(payload / "specs/linux-system.yaml", signal)
        (payload / "scenarios/sim-probe-additive.yaml").write_text('''version: 1
name: sim-probe-additive
description: Synthetic additive fault probe
requires_roles: [db]
manifest: {root_cause: synthetic additive fault}
timeline:
  - at: 0s
    description: Add OOM events to the first node
    target: {role: db, node_index: 1, signal: oom_kill_rate}
    effect: add
    amount: 1.0
  - at: 0s
    description: Add interface errors to the first node
    target: {role: db, node_index: 1, instance: eth0, signal: net_err_rate}
    effect: add
    amount: 40.0
''')
        (payload / "control.yaml").write_text(f"active:\n  - scenario: sim-probe-additive\n    started_at: {int(time.time())}\n")
        run("docker", "start", container)
        for host, expected in ((hosts[0], 1.0), (hosts[1], 0.0)):
            wait_for(f"{host} OOM rate = {expected} through Netdata", lambda h=host, e=expected: metric(h, "mem.oom_kill", e))
        wait_for("targeted additive fault readings appear in journal messages", faults_visible, timeout=240)
        untouched = journal(hosts[1])
        assert all("OOM kills" not in row.get("MESSAGE", "") and "interface errors" not in row.get("MESSAGE", "") for row in untouched)
        targeted = journal(hosts[0])
        assert all(str(row.get("_PID")) == "0" for row in targeted if row.get("SYSLOG_IDENTIFIER") == "kernel")
        function = "systemd-journal after:-300 before:0 last:1000 __logs_sources:all-remote-systems"

        def agent_logs():
            try:
                data = query("/api/v1/function?" + urlencode({"function": function}))
            except HTTPError as error:
                if error.code == 412:
                    raise Exception("Netdata log-query acceptance requires authenticated Cloud SSO; metrics and journal checks passed, function acceptance remains blocked") from error
                raise
            (work / "agent-logs.json").write_text(json.dumps(data))
            text = json.dumps(data)
            return "OOM kills observed at 1.00" in text and "interface errors 40.00/s" in text

        wait_for("Netdata journal function returns the correlated messages", agent_logs)
        print("PASS: additive OOM/interface faults reach metrics and real Netdata logs; untargeted node stays quiet", flush=True)
    finally:
        exists = subprocess.run(["docker", "container", "inspect", "--format", "{{.Id}}", container], capture_output=True).returncode == 0
        if exists:
            run("bash", script, "teardown", name, env=env)
        print(f"Probe inputs retained at {work}", flush=True)


if __name__ == "__main__":
    main()
