"""Package one tested executable, retaining Unix permissions inside Mac ZIPs."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
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
