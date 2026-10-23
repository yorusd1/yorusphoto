<p align="center">
  <img src="assets/yorusphoto/icon-256.png" alt="yorusphoto YS logo" width="128">
</p>

# yorusphoto

A native Rust image editor with layers, masks, PSD support, an integrated Codex assistant and YorusAi Direct image workflows. This repository contains the yorusphoto fork of [PhotoCraft](https://github.com/storytold/photocraft), including the current v7 changes.

## Editing with AI

- **Codex workspace:** ChatGPT sign-in, runtime-discovered models and reasoning settings, saved chats and drafts, slash commands, reported context usage, compaction status, account allowances and reset times. Stable Codex runtime updates are checked automatically.
- **Selected-area generation:** the selected region becomes the image reference. Results arrive on a separate layer at the captured position and dimensions, preserving the source layer and any newer selection.
- **Whole-image generation:** results open in a separate canvas at the requested dimensions.
- **YorusAi Direct:** native provider workflows for image generation, remix, upscaling and layer separation, using separately configured API keys.
- **Agent control:** an authenticated local control channel and a stdio MCP bridge expose editor commands, document inspection, screenshots and image-placement workflows.

New rectangular and elliptical selections stay within the canvas. Starting outside projects the start onto the nearest edge or corner. Fixed ratios fit inside the available canvas, so a square remains square when the pointer crosses an edge; the preview matches the finished selection.

Model availability and allowances come from the signed-in Codex account. Context tokens and account credits are separate measurements. API-provider access for YorusAi Direct is billed separately by the configured provider.

## Windows portable

[Download the Windows 64-bit portable release](https://github.com/yorusd1/yorusphoto/releases/latest). Under **Assets**, choose `yorusphoto-v8-windows-x64-portable.zip`, extract the entire ZIP into a writable folder, and run `Start-yorusphoto.cmd`. No installation or source checkout is required. A matching SHA-256 checksum is provided. Downloads require access to this private repository.

The launcher initializes portable settings, the workspace and the local MCP configuration. Saved conversations and settings remain local. To update from v7, extract v8 into a new folder; your v7 folder and its settings are left untouched. v8 makes Camera Raw responsive and adds Auto, the White Balance tool, per-channel curves, colour-grading wheels, Black & White and Calibration ([details](docs/camera-raw-histogram.md)). Sign in with your own ChatGPT account for Codex, or configure your own provider keys for YorusAi Direct.

Portable binaries, downloaded Codex runtimes, private settings, chat histories, credentials and local recovery copies are excluded from this Git repository. The repository contains the source, branding, packaging scripts and reusable checks.

See [the yorusphoto guide](docs/yorusphoto.md) for setup, commands, authentication, model discovery, updates and placement behavior. See [performance measurements](docs/yorusphoto-performance.md) for the v7 optimization results and their limits.

## Build and verify

The desktop UI uses Rust, egui/eframe and wgpu. The web target compiles the same editor to WebAssembly. Machine identifiers and crate names retain `photocraft` for compatibility.

```sh
cargo build --release -p photocraft -p photocraft-cli
cargo test -p photocraft-ui-egui --lib
cargo clippy -p photocraft-ui-egui --all-targets -- -D warnings
cargo xtask layers
cargo xtask wasm
```

See [development](docs/development.md), [architecture](docs/architecture.md) and [contributing](docs/contributing.md) for toolchains, optional dependencies and the full test gates. Windows packaging is implemented by `packaging/windows/package-yorusphoto.ps1`. The [packaging tools](scripts/yorusphoto/README.md) fetch a pinned, verified public Codex runtime and validate the portable archive.

The canvas-boundary update passed 2,413 UI library tests, six GPU canvas integration tests, eight native boundary checks and 21 native editor/workspace checks. Eleven existing UI tests remain ignored. Saved-chat restoration, source preservation, exact captured-area placement and independent new-canvas dimensions were checked on the installed portable. This update's local workflow checks made no model requests. Build logs, screenshots and private-machine verification reports remain local.

## License and attribution

Source code is licensed under [MIT](LICENSE-MIT) OR [Apache-2.0](LICENSE-APACHE). Existing copyright notices and [attributions](ATTRIBUTION.md) are retained. yorusphoto branding has a [separate rights notice](assets/yorusphoto/LICENSE-brand.txt). OpenAI marks identify the integrated service and are subject to their [notice](assets/yorusphoto/NOTICE-OpenAI.txt). Codex runtime distributions include their own license and notice. Provider names and trademarks belong to their respective owners.
