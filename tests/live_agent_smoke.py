#!/usr/bin/env python3
"""Opt-in one-vnode acceptance; owns only its uniquely named container/payload."""
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import tempfile
import time
from urllib.request import urlopen
import uuid

ROOT = Path(__file__).resolve().parents[1]


def run(*args, env=None, timeout=180):
    print(f"{ROOT} > {shlex.join(map(str, args))}", flush=True)
    result = subprocess.run(args, cwd=ROOT, env=env, text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {result.stderr[-3000:]}")
    return result.stdout


def wait_for(description, probe, timeout=180):
    deadline = time.monotonic() + timeout
    error = None
    while time.monotonic() < deadline:
        try:
            if probe():
                print(f"PASS: {description}", flush=True)
                return
        except (OSError, ValueError, RuntimeError) as exc:
            error = exc
        time.sleep(2)
    raise RuntimeError(f"timed out: {description}; last error: {error}")


def main():
    if os.geteuid() != 0:
        raise SystemExit("run with sudo: the container creates root-owned journal files")
    name = "sim-check-" + uuid.uuid4().hex[:10]
    container = "infra-sim-" + name
    work = Path(tempfile.mkdtemp(prefix="infra-sim-live-"))
    environment = work / "environment.yaml"
    environment.write_text(f'''version: 1
name: {name}
seed: 421
update_every: 1
generator: specs/linux-system.yaml
specs: specs
scenarios: scenarios
nodes:
  - hostname: {name}-01
    guid: {uuid.uuid4()}
    role: web
    services: [nginx]
    attrs: {{cores: 4, ram_total_kb: 8388608, disk_total_kb: 524288000}}
    instances:
      net: [{{name: eth0, weight: 1.0}}]
      disk: [{{name: sda, weight: 1.0}}]
      mount: [{{name: /, weight: 1.0, attrs: {{disk_total_kb: 524288000}}}}]
''')
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = dict(os.environ, INFRA_SIM_STATE_DIR=str(work / "state"))
    # Acceptance never claims to Cloud, including on an operator's configured host.
    for key in ("NETDATA_CLAIM_TOKEN", "NETDATA_CLAIM_ROOMS", "INFRA_SIM_CLAIM_ROOMS"):
        env.pop(key, None)
    script = str(ROOT / "scripts/sim-docker.sh")
    base = f"http://127.0.0.1:{port}"

    def query(path):
        with urlopen(base + path, timeout=5) as response:
            return json.load(response)

    def telemetry():
        processes = run("docker", "exec", container, "ps", "-eo", "args")
        for mode in ("logs", "otlp", "exporters"):
            prefix = f"/etc/netdata/custom-plugins.d/infra-sim.plugin --{mode}"
            if sum(line.startswith(prefix) for line in processes.splitlines()) != 1:
                return False
        return True

    created = False
    try:
        # Track our exact container even when creation fails after Docker starts it.
        run("bash", script, "create", name, str(environment), "--port", str(port), env=env)
        created = True
        wait_for("vnode registered", lambda: any(n.get("nm") == name + "-01" for n in query("/api/v3/nodes").get("nodes", [])))
        wait_for("CPU samples collected by Netdata", lambda: any(any(v is not None for v in row[1:]) for row in query(f"/host/{name}-01/api/v1/data?chart=system.cpu&after=-30").get("data", [])))
        wait_for("one process per telemetry mode", telemetry)
        run("bash", script, "telemetry", name, "stop", env=env)
        run("bash", script, "telemetry", name, "start", env=env)
        wait_for("standalone telemetry restart", telemetry)
        run("docker", "restart", container)
        wait_for("telemetry survives container restart", telemetry)
        wait_for("Netdata collects after restart", lambda: bool(query(f"/host/{name}-01/api/v1/charts").get("charts")))
        print("PASS: one-vnode live lifecycle; no Cloud or fidelity acceptance claimed", flush=True)
    finally:
        exists = subprocess.run(["docker", "container", "inspect", "--format", "{{.Id}}", container], capture_output=True).returncode == 0
        if exists:
            run("bash", script, "teardown", name, env=env)
        elif created:
            raise RuntimeError("test container disappeared before teardown verification")
        # Retain test archives and diagnostic inputs; never remove unrelated state.
        print(f"Test inputs retained at {work}", flush=True)


if __name__ == "__main__":
    main()
