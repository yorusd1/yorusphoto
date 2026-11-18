# yorusphoto portable with Codex and YorusAi

A native Windows image editor with a Codex assistant and YorusAi image tools. Both are built into the app, with a minimal conversation-style panel on the right.

## Start and sign in

1. Extract the complete portable folder to a writable Windows folder.
2. Open **Start-yorusphoto.cmd**. The launcher normally keeps editor preferences below `data/`, creates `workspace/`, and starts an authenticated control endpoint on `127.0.0.1:50495`. If you previously opened the EXE directly and saved chats in `yorusphotoData/`, the launcher reuses that directory when `data/` has no chat history. It does not move or copy either history; when both exist, `data/` retains precedence.
3. The **Codex** panel opens on the right by default. Use its settings button to view **ChatGPT account**. The panel automatically checks the Codex session: an existing login shows **Signed in with ChatGPT**, the email address when Codex supplies one, and the reported plan. **Refresh account** checks again. If signed out, **Continue with ChatGPT** opens your browser and shows **Open sign-in page**, **Copy link** and **Cancel sign-in** in the panel. Use the visible link if your browser does not open automatically. The GPT button in the right rail reopens or closes the panel; the wand button opens YorusAi.

**Switch account** signs out before opening a new sign-in flow; **Sign out** disconnects the shared Codex session. These controls also affect other Codex clients using the same Codex home. Saved chats, drafts and image references stay on this device. Account identity is held only in memory and excluded from editor preferences, saved chats and control/MCP inspection. If a check fails, the panel labels any previous account as last confirmed rather than claiming sign-out. `/account` opens these controls and `/logout` signs out.

The package includes the Codex runtime. It does not include anyone's credentials. Codex manages its own sign-in separately from the portable editor settings, normally in your user Codex directory. Image generation uses the ChatGPT account connected to Codex and the account's available image-generation entitlement and limits. The Codex workflow does not need an OpenAI API key. YorusAi Direct uses separately configured provider API keys.

If you move the portable folder, run the launcher again to update the absolute paths in `mcp-config.json`. Port 50495 must be available; start the PowerShell launcher with `-ControlPort 50496` to choose another port. Opening `yorusphoto.exe` directly keeps settings in `yorusphotoData/` beside the executable; use the launcher for its dedicated `data/` settings and external MCP access.

## Canvas-edge region selection

Rectangular and elliptical marquees stay inside the canvas while they are drawn. A press outside the image projects onto its nearest edge or corner; dragging from northeast starts at the top-right corner. Moving or releasing outside stops at the image boundary. Fixed Ratio and Shift-square constraints fit within the available canvas before preview and commit, so reaching an edge does not crop a square into a rectangle. A Fixed Size box follows the pointer, as in Photoshop, and slides back onto the canvas at its edges; only a canvas smaller than the box shrinks it, keeping its proportions. Holding Space moves the shape you see, whole pixels at a time, and stops it at the edges; pressing or releasing Space never makes it jump, and sizing resumes from where it is. The live outline and size readout describe the committed pixel selection.

This changes creation of new marquees. Existing selection and layer movement retain their ordinary behavior. Codex and YorusAi continue to capture the selected region and return a separate layer at its captured bounds; whole-image generation opens an independent canvas.

## Reference images in the Codex Images tab

Open **Codex → Images → Add image…** to attach a PNG, JPEG or WebP. Add up to four uploaded images; each has a thumbnail, name, dimensions and a remove button. **Clear** removes all uploaded references. Describe which image supplies the subject, composition or style in your prompt.

In **Selected area** mode, the captured selection is reference 1 and the uploaded images follow it. The generated result remains a separate layer at the captured position and size. In **New canvas** mode, uploaded images guide generation while the result opens in an independent canvas; no existing document is required. Uploading a reference does not open it as a document or change the placement rectangle. The Assistant tab's selected-area attachment remains independent of these Images-tab uploads.

Attachments stay available for reuse until removed or the app closes. They are kept in transient memory, not saved into editor preferences or local chat history. Uploaded PNGs together must fit within 32 MiB; decoded images are limited to 16 megapixels. JPEG/WebP attachments are converted to PNG for Codex. Only small thumbnails are uploaded to the editor's GPU; shared image buffers avoid duplicating full references when a request starts. The selected crop plus uploads may total at most 64 MiB. Codex receives the images as structured conversation attachments and is instructed to use the exact attached-image count.

Agents can use `codex.references.pick`, `codex.references.add` (`image` as base64, optional `name`), `codex.references.remove` (`index`, zero-based) and `codex.references.clear`. Inspection exposes counts and image dimensions, without image bytes or source paths. Reference edits are blocked while an AI operation is running.

## Codex workspace, models and saved chats

