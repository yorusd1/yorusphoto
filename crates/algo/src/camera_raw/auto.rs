//! Camera Raw's automatic settings: **Auto** (the Light sliders from the image's histogram),
//! **Auto** white balance, and the White Balance tool's neutral point.
//!
//! Adobe documents what these do, not how: Auto sets Exposure, Contrast, Highlights, Shadows,
//! Whites and Blacks for a balanced tonal range, and the White Balance tool makes the clicked
//! colour neutral. The rules here are our own heuristics on the image statistics; the White
//! Balance solution is exact for this pipeline's [`white_balance_gains`].

use super::transfer::Lut;
use super::{CameraRaw, luma, white_balance_gains};

/// Most pixels the statistics look at (an even grid over the image).
const SAMPLES: usize = 65_536;

/// Temperature and Tint (whole slider units, −100 … 100) that make `rgb` (display-encoded, as
/// White Balance decodes it) neutral; `None` for a black, clipped-to-zero or non-finite colour.
pub fn neutral_white_balance(rgb: [f32; 3]) -> Option<(f32, f32)> {
    let srgb = Lut::srgb();
    neutral_linear(rgb.map(|v| srgb.decode(v)))
}

fn neutral_linear([r, g, b]: [f32; 3]) -> Option<(f32, f32)> {
    if ![r, g, b].iter().all(|v| v.is_finite() && *v > 1e-5) {
        return None;
    }
    // The gains are [1 + 0.35 t, 1 − 0.25 n, 1 − 0.35 t]: red and blue meet at t, then green
    // meets their mean at n (exactly neutral unless a slider limit is reached).
    let t = ((b - r) / (0.35 * (r + b))).clamp(-1.0, 1.0);
    let rb = (r * (1.0 + 0.35 * t) + b * (1.0 - 0.35 * t)) / 2.0;
    let n = ((1.0 - rb / g) / 0.25).clamp(-1.0, 1.0);
    Some(((t * 100.0).round(), (n * 100.0).round()))
}

/// Up to [`SAMPLES`] opaque, finite pixels of `px`.
fn samples(px: &[[f32; 4]]) -> impl Iterator<Item = &[f32; 4]> {
    px.iter().step_by(px.len().div_ceil(SAMPLES).max(1)).filter(|q| q[3] > 0.0 && q.iter().all(|v| v.is_finite()))
}

/// Auto white balance: the mean colour of the unclipped, not-too-dark pixels, near-neutral ones
/// weighted most (a grey world that saturated subjects do not dominate), made neutral.
pub fn auto_white_balance(px: &[[f32; 4]]) -> Option<(f32, f32)> {
    let srgb = Lut::srgb();
    let mut sum = [0.0f64; 3];
    let mut weight = 0.0f64;
    for q in samples(px) {
        if q[..3].iter().any(|v| *v >= 0.98) {
            continue;
        }
        let l = [0, 1, 2].map(|c| srgb.decode(q[c]).max(0.0));
        let (mx, mn) = (l[0].max(l[1]).max(l[2]), l[0].min(l[1]).min(l[2]));
        if luma(l) < 0.003 || mx <= 0.0 {
            continue;
        }
        let w = 1.0 / (1.0 + 8.0 * f64::from((mx - mn) / mx));
        for c in 0..3 {
            sum[c] += f64::from(l[c]) * w;
        }
        weight += w;
    }
    if weight <= 0.0 {
        return None;
    }
    neutral_linear(sum.map(|v| (v / weight) as f32))
}

