#!/usr/bin/env python3
"""Describe and checksum only the explicitly selected simulation artifacts."""
import hashlib
import json
from pathlib import Path
import sys


def write_manifest(root, image):
    required = ("environment.yaml", "control.yaml", "infra-sim.plugin", "specs", "scenarios")
    for name in required:
        if not (root / name).exists():
            raise ValueError(f"archive is missing {name}")
    selected = list(required)
    if (root / "lint-evidence.json").is_file():
        selected.append("lint-evidence.json")
    recorded = (root / "recording").is_dir()
    if recorded:
        if not (root / "recording/finalized").is_file():
            raise ValueError("archive recording has not been finalized")
        selected.append("recording")
    checksums = {}
    for name in selected:
        target = root / name
        if target.is_symlink():
            raise ValueError("archive artifacts must not contain symlinks")
        paths = sorted(target.rglob("*")) if target.is_dir() else [target]
        for path in paths:
            if path.is_symlink():
                raise ValueError("archive artifacts must not contain symlinks")
            if path.is_file():
                with path.open("rb") as source:
                    checksums[path.relative_to(root).as_posix()] = hashlib.file_digest(source, "sha256").hexdigest()
    manifest = {"version": 1, "image": image, "raw_recording": recorded,
                "sha256": checksums}
    temporary = root / "archive.json.tmp"
    temporary.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    temporary.replace(root / "archive.json")


if __name__ == "__main__":
    write_manifest(Path(sys.argv[1]), sys.argv[2])
