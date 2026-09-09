"""HTTP contracts against the built console, with Docker fully isolated."""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import unittest
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


class ConsoleApiTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        binary = ROOT / "target/debug/infra-sim-console"
        if not binary.exists():
            raise unittest.SkipTest("cargo build -p sim-console first")
        cls.tmp = tempfile.TemporaryDirectory(prefix="sim-api-")
        cls.root = Path(cls.tmp.name)
        fake = cls.root / "docker"
        fake.write_text('#!/bin/sh\nexit 0\n')
        fake.chmod(0o755)
        cls.policy = cls.root / "console.yaml"
        cls.policy.write_text("ttl_days: 30\n")
        (cls.root / "control.yaml").write_text("active: []\n")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        cls.url = f"http://127.0.0.1:{port}"
        env = dict(os.environ, PATH=str(cls.root) + os.pathsep + os.environ["PATH"],
                   INFRA_SIM_STATE_DIR=str(cls.root / "state"))
        env.pop("INFRA_SIM_TOKEN", None)
        cls.process = subprocess.Popen([str(binary), "--bind", f"127.0.0.1:{port}",
                                       "--repo", str(cls.root), "--budgets", str(cls.policy),
                                       "--environment", str(cls.root / "environment.yaml")],
                                      env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                with urlopen(cls.url, timeout=1):
                    return
            except URLError:
                if cls.process.poll() is not None:
                    raise RuntimeError("console exited before startup")
                time.sleep(0.05)
        cls.process.terminate()
        cls.process.wait(timeout=5)
        raise RuntimeError("console did not start")

    @classmethod
    def tearDownClass(cls):
        cls.process.send_signal(signal.SIGINT)
        cls.process.wait(timeout=10)
        cls.tmp.cleanup()

    def request(self, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        request = Request(self.url + path, data=data, headers={"Content-Type": "application/json"})
        try:
            with urlopen(request, timeout=5) as response:
                return response.status, response.read().decode()
        except HTTPError as error:
            try:
                return error.code, error.read().decode()
            finally:
                error.close()

    def test_teardown_requires_name(self):
        for body in ({}, {"name": ""}, {"name": "  "}):
            code, _ = self.request("/api/teardown", body)
            self.assertEqual(code, 422)

    def test_clock_route_propagates_inactive_scenario_error(self):
        code, body = self.request("/api/sim/local/scenario/disk-fill/advance", {"seconds": 300})
        self.assertEqual(code, 400)
        self.assertIn("not running", body)

    def test_invalid_policy_refuses_create_and_marks_health(self):
        self.policy.write_text("ttl_days: invalid\n")
        try:
            _, body = self.request("/api/create", {"name": "sim-test", "owner": "test", "groups": []})
            self.assertIn("console.yaml", body)
            code, body = self.request("/api/health")
            self.assertEqual(code, 200)
            self.assertFalse(json.loads(body)["policy_ok"])
            self.assertFalse(json.loads(body)["ok"])
            self.assertNotIn(str(self.policy), body)
        finally:
            self.policy.write_text("ttl_days: 30\n")


if __name__ == "__main__":
    unittest.main()
