//! Camera Raw's interactive preview develop, off the UI thread.
//!
//! The dialog asks for a render whenever a control moves; at most one runs at a time and, when it
//! lands, the next one starts with the newest settings (so the preview never queues behind old
//! values and the UI never waits on the develop). While a control is being dragged a *draft*
//! develops a half-size copy and scales it back up, as Camera Raw shows a coarser preview during
//! a drag; releasing the control renders the full proxy. Each size keeps a
//! [`Developer`](photocraft_algo::camera_raw::Developer), so moving a colour, detail or effects
//! control does not redo the tone stages. Wasm has no worker: renders run inline there.
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

use photocraft_algo::camera_raw::{CameraRaw, Developer};
use photocraft_algo::histogram::RgbHistogram;
use photocraft_compose::Buffer;
use photocraft_engine::display_color::CanvasDisplay;
use photocraft_geom::Rect;

/// Proxies smaller than this many pixels never draft (a full render is already quick).
const DRAFT_MIN_PIXELS: usize = 200_000;

/// A finished render.
pub(crate) struct Rendered {
    pub(crate) draft: bool,
    /// Developed proxy pixels (always the full proxy size, also for drafts).
    pub(crate) px: Vec<[f32; 4]>,
    pub(crate) histogram: RgbHistogram,
    pub(crate) histogram_ms: f64,
    pub(crate) image: egui::ColorImage,
    pub(crate) ms: f64,
}

/// One preview size's source pixels and kept stages.
#[derive(Default)]
struct Level {
    px: Vec<[f32; 4]>,
    w: usize,
    h: usize,
    developer: Developer,
}

/// What a render reads, owned by whichever thread is rendering.
pub(crate) struct Source {
    proxy: Vec<[f32; 4]>,
    selection: Option<Vec<f32>>,
    pw: usize,
    ph: usize,
    full_w: usize,
    float: bool,
    /// The open-time dialog's camera tone curve (see `CameraRawDialog::raw_curve`).
    raw_curve: Option<Vec<[f32; 2]>>,
    /// The raw stage settings `full` and `half` were retoned with, and a counter naming that
    /// source for the developers.
    retoned: Option<photocraft_io::raw::Tone>,
    generation: u64,
    full: Level,
    half: Option<Level>,
}

impl Source {
    pub(crate) fn new(
        proxy: Vec<[f32; 4]>,
        selection: Option<Vec<f32>>,
        (pw, ph): (usize, usize),
        full_w: usize,
        float: bool,
        raw_curve: Option<Vec<[f32; 2]>>,
    ) -> Self {
        Source { proxy, selection, pw, ph, full_w, float, raw_curve, retoned: None, generation: 0, full: Level::default(), half: None }
    }

    /// The pixels to develop at full and half size for `p`, retoned for the raw stage first when
    /// opening a raw (that stage acts on the raw data, so it is part of the source).
    fn prepare(&mut self, p: &CameraRaw, draft: bool) {
        let tone = self.raw_curve.as_ref().map(|_| super::camera_raw_ui::raw_stage(p));
        if self.full.px.is_empty() || self.retoned != tone {
            let mut px = self.proxy.clone();
            if let (Some(curve), Some(tone)) = (&self.raw_curve, tone) {
                photocraft_io::raw::retone(&mut px, self.pw, self.ph, curve, tone);
            }
            self.full = Level { px, w: self.pw, h: self.ph, developer: std::mem::take(&mut self.full.developer) };
            self.half = None;
            self.retoned = tone;
            self.generation = self.generation.wrapping_add(1);
        }
        if draft && self.half.is_none() {
            let (w, h) = (self.pw.div_ceil(2), self.ph.div_ceil(2));
            let mut px = vec![[0.0f32; 4]; w * h];
            for (i, o) in px.iter_mut().enumerate() {
                let (x, y) = (i % w * 2, i / w * 2);
                let mut sum = [0.0f32; 4];
                let mut n = 0.0;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    if let Some(q) = (x + dx < self.pw).then(|| self.full.px.get((y + dy) * self.pw + x + dx)).flatten() {
                        sum.iter_mut().zip(q).for_each(|(s, v)| *s += v);
                        n += 1.0;
                    }
                }
                *o = sum.map(|v| v / f32::max(n, 1.0));
            }
            self.half = Some(Level { px, w, h, developer: Developer::default() });
        }
    }

    /// Develops for `p` (a draft when asked and worthwhile), blends by the selection, counts the
    /// histogram and converts for display.
    pub(crate) fn render(&mut self, p: &CameraRaw, draft: bool, display: &CanvasDisplay) -> Rendered {
        let t0 = crate::gpu_canvas::now_ms();
        let draft = draft && self.pw * self.ph >= DRAFT_MIN_PIXELS;
        self.prepare(p, draft);
        let mut p = p.clone();
        if self.raw_curve.is_some() {
            (p.shadows, p.highlights, p.clarity, p.texture, p.dehaze) = (0.0, 0.0, 0.0, 0.0, 0.0);
        }
        let generation = self.generation;
        let full_w = self.full_w.max(1) as f32;
        let float = self.float;
        let develop = |level: &mut Level| {
            let mut p = p.clone();
            p.pixel_scale = (level.w as f32 / full_w).min(1.0);
            level.developer.develop(&level.px, generation, level.w, level.h, &p, float)
        };
        let mut px = match self.half.as_mut().filter(|_| draft) {
            Some(half) => {
                let (hw, hh) = (half.w, half.h);
                let small = develop(half);
                upscale(&small, hw, hh, self.pw, self.ph)
            }
            None => develop(&mut self.full),
        };
        if let Some(mask) = &self.selection {
            for ((out, original), coverage) in px.iter_mut().zip(&self.proxy).zip(mask) {
                for (out, original) in out.iter_mut().zip(original) {
                    *out = *original + (*out - *original) * coverage;
                }
            }
        }
        let histogram_start = crate::gpu_canvas::now_ms();
        let histogram = RgbHistogram::from_rgba(&px);
        let histogram_ms = crate::gpu_canvas::now_ms() - histogram_start;
        let image = image(&px, self.pw, self.ph, display);
        Rendered { draft, px, histogram, histogram_ms, image, ms: crate::gpu_canvas::now_ms() - t0 }
    }
}

