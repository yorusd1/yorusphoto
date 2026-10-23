//! Shadows and Highlights as a local Laplacian filter.
//!
//! Adobe has said Camera Raw's process version 2012 rests on local Laplacian filters (S. Paris,
//! S. W. Hasinoff, J. Kautz, *Local Laplacian Filters: Edge-aware Image Processing with a
//! Laplacian Pyramid*, SIGGRAPH 2011). This follows that paper, in its fast form (M. Aubry et
//! al., *Fast Local Laplacian Filters*, ACM TOG 2014: the remapping sampled at a few reference
//! values, each pyramid coefficient interpolated between them), on the log2 luminance:
//!
//! - **Shadows** raises pixels darker than their surroundings, by an amount set by their own and
//!   their surroundings' place in the image's range (1st to 99th percentile); **Highlights**
//!   pulls pixels brighter than their surroundings toward them. Differences under a threshold
//!   are left alone, so fine detail stays.
//! - Shadows then keeps the image's 99th percentile, Highlights its 1st (Photoshop keeps the
//!   brightest content under Shadows and the darkest under Highlights).
//! - A uniform image is unchanged, edges keep no halo, and the result does not depend on the
//!   image size.
//!
//! A coefficient takes part only from the two references bracketing its own level's value, so
//! the coefficients are grouped by reference once and each reference evaluates only its groups
//! (the coarser level brought up just at those samples). References run in parallel; the
//! remapped full-size level is never stored, only the few rows its first blur is reading.
//!
//! The strengths were fitted to Photoshop 25.4's Camera Raw Filter on ten public-domain photos
//! (black-box measurements, no Adobe code or data). It is an approximation: on those photos the
//! mean error against Photoshop is 2.8–9.4 levels (of 255) where the previous heuristic's was
//! 3.7–14.3.

use super::{Encoding, luma};
use crate::photo_util::{par_map, par_rows};

/// Spacing of the fast filter's reference values in stops. Its remapping changes within about
/// half a stop, and coefficients interpolated across wider gaps show as contour lines on smooth
/// gradients (12 references over a whole raw's range left visible steps; at an eighth of a stop a
/// ramp's slope varies by under a third, as the remapping itself makes it).
const REF_STEP: f32 = 0.125;
/// The filter runs on a copy at most this many pixels across; the change it makes is brought back
/// to full size by a guided upsampling, so a preview and the full image agree.
const WORK_SIDE: usize = 1600;
/// Knots of the strength curves over the surroundings' place in the image range (0 … 1).
const KNOTS: usize = 5;
/// Images up to this many pixels keep their decoded channels between measuring and applying the
/// change (a preview's are reused; a full image's would cost too much memory).
const KEEP_LINEAR: usize = 1 << 22;

/// Shadows: how far a pixel darker than its surroundings rises, in stops per unit slider, as
/// a function of its own place in the image range (`own`) times a factor of its surroundings'
/// place (`around`); it rises fully once it is `sigma` stops below them. The lift depends on
/// the pixel's tone, not on the size of the difference, so it is the same at every pyramid
/// level and a big edge keeps no halo. Lowering (negative values) is `down` times as strong.
const SHADOWS: Lift = Lift { own: [-0.017, 0.466, 0.886, 1.221, -0.081], around: [4.566, 4.067, 1.088, 0.149, 0.878], sigma: 0.571, down: 0.808 };
const HIGHLIGHTS: Side = Side { pull: [0.179, 0.141, 0.196, 0.629, 2.07], push: [0.156, 0.08, 0.175, 0.049, 1.261], sigma: 0.05 };

/// See [`SHADOWS`].
struct Lift {
    own: [f32; KNOTS],
    around: [f32; KNOTS],
    sigma: f32,
    down: f32,
}

/// Highlights: strengths per unit slider over the surroundings' place in the image range, for
/// lowering (`pull`, toward the surroundings) and raising (`push`), and the detail threshold in
/// stops.
struct Side {
    pull: [f32; KNOTS],
    push: [f32; KNOTS],
    sigma: f32,
}

/// One channel, `w × h`.
struct Plane {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

/// Mirrors `i` into `0..n` (the edge sample is not repeated).
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n <= 1 {
        return 0;
    }
    let p = 2 * (n - 1);
    let m = i.rem_euclid(p);
    (if m >= n { p - m } else { m }) as usize
}

const TAPS: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// The coarser samples one sample of a finer level is brought up from: the binomial taps around
/// it that land on even (kept) positions, each doubled (so four times in two dimensions). The
/// mirroring keeps parity, so there are three for an even sample and two for an odd one.
#[derive(Clone, Copy)]
struct Up {
    n: usize,
    at: [usize; 3],
    wt: [f32; 3],
}

/// One pyramid step, from a `w × h` level to the `cw × ch` one below it.
struct Step {
    w: usize,
    h: usize,
    cw: usize,
    /// Rows (columns) of the finer level each coarser row (column) blurs.
    down_rows: Vec<[usize; 5]>,
    down_cols: Vec<[usize; 5]>,
    /// The coarser rows (columns) each finer row (column) comes up from.
    up_rows: Vec<Up>,
    up_cols: Vec<Up>,
}

impl Step {
    fn new((w, h): (usize, usize), (cw, ch): (usize, usize)) -> Self {
        let down = |n: usize, coarse: usize| -> Vec<[usize; 5]> { (0..coarse).map(|i| std::array::from_fn(|k| mirror((2 * i + k) as isize - 2, n))).collect() };
        let up = |n: usize, coarse: usize| -> Vec<Up> {
            (0..n)
                .map(|i| {
                    let mut u = Up { n: 0, at: [0; 3], wt: [0.0; 3] };
                    for (k, tap) in TAPS.iter().enumerate() {
                        let m = mirror((i + k) as isize - 2, n);
                        if m.is_multiple_of(2) && m / 2 < coarse && u.n < 3 {
                            u.at[u.n] = m / 2;
                            u.wt[u.n] = 2.0 * tap;
                            u.n += 1;
                        }
                    }
                    u
                })
                .collect()
        };
        Step { w, h, cw, down_rows: down(h, ch), down_cols: down(w, cw), up_rows: up(h, ch), up_cols: up(w, cw) }
    }

