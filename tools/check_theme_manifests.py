#!/usr/bin/env python3
"""Static checks for Chromium theme manifests bundled with Archon.

The script walks `extensions/themes`, validates that every directory contains a
`manifest.json`, and enforces a few invariants that routinely go missing when
importing themes: the manifest must target Manifest V3, provide a version/name,
include a `theme` block, and supply at least one of `colors` or `images`.

Usage
-----
Run from the repository root:

    python tools/check_theme_manifests.py

The script exits with a non-zero status if any manifest fails validation.
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, List

THEMES_DIR = Path(__file__).resolve().parents[1] / "extensions" / "themes"


@dataclass
class ManifestIssue:
    path: Path
    message: str

    def format(self) -> str:
        return f"{self.path}: {self.message}"


def iter_manifest_paths(root: Path) -> Iterable[Path]:
    for entry in sorted(root.iterdir()):
        if entry.name.startswith("."):
            continue
        if not entry.is_dir():
            continue
        manifest_path = entry / "manifest.json"
        yield manifest_path


def load_manifest(path: Path) -> dict | ManifestIssue:
    if not path.exists():
        return ManifestIssue(path, "missing manifest.json")
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as err:
        return ManifestIssue(path, f"invalid JSON: {err}")


def validate_manifest(path: Path, manifest: dict) -> List[ManifestIssue]:
    issues: List[ManifestIssue] = []

    manifest_version = manifest.get("manifest_version")
    if manifest_version != 3:
        issues.append(
            ManifestIssue(path, f"expected manifest_version 3, found {manifest_version!r}")
        )

    for field in ("name", "version", "theme"):
        if field not in manifest:
            issues.append(ManifestIssue(path, f"missing required field '{field}'"))

    if isinstance(manifest.get("theme"), dict):
        theme_block = manifest["theme"]
        if not any(key in theme_block for key in ("colors", "images")):
            issues.append(
                ManifestIssue(
                    path, "theme block should include at least 'colors' or 'images'"
                )
            )
    else:
        issues.append(ManifestIssue(path, "theme section must be an object"))

    return issues


def main() -> int:
    if not THEMES_DIR.exists():
        print(f"error: themes directory not found: {THEMES_DIR}", file=sys.stderr)
        return 2

    all_issues: List[ManifestIssue] = []

    for manifest_path in iter_manifest_paths(THEMES_DIR):
        manifest_or_issue = load_manifest(manifest_path)
        if isinstance(manifest_or_issue, ManifestIssue):
            all_issues.append(manifest_or_issue)
            continue
        all_issues.extend(validate_manifest(manifest_path, manifest_or_issue))

    if all_issues:
        print("Theme manifest validation failed:")
        for issue in all_issues:
            print(f"  - {issue.format()}")
        return 1

    print("All theme manifests look good.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
