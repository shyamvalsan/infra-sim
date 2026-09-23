#!/usr/bin/env python3
"""Real producer/replay processes, isolated synthetic inputs and owned PIDs."""
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import time
import unittest
from urllib.error import URLError
from urllib.request import urlopen
import uuid

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get("INFRA_SIM_TEST_BINARY", ROOT / "target/debug/infra-sim"))


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def frames(directory):
    limit = struct.unpack("<Q", (directory / "committed").read_bytes())[0]
    with (directory / "events.bin").open("rb") as source:
        assert source.read(8) == b"ISIMREC1"
        while source.tell() < limit:
            metadata_len, payload_len = struct.unpack("<IQ", source.read(12))
            metadata = json.loads(source.read(metadata_len))
            yield metadata, source.read(payload_len)


class RecordingRuntime(unittest.TestCase):
    def setUp(self):
        if not BINARY.is_file():
            self.fail("build infra-sim first, or set INFRA_SIM_TEST_BINARY")
        self.work = Path(tempfile.mkdtemp(prefix="infra-sim-recording-runtime-"))
        self.recording = self.work / "recording"
        self.children = []
        self.handles = []
        self.environment = self.work / "environment.yaml"
        nodes = "\n".join(
            f"  - hostname: sim-record-{i}\n    guid: {uuid.uuid4()}\n"
            "    role: web\n    services: [nginx]\n"
            "    attrs: {cores: 4, ram_total_kb: 8388608, swap_total_kb: 4194304, disk_total_kb: 524288000}"
            for i in range(2)
        )
        self.environment.write_text(
            "version: 1\nname: sim-record\nseed: 421\nupdate_every: 1\nwarmup_incidents: false\n"
            f"generator: {ROOT}/specs/linux-system.yaml\nspecs: {ROOT}/specs\n"
            f"scenarios: {ROOT}/scenarios\nnodes:\n{nodes}\n"
        )

    def tearDown(self):
        for child in self.children:
            if child.poll() is None:
                child.send_signal(signal.SIGTERM)
                try:
                    child.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
        for handle in self.handles:
            handle.close()
        # Retain exact inputs and raw output only for failed-run diagnosis.
        result = getattr(getattr(self, "_outcome", None), "result", None)
        failed = result is None or any(
            test is self for test, _ in [*result.errors, *result.failures])
        if not failed:
            shutil.rmtree(self.work, ignore_errors=True)

    def start(self, *args, recording=True, output="output", expected=None):
        stdout = (self.work / output).open("wb")
        stderr = (self.work / f"{output}.stderr").open("wb")
        self.handles.extend([stdout, stderr])
        env = dict(os.environ)
        env.pop("INFRA_SIM_RECORD_DIR", None)
        env.pop("INFRA_SIM_RECORD_MAX_BYTES", None)
        if recording:
            env.update(INFRA_SIM_RECORD_DIR=str(self.recording), INFRA_SIM_RECORD_MAX_BYTES="10000000")
        env.pop("INFRA_SIM_RECORD_EXPECTED", None)
        if expected:
            env["INFRA_SIM_RECORD_EXPECTED"] = expected
        child = subprocess.Popen([str(BINARY), "--environment", str(self.environment), *args],
                                 stdout=stdout, stderr=stderr, env=env)
        self.children.append(child)
        return child

    def command(self, *args):
        return subprocess.run([str(BINARY), *map(str, args)], capture_output=True, timeout=15, check=True)

    def stop(self, child):
        child.send_signal(signal.SIGTERM)
        self.assertEqual(child.wait(timeout=15), 0)

    def scrape(self, port, route):
        deadline = time.monotonic() + 10
        while True:
            try:
                with urlopen(f"http://127.0.0.1:{port}{route}", timeout=5) as response:
                    return response.read()
            except (OSError, URLError):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.05)

    def test_exporters_on_a_fleet_without_application_nodes_record_a_clean_session(self):
        # Create enables exporters on every fleet; with no web, lb or k8s-worker
        # node the exporter process serves nothing but must still account for
        # its expected session, or every such archive would read incomplete.
        text = self.environment.read_text().replace("role: web\n    services: [nginx]", "role: db")
        self.environment.write_text(text)
        port = free_port()
        child = self.start("--exporters", "--exporter-port", str(port), output="exporters",
                           expected="exporters")
        deadline = time.monotonic() + 10
        while b"endpoint(s)" not in (self.work / "exporters.stderr").read_bytes():
            self.assertLess(time.monotonic(), deadline, "exporters never started")
            time.sleep(0.05)
        self.assertIn(b"0 endpoint(s)", (self.work / "exporters.stderr").read_bytes())
        self.stop(child)
        status = json.loads(self.command("--finalize-recording", self.recording).stdout)
        self.assertIsNone(status["incomplete"])
        kinds = [metadata["kind"] for metadata, _ in frames(self.recording)]
        self.assertEqual((kinds.count("start"), kinds.count("stop")), (1, 1))

    def test_metrics_clean_shutdown_and_byte_exact_replay(self):
        child = self.start(output="metrics")
        deadline = time.monotonic() + 10
        while (self.work / "metrics").stat().st_size == 0 and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertGreater((self.work / "metrics").stat().st_size, 0)
        time.sleep(1.2)
        self.stop(child)
        status = json.loads(self.command("--finalize-recording", self.recording).stdout)
        self.assertIsNone(status["incomplete"])
        replay = self.command("--replay-recording", self.recording).stdout
        self.assertEqual(replay, (self.work / "metrics").read_bytes())
        self.assertIn(b"BEGIN", replay)

    def test_exporter_routes_replay_independently_in_different_scrape_order(self):
        port = free_port()
        child = self.start("--exporters", "--exporter-port", str(port), output="exporters")
        route_a, route_b = "/metrics/sim-record-0", "/metrics/sim-record-1"
        expected_a = self.scrape(port, route_a)
        expected_b = self.scrape(port, route_b)
        self.stop(child)
        status = json.loads(self.command("--finalize-recording", self.recording).stdout)
        self.assertIsNone(status["incomplete"])
        self.assertEqual(sum(m["kind"] == "exporter" for m, _ in frames(self.recording)), 2)
        replay_port = free_port()
        replay = self.start("--replay-recording", str(self.recording), "--exporters",
                            "--exporter-port", str(replay_port), recording=False, output="replayed")
        self.assertEqual(self.scrape(replay_port, route_b), expected_b)
        self.assertEqual(self.scrape(replay_port, route_a), expected_a)
        self.assertEqual(replay.wait(timeout=10), 0)

    def test_interrupted_session_requires_explicit_prefix_replay(self):
        child = self.start(output="interrupted")
        deadline = time.monotonic() + 10
        while (self.work / "interrupted").stat().st_size == 0 and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertGreater((self.work / "interrupted").stat().st_size, 0)
        child.kill()
        child.wait(timeout=5)
        status = json.loads(self.command("--finalize-recording", self.recording).stdout)
        self.assertIsNotNone(status["incomplete"])
        refused = subprocess.run([str(BINARY), "--replay-recording", str(self.recording)], capture_output=True, timeout=10)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(refused.stdout, b"")
        self.assertIn(b"--allow-incomplete-recording", refused.stderr)
        self.assertTrue(self.command("--replay-recording", self.recording, "--allow-incomplete-recording").stdout)

    def test_lint_evidence_is_opt_in_and_records_failure(self):
        scenarios = self.work / "scenarios"
        scenarios.mkdir()
        self.environment.write_text(self.environment.read_text().replace(
            f"scenarios: {ROOT}/scenarios", f"scenarios: {scenarios}"))
        evidence = self.work / "lint-evidence.json"
        self.command("--environment", self.environment, "--lint", "1")
        self.assertFalse(evidence.exists())
        self.command("--environment", self.environment, "--lint", "1", "--lint-evidence", evidence)
        self.assertTrue(json.loads(evidence.read_text())["passed"])
        (scenarios / "invalid-target.yaml").write_text(
            "version: 1\nname: invalid-target\nmanifest: {root_cause: synthetic}\n"
            "timeline:\n  - at: 0s\n    target: {signal: nonexistent_synthetic_signal}\n"
            "    effect: add\n    amount: 1\n")
        result = subprocess.run([str(BINARY), "--environment", str(self.environment), "--lint", "1",
                                 "--lint-evidence", str(evidence)], capture_output=True, timeout=15)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(json.loads(evidence.read_text())["passed"])


if __name__ == "__main__":
    unittest.main()
