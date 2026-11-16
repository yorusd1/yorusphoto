"""Fetch and verify an official native Codex release without npm install or scripts.

The Windows alias is resolved from @openai/codex's pinned npm metadata. Both tarballs
must match registry SHA-512 integrity before bounded, link-free extraction. Output is
a fresh repository-local toolchain directory; existing versions are never replaced.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import urllib.parse
import urllib.request
import uuid

REGISTRY = "https://registry.npmjs.org/@openai/codex/"
TARGET = "x86_64-pc-windows-msvc"
MAX_TARBALL = 600 * 1024 * 1024
MAX_EXPANDED = 2 * 1024 * 1024 * 1024


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def hidden_process_options() -> dict:
    if os.name != "nt":
        return {}
    startup = subprocess.STARTUPINFO()
    startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startup.wShowWindow = subprocess.SW_HIDE
    return {"startupinfo": startup, "creationflags": subprocess.CREATE_NO_WINDOW}


def fetch(url: str, maximum: int) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": "yorusphoto-runtime-verification"})
    with urllib.request.urlopen(request, timeout=120) as response:
        body = response.read(maximum + 1)
    require(len(body) <= maximum, "Official distribution input exceeded its size bound.")
    return body


def metadata(version: str) -> dict:
    value = json.loads(fetch(REGISTRY + version, 4 * 1024 * 1024))
    require(value.get("name") == "@openai/codex" and value.get("version") == version,
            "Official package metadata does not match the pinned version.")
    return value


def verified_tarball(info: dict, destination: Path) -> tuple[Path, str]:
    distribution = info["dist"]
    integrity = distribution["integrity"]
    require(integrity.startswith("sha512-"), "The official package does not provide SHA-512 integrity.")
    expected = base64.b64decode(integrity.removeprefix("sha512-"), validate=True)
    require(len(expected) == 64, "Invalid official SHA-512 integrity.")
    url = distribution["tarball"]
    parsed = urllib.parse.urlparse(url)
    require(parsed.scheme == "https" and parsed.netloc == "registry.npmjs.org"
            and parsed.path == f"/@openai/codex/-/codex-{info['version']}.tgz",
            "The package tarball does not use the expected official registry path.")
    body = fetch(url, MAX_TARBALL)
    require(hashlib.sha512(body).digest() == expected, "Official package SHA-512 integrity failed.")
    path = destination / f"codex-{info['version']}.tgz"
    with path.open("xb") as stream:
        stream.write(body)
    return path, integrity


def extract_verified(archive: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=False)
    root = destination.resolve()
    total = 0
    count = 0
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar:
            count += 1
            require(count <= 10000, "Distribution contains too many archive entries.")
            parts = PurePosixPath(member.name).parts
            require(parts and parts[0] == "package" and not any(part in {"", ".", ".."} or ":" in part or "\\" in part for part in parts),
                    "Unsafe distribution archive path.")
            require(member.isdir() or member.isfile(), "Distribution links and special files are not allowed.")
            target = root.joinpath(*parts)
            require(target.resolve().is_relative_to(root), "Distribution archive path escaped its destination.")
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            total += member.size
            require(0 <= member.size <= MAX_TARBALL and total <= MAX_EXPANDED, "Distribution expanded size exceeded its bound.")
            target.parent.mkdir(parents=True, exist_ok=True)
            source = tar.extractfile(member)
            require(source is not None, "Could not read a verified distribution member.")
            with source, target.open("xb") as stream:
                shutil.copyfileobj(source, stream)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", default="0.169.0")
    parser.add_argument("--source-dir", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version), "Choose an exact stable version.")
    source = args.source_dir.resolve(strict=True)
    cache = source / ".toolchain" / "codex-latest"
    cache.mkdir(parents=True, exist_ok=True)
    final = cache / args.version
    require(not final.exists(), "The pinned runtime directory already exists; it was not changed.")
    stage = cache / f".staging-{args.version}-{uuid.uuid4().hex}"
    stage.mkdir()
    main_info = metadata(args.version)
    alias = main_info.get("optionalDependencies", {}).get("@openai/codex-win32-x64")
    platform_version = f"{args.version}-win32-x64"
    require(alias == f"npm:@openai/codex@{platform_version}", "The official Windows alias differs from the expected pinned package.")
    platform_info = metadata(platform_version)
    archives = stage / "archives"
    archives.mkdir()
    platform_archive, platform_integrity = verified_tarball(platform_info, archives)
    main_archive, main_integrity = verified_tarball(main_info, archives)
    extract_verified(platform_archive, stage / "native")
    extract_verified(main_archive, stage / "wrapper")
    package = stage / "native" / "package"
    package_info = json.loads((package / "package.json").read_text(encoding="utf-8"))
    require(package_info.get("name") == "@openai/codex" and package_info.get("version") == platform_version,
            "Extracted native package identity mismatch.")
    vendor = package / "vendor" / TARGET
    runtime_info = json.loads((vendor / "codex-package.json").read_text(encoding="utf-8"))
    require(runtime_info.get("version") == args.version and runtime_info.get("target") == TARGET,
            "Extracted native runtime metadata mismatch.")
    binary = vendor / "bin" / "codex.exe"
    version = subprocess.run([str(binary), "--version"], capture_output=True, text=True, check=True,
                             timeout=30, **hidden_process_options()).stdout.strip()
    require(version == f"codex-cli {args.version}", "Verified executable reports a different version.")
    notices = stage / "notices"
    notices.mkdir()
    commit = main_info.get("gitHead")
    ref = commit if isinstance(commit, str) and re.fullmatch(r"[0-9a-f]{40}", commit) else f"rust-v{args.version}"
    for name in ("LICENSE", "NOTICE"):
        candidates = [package / name, stage / "wrapper" / "package" / name]
        found = next((path for path in candidates if path.is_file()), None)
        body = found.read_bytes() if found else fetch(f"https://raw.githubusercontent.com/openai/codex/{ref}/{name}", 4 * 1024 * 1024)
        (notices / name).write_bytes(body)
    files = []
    for path in sorted(vendor.rglob("*")):
        require(not path.is_symlink(), "Native runtime contains a symbolic link.")
        if path.is_file():
            with path.open("rb") as stream:
                checksum = hashlib.file_digest(stream, "sha256").hexdigest()
            files.append({"path": path.relative_to(vendor).as_posix(), "bytes": path.stat().st_size,
                          "sha256": checksum})
    distribution = {
        "version": args.version, "target": TARGET, "package": "@openai/codex", "platformAlias": "@openai/codex-win32-x64",
        "platformVersion": platform_version, "platformIntegrity": platform_integrity, "wrapperIntegrity": main_integrity,
        "sourceRef": ref, "files": files,
    }
    (stage / "distribution-manifest.json").write_text(json.dumps(distribution, indent=2), encoding="utf-8")
    (stage / "registry-metadata.json").write_text(json.dumps({"main": main_info, "platform": platform_info}, indent=2), encoding="utf-8")
    stage.rename(final)
    print(json.dumps({"version": args.version, "vendor": str(final / "native" / "package" / "vendor" / TARGET),
                      "binary": str(final / "native" / "package" / "vendor" / TARGET / "bin" / "codex.exe"),
                      "notices": str(final / "notices"), "manifest": str(final / "distribution-manifest.json"),
                      "integrityVerified": True, "files": len(files)}, indent=2))


if __name__ == "__main__":
    main()