/// `settings` with Auto's Exposure, Contrast, Highlights, Shadows, Whites and Blacks for `px`
/// (display-encoded, before Camera Raw), measured after the current white balance. The other
/// settings are kept; an image with too few opaque pixels gets neutral Light sliders.
pub fn auto_tone(px: &[[f32; 4]], settings: &CameraRaw) -> CameraRaw {
    let mut out = CameraRaw { exposure: 0.0, contrast: 0.0, highlights: 0.0, shadows: 0.0, whites: 0.0, blacks: 0.0, ..settings.clone() };
    let gains = white_balance_gains(settings.temperature, settings.tint);
    let norm = luma(gains).max(1e-6);
    let srgb = Lut::srgb();
    // Linear luminance and brightest channel of each sample.
    let (mut ys, mut ms): (Vec<f32>, Vec<f32>) = samples(px)
        .map(|q| {
            let l = [0, 1, 2].map(|c| srgb.decode(q[c]).max(0.0) * gains[c] / norm);
            (luma(l), l[0].max(l[1]).max(l[2]))
        })
        .unzip();
    if ys.len() < 16 {
        return out;
    }
    ys.sort_by(f32::total_cmp);
    ms.sort_by(f32::total_cmp);
    let pct = |v: &[f32], q: f32| v.get(((v.len() - 1) as f32 * q).round() as usize).copied().unwrap_or(0.0);
    let n = ys.len() as f32;
    // Exposure brings the median toward middle grey (18 %), three quarters of the way.
    let median = pct(&ys, 0.5).max(1e-4);
    let exposure = ((0.75 * (0.18 / median).log2()).clamp(-3.0, 3.0) * 20.0).round() / 20.0;
    let k = 2f32.powf(exposure);
    let (lo, hi) = (pct(&ys, 0.005) * k, pct(&ms, 0.995) * k);
    // Highlights recovers bright and clipped areas, Shadows opens dark ones, in proportion to
    // how much of the image they cover.
    let bright = ms.iter().filter(|m| **m * k > 0.75).count() as f32 / n;
    let dark = ys.iter().filter(|y| **y * k < 0.02).count() as f32 / n;
    let highlights = -(bright * 250.0 + (hi - 1.0).max(0.0) * 60.0).min(80.0);
    let shadows = (dark * 220.0).min(70.0);
    // Contrast toward a five-stop spread between the 10th and 90th percentiles.
    let spread = (pct(&ys, 0.9).max(1e-4) / pct(&ys, 0.1).max(1e-4)).log2();
    let contrast = ((5.0 - spread) * 6.0).clamp(-25.0, 35.0);
    // Whites opens a dull top end or reins in clipping; Blacks sets a raised black point.
    let whites = if hi < 0.85 {
        ((0.85 - hi) / 0.85 * 70.0).min(50.0)
    } else if hi > 1.0 {
        -((hi - 1.0) * 40.0).min(40.0)
    } else {
        0.0
    };
    let blacks = if lo > 0.004 { -((lo - 0.004) * 1500.0).min(40.0) } else { 0.0 };
    out.exposure = exposure;
    out.contrast = contrast.round();
    out.highlights = highlights.round();
    out.shadows = shadows.round();
    out.whites = whites.round();
    out.blacks = blacks.round();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera_raw::develop;

    fn grey(v: f32, n: usize) -> Vec<[f32; 4]> {
        vec![[v, v, v, 1.0]; n]
    }

    #[test]
    fn the_white_balance_tool_makes_the_clicked_colour_neutral() {
        // Casts within the sliders' reach (±35 % red / blue, ±25 % green).
        for rgb in [[0.6, 0.5, 0.45], [0.42, 0.5, 0.55], [0.5, 0.55, 0.5], [0.53, 0.48, 0.52]] {
            let (temperature, tint) = neutral_white_balance(rgb).unwrap();
            let mut px = vec![[rgb[0], rgb[1], rgb[2], 1.0]];
            develop(&mut px, 1, 1, &CameraRaw { temperature, tint, ..Default::default() }, false);
            let q = px[0];
            assert!((q[0] - q[1]).abs() < 0.01 && (q[1] - q[2]).abs() < 0.01, "{rgb:?} → {q:?} ({temperature}, {tint})");
        }
        assert_eq!(neutral_white_balance([0.5, 0.5, 0.5]), Some((0.0, 0.0)));
        for bad in [[0.0, 0.5, 0.5], [f32::NAN, 0.5, 0.5], [0.5, f32::INFINITY, 0.5], [-1.0, -1.0, -1.0]] {
            assert_eq!(neutral_white_balance(bad), None, "{bad:?}");
        }
        // Beyond the slider range the result is clamped, not wrong in sign.
        let (t, _) = neutral_white_balance([0.02, 0.5, 0.95]).unwrap();
        assert_eq!(t, 100.0);
    }

    #[test]
    fn auto_white_balance_removes_a_colour_cast() {
        let cast: Vec<[f32; 4]> = (0..4096)
            .map(|i| {
                let v = 0.2 + 0.6 * (i % 64) as f32 / 63.0;
                [v * 1.0, v * 0.9, v * 0.7, 1.0]
            })
            .collect();
        let (temperature, tint) = auto_white_balance(&cast).unwrap();
        assert!(temperature < -20.0, "a warm cast is cooled: {temperature}");
        let mut px = cast.clone();
        develop(&mut px, 64, 64, &CameraRaw { temperature, tint, ..Default::default() }, false);
        let mean = |c: usize| px.iter().map(|q| q[c]).sum::<f32>() / px.len() as f32;
        assert!((mean(0) - mean(2)).abs() < 0.03, "{} {}", mean(0), mean(2));
        assert_eq!(auto_white_balance(&[]), None);
        assert_eq!(auto_white_balance(&grey(1.0, 16)), None, "only clipped pixels");
        assert_eq!(auto_white_balance(&[[0.5, 0.5, 0.5, 0.0]; 8]), None, "only transparent pixels");
    }

    #[test]
    fn auto_tone_moves_the_light_sliders_the_right_way() {
        let ramp = |lo: f32, hi: f32| -> Vec<[f32; 4]> {
            (0..4096)
                .map(|i| {
                    let v = lo + (hi - lo) * (i % 256) as f32 / 255.0;
                    [v, v, v, 1.0]
                })
                .collect()
        };
        let dark = auto_tone(&ramp(0.02, 0.35), &CameraRaw::default());
        assert!(dark.exposure > 0.5 && dark.shadows > 0.0, "{dark:?}");
        let bright = auto_tone(&ramp(0.6, 1.0), &CameraRaw::default());
        assert!(bright.exposure < 0.0, "{bright:?}");
        // A well-exposed image with clipped areas keeps its exposure and recovers highlights.
        let clipped: Vec<[f32; 4]> = (0..4096).map(|i| if i % 7 == 0 { [1.0; 4] } else { [0.45, 0.45, 0.45, 1.0] }).collect();
        let clipped = auto_tone(&clipped, &CameraRaw::default());
        assert!(clipped.exposure.abs() < 0.3 && clipped.highlights < -20.0, "{clipped:?}");
        let flat = auto_tone(&ramp(0.4, 0.55), &CameraRaw::default());
        assert!(flat.contrast > 10.0, "{flat:?}");
        // Other settings survive, Light sliders are replaced, and results are whole levels.
        let kept = auto_tone(&ramp(0.02, 0.35), &CameraRaw { vibrance: 30.0, exposure: 4.0, ..Default::default() });
        assert_eq!(kept.vibrance, 30.0);
        assert_eq!(kept.exposure, dark.exposure);
        for v in [kept.contrast, kept.highlights, kept.shadows, kept.whites, kept.blacks] {
            assert_eq!(v, v.round());
        }
        // Too little to measure: neutral Light sliders.
        assert_eq!(auto_tone(&[], &CameraRaw { exposure: 2.0, ..Default::default() }), CameraRaw::default());
        let odd = vec![[f32::NAN, f32::INFINITY, -3.0, 1.0]; 64];
        let r = auto_tone(&odd, &CameraRaw::default());
        assert!([r.exposure, r.contrast, r.highlights, r.shadows, r.whites, r.blacks].iter().all(|v| v.is_finite()));
    }
}