The assistant uses Codex's real app-server for chat, editor tools, image generation, durable threads, model discovery, context usage, compaction and account limits. The right panel includes the runtime-reported model catalog and supported reasoning efforts. A selection applies to the next turn and is retained per chat; unsupported combinations are rejected. Models are discovered rather than hard-coded. The catalog describes available choices, while a completed request verifies actual access; a catalog entry alone does not grant a model entitlement.

The context strip shows Codex's last reported token usage against its reported model context window, reckoned as Codex does (the roughly 12,000 tokens of instructions and tools that are always present count as neither used nor free), with a separate conversation-total breakdown in **Usage**. These numbers are not credits. New chats show usage as unavailable until Codex reports it. Restored or pre-compaction measurements are identified as last reported. The runtime manages automatic compaction; the panel shows its progress, completion or failure. **Compact context now** and `/compact` call the real compaction interface and wait for completion. The full displayed transcript remains saved while Codex condenses its model context.

Use the clock button for **Saved chats**. Create a new chat, search titles or content, switch chats, rename them, or archive and restore them. Renaming or archiving a saved chat does the same to its Codex thread, as the Codex app does; if Codex refuses, the editor's copy is still saved and the panel says so. Each chat retains its thread ID, displayed messages, unsent draft, model, reasoning choice and last reported token counts. Switching is blocked during active work, failed history writes or pending image placement. **Find saved Codex threads** can import older durable threads created by yorusphoto, including threads from a previous portable location. Unrelated Codex clients' conversations, and threads of another ChatGPT account signed in to the same Codex home, are excluded from that finder.

Local history lives in `codex-chat.json` below the editor settings directory, with `codex-chat.previous.json` retaining the preceding valid snapshot. Version 1 history from v4 migrates automatically to the version 2 chat collection while preserving the old snapshot. History uses atomic replacement; damaged files and failed writes preserve the existing data. The collection supports 100 chats, 100 displayed messages per chat, 128 KiB per message/draft and 16 MiB total. Exceeding a limit reports an error without evicting chats. Codex retains its underlying durable thread history separately. Account data, OAuth links, credentials, image bytes and generated-image placement state are excluded from the chat collection.

The older v3 panel did not persist its local transcript or thread pointer. The durable-thread finder can retrieve editor-owned threads that Codex actually saved; it cannot recover unsent text that existed only in the old panel's memory.

## Automatic Codex runtime updates

The v11 and v12 portables include Codex **0.169.0**, the official stable release verified for these builds (v6 to v10 included 0.163.0). Its complete native runtime includes the accompanying resources and notices. Your model choices depend on the catalog returned by your runtime and account.

**Automatically install stable Codex updates** is enabled by default in the Codex connection settings. The native worker checks the official stable package channel daily; **Check now** or `/update` checks immediately. `/updates` opens the settings and status window. The preference is retained in editor settings and can be switched off. The panel reports active/latest versions, check status and failures.

Updates download the platform's complete official runtime, verify its published SHA-512 integrity, and validate compatibility before activation. Versions are stored separately below the editor settings directory's `codex-updates/`, with atomic activation and the previous version retained. A running task keeps its existing process; a newly activated version applies to the next Codex operation. Offline or failed updates leave the previous runtime available, with the packaged runtime as a fallback. The updater does not migrate or replace saved chats, sign-in credentials, canvases or image-placement state.

An explicitly selected **Codex executable** takes precedence over managed updates. Leave it empty for automatic runtime selection. These updates maintain the Codex engine inside yorusphoto; changes to the image editor itself are delivered as a new yorusphoto build.

## Account usage and limit warnings

Open **Usage** beside the context counter, or use `/status` or `/usage`. The panel refreshes account information on startup, after connection and work, and approximately once per minute while idle. Manual Refresh is available. It shows the reported account plan, separate allowance buckets, primary/secondary windows, remaining percentages, credit balance when supplied, individual spend controls, and available reset credits when supplied. Every reset includes its UTC timestamp and relative countdown. Past reset times say a refresh is needed; the app never fabricates a replenished allowance.

Warnings appear at 80%, 95% and 100% reported usage and on explicit spending/ordinary-usage restrictions. Credit information remains visible in Usage; an empty-credit warning is suppressed while Codex reports remaining included allowance, unless the backend reports an explicit restriction. The Usage view retains all warnings and marks stale observations. A missing balance, window or reset remains **unavailable**. An unlimited balance is displayed only when Codex explicitly reports it. These account allowances are distinct from context tokens and from YorusAi Direct provider billing. Requests remain subject to the actual backend's limits, and returned rejection errors are shown in the panel.

This is the native yorusphoto workspace around Codex. The following slash commands are implemented locally and do not become model prompts. Unknown commands and invalid arguments are rejected before any model request.

