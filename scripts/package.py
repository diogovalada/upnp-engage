"""Package one tested executable, retaining Unix permissions inside archives."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

platform = os.environ["PLATFORM"]
binary = Path("target/release/upnp-engage" + (".exe" if platform.startswith("windows-") else ""))
version = subprocess.check_output([str(binary.resolve()), "--version"], text=True).strip().split()[-1]
dist = Path("dist")
dist.mkdir(exist_ok=True)
name = f"upnp-engage-{platform}"
if platform.startswith("macos-"):
    # .command associates the executable with Terminal in Finder. It is the
    # Mach-O binary itself, not a shell wrapper; ZIP preserves its execute bit.
    command = dist / "upnp-engage.command"
    shutil.copy2(binary, command)
    command.chmod(0o755)
    subprocess.run(["codesign", "--force", "--sign", "-", str(command)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(command)], check=True)
    subprocess.run([str(command.resolve()), "--version"], check=True)
    asset = dist / (name + ".zip")
    with zipfile.ZipFile(asset, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.write(command, command.name)
    command.unlink()
elif platform.startswith("linux-"):
    asset = dist / (name + ".tar.gz")
    with tarfile.open(asset, "w:gz") as archive:
        entry = archive.gettarinfo(binary, arcname=name)
        entry.mode = 0o755
        entry.uid = entry.gid = 0
        entry.uname = entry.gname = ""
        with binary.open("rb") as contents:
            archive.addfile(entry, contents)
    # Check the actual download path: extracting must produce a runnable file,
    # without requiring users to fix its permissions themselves.
    with tempfile.TemporaryDirectory(prefix="upnp-package-") as directory:
        subprocess.run(["tar", "-xzf", str(asset.resolve()), "-C", directory], check=True)
        extracted = Path(directory) / name
        if extracted.stat().st_mode & 0o777 != 0o755:
            raise ValueError("Linux archive did not preserve executable permissions")
        actual = subprocess.check_output([str(extracted), "--version"], text=True).strip()
        if actual != f"upnp-engage {version}":
            raise ValueError("Linux archive contains an unexpected executable version")
else:
    asset = dist / (name + (".exe" if platform.startswith("windows-") else ""))
    shutil.copy2(binary, asset)

manifest = {
    "commit": os.environ["GITHUB_SHA"],
    "version": version,
    "platform": platform,
    "asset": asset.name,
    "sha256": hashlib.sha256(asset.read_bytes()).hexdigest(),
}
(dist / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
print(f"Packaged {asset.name} ({version})")