    /// One coarser row `out` from the five finer `rows` it blurs, through the row buffer `t`.
    fn blur(&self, rows: [&[f32]; 5], t: &mut [f32], out: &mut [f32]) {
        let n = rows.iter().map(|r| r.len()).min().unwrap_or(0).min(t.len());
        let [r0, r1, r2, r3, r4] = rows.map(|r| &r[..n]);
        for (x, o) in t[..n].iter_mut().enumerate() {
            *o = TAPS[0] * r0[x] + TAPS[1] * r1[x] + TAPS[2] * r2[x] + TAPS[3] * r3[x] + TAPS[4] * r4[x];
        }
        let t = &t[..n];
        let at = |x: usize| t.get(x).copied().unwrap_or(0.0);
        for (c, o) in self.down_cols.iter().zip(out) {
            *o = TAPS[0] * at(c[0]) + TAPS[1] * at(c[1]) + TAPS[2] * at(c[2]) + TAPS[3] * at(c[3]) + TAPS[4] * at(c[4]);
        }
    }

    /// The next coarser level of `src` into `dst` (the binomial blur at every other sample);
    /// `t` holds a row.
    fn down(&self, src: &[f32], t: &mut [f32], dst: &mut [f32]) {
        let w = self.w;
        for (r, o) in self.down_rows.iter().zip(dst.chunks_exact_mut(self.cw.max(1))) {
            self.blur(r.map(|y| src.get(y * w..(y + 1) * w).unwrap_or(&[])), t, o);
        }
    }

    /// [`down`](Self::down) from a level made a row at a time: `fill(y, row)` writes row `y`,
    /// kept in `ring` while the next coarser rows read it.
    fn down_from(&self, mut fill: impl FnMut(usize, &mut [f32]), ring: &mut Ring, t: &mut [f32], dst: &mut [f32]) {
        let w = self.w;
        for (r, o) in self.down_rows.iter().zip(dst.chunks_exact_mut(self.cw.max(1))) {
            // The rows one coarser row reads are at most five consecutive ones: distinct slots.
            for &y in r {
                let s = y % RING;
                if ring.tags[s] != Some(y)
                    && let Some(row) = ring.rows.get_mut(s * w..(s + 1) * w)
                {
                    fill(y, row);
                    ring.tags[s] = Some(y);
                }
            }
            self.blur(r.map(|y| ring.rows.get((y % RING) * w..(y % RING + 1) * w).unwrap_or(&[])), t, o);
        }
    }

    /// The coarser level `c` brought up at the finer sample (`x`, `y`).
    fn up_at(&self, c: &[f32], x: usize, y: usize) -> f32 {
        let (Some(ry), Some(rx)) = (self.up_rows.get(y), self.up_cols.get(x)) else { return 0.0 };
        let mut s = 0.0;
        for a in 0..ry.n {
            let row = c.get(ry.at[a] * self.cw..).unwrap_or(&[]);
            let mut t = 0.0;
            for b in 0..rx.n {
                t += rx.wt[b] * row.get(rx.at[b]).copied().unwrap_or(0.0);
            }
            s += ry.wt[a] * t;
        }
        s
    }
}

/// Levels down to about 2 pixels on the short side.
fn depth(w: usize, h: usize) -> usize {
    let mut s = w.min(h);
    let mut n = 1;
    while s > 2 {
        s = s.div_ceil(2);
        n += 1;
    }
    n
}

/// Rows [`Step::down_from`] keeps.
const RING: usize = 8;

/// The rows of a level made a row at a time (`tags`: which row each slot holds).
struct Ring {
    rows: Vec<f32>,
    tags: [Option<usize>; RING],
}

/// A level's coefficients grouped by the lower of the two references bracketing their value:
/// group `k` is `idx[start[k]..start[k + 1]]`, `frac` how far each lies toward reference `k + 1`;
/// `pos` is where each coefficient is in that order.
struct Groups {
    start: Vec<usize>,
    idx: Vec<usize>,
    frac: Vec<f32>,
    pos: Vec<usize>,
}

impl Groups {
    fn new(v: &[f32], lo: f32, hi: f32, step: f32, refs: usize) -> Self {
        let placed = par_map(v.len(), |i| {
            let t = (v[i].clamp(lo, hi) - lo) / step;
            let k = (t.max(0.0) as usize).min(refs - 2);
            (k, (t - k as f32).clamp(0.0, 1.0))
        });
        let mut start = vec![0usize; refs + 1];
        for &(k, _) in &placed {
            start[k + 1] += 1;
        }
        for k in 0..refs {
            start[k + 1] += start[k];
        }
        let mut fill = start.clone();
        let (mut idx, mut frac, mut pos) = (vec![0usize; v.len()], vec![0.0f32; v.len()], vec![0usize; v.len()]);
        for (i, &(k, f)) in placed.iter().enumerate() {
            let p = fill[k];
            fill[k] += 1;
            idx[p] = i;
            frac[p] = f;
            pos[i] = p;
        }
        Groups { start, idx, frac, pos }
    }

    fn range(&self, k: usize) -> std::ops::Range<usize> {
        self.start.get(k).copied().unwrap_or(0)..self.start.get(k + 1).copied().unwrap_or(0)
    }

    /// Where reference `k` takes part: its own group (weight `1 − frac`), then the group below
    /// (weight `frac`).
    fn of(&self, k: usize) -> impl Iterator<Item = (usize, bool)> + '_ {
        let below = if k > 0 { self.range(k - 1) } else { 0..0 };
        self.range(k).map(|p| (p, false)).chain(below.map(|p| (p, true)))
    }
}