| Command | Action |
|---|---|
| `/account` | Connected account, refresh, switch account and sign-out controls |
| `/logout` | Sign out of the shared Codex session while retaining saved chats |
| `/help` | Command reference |
| `/status` | Context, compaction and account details |
| `/usage`, `/limits` | Refresh account allowances and credits |
| `/model [model] [effort]` | Open selector or choose a catalog model; `auto` restores the runtime default |
| `/effort <effort|auto>` | Choose a supported reasoning level |
| `/compact` | Compact the current Codex context |
| `/new`, `/clear` | Save this conversation and start a fresh one |
| `/chats`, `/resume` | Open saved chats |
| `/resume <chat ID>` | Switch to a saved chat or import an editor-owned thread |
| `/rename <title>` | Rename the current saved chat |
| `/archive` | Archive the current saved chat |
| `/unarchive <chat ID>` | Restore a saved chat from the archive |
| `/fork` | Fork the current durable Codex history |
| `/stop`, `/cancel` | Cancel active work |
| `/updates` | Open automatic Codex update settings and status |
| `/update` | Check and install the latest stable Codex runtime |

The `/` menu in the composer lists these controls. Model selection, chat management, status, refresh, compaction and thread import/fork are also discoverable through MCP. Terminal-specific commands outside this list are not implemented by the native image-editor panel.

## Generate into a selected area

1. Open an image and select an area with a selection tool.
2. Open the Codex panel's image tab and choose **Selected area**.
3. Choose a ratio and use **Apply ratio to selection** if you want the selection itself to change. Enter your prompt, quality preference, and preferred resolution for the image's long edge. The saved selection supplies the aspect ratio.
4. Use the send arrow to generate.

The selected rectangle is captured from the visible merged image and sent as the image reference. The app saves the original position, dimensions, and selection coverage before generation starts. The returned image becomes a separate pixel layer above the original active layer, filling that rectangle. Nonrectangular holes and feathered coverage are retained. Changing the selection during generation does not move the destination. Placing the layer is one undo step; source pixels stay intact.

When the result has more pixels than the selection, **Grow canvas to fit** (on by default) enlarges the whole canvas by one factor, so every generated pixel is kept: the existing layers are resampled to the larger size and the new layer lands at the same place, at the same size relative to the image. Switch it off to scale the result down into the selection instead. A canvas never grows beyond 16,384 pixels a side or 100 megapixels; past that the result is scaled to fit. Results of the same request after a growth use the grown canvas.

You can switch tabs during generation: the result returns to its original document and your current tab stays active. If you close or resize the source canvas, or remove its original anchor layer, automatic placement fails safely and the result is kept for recovery. Undo the geometry/layer change, then retry placement, or choose **Open retained results in new canvases** or **Discard retained results**. Multiple generated results are queued for recovery. Starting another generation is blocked while a result still needs placement.

**Quality and generated resolution are preferences sent to Codex.** The model chooses its native generation quality and size automatically. The requested dimensions are enforced locally when creating a new canvas, and a selected-area result keeps the captured place and size relative to the canvas (with **Grow canvas to fit**, the whole canvas is enlarged instead of the result being shrunk). Resampling does not add native model detail.

## Generate an entire image

Choose **New canvas**, enter a prompt, set the desired canvas dimensions, and generate. You can choose a ratio and use **Apply ratio to canvas size** to update those dimensions. This mode creates a new independent RGB canvas containing the result, while keeping your existing documents open. It does not require an existing canvas or a selection.

## Animations, flipbooks and cutouts

Tick **Animation (GIF)** in the Images tab and choose 2 to 16 frames to ask Codex for a looping GIF. In **Selected area** mode every frame becomes its own layer at the captured place, the first one shown, ready for a flipbook; in **New canvas** mode they open as layers of a new canvas, or as one flipbook sheet with **Frames as one flipbook sheet**. **Transparent** asks for a cutout on a transparent background: sprites, icons and effect textures. GIF output is a Codex 0.169 feature that OpenAI still marks as under development, and its image service may refuse it for an account. When it does, Codex draws the frames as one grid image instead (2 × 2 for 4 frames, 3 × 3 for 8, read left to right), and the editor cuts that sheet into the same frame layers.

**Layer › Flipbook › Make Flipbook Sheet…** lays the selected frame layers (bottom to top) out left to right and top to bottom in a new document: fitted, square, or the smallest of Roblox's 2×2, 4×4 and 8×8 grids. **Split Flipbook Sheet…** cuts a sheet back into one layer per frame. Both open a new document and leave the source unchanged.

### Animation Preview

When Codex returns an animation, the **Animation Preview** window opens: the GIF plays on the left at its own speed, and on the right the flipbook sheet it makes is laid out in its grid. The frame on show is lit in the sheet and in the strip of every frame along the bottom, so you can see which cell each moment of the GIF lands in.

