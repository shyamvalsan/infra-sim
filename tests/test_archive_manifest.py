#!/usr/bin/env python3
"""Archive inventory refuses incomplete, unsealed or symlinked artifacts."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("archive_manifest", ROOT / "scripts/archive_manifest.py")
archive_manifest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(archive_manifest)


class ArchiveManifestTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        for name in ("environment.yaml", "control.yaml", "infra-sim.plugin"):
            (self.root / name).write_text(name)
        (self.root / "specs/nested").mkdir(parents=True)
        (self.root / "specs/nested/service.yaml").write_text("spec")
        (self.root / "scenarios").mkdir()
        (self.root / "recording").mkdir()
        (self.root / "recording/events.bin").write_bytes(b"ISIMREC1")

    def tearDown(self):
        self.directory.cleanup()

    def refused(self, message):
        with self.assertRaisesRegex(ValueError, message):
            archive_manifest.write_manifest(self.root, "sha256:test")
        self.assertFalse((self.root / "archive.json").exists())

    def test_an_unsealed_recording_is_never_inventoried(self):
        self.refused("not been finalized")

    def test_a_missing_required_artifact_is_refused(self):
        (self.root / "recording/finalized").write_text("1\n")
        (self.root / "control.yaml").unlink()
        self.refused("missing control.yaml")

    def test_symlinked_artifacts_are_refused(self):
        (self.root / "recording/finalized").write_text("1\n")
        (self.root / "specs/nested/link.yaml").symlink_to(self.root / "environment.yaml")
        self.refused("symlinks")

    def test_hashes_cover_nested_files_and_the_recording(self):
        (self.root / "recording/finalized").write_text("1\n")
        archive_manifest.write_manifest(self.root, "sha256:test")
        manifest = json.loads((self.root / "archive.json").read_text())
        self.assertTrue(manifest["raw_recording"])
        self.assertEqual(manifest["sha256"]["specs/nested/service.yaml"],
                         hashlib.sha256(b"spec").hexdigest())
        self.assertIn("recording/events.bin", manifest["sha256"])
        self.assertNotIn("lint-evidence.json", manifest["sha256"])


if __name__ == "__main__":
    unittest.main()
