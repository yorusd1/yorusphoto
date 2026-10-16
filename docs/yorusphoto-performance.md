# yorusphoto v7 placement and interactive performance

This Windows update addresses Direct image geometry and specific interactive hot paths. It
does not establish a Photoshop performance comparison or close the broader G2 performance gap.
The portable is built with Rust's optimized release profile (level 3, thin LTO, one codegen unit),
retaining normal error handling and unwinding. Packaging now defaults to release binaries.

## YorusAi Direct geometry

Direct OpenAI image generation previously normalized Auto to landscape 3:2. Auto now derives
its request size from the original captured selection or the requested new canvas. Request
dimensions approximate that ratio within the model's multiple-of-16, pixel-count and
aspect-ratio limits; local destination bounds remain exact. Very wide or
tall references use the provider's 3:1 limit with a visible status message. Explicit saved
ratios remain explicit. This follows the [official image editing API](https://developers.openai.com/api/reference/resources/images/methods/edit)
and its [size guidance](https://developers.openai.com/cookbook/examples/multimodal/image-gen-models-prompting-guide#gpt-image-2-size-options).
Native request mocks exercise portrait edits and square fresh generations without paid calls.

Layer separation previously fitted an entire padded PNG into a foreground object's bounding
box, adding its transparent border a second time. Explicitly positioned foreground parts now
fit their visible pixel extent into the provider's box. Only fully transparent outside pixels
are trimmed; faint alpha, soft shadows and internal holes survive. Base images, unpositioned
parts and ordinary remix/upscale frames retain their original framing.

The final capture/import engine shared with Codex is unchanged. Returned results still use
the captured rectangle and coverage mask, preserve a newer selection, create separate layers,
and leave source pixels intact. Whole-image generation still opens a new canvas. Provider
content can intentionally change composition; exact local placement does not force the model
to preserve every feature inside its generated frame.

## Performance changes and measurement

- Non-tile-aligned raster movement copies independent encoded tiles in parallel on native
  builds. Aligned moves retain copy-on-write tile sharing; small surfaces and WebAssembly
  retain the sequential path. Pixel format, bit depth and geometry are unchanged.
- The Navigator keeps its proxy rendering path after large-document edits. Previously a
  visibility change could trigger an exact full-resolution CPU composite beside the GPU
  canvas. Exact and proxy thumbnails are never mixed, including when styling an empty layer
  changes sampling policy without changing document pixels.
- Reusable effect scratch textures have an idle byte limit: at most 1/16 of the configured
  compositor budget, capped at 128 MiB. Required current-frame work remains available and
  can exceed that allowance. Closing a document releases shared scratch. Diagnostics now
  count scratch that older compositor totals omitted; those totals are not directly
  comparable across versions.

Measurements use an Intel Core i9-12900, 64 GiB RAM and NVIDIA RTX 4070 with 12 GiB VRAM.
Before and after algorithms are built in release mode against an isolated pre-change source
snapshot. The raster fixture allocates unique dense source tiles at 24 and 36 megapixels,
with one sample depth per process. The Navigator fixture measures the actual warmed UI
cache/update path for three synthetic full-canvas RGB8 layers; it excludes GPU upload and
full-window frame time. These are bounded synthetic cases rather than a benchmark of
arbitrary customer documents.

Paired raster-copy results (five timed moves per depth, after warm-up):

| Surface | Depth | Before median | After median | Speedup |
|---|---|---:|---:|---:|
| 6000 × 4000 | RGBA8 | 33.30 ms | 11.61 ms | 2.87× |
| 6000 × 4000 | RGBA16 | 62.00 ms | 21.60 ms | 2.87× |
| 6000 × 4000 | RGBA32f | 101.36 ms | 40.10 ms | 2.53× |
| 6000 × 6000 | RGBA8 | 51.89 ms | 17.67 ms | 2.94× |
| 6000 × 6000 | RGBA16 | 91.93 ms | 32.36 ms | 2.84× |
| 6000 × 6000 | RGBA32f | 151.93 ms | 59.87 ms | 2.54× |

Sampled CPU working set increased by approximately 1.4–2.0 MiB;
this small difference is consistent with worker/runtime overhead, without isolating
its cause. The required source and destination pixel storage is unchanged. These timings
exclude GPU uploads and compositing. The earlier cold baseline is retained separately;
the table uses the immediate paired rerun to limit machine-load and thermal differences.

Paired interactive-path medians (seven timed operations after warm-up):

| Surface | Measured operation | Before | After |
|---|---|---:|---:|
| 24 MP | Navigator visibility update | 310.43 ms | 40.28 ms |
| 36 MP | Navigator visibility update | 594.09 ms | 39.79 ms |
| 24 MP | GPU full-layer drag, no shadow | 119.26 ms | 94.22 ms |
| 36 MP | GPU full-layer drag, no shadow | 180.09 ms | 143.54 ms |
| 24 MP | GPU full-layer drag, drop shadow | 175.51 ms | 154.28 ms |
| 36 MP | GPU full-layer drag, drop shadow | 285.82 ms | 236.02 ms |

The Navigator's sampled process working-set peaks fell by about 140 MiB at 24 MP and
151 MiB at 36 MP in this fixture. GPU-only visibility refresh remained around 6 ms;
the much larger previous visibility delay came from the separate Navigator CPU path.
The drag harness includes move-tool mutation and GPU refresh, without the Navigator or
other whole-window UI work. Its smaller improvement does not establish 60 FPS dragging.
Some 36 MP shadow samples varied substantially (candidate 229–349 ms); seven sequential
before/after samples cannot isolate background load or thermal drift. Existing user apps
remained running, while Cargo, compression and other verification apps were held during
these paired measurements. No broad RAM reduction or total-VRAM comparison is inferred.

Measurements, binary/source hashes, CPU working-set measurements and rendering details are
recorded under verification/v7-*.json. The separate effect scratch regression measures
retained logical texture bytes for growing windows; it does not claim equivalent physical
driver VRAM savings. Documents with unsupported GPU features, complex effects, very large
history or other resource pressure still require further profiling.

In that real-GPU allocation regression, 57 successive growing texture pairs would retain
182,886,400 logical bytes under the old age-only policy. The bounded pool retained
8,388,608 bytes, including the active pair, and passed active-view validity and close/age
reclamation checks. This fixture measures scratch-pool retention, not total process VRAM.

An actively reused scratch texture may be larger than a later operation needs. Such a texture
remains exempt from idle eviction while used each frame; the idle limit does not implement
immediate shrink reclamation or a strict total scratch cap.

The existing canvas/layer budgets still derive from system RAM and the editor's memory
preference, rather than a dynamic dedicated-VRAM budget. Safe adapter-matched budget queries
and coordinating all GPU pools remain follow-up work; v7 does not claim to eliminate
oversubscription when other applications consume VRAM. Large moves still copy and upload
their raster tiles. Avoiding those transfers requires a separate non-destructive GPU move
preview architecture with mask/effect/group parity.
