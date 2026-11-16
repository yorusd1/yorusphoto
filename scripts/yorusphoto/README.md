# yorusphoto portable distribution tools

These Windows distribution tools use Python 3.11 or later and its standard library.
Run the examples from the repository root. Building the editor also requires Rust,
Cargo, and the Windows toolchain described in [the build notes](../../docs/yorusphoto.md#local-limits-and-build-notes).

## Fetch the pinned Codex runtime

```powershell
python scripts/yorusphoto/fetch_codex_runtime.py --version 0.169.0
```

The script downloads the official `@openai/codex` wrapper and Windows native package
from the npm registry. It verifies their published SHA-512 integrity, performs bounded
extraction without archive links, checks the native executable's version, and retains
the distribution's notices and a file-hash manifest. It runs only `codex --version`;
it does not sign in, generate images, run a chat, or download model weights.

The verified files are written below `.toolchain/codex-latest/0.169.0/`. An existing
version directory is left unchanged. `--source-dir` can select another repository
checkout. A newer runtime requires choosing its exact stable version explicitly.
The pinned version records this build's input; it does not claim to be the newest
release available today.

## Build and assemble a portable copy

The interface translations are a build input since PhotoCraft 0.23.1: check out
[craft-i18n](https://github.com/storytold/craft-i18n) at the commit in `craft-i18n.rev`
below `.toolchain/craft-i18n` (the wrapper sets `CRAFT_I18N_DIR` to it, and requires it for
release builds). Without it the editor is English only.

```powershell
git clone https://github.com/storytold/craft-i18n .toolchain/craft-i18n
git -C .toolchain/craft-i18n checkout (Get-Content craft-i18n.rev)
powershell -File packaging/windows/cargo-yorusphoto.ps1 build --release -p photocraft -p photocraft-cli
powershell -File packaging/windows/package-yorusphoto.ps1 -Name yorusphoto-portable
```

The Cargo wrapper expects the LLVM MinGW installation documented in the build
notes. The packager uses the release binaries and discovers the verified runtime
cache, then copies its public assets, resources, and required licenses. It creates
a fresh folder below `dist/` and refuses to replace an existing output. Optional
`-BuildDirectory`, `-CodexVendorDirectory`, `-CodexDistributionManifest`,
`-CodexLicensePath`, and `-CodexNoticePath` parameters select explicit verified inputs.

To create the ZIP after assembly:

```powershell
powershell -File packaging/windows/package-yorusphoto.ps1 -Name yorusphoto-portable -ArchiveOnly
python scripts/yorusphoto/verify_portable_archive.py dist/yorusphoto-portable --report dist/yorusphoto-portable-verification.json
```

The verifier checks the explicit public-file allowlist, runtime manifest hashes,
licenses, ZIP CRCs, and equality between packaged files and the extracted folder.
It also scans the project's editor and CLI for Windows, macOS, or Linux home paths
embedded by the compiler, including UTF-16 strings. The Cargo wrapper remaps local
build paths to public prefixes. This gate excludes third-party runtime binaries
and license text, whose public distribution is checked separately. Scan reports
contain categories and byte offsets without copying identity-bearing matches.
The verifier rejects private state in the ZIP and writes a SHA-256 sidecar plus the
requested report. Run it with a normal Python interpreter so its integrity assertions
remain enabled. Do not use `python -O`.

Run the privacy gate's synthetic regressions with:

```powershell
python -m unittest discover -s scripts/yorusphoto -p test_verify_release_privacy.py
```

`dist/`, `.toolchain/`, local `verification/` reports, credentials, editor settings,
chat history, workspaces, generated images, and build outputs are excluded from the
source repository. Use synthetic fixtures and isolated settings directories for
local validation. Cargo unit and integration tests remain alongside their Rust
source.