/// The local Laplacian filter of `u`; `remap(first, values, out, reference)` writes the remapping
/// for one reference of `values`, the samples of `u` from index `first` on. The coarsest level is
/// kept.
fn llf(u: &Plane, remap: impl Fn(usize, &[f32], &mut [f32], f32) + Sync) -> Plane {
    let same = || Plane { w: u.w, h: u.h, v: u.v.clone() };
    let n = depth(u.w, u.h);
    // References span the image's tones; the deepest 0.1 % (noise down to the floor) share the
    // lowest reference instead of stretching the spacing.
    let hi = u.v.iter().fold(f32::NEG_INFINITY, |a, &v| a.max(v));
    let lo = quantile(&u.v, 0.001).min(hi);
    if n < 2 || !hi.is_finite() || !lo.is_finite() || hi - lo <= 1e-6 {
        return same();
    }
    let refs = (((hi - lo) / REF_STEP).ceil() + 1.0).clamp(2.0, 96.0) as usize;
    let step = (hi - lo) / (refs - 1) as f32;
    let mut dims = vec![(u.w, u.h)];
    for _ in 1..n {
        let (w, h) = dims[dims.len() - 1];
        dims.push((w.div_ceil(2), h.div_ceil(2)));
    }
    let steps: Vec<Step> = dims.windows(2).map(|d| Step::new(d[0], d[1])).collect();
    let mut g = vec![u.v.clone()];
    let mut t = vec![0.0f32; u.w];
    for (st, d) in steps.iter().zip(&dims[1..]) {
        let mut next = vec![0.0f32; d.0 * d.1];
        st.down(&g[g.len() - 1], &mut t, &mut next);
        g.push(next);
    }
    let groups: Vec<Groups> = par_map(n - 1, |l| Groups::new(&g[l], lo, hi, step, refs));
    // Per reference and level, the weighted coefficients in the order of `Groups::of`. Level 0
    // is never stored: the first step remaps it a row at a time, its coefficients one by one.
    let parts: Vec<Vec<Vec<f32>>> = par_map(refs, |k| {
        if groups.iter().all(|gr| gr.of(k).next().is_none()) {
            return Vec::new();
        }
        let r = lo + step * k as f32;
        let mut levels: Vec<Vec<f32>> = dims[1..].iter().map(|(w, h)| vec![0.0f32; w * h]).collect();
        let mut t = vec![0.0f32; u.w];
        let mut ring = Ring { rows: vec![0.0f32; RING * u.w], tags: [None; RING] };
        if let Some(first) = levels.first_mut() {
            let fill = |y: usize, out: &mut [f32]| remap(y * u.w, u.v.get(y * u.w..(y + 1) * u.w).unwrap_or(&[]), out, r);
            steps[0].down_from(fill, &mut ring, &mut t, first);
        }
        for l in 1..steps.len() {
            let (fine, coarse) = levels.split_at_mut(l);
            steps[l].down(&fine[l - 1], &mut t, &mut coarse[0]);
        }
        (0..n - 1)
            .map(|l| {
                let (st, gr, coarse) = (&steps[l], &groups[l], &levels[l]);
                gr.of(k)
                    .map(|(p, below)| {
                        let (i, f) = (gr.idx[p], gr.frac[p]);
                        let fine = if l == 0 {
                            let mut o = [0.0];
                            remap(i, u.v.get(i..i + 1).unwrap_or(&[]), &mut o, r);
                            o[0]
                        } else {
                            levels[l - 1].get(i).copied().unwrap_or(0.0)
                        };
                        let wgt = if below { f } else { 1.0 - f };
                        wgt * (fine - st.up_at(coarse, i % st.w, i / st.w))
                    })
                    .collect()
            })
            .collect()
    });
    // A coefficient's two parts, in group order, then summed back at each sample.
    let lap: Vec<Vec<f32>> = par_map(n - 1, |l| {
        let gr = &groups[l];
        let (mut own, mut below) = (vec![0.0f32; gr.idx.len()], vec![0.0f32; gr.idx.len()]);
        for (k, part) in parts.iter().enumerate() {
            let Some(c) = part.get(l) else { continue };
            let mine = gr.range(k);
            let (a, b) = c.split_at(mine.len().min(c.len()));
            for (o, v) in own.get_mut(mine).unwrap_or(&mut []).iter_mut().zip(a) {
                *o = *v;
            }
            if k > 0 {
                for (o, v) in below.get_mut(gr.range(k - 1)).unwrap_or(&mut []).iter_mut().zip(b) {
                    *o = *v;
                }
            }
        }
        gr.pos.iter().map(|&p| own[p] + below[p]).collect()
    });
    let mut o = g[n - 1].clone();
    for (st, lap) in steps.iter().zip(&lap).rev() {
        let mut next = vec![0.0f32; st.w * st.h];
        par_rows(&mut next, st.w, 1, |y, row| {
            for (x, v) in row.iter_mut().enumerate() {
                *v = st.up_at(&o, x, y) + lap[y * st.w + x];
            }
        });
        o = next;
    }
    Plane { w: u.w, h: u.h, v: o }
}

