"""Validate a versioned release and collect artifacts from this workflow run."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys


def validate():
    tag = os.environ["GITHUB_REF_NAME"]
    if not re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", tag):
        raise ValueError("Use a stable version tag such as v0.1.0")
    version = tag[1:]
    package = Path("Cargo.toml").read_text(encoding="utf-8").split("[dependencies]", 1)[0]
    crate_version = re.search(r'^version\s*=\s*"([^"]+)"', package, re.M).group(1)
    if version != crate_version:
        raise ValueError(f"Tag {tag} does not match Cargo.toml version {crate_version}")
    notes = Path("CHANGELOG.md").read_text(encoding="utf-8")
    section = re.search(rf"^## {re.escape(tag)}\s*\n(.*?)(?=^## |\Z)", notes, re.M | re.S)
    if not section or not section.group(1).strip():
        raise ValueError(f"Add release notes for {tag} to CHANGELOG.md")
    return version, section.group(1).strip() + "\n"


def assemble(version, notes):
    expected = {
        "windows-x86_64": "upnp-engage-windows-x86_64.exe",
        "linux-x86_64": "upnp-engage-linux-x86_64.tar.gz",
        "macos-arm64": "upnp-engage-macos-arm64.zip",
        "macos-x86_64": "upnp-engage-macos-x86_64.zip",
    }
    verified = []
    for platform, name in expected.items():
        folder = Path("artifacts") / f"upnp-engage-{platform}"
        manifest = json.loads((folder / "manifest.json").read_text(encoding="utf-8"))
        for key, value in {"commit": os.environ["GITHUB_SHA"], "version": version, "platform": platform, "asset": name}.items():
            if manifest[key] != value:
                raise ValueError(f"{platform}: unexpected {key}")
        asset = folder / name
        digest = hashlib.sha256(asset.read_bytes()).hexdigest()
        if digest != manifest["sha256"]:
            raise ValueError(f"{platform}: checksum mismatch")
        verified.append((asset, digest))
    destination = Path("release")
    destination.mkdir(exist_ok=False)
    for asset, _ in verified:
        shutil.copy2(asset, destination / asset.name)
    (destination / "SHA256SUMS.txt").write_text(
        "".join(f"{digest}  {asset.name}\n" for asset, digest in verified), encoding="utf-8"
    )
    Path("release-notes.txt").write_text(notes, encoding="utf-8")


if __name__ == "__main__":
    version, notes = validate()
    if sys.argv[1:] == ["assemble"]:
        assemble(version, notes)
    elif sys.argv[1:] != ["validate"]:
        raise ValueError("Expected validate or assemble")
    print(f"Release v{version} validated")