- **Play and step:** click the player or press Space to play or pause; the buttons, or ← and →, step one frame. The slider sets the speed in frames per second (it starts at the GIF's own timing).
- **Pick a frame:** click a cell of the sheet or a thumbnail in the strip.
- **Grid:** Fit, Square or Roblox (2×2, 4×4 or 8×8) shows the sheet as **Make Flipbook Sheet** would make it, with its size in pixels; the window's **Make Flipbook Sheet** button makes it.
- **Background:** a checkerboard, dark or light behind transparent pixels.

**Layer › Flipbook › Preview Animation…** opens the window for any animation: the selected frame layers, the frames beside the active one, or a flipbook sheet (its grid read from its name, such as `flipbook 4x4`, and set with **Columns** and **Rows**), which **Split Flipbook Sheet** then cuts into frames. The preview follows edits: paint on a frame and it is drawn again.

## Ask Codex to edit

Use the **Assistant** tab to describe an editing task. Codex can inspect the live editor and run the same engine commands used by the menus. Enable **Selected area** when the task should use a selection, and **GIF** to let it make animations. Choose **Stop** to cancel an active task.

While Codex works you can keep typing: the send arrow beside **Stop** adds your message to the running task, as in Codex. The status line shows what Codex is thinking about, its plan's progress, web searches and reconnection attempts. Each of Codex's messages is shown as its own paragraph. If Codex answers a turn with a different model than the one you chose (a safety reroute), the panel says so and your choice is unchanged. When an image request hits your ChatGPT plan's image limit, the panel says when it resets.

The underlying supported integration is the [Codex app-server](https://learn.chatgpt.com/docs/app-server). Image-generation behavior is documented in [ChatGPT image generation](https://learn.chatgpt.com/docs/image-generation).

## Camera Raw (v8)

Filter › Camera Raw Filter no longer lags while you move sliders. The preview develops in the
background, shows a quicker half-size draft while you drag and the full preview when you let go,
and colour, detail and effects sliders reuse the tone work already done. On a 900 × 600 preview a
typical edit went from about 2 s to about 0.1 s per change.

New controls: **Auto** (Ctrl+Shift+U), White Balance As Shot / Auto / Custom and the White Balance
tool (I), RGB / Red / Green / Blue point curves and region splits, colour-grading wheels,
**Black & White** (V) with a B&W Mix, and a Calibration panel. Double-clicking a slider restores
that control's own default; right-click a section header to reset the section. Black & White and
Calibration have no verified PSD mapping yet, so a filter using them is not written to PSD.
Details: [Camera Raw notes](camera-raw-histogram.md).

## YorusAi Direct image tools

v13 fixes **Nano Banana** on Replicate. Replicate refuses Google's Nano Banana models an image uploaded through its files API: the request fails at once with HTTP 500, "An unexpected error ocurred" (measured on Nano Banana 2.1 and 2 in November 2026), so every Nano Banana remix with a reference image failed. Nano Banana's reference images now go inline in the request instead, as Replicate's docs suggest for small files: an image over 1 MB is re-encoded to fit (JPEG when it's fully opaque, PNG to keep transparency) and scaled down only when it has to be. Ideogram, FLUX, Seedream 5.0 Pro and Crystal Upscale accept uploaded files and keep using them.

v11 replaces Seedream 5.0 Pro with **Seedream 5.5** on BytePlus ModelArk (`dola-seedream-5-5-261028`) and removes OpenAI's GPT Image API from YorusAi Direct; Codex's own image generation through your ChatGPT account is unchanged. A saved Seedream 5.0 Pro choice moves to Seedream 5.5 and a saved GPT Image choice to Nano Banana. Seedream 5.5 takes 1K, 1.5K or 2K output, follows the selection or canvas shape with **Auto**, and can return a transparent background (a PNG with alpha) for sprites, icons and effect textures. Its watermark is off. A ModelArk account has to activate the model in the BytePlus console once before its first request. With only a Replicate key, Seedream runs Seedream 5.0 Pro, which Replicate hosts. Foreground layer-separation images account for transparent outside padding when fitting the provider's bounding box.

Choose **YorusAi** at the top of the assistant panel, or the wand in the right rail. This is a native port of the supplied local plugin. It contains **Remix**, **Upscale**, and **Layers** modes, reference-image attachments, a prompt builder, and model-specific ratio, resolution and quality controls.

Open **API keys** and save the provider key you want to use. Replicate supplies Nano Banana, Ideogram, FLUX, Seedream 5.0 Pro and Crystal Upscale workflows. BytePlus provides Seedream 5.5 and layer separation. Model availability and billing are controlled by those providers. These requests use your provider account and do not consume the Codex ChatGPT subscription's image allowance. No provider key is included in this package.

On Windows, keys are stored in Windows Credential Manager for your current account. Editor preferences, UI inspection and MCP expose only whether a key is configured. Password drafts, image attachments and provider secrets are not serialized into portable settings. Keys do not travel with the portable folder.

For **Selected area**, the visible crop is the reference and the saved selection is the exact destination. Returned variants or separated parts are placed on separate layers; part coordinates are normalized into the original captured frame before placement. **New canvas** creates independent canvases. Upscale also supports an explicit new-canvas destination to preserve its larger pixel dimensions. A result that cannot be placed is kept for recovery. Stop prevents insertion of returned results; cancellation of remote processing depends on provider support.

**Grow canvas to fit** works as in the Codex Images tab and is on by default: a result with more pixels than the selection enlarges the whole canvas so every pixel is kept. Switch it off for small selected-area edits, or a 2K result in a small box can grow a 1920 × 1080 picture several times over.

The first successful run needs a valid provider key and available provider credits. Provider request contracts and placement are covered by local mock tests. v13's Nano Banana requests were also checked live: whole-picture remixes and selected-area edits at 2K, placed on the canvas in 20 to 50 seconds.

## Connect another MCP client

Start yorusphoto through the launcher first. Add the server entry from the generated `mcp-config.json` to the MCP client's configuration. It starts the bundled CLI with this structure:

```json
{
  "mcpServers": {
    "yorusphoto": {
      "command": "C:\\path\\to\\yorusphoto-portable\\yorusphoto-cli.exe",
      "args": ["mcp", "--bridge", "127.0.0.1:50495", "--control-token-file", "C:\\path\\to\\yorusphoto-portable\\data\\control.token"]
    }
  }
}
```

The MCP bridge provides `command_list`, `command_run`, `command_batch`, `doc_inspect`, `ui_inspect`, `ui_screenshot`, and `control_call` for the running editor. Discover command IDs and parameters with `command_list`. Automation file reads and writes are limited to the portable `workspace/` directory; interactive Open/Save dialogs still work normally.

External agents discover the native `yorusai.run`, `yorusai.remix`, `yorusai.upscale`, `yorusai.layers`, `yorusai.inspect` and `yorusai.cancel` commands through `command_list`. For example, `command_run` with `id: "yorusai.remix"` and `params: {"prompt":"Add tropical plants","mode":"selection"}` uses the same capture, provider and placement workflow as the panel. Poll `yorusai.inspect` for completion. `yorusai.remix` also takes `fit`, as `ai.generate` does: `"exact"` resamples a selected-area result into the selection and `"grow"` enlarges the canvas to keep its pixels (the panel's **Grow canvas to fit**). API keys are configured in the local panel.

Agents can also start Codex image generation without transferring image bytes over MCP:

```json
{
  "method": "engine.execute",
  "params": {
    "command": "ai.generate",
    "params": {
      "prompt": "Replace the selected area with a lush tropical garden",
      "mode": "selection",
      "quality": "high",
      "width": 1024,
      "height": 1024
    }
  }
}
```

Pass that object to `control_call`. Use `"mode": "canvas"` to generate a new independent image. Add `"animation": 8` for a looping GIF of 8 frames (one layer per frame; `"frames": "sheet"` lays a new canvas out as a flipbook sheet), `"transparent": true` for a cutout, and `"fit": "exact"` to scale a larger result into the selection instead of growing the canvas. Generation starts asynchronously; poll `ui_inspect` and read the Codex panel status until it finishes or reports an error. The built-in panel and external agents use the same capture and placement behavior.

An animation that arrives opens the Animation Preview; `ui_inspect` reports it as `animationPreview` (its frames, the frame on show, playing or not, speed and grid). `layer.flipbook.preview` opens it from a script: `{}` previews what the active document holds, `{"layers": [ids]}` those frame layers, `{"sheet": {"layer": id, "columns": 4, "rows": 4}}` a sheet, and `"play": false` opens it paused.

The lower-level `ai.capture`, `ai.apply`, and `ai.open` commands use base64 image payloads. Existing MCP/control transport limits remain **1 MiB per request** and **8 MiB per response**, so these inline commands are suitable only for small images over MCP. Use `ai.generate` to start the native generation workflow for larger images.

Keep `data/control.token` private. The launcher restricts the settings directory and token to the current Windows user; it never writes the token into MCP configuration or command arguments. Only local loopback control is enabled.

## Local limits and build notes

- Capture and placement accept up to **16,777,216 pixels** total and **16,384 pixels** per side. Returned images must be PNG, JPEG, or WebP, with at most **64 MiB** of encoded bytes; decoder allocations are capped at **128 MiB**.
- The native Codex bridge accepts reference PNGs and generated PNG or GIF files up to **32 MiB**. An animation keeps at most 256 frames, all the same size. The panel's canvas dimensions and preferred long edge range from **64 to 3840 pixels**; engine commands have the larger limits above.
- References are color-managed sRGB PNGs. Returned layers are converted to the destination document's current profile, color model, and 8-, 16-, or 32-bit depth. Selection coverage is preserved in a 16-bit grayscale PNG.
- A new independent canvas uses sRGB RGB pixels and the returned image's 8- or 16-bit depth.
- This custom portable build is unsigned. Version 7 uses the optimized release profile; older local builds used the development profile. Packaging defaults to release binaries and also accepts an explicit build directory.

The interface translations are a build input: check out [craft-i18n](https://github.com/storytold/craft-i18n) at the commit in `craft-i18n.rev` below `.toolchain/craft-i18n` first (see the [packaging tools](../scripts/yorusphoto/README.md)); without it the editor is English only. To rebuild from this checkout with the installed local GNU/LLVM toolchain:

```powershell
powershell -File packaging/windows/cargo-yorusphoto.ps1 build --release -p photocraft -p photocraft-cli
powershell -File packaging/windows/package-yorusphoto.ps1 -Name yorusphoto-portable-next
```

The packaging script accepts an explicit `-BuildDirectory`, `-CodexVendorDirectory`, `-CodexLicensePath`, `-CodexNoticePath`, `-EguiFontsDirectory`, and `-RuntimeDlls` when those inputs live elsewhere. It assembles a fresh folder and refuses to replace existing packages. Validate the binaries, UI, generation, and MCP connection before creating a ZIP with `-Name <the same folder name> -ArchiveOnly`. The ZIP uses an explicit distribution-file allowlist and excludes user settings, control tokens, credentials, logs, and generated workspaces. MCP configuration is regenerated locally by the launcher after extraction.

## Verification status

v13 fixed Nano Banana in YorusAi Direct and passed 3,661 engine library tests (16 ignored), 296 native app tests (3 ignored), 85 CLI tests and 7,128 UI library tests (24 ignored), with the pinned craft-i18n translations and craft-fonts. Strict Clippy (warnings denied) for the editor, app, engine and CLI crates and the dependency layer check (30 crates, no violations) passed. New tests cover Nano Banana's inline references (no files API upload, and the retry on Nano Banana 2), the other models' uploads with their inline fallback, a reference over 1 MB re-encoded under it (JPEG when opaque, PNG keeping transparency, the shape kept), and `fit` on `yorusai.remix`. The release binaries were built with their build paths remapped and passed the release privacy check. The packaged portable passed the nine local editor/MCP checks and one live Nano Banana 2.1 request through YorusAi Direct on a real Replicate account: a selected-area edit at 1K with `fit: "exact"`, placed as its own layer in 20 seconds with the canvas kept at 1920 × 1080.

v12 added the Animation Preview and passed 3,661 engine library tests (16 ignored), 294 native app tests (3 ignored), 85 CLI tests and 7,127 of 7,128 UI library tests (24 ignored), with the pinned craft-i18n translations and craft-fonts. The UI test left over, upstream's check that opening a document leaves the native menus' structure alone, fails only when it runs at the same moment as the plug-in test, which installs a sample plug-in into the menus between its two looks; it passes on its own. Strict Clippy (warnings denied) for the editor, app, engine and CLI crates and the dependency layer check (30 crates, no violations) passed. New tests cover the preview's frames (whole frames, a placed area, a sheet's cells), finding an animation from the selection, a frame or a sheet's name, playback timing, the window opening on Codex results, and Make and Split Flipbook Sheet from it. The release binaries were built with their build paths remapped and passed the release privacy check. The packaged portable passed the nine local editor/MCP checks and the Animation Preview's on-screen checks: opening on frame layers and on a sheet, playing, the hover sign, Space, the arrow keys (they never reach the layer under the window) and a click on a sheet's cell. No model or paid provider request was made.

v11 merged PhotoCraft 0.23.1 and passed 7,124 UI library tests (24 ignored), 3,658 engine library tests (16 ignored), 294 native app tests (3 ignored) and 85 CLI tests. The UI tests were built with the pinned craft-i18n translations and craft-fonts, as upstream's CI runs them; without the fonts, 25 tests that draw Japanese, Chinese, Korean or Arabic labels in their first headless frame stop at egui's missing-glyph check (the app itself loads Windows' fonts for those scripts). Strict Clippy (warnings denied) for the editor, app, engine and CLI crates and the dependency layer check (30 crates, no violations) passed; the WebAssembly check was not run (its target isn't installed on the build machine). New tests cover canvas growth and exact fit, GIF and WebP frames as layers or a flipbook sheet, Layer › Flipbook, the bounded marquee's Space moves and Fixed Size sliding, Seedream 5.5 requests, the removed GPT Image route, and the Codex bridge's GIF and inline images, image-limit messages, paragraphs, reasoning headings, plan status, steering and reroutes. The release binaries were built with their build paths remapped and passed the release privacy check. The packaged v11 portable passed ten editor/MCP checks with synthetic pixels: canvas growth from 40 × 30 to 160 × 120 with the layer at the scaled selection, an exact fit, a four-frame GIF placed as frame layers with the first shown, a Roblox 2 × 2 flipbook sheet and its split, a GIF opened as a sheet, and the new Codex settings. On a connected ChatGPT account, through the bundled Codex 0.169.0: a still image opened in a new canvas; a message sent while a chat task ran joined that turn (its reply answered both); and a four-frame animation into a selected area. Codex's image service refused GIF output for that account, so Codex drew the frames as a 2 × 2 grid and the editor placed them as four frame layers, growing the canvas to 1254 × 1254 to keep every pixel. No YorusAi provider request was made: Seedream 5.5 requests are covered by local mock tests (a live run needs the model activated in the ModelArk console and is billed to that account).

The later canvas-boundary selection update passed all 2,413 UI library tests (11 pre-existing ignores), including 13 new boundary/ratio regressions. Six GPU canvas integration tests passed for zoom scaling, crop beyond the canvas and drag previews. The focused marquee, Space and snapping suites also passed; this change leaves provider networking, saved-chat services and image-import commands unchanged. Final native and installed-file evidence is recorded in `verification/selection-boundary-final-verification.json` in the source checkout.

The v7 optimized update passed 2,666 tests across nine affected unit/integration targets
(11 ignored, zero failed): UI 2,400, native 163, CLI four, raster 23 and GPU 76. This includes
16 native Direct tests and 20 Direct panel tests covering Auto request sizing, transparent
foreground padding, odd-offset captures, irregular/fractional masks, PNG/JPEG/WebP results,
changed selections, source preservation, undo and new canvases. All-target checks, strict
Clippy, WebAssembly, scoped formatting, dependency layers (29 crates), the optimized native
app/CLI build and the targeted quick performance gate passed.

The stripped release portable passed 11 editor/MCP checks and ten saved-chat/workspace
checks across three launches. Local fixture placement returned a separate layer at exactly
`(8,9)` with size `16x12`; source pixels and a newer selection survived. Whole-image import
opened an independent `128x96` canvas. Both saved conversations retained their transcripts,
unsent drafts and model/reasoning choices after restart. These checks made no subscription
turns, image requests or paid Direct-provider calls. The current Codex runtime, model
discovery, update mechanism and image capture/apply rules are retained.
All 14 native screenshots passed independent visual inspection, including the compact
composer, Direct tools and saved-chat views. Exact selected-area bounds are established by
the native assertions; that source tab is inactive in the final canvas screenshot.

Paired optimized 24/36-megapixel measurements show 2.5–2.9× faster raster copying,
Navigator visibility updates from 310/594 ms to about 40 ms, and approximately 20–21%
lower full-layer drag latency without shadows. Lower sampled RAM applies to the Navigator
fixture; GPU-workflow RAM remained approximately unchanged. GPU scratch tests confirm
bounded idle retention while required active textures remain valid. Full measurement scope,
variability and remaining VRAM/move-preview work are in `yorusphoto-performance.md`.

The v6 update passed 2,392 UI library tests (11 ignored), 155 native tests and four CLI library tests. After the final update-preference persistence fix, all 57 focused Codex UI tests, all 155 native tests and all four CLI library tests passed again. All-target Clippy with warnings denied, WebAssembly, dependency layers (29 crates), parity (631/631), formatting (14 Rust files) and the final native app/CLI build passed.

The final portable passed six real updater checks, ten saved-chat/workspace checks across three launches and 13 editor/MCP/live-generation checks. A controlled copy starting with Codex 0.150.1 automatically downloaded and SHA-512-verified 0.163.0, retained all 47 package files, passed isolated compatibility probes and atomically activated the release. Its native model discovery returned nine choices. Daily throttling, explicit manual checks and keeping updates disabled after restart passed. All eight workspace/updater screenshots passed independent visual inspection.

One actual Codex editor task created the requested test layer. One actual selected-reference generation returned a separate layer at exactly `(32,24)` with size `64x48`, while preserving the newer selection at `(1,1)` with size `4x3`. Local capture/undo checks preserved source pixels, and whole-image placement created an independent `128x96` canvas. An optional byte-replay comparison was skipped because the returned PNG exceeded the existing 1 MiB control-request limit; the native generation and exact visible layer bounds passed. No Direct-provider request was made. Launcher history-directory selection passed all three fixtures under both Windows PowerShell 5 and PowerShell 7.

The v5 workspace controls passed 2,383 UI library tests (11 ignored), 139 native library tests and four CLI library tests. The nullable-usage update also passed all 46 focused Codex UI tests, all 139 native tests and all four CLI library tests. The final layout and credit-warning refinements passed 47 focused Codex UI tests, including a rendered composer-visibility regression. All-target Clippy with warnings denied, scoped formatting, WebAssembly, all 29 dependency layers and parity (631/631) passed. Historical v4/v3 results below cover earlier sign-in and unchanged editor/provider workflows.

The final packaged v5 passed ten workspace checks across three native launches and 11 editor/MCP checks. Both saved chats retained their transcripts, drafts and model/reasoning settings; local search, archive/restore and slash-command validation passed without a model request. The editor checks reconfirmed exact captured-area layer placement, source preservation and independent new-canvas dimensions. All six final workspace screenshots passed visual inspection, including the compact composer and the real account usage/reset display.

A separate 13-check live trial used one real Codex reply to verify token/context telemetry, manual compaction, durable thread fork/read/list and restart restoration. That trial preceded the final UI layout and credit-warning polish; the native protocol remained unchanged. Its two test-created durable threads were subsequently archived after ownership verification. No image or Direct-provider requests were made for the v5 workspace checks. Final portable imports contain Windows system DLLs only.

The v4 sign-in and current-chat restoration fix passed 2,354 UI library tests (11 ignored), 127 native library tests and four CLI library tests. This includes six sign-in protocol regressions, five native disk/failure history tests and four UI lifecycle/restart tests. All-target Clippy with warnings denied, formatting, WebAssembly, dependency layers and the final native build passed.

The packaged v4 passed all 11 local editor/MCP checks and all seven new sign-in/history checks. A dedicated native process confirmed the existing ChatGPT session, edited only an unsent draft through the real UI, preserved its previous snapshot, closed cleanly and restored its thread, transcript and draft after relaunch. Native screenshots were visually inspected. These v4 checks made no model turns, image requests or paid provider calls. Historical v3 results below cover the unchanged editor, image geometry and Direct provider workflows.

The v3 build adds the default right assistant sidebar, ChatGPT-style native UI, Yorus branding cleanup, and the native YorusAi Direct workflows. October 15–16 checks and packaging results are recorded with the source in `verification/` and `log/devlog.md`.

October 15 unit and integration checks:

| Suite | Passed | Ignored |
|---|---:|---:|
| Engine library | 1,439 | 13 |
| I/O library | 157 | 1 |
| UI library | 2,350 | 11 |
| Native app library | 116 | 0 |
| Automation library | 64 | 0 |
| CLI library and integration | 31 | 0 |
| Native startup/control integration | 4 | 0 |

The final portable was checked on October 16 through its real authenticated stdio MCP bridge: all 11 checks passed, including default sidebar visibility, discovery of nine native workflow commands, invalid-input rejection without provider requests, exact captured placement, original-source preservation and fresh-canvas dimensions. The Assistant, Images, Remix, Upscale, Layers, API-key and compact-window screens were visually inspected. Final all-targets Clippy passed with warnings denied; formatting, the 29-crate layer gate, WebAssembly checks and menu parity (631/631) passed.

The eight Direct backend tests use local HTTP mocks, while 17 Direct UI tests cover captured selection placement, masks, source preservation, document switching, new canvases, layer ordering, recovery, cancellation and credential privacy. New default/migration and short-window tests cover the Codex sidebar. The earlier real subscription image validation below still applies to the unchanged Codex generation backend.


Checks completed in this checkout on October 12, 2026:

| Suite | Passed | Ignored |
|---|---:|---:|
| Engine unit tests | 1,439 | 13 |
| UI unit tests | 2,326 | 11 |
| Native app unit tests | 108 | 0 |

Clippy, the dependency-layer check (29 crates), and the WebAssembly check passed. Packaging script smoke tests verified initialization, MCP paths, helper binaries, portable markers, license notices, and exclusion of private state.

The adversarial command suite did not pass: the untouched `file.print` and `file.printOneCopy` commands exceeded its four-second timeout. No new generation command was listed as a failure.

The finished portable app was tested through its real stdio MCP connection: command discovery, editing and undo, scoped file access, original-source preservation, and exact-size new canvases passed. A real subscription Codex chat created the requested named layer. A real subscription image created a separate 64 × 48 layer at (32, 24), while the selection changed during generation remained at (1, 1) with dimensions 4 × 3. The native UI screenshot was visually inspected.

The v2 portable build corrects a Windows reference-read failure. Codex receives the captured PNG as an original-detail conversation attachment, and image editing uses that attached image rather than asking the image tool to reread its temporary filename. The attachment regression, all 108 native tests, formatting, and Clippy passed.

On October 13, the corrected portable app edited the supplied 3068 × 3068 photograph through a real subscription image request. It returned a new layer at the exact captured bounds, retained a selection changed during generation, and preserved the original layer's decoded RGBA pixels byte for byte. The generated export differed from the original and the native screenshot and edited photograph were visually inspected.

One extra pixel-comparison check encountered the existing inline MCP request-size limit because the generated PNG was larger than its verification payload budget. Native image generation completed successfully; it transfers the result through local files and does not use that inline MCP payload. Transparent results can have smaller visible content bounds while their full PNG is mapped to the captured rectangle. Generation and placement tests cover that geometry and selection coverage.

PhotoCraft is licensed under MIT OR Apache-2.0. The bundled Codex runtime is Apache-2.0; its license and notice are included below `codex/`. Asset, font, and branding license notices are included below `licenses/`; the supplied yorusphoto brand image has its own rights notice.