/// A render that reports a panic as an error instead of taking the dialog down.
fn render_guarded(source: &Mutex<Source>, p: &CameraRaw, draft: bool, display: &CanvasDisplay) -> Result<Rendered, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut source = source.lock().unwrap_or_else(PoisonError::into_inner);
        source.render(p, draft, display)
    }))
    .map_err(|_| tl!("Camera Raw preview failed; the previous preview remains").to_string())
}

/// `px` (straight RGBA) as a texture image through the document's display transform.
pub(crate) fn image(px: &[[f32; 4]], w: usize, h: usize, display: &CanvasDisplay) -> egui::ColorImage {
    let buf = Buffer { rect: Rect::new(0, 0, w as i32, h as i32), px: px.to_vec() };
    let img = display.to_rgba8(&buf);
    egui::ColorImage::from_rgba_unmultiplied([img.width as usize, img.height as usize], &img.pixels)
}

/// `src` (`sw × sh`) bilinearly scaled to `w × h`.
fn upscale(src: &[[f32; 4]], sw: usize, sh: usize, w: usize, h: usize) -> Vec<[f32; 4]> {
    if sw == 0 || sh == 0 || src.len() != sw * sh {
        return vec![[0.0; 4]; w * h];
    }
    let at = |x: usize, y: usize| src.get(y.min(sh - 1) * sw + x.min(sw - 1)).copied().unwrap_or([0.0; 4]);
    let (fx, fy) = (sw as f32 / w.max(1) as f32, sh as f32 / h.max(1) as f32);
    (0..w * h)
        .map(|i| {
            let (x, y) = (((i % w) as f32 + 0.5) * fx - 0.5, ((i / w) as f32 + 0.5) * fy - 0.5);
            let (x, y) = (x.max(0.0), y.max(0.0));
            let (x0, y0) = (x as usize, y as usize);
            let (tx, ty) = (x - x0 as f32, y - y0 as f32);
            let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
            [0, 1, 2, 3].map(|k| {
                let top = a[k] + (b[k] - a[k]) * tx;
                let bottom = c[k] + (d[k] - c[k]) * tx;
                top + (bottom - top) * ty
            })
        })
        .collect()
}

/// The dialog's side: the shared source, the running render and what was shown last.
pub(crate) struct Renderer {
    source: Arc<Mutex<Source>>,
    pending: Option<(u64, Receiver<Result<Rendered, String>>)>,
    seq: u64,
    applied: u64,
    /// The last shown render was a draft (a full one follows when the drag ends).
    pub(crate) showing_draft: bool,
}

impl Renderer {
    pub(crate) fn new(source: Source) -> Self {
        Renderer { source: Arc::new(Mutex::new(source)), pending: None, seq: 0, applied: 0, showing_draft: false }
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Renders now, on this thread (control channel, tests, opening). Waits for a running
    /// background render to release the source; that older result is then ignored.
    pub(crate) fn render_now(&mut self, p: &CameraRaw, display: &CanvasDisplay) -> Rendered {
        self.seq += 1;
        self.applied = self.seq;
        self.showing_draft = false;
        let mut source = self.source.lock().unwrap_or_else(PoisonError::into_inner);
        source.render(p, false, display)
    }

    /// Starts a background render of `p` unless one is running (then `false`: ask again when it
    /// lands, with the newest settings).
    pub(crate) fn request(&mut self, ctx: &egui::Context, p: &CameraRaw, draft: bool, display: Arc<CanvasDisplay>) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.seq += 1;
        let seq = self.seq;
        let (tx, rx) = std::sync::mpsc::channel();
        let work = {
            let (source, p, ctx, tx) = (self.source.clone(), p.clone(), ctx.clone(), tx.clone());
            move || {
                let _ = tx.send(render_guarded(&source, &p, draft, &display));
                ctx.request_repaint();
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if std::thread::Builder::new().name("camera-raw-proxy".into()).spawn(work).is_err() {
            // No thread to be had: report it; the shown preview stays.
            let _ = tx.send(Err(tl!("Cannot start the Camera Raw preview worker").to_string()));
        }
        #[cfg(target_arch = "wasm32")]
        {
            drop(tx);
            work();
        }
        self.pending = Some((seq, rx));
        true
    }

    /// A finished background render newer than what is shown, if one has landed.
    pub(crate) fn poll(&mut self) -> Option<Result<Rendered, String>> {
        let (seq, rx) = self.pending.as_ref()?;
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(tl!("Camera Raw preview worker stopped").to_string()),
        };
        let seq = *seq;
        self.pending = None;
        if seq <= self.applied {
            return None; // A synchronous render already showed newer settings.
        }
        self.applied = seq;
        if let Ok(r) = &result {
            self.showing_draft = r.draft;
        }
        Some(result)
    }
}
