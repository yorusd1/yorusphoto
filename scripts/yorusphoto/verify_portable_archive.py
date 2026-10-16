"""Check a portable ZIP against its explicit public distribution and validated folder."""

import argparse
import datetime
import hashlib
import json
from pathlib import Path, PurePosixPath
import zipfile


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def runtime_allowlist(package):
    root = package / "codex"
    manifest_path = root / "distribution-manifest.json"
    if not manifest_path.is_file():
        # v1-v5 artifacts predate resource manifests and contained native binaries only.
        return {path.relative_to(package).as_posix() for folder in ("bin", "codex-path", "codex-resources")
                for path in (root / folder).rglob("*") if path.is_file() and path.suffix.lower() in {".exe", ".dll"}}
    assert manifest_path.stat().st_size <= 4 * 1024 * 1024, "Oversized runtime distribution manifest"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
    rows = manifest.get("files", [])
    assert 0 < len(rows) <= 10000, "Invalid runtime distribution manifest"
    expected = {"codex/distribution-manifest.json"}
    seen = set()
    forbidden = {"data", "yorusphotodata", "auth.json", "config.toml", "control.token", "sessions", "logs",
                 "codex-chat.json", "codex-chat.previous.json", "mcp-config.json"}
    for row in rows:
        name = row["path"]
        path = PurePosixPath(name)
        assert isinstance(name, str) and "\\" not in name and ":" not in name and not path.is_absolute(), "Unsafe runtime distribution path"
        assert str(path) == name and not any(part in {".", ".."} or part.lower() in forbidden for part in path.parts), "Private or traversing runtime path"
        assert name == "codex-package.json" or (len(path.parts) >= 2 and path.parts[0] in {"bin", "codex-path", "codex-resources"}), "Unknown runtime root"
        assert name.casefold() not in seen, "Duplicate runtime distribution path"
        seen.add(name.casefold())
        file = root.joinpath(*path.parts)
        assert not file.is_symlink() and file.resolve().is_relative_to(root.resolve()) and file.is_file(), "Missing or linked runtime distribution file"
        assert file.stat().st_size == row["bytes"] and digest(file) == row["sha256"], "Runtime distribution hash mismatch"
        expected.add("codex/" + name)
    metadata = json.loads((root / "codex-package.json").read_text(encoding="utf-8"))
    assert metadata["version"] == manifest["version"] and metadata["target"] == manifest["target"], "Runtime manifest version mismatch"
    return expected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    package = args.package.resolve()
    archive = package.with_name(package.name + ".zip")
    expected = {
        "yorusphoto.exe", "yorusphoto-cli.exe", "Start-yorusphoto.ps1", "Start-yorusphoto.cmd",
        "portable.txt", "README.md", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "ATTRIBUTION.md",
        "codex/LICENSE", "codex/NOTICE", "workspace/",
    }
    if (package / "yorusphoto-performance.md").is_file():
        expected.add("yorusphoto-performance.md")
    licenses = {
        "yorusphoto-brand.txt", "openai-marks.txt", "yorusai-direct.txt", "artcraft-brand.txt",
        "photocraft-app-icon.txt", "lucide-icons.txt", "noun-magnetic-lasso.txt",
        "symmetry-icons.txt", "scowl-dictionary.txt", "OFL-Inter.txt", "OFL-JetBrainsMono.txt",
        "egui-Hack-Regular.txt", "egui-NotoEmoji-OFL.txt", "egui-Ubuntu-UFL.txt",
        "egui-emoji-icon-font-MIT.txt",
    }
    expected.update("licenses/" + name for name in licenses)
    if (package / "codex/codex-package.json").is_file():
        expected.add("codex/codex-package.json")
    expected.update(path.name for path in package.glob("*.dll") if path.is_file())
    for folder in ("bin", "codex-path", "codex-resources"):
        root = package / "codex" / folder
        assert root.is_dir(), f"Missing public runtime folder: {folder}"
    expected.update(runtime_allowlist(package))
    with zipfile.ZipFile(archive) as zipped:
        names = zipped.namelist()
        assert len(names) == len({name.casefold() for name in names}), "Duplicate archive entries"
        assert set(names) == expected, "Archive differs from public distribution allowlist"
        assert zipped.testzip() is None, "ZIP CRC failure"
        for name in names:
            if name.endswith("/"):
                assert name == "workspace/" and zipped.getinfo(name).file_size == 0
                continue
            with zipped.open(name) as member:
                assert hashlib.file_digest(member, "sha256").hexdigest() == digest(package / name), \
                    f"Archive input mismatch: {name}"
        assert not any(part.lower() in {"data", "yorusphotodata", "auth.json", "control.token",
                                       "codex-chat.json", "codex-chat.previous.json", "mcp-config.json"}
                       for name in names for part in Path(name).parts), "Private state in archive"
    sha256 = digest(archive)
    archive.with_suffix(".zip.sha256").write_text(f"{sha256}  {archive.name}\n", encoding="ascii")
    report = {
        "createdAtUtc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "package": str(archive), "bytes": archive.stat().st_size, "sha256": sha256,
        "entries": len(names), "licenseCount": len(licenses), "crcPassed": True,
        "allowlistPassed": True, "allPackagedFilesMatch": True, "privateStateExcluded": True,
        "emptyWorkspaceOnly": True,
        "binaryHashes": {name: digest(package / name) for name in ("yorusphoto.exe", "yorusphoto-cli.exe")},
        "readmeHash": digest(package / "README.md"), "passed": True,
        "runtimeManifestVerified": (package / "codex/distribution-manifest.json").is_file(),
        "runtimeResourceLicenseCount": sum("/licenses/" in name for name in names),
    }
    args.report.write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