/// The `q` quantile (0 … 1) of `v`.
fn quantile(v: &[f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    let k = (((s.len() - 1) as f32 * q).round() as usize).min(s.len() - 1);
    *s.select_nth_unstable_by(k, f32::total_cmp).1
}

/// 1st and 99th percentiles of the log luminance on a copy at most 256 pixels across.
fn range(u: &Plane) -> (f32, f32) {
    let f = u.w.max(u.h).div_ceil(256).max(1);
    let (sw, sh) = (u.w.div_ceil(f), u.h.div_ceil(f));
    let mut sum = vec![(0.0f64, 0u32); sw * sh];
    for y in 0..u.h {
        for x in 0..u.w {
            let c = &mut sum[(y / f) * sw + x / f];
            c.0 += f64::from(u.v[y * u.w + x]);
            c.1 += 1;
        }
    }
    let cells: Vec<f32> = sum.iter().filter(|c| c.1 > 0).map(|c| (c.0 / f64::from(c.1)) as f32).collect();
    (quantile(&cells, 0.01), quantile(&cells, 0.99))
}

/// `tanh` over 0 … 10 at 1/64 steps (linear in between: off by under 3·10⁻⁵); 1 past it.
struct Tanh(Vec<f32>);

impl Tanh {
    const STEP: f32 = 1.0 / 64.0;
    fn new() -> Self {
        Tanh((0..=640).map(|i| (i as f32 * Self::STEP).tanh()).collect())
    }
    fn at(&self, z: f32) -> f32 {
        let p = z.max(0.0) / Self::STEP;
        let i = p as usize;
        match (self.0.get(i), self.0.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * (p - i as f32),
            _ => 1.0,
        }
    }
}

/// `d` (≥ 0) past the detail threshold: about 0 below `sigma`, `d − sigma` well above it.
fn excess(d: f32, sigma: f32, tanh: &Tanh) -> f32 {
    d - sigma * tanh.at(d / sigma)
}

fn knot(t: f32, k: &[f32; KNOTS]) -> f32 {
    let x = t.clamp(0.0, 1.0) * (KNOTS - 1) as f32;
    let i = (x as usize).min(KNOTS - 2);
    k[i] + (k[i + 1] - k[i]) * (x - i as f32)
}

/// The Highlights pass on the log luminance `u` (`amount` −1 … 1): pixels brighter than their
/// surroundings move toward them (or away, for positive values) by the part of the difference
/// past `sigma`, then the darkest content is put back where it was.
fn highlights_pass(u: &Plane, amount: f32, side: &Side) -> Plane {
    let (lo, hi) = range(u);
    let span = (hi - lo).max(1.0);
    let strengths = if amount < 0.0 { &side.pull } else { &side.push };
    let tanh = Tanh::new();
    let o = llf(u, |_, v, out, g| {
        let k = knot((g - lo) / span, strengths) * amount;
        for (o, &x) in out.iter_mut().zip(v) {
            let d = x - g;
            *o = if d > 0.0 { x + k * excess(d, side.sigma, &tanh) } else { x };
        }
    });
    let shift = quantile(&u.v, 0.01) - quantile(&o.v, 0.01);
    Plane { w: o.w, h: o.h, v: o.v.iter().map(|v| v + shift).collect() }
}

/// The Shadows pass on the log luminance `u` (`amount` −1 … 1).
fn shadows_pass(u: &Plane, amount: f32) -> Plane {
    let (lo, hi) = range(u);
    let span = (hi - lo).max(1.0);
    let k = if amount > 0.0 { amount } else { amount * SHADOWS.down };
    // The pixel's own factor is the same for every reference.
    let own = par_map(u.v.len(), |i| knot((u.v[i] - lo) / span, &SHADOWS.own));
    let o = llf(u, |first, v, out, g| {
        let around = k * knot((g - lo) / span, &SHADOWS.around);
        for ((o, &x), &own) in out.iter_mut().zip(v).zip(own.get(first..).unwrap_or(&[])) {
            let below = ((g - x) / SHADOWS.sigma).clamp(0.0, 1.0);
            *o = if below == 0.0 { x } else { x + around * own * (below * below * (3.0 - 2.0 * below)) };
        }
    });
    // Keep the brightest content.
    let shift = quantile(&u.v, 0.99) - quantile(&o.v, 0.99);
    Plane { w: o.w, h: o.h, v: o.v.iter().map(|v| v + shift).collect() }
}

/// Applies Shadows and Highlights (−100 … 100) to straight RGBA pixels encoded with `enc`.
pub(crate) fn apply(px: &mut [[f32; 4]], w: usize, h: usize, shadows: f32, highlights: f32, enc: Encoding) {
    if (shadows == 0.0 && highlights == 0.0) || w < 2 || h < 2 || px.len() < w * h {
        return;
    }
    const FLOOR: f32 = 1.0 / 65536.0;
    // Tabulated transfer curves: powf per channel dominated the passes over full-size pixels.
    let lut = super::transfer::Lut::of(enc);
    let decode = |i: usize| [0, 1, 2].map(|c| lut.decode(px[i][c]));
    let linear = (w * h <= KEEP_LINEAR).then(|| par_map(w * h, decode));
    let u = Plane {
        w,
        h,
        v: par_map(w * h, |i| (luma(linear.as_ref().and_then(|l| l.get(i).copied()).unwrap_or_else(|| decode(i))).max(0.0) + FLOOR).log2()),
    };
    // The filter on a copy at most WORK_SIDE across (box-averaged in log).
    let f = w.max(h).div_ceil(WORK_SIDE).max(1);
    let small = if f == 1 { Plane { w, h, v: u.v.clone() } } else { shrink(&u, f) };
    let mut o = Plane { w: small.w, h: small.h, v: small.v.clone() };
    if highlights != 0.0 {
        o = highlights_pass(&o, highlights.clamp(-100.0, 100.0) / 100.0, &HIGHLIGHTS);
    }
    if shadows != 0.0 {
        o = shadows_pass(&o, shadows.clamp(-100.0, 100.0) / 100.0);
    }
    // The change in log luminance, at full size.
    let change: Vec<f32> = if f == 1 {
        o.v.iter().zip(&u.v).map(|(a, b)| a - b).collect()
    } else {
        let d: Vec<f32> = o.v.iter().zip(&small.v).map(|(a, b)| a - b).collect();
        guided_upsample(&small, &d, &u)
    };
    par_rows(&mut px[..w * h], w, 1, |y, row| {
        for (x, q) in row.iter_mut().enumerate() {
            let i = y * w + x;
            let gain = change[i].exp2();
            let lin = linear.as_ref().and_then(|l| l.get(i).copied()).unwrap_or_else(|| [0, 1, 2].map(|c| lut.decode(q[c])));
            for c in 0..3 {
                q[c] = lut.encode(lin[c] * gain);
            }
        }
    });
}

/// `u` averaged over `f × f` blocks.
fn shrink(u: &Plane, f: usize) -> Plane {
    let (sw, sh) = (u.w.div_ceil(f), u.h.div_ceil(f));
    let mut v = vec![0.0f32; sw * sh];
    par_rows(&mut v, sw, 1, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let (x0, y0) = (x * f, y * f);
            let (x1, y1) = ((x0 + f).min(u.w), (y0 + f).min(u.h));
            let mut s = 0.0f64;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    s += f64::from(u.v[yy * u.w + xx]);
                }
            }
            *o = (s / ((x1 - x0) * (y1 - y0)) as f64) as f32;
        }
    });
    Plane { w: sw, h: sh, v }
}

