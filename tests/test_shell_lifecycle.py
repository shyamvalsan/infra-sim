"""Exercise real shell functions with isolated payloads and a fake Docker CLI."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = (ROOT / "scripts/sim-docker.sh").read_text()
FUNCTIONS = SOURCE[SOURCE.index("cmd_teardown() {"):SOURCE.index("# --- dispatch")]
RUN = SOURCE[SOURCE.index("run() {"):SOURCE.index("\ndie()")]


class LifecycleTests(unittest.TestCase):
    def test_all_command_helpers_preserve_failure(self):
        scripts = list((ROOT / "scripts").glob("*.sh")) + list(ROOT.glob("*.sh"))
        checked = 0
        for script in scripts:
            text = script.read_text()
            if "run() {" not in text:
                continue
            helper = text[text.index("run() {"):].split("\n}", 1)[0] + "\n}"
            code = 'RED= GREEN= YELLOW= GRAY= NC=\n' + helper
            result = subprocess.run(["bash", "-c", code + '\nrun bash -c "exit 23"'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 23, str(script))
            self.assertIn("exit code 23", result.stderr)
            checked += 1
        self.assertEqual(checked, 7)

    def teardown(self, fail):
        with tempfile.TemporaryDirectory(prefix="sim-lifecycle-") as tmp:
            root = Path(tmp)
            payload = root / "state/sim-test"
            (payload / "scenarios").mkdir(parents=True)
            (payload / "specs").mkdir()
            (payload / "control.yaml").write_text("active: []\n")
            (root / "scripts").mkdir()
            (root / "scripts/archive_manifest.py").write_text((ROOT / "scripts/archive_manifest.py").read_text())
            (payload / "environment.yaml").write_text("name: sim-test\n")
            (payload / "scenarios/fault.yaml").write_text("name: test\n")
            prelude = '''set -euo pipefail
RED= GREEN= YELLOW= GRAY= NC=
require_docker() { :; }
container_of() { echo "infra-sim-$1"; }
info() { :; }
die() { echo "$*" >&2; exit 1; }
docker() {
  if [ "$1" = ps ]; then echo infra-sim-sim-test;
  elif [ "$1" = inspect ]; then echo sha256:synthetic;
  elif [ "$1" = cp ]; then touch "$3";
  elif [ "$1" = stop ]; then [ "$FAIL" != stop ];
  elif [ "$1" = rm ]; then
    echo removed >> "$REPO/docker-actions"
    [ "$FAIL" != docker ]
  fi
}
cp() { if [ "$FAIL" = copy ]; then return 28; else command cp "$@"; fi; }
'''
            env = dict(os.environ, REPO=tmp, STATE_DIR=str(root / "state"), FAIL=fail)
            result = subprocess.run(["bash", "-c", prelude + RUN + FUNCTIONS + '\ncmd_teardown sim-test'], env=env, capture_output=True, text=True)
            if fail:
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertTrue(payload.exists())
                if fail == "copy":
                    self.assertFalse((root / "docker-actions").exists())
            else:
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(payload.exists())
                self.assertEqual(len(list((root / "archive").glob("*/environment.yaml"))), 1)

    def test_failed_archive_preserves_container_and_payload(self):
        self.teardown("copy")

    def test_failed_stop_preserves_container_and_payload(self):
        self.teardown("stop")

    def test_failed_container_removal_preserves_payload(self):
        self.teardown("docker")

    def test_successful_teardown_archives_then_removes(self):
        self.teardown("")

    def test_claim_values_are_not_arguments_or_output(self):
        start = SOURCE.index("  local -a claim_env=()")
        end = SOURCE.index('\n  info "simulation', start)
        code = 'set -eu\nRED= GREEN= YELLOW= GRAY= NC=\n' + RUN + '''
info() { :; }
warn() { :; }
die() { exit 1; }
docker() {
  printf '%s\\n' "$@" >&2
  [ "$NETDATA_CLAIM_TOKEN" = synthetic-token ]
  [ "$NETDATA_CLAIM_ROOMS" = synthetic-room ]
}
probe() {
claim=yes rooms=synthetic-room url=https://example.invalid owner= public_dashboards=no exporters=yes
container=sim-test name=sim-test LABEL=simulation port=19900 dir=/tmp/sim-test IMAGE=test runtime_image=test
''' + SOURCE[start:end] + '\n}\nprobe'
        result = subprocess.run(["bash", "-c", code], env=dict(os.environ, NETDATA_CLAIM_TOKEN="synthetic-token"), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("synthetic-token", result.stdout + result.stderr)
        self.assertNotIn("synthetic-room", result.stdout + result.stderr)

    def test_create_lint_honours_hours_and_reports_violations(self):
        start = SOURCE.index("  # The one fidelity lint of a container create")
        end = SOURCE.index("  local -a claim_env=()", start)
        code = 'set -eu\nRED= GREEN= YELLOW= GRAY= NC=\n' + RUN + r'''
info() { printf '%s\n' "$*" >&2; }
docker() {
  printf '%s\n' "$*" >> "$dir/docker-calls"
  printf '  PASS  sim-node-01\n  disk_space./ used exceeds its mount\n'
  [ "$LINT_RESULT" = pass ]
}
probe() {
runtime_image=sha256:synthetic
''' + SOURCE[start:end] + '\n}\nprobe'
        for hours, outcome in (("0", "pass"), ("2", "pass"), ("2", "fail")):
            with tempfile.TemporaryDirectory(prefix="sim-lint-") as tmp:
                payload = Path(tmp)
                (payload / "lint-evidence.json").write_text("{}")
                env = dict(os.environ, dir=tmp, lint_hours=hours, LINT_RESULT=outcome)
                result = subprocess.run(["bash", "-c", code], env=env, capture_output=True, text=True)
                calls = payload / "docker-calls"
                if hours == "0":
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertFalse(calls.exists(), "a skipped lint must not run")
                    self.assertFalse((payload / "lint-evidence.json").exists(), "stale evidence kept")
                    self.assertIn("unverified", result.stderr)
                elif outcome == "pass":
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("--lint 2 --lint-evidence", calls.read_text())
                    self.assertIn("PASS", (payload / "lint.out").read_text())
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("used exceeds its mount", result.stderr)
                    self.assertNotIn("PASS", result.stderr)
                    self.assertIn("fidelity lint failed", result.stderr)


if __name__ == "__main__":
    unittest.main()
