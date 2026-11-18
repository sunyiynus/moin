#!/usr/bin/env python3
"""Build a native macOS .app bundle and archive, including the MCP CLI."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tarfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=["aarch64-apple-darwin", "x86_64-apple-darwin"])
    parser.add_argument("--debug", action="store_true", help="package a development build")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("macOS packaging requires a Mac with Xcode and Rust installed")

    root = Path(__file__).resolve().parent.parent
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root
    ))
    package = next(p for p in metadata["packages"] if p["name"] == "moin")
    host = next(line.split(": ", 1)[1] for line in subprocess.check_output(
        ["rustc", "-vV"], text=True
    ).splitlines() if line.startswith("host: "))
    target = args.target or host
    command = ["cargo", "build", "--locked", "--bins"]
    if not args.debug:
        command.append("--release")
    if args.target:
        command.extend(["--target", args.target])
    subprocess.run(command, cwd=root, check=True)
    binaries = Path(metadata["target_directory"])
    if args.target:
        binaries /= args.target
    binaries /= "debug" if args.debug else "release"

    destination = root / "dist" / target
    bundle = destination / "Moin.app"
    executable_dir = bundle / "Contents" / "MacOS"
    executable_dir.mkdir(parents=True, exist_ok=True)
    (bundle / "Contents" / "Resources").mkdir(exist_ok=True)
    for name in ("moin", "moin-cli"):
        output = executable_dir / name
        shutil.copy2(binaries / name, output)
        output.chmod(0o755)
    info = {
        "CFBundleName": "Moin",
        "CFBundleDisplayName": "Moin",
        "CFBundleIdentifier": "dev.moin.launcher",
        "CFBundleExecutable": "moin",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": package["version"],
        "CFBundleVersion": package["version"],
        "LSUIElement": True,
        "NSHighResolutionCapable": True,
        "NSPrincipalClass": "NSApplication",
    }
    with (bundle / "Contents" / "Info.plist").open("wb") as stream:
        plistlib.dump(info, stream)
    # Local builds use ad-hoc signing. A release identity can be supplied for distribution.
    identity = os.environ.get("MOIN_SIGN_IDENTITY", "-")
    signing = ["codesign", "--force", "--sign", identity]
    if identity != "-":
        signing += ["--options", "runtime", "--timestamp", "--entitlements",
                    str(root / "packaging" / "macos-entitlements.plist")]
    subprocess.run(signing + [str(executable_dir / "moin-cli")], check=True)
    subprocess.run(signing + [str(bundle)], check=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(bundle)], check=True)
    archive = destination / f"moin-{target}.tar.gz"
    with tarfile.open(archive, "w:gz") as stream:
        stream.add(bundle, arcname="Moin.app")
    print(f"Bundle: {bundle}\nArchive: {archive}")


if __name__ == "__main__":
    main()