/// The change `d` computed on `small` brought to the size of `full`, following its edges: a local
/// linear model of the change against the log luminance (the guided filter of K. He, J. Sun,
/// *Fast Guided Filter*, 2015), fitted on the small copy and evaluated on the full one.
fn guided_upsample(small: &Plane, d: &[f32], full: &Plane) -> Vec<f32> {
    let (sw, sh) = (small.w, small.h);
    let r = 2;
    let eps = 0.01;
    let mean = |v: &[f32]| super::box_mean(v, sw, sh, r);
    let ud: Vec<f32> = small.v.iter().zip(d).map(|(a, b)| a * b).collect();
    let uu: Vec<f32> = small.v.iter().map(|a| a * a).collect();
    let (mu, md, mud, muu) = (mean(&small.v), mean(d), mean(&ud), mean(&uu));
    let a: Vec<f32> = (0..sw * sh).map(|i| (mud[i] - mu[i] * md[i]) / (muu[i] - mu[i] * mu[i] + eps)).collect();
    let b: Vec<f32> = (0..sw * sh).map(|i| md[i] - a[i] * mu[i]).collect();
    let (ma, mb) = (mean(&a), mean(&b));
    let (fx, fy) = (sw as f32 / full.w as f32, sh as f32 / full.h as f32);
    let mut out = vec![0.0f32; full.w * full.h];
    par_rows(&mut out, full.w, 1, |y, row| {
        let sy = (y as f32 + 0.5) * fy - 0.5;
        for (x, o) in row.iter_mut().enumerate() {
            let sx = (x as f32 + 0.5) * fx - 0.5;
            let av = crate::photo_util::bilinear(&ma, sw, sh, 1, 0, sx, sy);
            let bv = crate::photo_util::bilinear(&mb, sw, sh, 1, 0, sx, sy);
            *o = av * full.v[y * full.w + x] + bv;
        }
    });
    out
}

/// The implementation before the grouped evaluation, kept verbatim as the tests' reference.
#[cfg(test)]
mod reference {
    use super::*;

    /// Mirrors `i` into `0..n` (the edge sample is not repeated).
    fn mirror(i: isize, n: usize) -> usize {
        let n = n as isize;
        if n == 1 {
            return 0;
        }
        let p = 2 * (n - 1);
        let m = i.rem_euclid(p);
        (if m >= n { p - m } else { m }) as usize
    }

    const TAPS: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

    /// The binomial 5-tap blur, sampled every `step` pixels from 0 (1 = full resolution).
    fn blur_step(p: &Plane, step: usize) -> Plane {
        let (w2, h2) = (p.w.div_ceil(step), p.h.div_ceil(step));
        // Columns first at the kept rows, then rows at the kept columns.
        let mut tmp = vec![0.0f32; p.w * h2];
        par_rows(&mut tmp, p.w, 1, |y2, row| {
            let y = (y2 * step) as isize;
            for (x, o) in row.iter_mut().enumerate() {
                *o = (0..5).map(|k| TAPS[k] * p.v[mirror(y + k as isize - 2, p.h) * p.w + x]).sum();
            }
        });
        let mut out = vec![0.0f32; w2 * h2];
        par_rows(&mut out, w2, 1, |y2, row| {
            let src = &tmp[y2 * p.w..(y2 + 1) * p.w];
            for (x2, o) in row.iter_mut().enumerate() {
                let x = (x2 * step) as isize;
                *o = (0..5).map(|k| TAPS[k] * src[mirror(x + k as isize - 2, p.w)]).sum();
            }
        });
        Plane { w: w2, h: h2, v: out }
    }

    /// The next coarser level.
    fn down(p: &Plane) -> Plane {
        blur_step(p, 2)
    }

    /// `p` brought up to `w × h`: zeros between samples, then the blur times four.
    fn up(p: &Plane, w: usize, h: usize) -> Plane {
        let mut z = Plane { w, h, v: vec![0.0; w * h] };
        for y in 0..p.h.min(h.div_ceil(2)) {
            for x in 0..p.w.min(w.div_ceil(2)) {
                z.v[2 * y * w + 2 * x] = p.v[y * p.w + x] * 4.0;
            }
        }
        blur_step(&z, 1)
    }

    /// Levels down to about 2 pixels on the short side.
    fn depth(w: usize, h: usize) -> usize {
        let mut s = w.min(h);
        let mut n = 1;
        while s > 2 {
            s = s.div_ceil(2);
            n += 1;
        }
        n
    }

    fn gaussian(p: Plane, n: usize) -> Vec<Plane> {
        let mut g = vec![p];
        for _ in 1..n {
            let next = down(&g[g.len() - 1]);
            g.push(next);
        }
        g
    }

