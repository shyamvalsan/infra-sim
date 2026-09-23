#!/usr/bin/env python3
"""Opt-in one-vnode acceptance; owns only its uniquely named container/payload."""
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import struct
import tempfile
import time
from urllib.request import urlopen
import uuid

ROOT = Path(__file__).resolve().parents[1]


def run(*args, env=None, timeout=180):
    print(f"{ROOT} > {shlex.join(map(str, args))}", flush=True)
    result = subprocess.run(args, cwd=ROOT, env=env, text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise RuntimeError(
            f"command failed ({result.returncode}): {shlex.join(map(str, args))}\n"
            f"stdout:\n{result.stdout[-3000:]}\nstderr:\n{result.stderr[-3000:]}"
        )
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
    attrs: {{cores: 4, ram_total_kb: 8388608, swap_total_kb: 4194304, inodes_total: 8000000, disk_total_kb: 524288000}}
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
        # Healthy modeled nodes can be silent. Exercise a real fault before
        # requiring journal-boundary capture instead of inventing baseline logs.
        control = work / "state" / name / "control.yaml"
        control.write_text(json.dumps({"active": [{
            "scenario": "memory-leak-oom", "started_at": int(time.time()) - 24 * 60
        }]}))
        recording = work / "state" / name / "recording"

        def all_boundaries_recorded():
            kinds = set()
            journal_frames = 0
            limit = struct.unpack("<Q", (recording / "committed").read_bytes())[0]
            with (recording / "events.bin").open("rb") as source:
                assert source.read(8) == b"ISIMREC1"
                while source.tell() < limit:
                    metadata_len, payload_len = struct.unpack("<IQ", source.read(12))
                    metadata = json.loads(source.read(metadata_len))
                    source.seek(payload_len, 1)
                    kinds.add(metadata["kind"])
                    journal_frames += metadata["kind"] == "journal"
            # Several journal frames, so replay compares more than one entry.
            return journal_frames >= 5 and {"metrics", "otlp_logs", "otlp_traces", "exporter"} <= kinds

        wait_for("all raw producer boundaries recorded", all_boundaries_recorded, timeout=240)
        print("PASS: one-vnode live lifecycle; no Cloud or fidelity acceptance claimed", flush=True)
    finally:
        exists = subprocess.run(["docker", "container", "inspect", "--format", "{{.Id}}", container], capture_output=True).returncode == 0
        if exists:
            run("bash", script, "teardown", name, env=env)
            archives = list((ROOT / "archive").glob(name + "-*"))
            assert len(archives) == 1, "expected exactly this test's archive"
            archive = archives[0]
            assert json.loads((archive / "archive.json").read_text())["raw_recording"]
            assert (archive / "recording/finalized").is_file()
            incomplete = archive / "recording/incomplete"
            assert not incomplete.exists(), incomplete.read_text() if incomplete.exists() else ""
            print("PASS: self-contained archive finalized without recording gaps", flush=True)
        elif created:
            raise RuntimeError("test container disappeared before teardown verification")
        # Retain test archives and diagnostic inputs; never remove unrelated state.
        print(f"Test inputs retained at {work}", flush=True)


if __name__ == "__main__":
    main()
