"""Release publication must reject incomplete, mismatched, or corrupted builds."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("release", Path(__file__).resolve().parents[1] / "scripts/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        previous = Path.cwd()
        self.addCleanup(self.directory.cleanup)
        self.addCleanup(os.chdir, previous)
        os.chdir(self.directory.name)
        self.environment = patch.dict(os.environ, GITHUB_SHA="tested-commit", GITHUB_REF_NAME="v0.1.0")
        self.environment.start()
        self.addCleanup(self.environment.stop)
        Path("Cargo.toml").write_text('[package]\nversion = "0.1.0"\n[dependencies]\n')
        Path("CHANGELOG.md").write_text("# Changelog\n\n## v0.1.0\n\nNew release.\n\n## v0.0.1\n\nOld notes.\n")
        self.assets = []
        for platform, suffix in [("windows-x86_64", ".exe"), ("linux-x86_64", ""), ("macos-arm64", ".zip"), ("macos-x86_64", ".zip")]:
            folder = Path("artifacts") / f"upnp-engage-{platform}"
            folder.mkdir(parents=True)
            asset = folder / f"upnp-engage-{platform}{suffix}"
            asset.write_bytes(b"test artifact")
            (folder / "manifest.json").write_text(json.dumps({
                "commit": "tested-commit", "version": "0.1.0", "platform": platform,
                "asset": asset.name, "sha256": hashlib.sha256(asset.read_bytes()).hexdigest(),
            }))
            self.assets.append(asset)

    def test_valid_release_contains_all_platforms_and_checksums(self):
        version, notes = release.validate()
        self.assertEqual(notes, "New release.\n")
        release.assemble(version, notes)
        self.assertEqual(len(list(Path("release").iterdir())), 5)
        self.assertEqual(len(Path("release/SHA256SUMS.txt").read_text().splitlines()), 4)

    def test_wrong_tag_or_version_is_rejected(self):
        for tag in ["release-abcdef", "v01.0.0", "v0.2.0"]:
            with patch.dict(os.environ, GITHUB_REF_NAME=tag), self.assertRaises(ValueError):
                release.validate()

    def test_missing_platform_prevents_publication(self):
        self.assets[-1].unlink()
        with self.assertRaises(FileNotFoundError):
            release.assemble(*release.validate())
        self.assertFalse(Path("release").exists())

    def test_wrong_commit_prevents_publication(self):
        with patch.dict(os.environ, GITHUB_SHA="other-commit"), self.assertRaises(ValueError):
            release.assemble(*release.validate())
        self.assertFalse(Path("release").exists())

    def test_corrupt_asset_prevents_publication(self):
        self.assets[-1].write_bytes(b"changed")
        with self.assertRaises(ValueError):
            release.assemble(*release.validate())
        self.assertFalse(Path("release").exists())


if __name__ == "__main__":
    unittest.main()