    /// The local Laplacian filter of `u` with `remap(value, reference)`; the coarsest level is kept.
    fn llf(u: &Plane, remap: impl Fn(f32, f32) -> f32 + Sync) -> Plane {
        let n = depth(u.w, u.h);
        let g = gaussian(Plane { w: u.w, h: u.h, v: u.v.clone() }, n);
        // References span the image's tones; the deepest 0.1 % (noise down to the floor) share the
        // lowest reference instead of stretching the spacing.
        let hi = u.v.iter().fold(f32::NEG_INFINITY, |a, &v| a.max(v));
        let lo = quantile(&u.v, 0.001).min(hi);
        if hi - lo <= 1e-6 || (hi - lo).is_nan() {
            return Plane { w: u.w, h: u.h, v: u.v.clone() };
        }
        let refs = (((hi - lo) / REF_STEP).ceil() as usize + 1).clamp(2, 96);
        let step = (hi - lo) / (refs - 1) as f32;
        let mut lap: Vec<Vec<f32>> = g[..n - 1].iter().map(|p| vec![0.0; p.v.len()]).collect();
        for k in 0..refs {
            let r = lo + step * k as f32;
            let mut m = Plane { w: u.w, h: u.h, v: u.v.clone() };
            par_rows(&mut m.v, u.w, 1, |_, row| {
                for v in row.iter_mut() {
                    *v = remap(*v, r);
                }
            });
            let gr = gaussian(m, n);
            for l in 0..n - 1 {
                let coarse = up(&gr[l + 1], gr[l].w, gr[l].h);
                let (gl, fine, lw) = (&g[l].v, &gr[l].v, g[l].w);
                par_rows(&mut lap[l], lw, 1, |y, row| {
                    for (x, o) in row.iter_mut().enumerate() {
                        let i = y * lw + x;
                        let wgt = (1.0 - (gl[i].clamp(lo, hi) - r).abs() / step).max(0.0);
                        if wgt > 0.0 {
                            *o += wgt * (fine[i] - coarse.v[i]);
                        }
                    }
                });
            }
        }
        let mut o = Plane { w: g[n - 1].w, h: g[n - 1].h, v: g[n - 1].v.clone() };
        for l in (0..n - 1).rev() {
            let mut next = up(&o, g[l].w, g[l].h);
            for (a, b) in next.v.iter_mut().zip(&lap[l]) {
                *a += b;
            }
            o = next;
        }
        o
    }

    /// The `q` quantile (0 … 1) of `v`.
    fn quantile(v: &[f32], q: f32) -> f32 {
        let mut s = v.to_vec();
        s.sort_by(f32::total_cmp);
        s[((s.len() - 1) as f32 * q).round() as usize]
    }

    /// 1st and 99th percentiles of the log luminance on a copy at most 256 pixels across.
    fn range(u: &Plane) -> (f32, f32) {
        let f = u.w.max(u.h).div_ceil(256).max(1);
        let (sw, sh) = (u.w.div_ceil(f), u.h.div_ceil(f));
        let mut sum = vec![(0.0f64, 0u32); sw * sh];
        for y in 0..u.h {
            for x in 0..u.w {
                let c = &mut sum[(y / f) * sw + x / f];
                c.0 += f64::from(u.v[y * u.w + x]);
                c.1 += 1;
            }
        }
        let cells: Vec<f32> = sum.iter().filter(|c| c.1 > 0).map(|c| (c.0 / f64::from(c.1)) as f32).collect();
        (quantile(&cells, 0.01), quantile(&cells, 0.99))
    }

    /// `d` past the detail threshold: about 0 below `sigma`, `d − sigma` well above it.
    fn excess(d: f32, sigma: f32) -> f32 {
        d - sigma * (d / sigma).tanh()
    }

    fn knot(t: f32, k: &[f32; KNOTS]) -> f32 {
        let x = t.clamp(0.0, 1.0) * (KNOTS - 1) as f32;
        let i = (x as usize).min(KNOTS - 2);
        k[i] + (k[i + 1] - k[i]) * (x - i as f32)
    }

    /// The Highlights pass on the log luminance `u` (`amount` −1 … 1): pixels brighter than their
    /// surroundings move toward them (or away, for positive values) by the part of the difference
    /// past `sigma`, then the darkest content is put back where it was.
    fn highlights_pass(u: &Plane, amount: f32, side: &Side) -> Plane {
        let (lo, hi) = range(u);
        let span = (hi - lo).max(1.0);
        let strengths = if amount < 0.0 { &side.pull } else { &side.push };
        let o = llf(u, |x, g| x + knot((g - lo) / span, strengths) * amount * excess((x - g).max(0.0), side.sigma));
        let shift = quantile(&u.v, 0.01) - quantile(&o.v, 0.01);
        Plane { w: o.w, h: o.h, v: o.v.iter().map(|v| v + shift).collect() }
    }

    /// The Shadows pass on the log luminance `u` (`amount` −1 … 1).
    fn shadows_pass(u: &Plane, amount: f32) -> Plane {
        let (lo, hi) = range(u);
        let span = (hi - lo).max(1.0);
        let k = if amount > 0.0 { amount } else { amount * SHADOWS.down };
        let o = llf(u, |x, g| {
            let below = ((g - x) / SHADOWS.sigma).clamp(0.0, 1.0);
            if below == 0.0 {
                return x;
            }
            let w = below * below * (3.0 - 2.0 * below);
            x + k * knot((x - lo) / span, &SHADOWS.own) * knot((g - lo) / span, &SHADOWS.around) * w
        });
        // Keep the brightest content.
        let shift = quantile(&u.v, 0.99) - quantile(&o.v, 0.99);
        Plane { w: o.w, h: o.h, v: o.v.iter().map(|v| v + shift).collect() }
    }

    /// Applies Shadows and Highlights (−100 … 100) to straight RGBA pixels encoded with `enc`.
    pub(crate) fn apply(px: &mut [[f32; 4]], w: usize, h: usize, shadows: f32, highlights: f32, enc: Encoding) {
        if (shadows == 0.0 && highlights == 0.0) || w < 2 || h < 2 {
            return;
        }
        const FLOOR: f32 = 1.0 / 65536.0;
        let u = Plane { w, h, v: px.iter().map(|q| (luma([0, 1, 2].map(|c| enc.decode(q[c]))).max(0.0) + FLOOR).log2()).collect() };
        // The filter on a copy at most WORK_SIDE across (box-averaged in log).
        let f = w.max(h).div_ceil(WORK_SIDE).max(1);
        let small = if f == 1 { Plane { w, h, v: u.v.clone() } } else { shrink(&u, f) };
        let mut o = Plane { w: small.w, h: small.h, v: small.v.clone() };
        if highlights != 0.0 {
            o = highlights_pass(&o, highlights.clamp(-100.0, 100.0) / 100.0, &HIGHLIGHTS);
        }
        if shadows != 0.0 {
            o = shadows_pass(&o, shadows.clamp(-100.0, 100.0) / 100.0);
        }
        // The change in log luminance, at full size.
        let change: Vec<f32> = if f == 1 {
            o.v.iter().zip(&u.v).map(|(a, b)| a - b).collect()
        } else {
            let d: Vec<f32> = o.v.iter().zip(&small.v).map(|(a, b)| a - b).collect();
            guided_upsample(&small, &d, &u)
        };
        par_rows(px, w, 1, |y, row| {
            for (x, q) in row.iter_mut().enumerate() {
                let gain = change[y * w + x].exp2();
                for c in 0..3 {
                    q[c] = enc.encode(enc.decode(q[c]) * gain);
                }
            }
        });
    }

    /// `u` averaged over `f × f` blocks.
    fn shrink(u: &Plane, f: usize) -> Plane {
        let (sw, sh) = (u.w.div_ceil(f), u.h.div_ceil(f));
        let mut v = vec![0.0f32; sw * sh];
        par_rows(&mut v, sw, 1, |y, row| {
            for (x, o) in row.iter_mut().enumerate() {
                let (x0, y0) = (x * f, y * f);
                let (x1, y1) = ((x0 + f).min(u.w), (y0 + f).min(u.h));
                let mut s = 0.0f64;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        s += f64::from(u.v[yy * u.w + xx]);
                    }
                }
                *o = (s / ((x1 - x0) * (y1 - y0)) as f64) as f32;
            }
        });
        Plane { w: sw, h: sh, v }
    }

    /// The change `d` computed on `small` brought to the size of `full`, following its edges: a local
    /// linear model of the change against the log luminance (the guided filter of K. He, J. Sun,
    /// *Fast Guided Filter*, 2015), fitted on the small copy and evaluated on the full one.
    fn guided_upsample(small: &Plane, d: &[f32], full: &Plane) -> Vec<f32> {
        let (sw, sh) = (small.w, small.h);
        let r = 2;
        let eps = 0.01;
        let mean = |v: &[f32]| super::super::box_mean(v, sw, sh, r);
        let ud: Vec<f32> = small.v.iter().zip(d).map(|(a, b)| a * b).collect();
        let uu: Vec<f32> = small.v.iter().map(|a| a * a).collect();
        let (mu, md, mud, muu) = (mean(&small.v), mean(d), mean(&ud), mean(&uu));
        let a: Vec<f32> = (0..sw * sh).map(|i| (mud[i] - mu[i] * md[i]) / (muu[i] - mu[i] * mu[i] + eps)).collect();
        let b: Vec<f32> = (0..sw * sh).map(|i| md[i] - a[i] * mu[i]).collect();
        let (ma, mb) = (mean(&a), mean(&b));
        let (fx, fy) = (sw as f32 / full.w as f32, sh as f32 / full.h as f32);
        let mut out = vec![0.0f32; full.w * full.h];
        par_rows(&mut out, full.w, 1, |y, row| {
            let sy = (y as f32 + 0.5) * fy - 0.5;
            for (x, o) in row.iter_mut().enumerate() {
                let sx = (x as f32 + 0.5) * fx - 0.5;
                let av = crate::photo_util::bilinear(&ma, sw, sh, 1, 0, sx, sy);
                let bv = crate::photo_util::bilinear(&mb, sw, sh, 1, 0, sx, sy);
                *o = av * full.v[y * full.w + x] + bv;
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: Encoding = Encoding(0.0);

    fn grey(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Vec<[f32; 4]> {
        (0..w * h).map(|i| f(i % w, i / w)).map(|v| [v, v, v, 1.0]).collect()
    }

    #[test]
    fn a_flat_image_is_unchanged() {
        let mut px = grey(64, 48, |_, _| 0.4);
        apply(&mut px, 64, 48, 100.0, -100.0, SRGB);
        assert!(px.iter().all(|q| (q[0] - 0.4).abs() < 1e-5));
    }

    #[test]
    fn shadows_raise_a_dark_area_without_a_halo() {
        // A 0.1 | 0.6 step (the dark half is the image's black point, which Photoshop barely
        // moves either): the bright half stays, and no halo forms on either side of the edge.
        let (w, h) = (256, 128);
        let mut px = grey(w, h, |x, _| if x < w / 2 { 0.1 } else { 0.6 });
        apply(&mut px, w, h, 100.0, 0.0, SRGB);
        let row = &px[64 * w..65 * w];
        assert!((row[w / 2 + 1][0] - 0.6).abs() < 0.002 && (row[w - 5][0] - 0.6).abs() < 0.002);
        let (far, near) = (row[10][0], row[w / 2 - 1][0]);
        assert!((far - 0.1).abs() < 0.004 && (far - near).abs() < 0.004, "{far} {near}");
    }

    #[test]
    fn grey_moves_as_in_photoshop() {
        // Mid-grey with 1 % black (0.015) and white (0.97) corners, Photoshop 25.4: Shadows +100
        // takes the grey from 127.5 to 165.3, Highlights −100 to 89.6 (of 255).
        let n = 256;
        let image = |c: f32| {
            grey(n, n, |x, y| {
                if x >= n - 26 && y >= n - 26 {
                    0.97
                } else if x >= n - 26 && y < 26 {
                    0.015
                } else {
                    c
                }
            })
        };
        for (s, hl, want) in [(100.0, 0.0, 165.3), (0.0, -100.0, 89.6)] {
            let mut px = image(0.5);
            apply(&mut px, n, n, s, hl, SRGB);
            let got = px[175 * n + 100][0] * 255.0;
            assert!((got - want).abs() < 5.0, "{s} {hl}: {got} vs {want}");
        }
    }

    #[test]
    fn highlights_lower_a_bright_area_and_keep_the_darkest() {
        let (w, h) = (192, 192);
        let mut px = grey(w, h, |x, y| if (64..128).contains(&x) && (64..128).contains(&y) { 0.8 } else { 0.2 });
        apply(&mut px, w, h, 0.0, -100.0, SRGB);
        assert!(px[96 * w + 96][0] < 0.75, "{}", px[96 * w + 96][0]);
        assert!((px[5 * w + 5][0] - 0.2).abs() < 0.01, "{}", px[5 * w + 5][0]);
    }

    #[test]
    fn smooth_gradients_stay_smooth() {
        // A soft ramp over eight stops, as in an out-of-focus background. Coefficients interpolated
        // between references too far apart drew contour lines across such ramps (Shadows +100 on
        // a D4 raw): the output must rise steadily, without ripples.
        let (w, h) = (640, 48);
        let mut px = grey(w, h, |x, _| crate::photo_util::linear_to_srgb((x as f32 / (w - 1) as f32 * 8.0 - 9.0).exp2()));
        for (s, hl) in [(100.0, 0.0), (0.0, -100.0), (-100.0, 100.0)] {
            let mut p = px.clone();
            apply(&mut p, w, h, s, hl, SRGB);
            let row: Vec<f32> = (0..w).map(|x| crate::photo_util::srgb_to_linear(p[24 * w + x][0]).max(1e-6).log2()).collect();
            let steps: Vec<f32> = row.windows(2).map(|d| d[1] - d[0]).collect();
            assert!(steps.iter().all(|d| *d > 0.0), "{s} {hl}: the ramp reverses");
            let jumps: Vec<f32> = steps.windows(2).map(|d| (d[1] - d[0]).abs()).collect();
            let ripple = jumps[16..jumps.len() - 16].iter().fold(0.0f32, |a, b| a.max(*b));
            // The ramp rises 0.0125 stops per pixel; its slope may change by a third at most.
            assert!(ripple < 0.004, "{s} {hl}: slope jumps by {ripple} stops per pixel");
        }
        px.clear();
    }

    #[test]
    fn the_result_does_not_depend_on_the_image_size() {
        let at = |n: usize| {
            let mut px = grey(n, n, |x, y| if x < n / 3 || y < n / 4 { 0.08 } else { 0.7 });
            apply(&mut px, n, n, 70.0, -60.0, SRGB);
            [px[(n / 8) * n + n / 8][0], px[(3 * n / 4) * n + 3 * n / 4][0]]
        };
        let (a, b) = (at(96), at(384));
        assert!((a[0] - b[0]).abs() < 0.02 && (a[1] - b[1]).abs() < 0.02, "{a:?} {b:?}");
    }

    /// Test images: a step edge, a ramp over ten stops, a photo-like mix and coloured noise.
    fn images() -> Vec<(&'static str, usize, usize, Vec<[f32; 4]>)> {
        let photo = |w: usize, h: usize| -> Vec<[f32; 4]> {
            (0..w * h)
                .map(|i| {
                    let (x, y) = (i % w, i / w);
                    let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                    let tex = (((x * 7 + y * 13) % 17) as f32 / 17.0 - 0.5) * 0.08;
                    let base = if fy < 0.3 { 0.75 + 0.2 * fx } else { 0.05 + 0.6 * fx * (1.0 - fy) };
                    let v = (base + tex).clamp(0.0, 1.0);
                    [v, (v * 0.9 + 0.05 * fy).min(1.0), (v * 0.7 + 0.2 * (1.0 - fy)).min(1.0), 1.0]
                })
                .collect()
        };
        let noise = |w: usize, h: usize| -> Vec<[f32; 4]> {
            (0..w * h).map(|i| [0, 1, 2].map(|c| crate::photo_util::hash01((i % w) as i64, (i / w) as i64, c)).map(|v| v * v)).map(|c| [c[0], c[1], c[2], 1.0]).collect()
        };
        vec![
            ("step", 161, 97, grey(161, 97, |x, _| if x < 80 { 0.08 } else { 0.7 })),
            ("ramp", 200, 40, grey(200, 40, |x, _| crate::photo_util::linear_to_srgb((x as f32 / 199.0 * 10.0 - 10.5).exp2()))),
            ("photo", 180, 120, photo(180, 120)),
            ("noise", 97, 64, noise(97, 64)),
            // Wider than WORK_SIDE: the filter runs on a shrunk copy and is upsampled.
            ("wide photo", 1700, 24, photo(1700, 24)),
        ]
    }

    #[test]
    fn grouped_evaluation_matches_the_reference_implementation() {
        let settings = [
            (100.0, 0.0),
            (50.0, 0.0),
            (-50.0, 0.0),
            (-100.0, 0.0),
            (0.0, 100.0),
            (0.0, 50.0),
            (0.0, -50.0),
            (0.0, -100.0),
            (60.0, -70.0),
            (-40.0, 80.0),
            (100.0, -100.0),
        ];
        let (mut worst_max, mut worst_mean) = (0.0f32, 0.0f32);
        for (name, w, h, px) in images() {
            for (s, hl) in settings {
                let (mut a, mut b) = (px.clone(), px.clone());
                apply(&mut a, w, h, s, hl, SRGB);
                reference::apply(&mut b, w, h, s, hl, SRGB);
                let d: Vec<f32> = a.iter().zip(&b).flat_map(|(p, q)| (0..3).map(move |c| (p[c] - q[c]).abs())).collect();
                let max = d.iter().fold(0.0f32, |m, v| m.max(*v));
                let mean = d.iter().sum::<f32>() / d.len() as f32;
                assert!(max <= 1e-3 && mean <= 1e-4, "{name} {s} {hl}: max {max} mean {mean}");
                (worst_max, worst_mean) = (worst_max.max(max), worst_mean.max(mean));
            }
        }
        eprintln!("local tone vs reference: max {worst_max:e}, mean {worst_mean:e}");
    }

    #[test]
    fn infinite_input_does_not_panic() {
        let mut px = grey(32, 32, |x, _| if x == 3 { f32::INFINITY } else { 0.4 });
        apply(&mut px, 32, 32, 50.0, -50.0, SRGB);
        let mut short = grey(4, 4, |_, _| 0.5);
        apply(&mut short, 8, 8, 50.0, 0.0, SRGB);
    }
}
