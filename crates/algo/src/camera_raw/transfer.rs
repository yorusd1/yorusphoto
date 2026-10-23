//! Tabulated transfer curves for the develop pipeline's per-pixel stages.
//!
//! Several stages decode the document's values to light and encode them again, pixel by pixel,
//! and `powf` dominated their cost (it is several times slower again with the windows-gnu
//! runtime). [`Lut`] tabulates both directions of an [`Encoding`] at 4096 intervals with linear
//! interpolation. Each table is indexed by a root of its argument (square or fourth) chosen so
//! the tabulated function has no steep start. Within 0 … 1 a lookup stays within 2 × 10⁻⁶ of the
//! exact curve (tested), well under one 16-bit level; in the first few intervals and outside
//! 0 … 1 (float documents) the exact functions are used.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use super::Encoding;

const N: usize = 4096;
/// Below this table index (as a fraction) the exact curve is used.
const EXACT_BELOW: f32 = 4.0 / N as f32;
/// Most distinct encodings kept; any further ones are tabulated per use.
const CACHED: usize = 16;

/// A table over `x` in 0 … 1 indexed by `x^(1/root)`.
struct Table {
    root: u8,
    v: Vec<f32>,
}

impl Table {
    fn new(root: u8, f: impl Fn(f32) -> f32) -> Self {
        Table { root, v: (0..=N).map(|i| f((i as f32 / N as f32).powi(i32::from(root)))).collect() }
    }

    /// The index fraction of `x` (0 … 1).
    #[inline]
    fn index(&self, x: f32) -> f32 {
        match self.root {
            1 => x,
            2 => x.sqrt(),
            _ => x.sqrt().sqrt(),
        }
    }

    /// The tabulated value at index fraction `s` (0 … 1).
    #[inline]
    fn at(&self, s: f32) -> f32 {
        let x = s * N as f32;
        let i = (x as usize).min(N - 1);
        let t = x - i as f32;
        match (self.v.get(i), self.v.get(i + 1)) {
            (Some(a), Some(b)) => a + (b - a) * t,
            _ => 0.0,
        }
    }
}

/// Both directions of an [`Encoding`] as lookup tables.
pub(crate) struct Lut {
    enc: Encoding,
    decode: Table,
    encode: Table,
}

impl Lut {
    fn build(enc: Encoding) -> Self {
        // Light is `v^γ`, the encoded value `x^(1/γ)` (sRGB is about γ 2.4 above a linear toe):
        // a root index of order k turns them into powers 2γ, k/γ of at least one.
        let gamma = if enc.0 > 0.0 { enc.0 } else { 2.4 };
        let decode_root = if gamma >= 1.0 { 1 } else { 2 };
        let encode_root = if enc.0 <= 0.0 || gamma <= 2.0 { 2 } else { 4 };
        Lut { enc, decode: Table::new(decode_root, |v| enc.decode(v)), encode: Table::new(encode_root, |x| enc.encode(x)) }
    }

    /// The tables of `enc`, built once per encoding.
    pub(crate) fn of(enc: Encoding) -> Arc<Lut> {
        static CACHE: Mutex<Vec<Arc<Lut>>> = Mutex::new(Vec::new());
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(lut) = cache.iter().find(|l| l.enc == enc) {
            return lut.clone();
        }
        let lut = Arc::new(Lut::build(enc));
        if cache.len() < CACHED {
            cache.push(lut.clone());
        }
        lut
    }

    /// The sRGB curve's tables.
    pub(crate) fn srgb() -> &'static Lut {
        static SRGB: OnceLock<Lut> = OnceLock::new();
        SRGB.get_or_init(|| Lut::build(Encoding::of(0.0)))
    }

    /// Encoded value → light, as [`Encoding::decode`].
    #[inline]
    pub(crate) fn decode(&self, v: f32) -> f32 {
        if !(0.0..=1.0).contains(&v) {
            return self.enc.decode(v);
        }
        let s = self.decode.index(v);
        if s < EXACT_BELOW { self.enc.decode(v) } else { self.decode.at(s) }
    }

    /// Light → encoded value, as [`Encoding::encode`].
    #[inline]
    pub(crate) fn encode(&self, v: f32) -> f32 {
        if !(0.0..=1.0).contains(&v) {
            return self.enc.encode(v);
        }
        let s = self.encode.index(v);
        if s < EXACT_BELOW { self.enc.encode(v) } else { self.encode.at(s) }
    }
}

/// A function of light in 0 … 1 tabulated like the sRGB encoding (square-root index), for
/// curves composed from several transfer functions.
pub(crate) struct LightCurve(Table);

impl LightCurve {
    pub(crate) fn new(f: impl Fn(f32) -> f32) -> Self {
        LightCurve(Table::new(2, f))
    }

    /// The function at `v`, clamped to 0 … 1 (NaN reads as 0).
    #[inline]
    pub(crate) fn at(&self, v: f32) -> f32 {
        let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        self.0.at(self.0.index(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_follow_the_exact_curves() {
        for gamma in [0.0, 1.8, 2.2, 2.4, 1.0, 0.5, 0.6, 3.0, 4.0] {
            let enc = Encoding::of(gamma);
            let lut = Lut::of(enc);
            let (mut de, mut ee) = (0.0f32, 0.0f32);
            for i in 0..=200_000 {
                let v = i as f32 / 200_000.0;
                de = de.max((lut.decode(v) - enc.decode(v)).abs());
                ee = ee.max((lut.encode(v) - enc.encode(v)).abs());
            }
            assert!(de < 2e-6 && ee < 2e-6, "gamma {gamma}: decode {de}, encode {ee}");
        }
    }

    #[test]
    fn out_of_range_values_use_the_exact_curve() {
        let enc = Encoding::of(0.0);
        let lut = Lut::srgb();
        for v in [-0.5, -1e-9, 0.0, 1.5, 8.0, f32::NAN, f32::INFINITY] {
            let (a, b) = (lut.decode(v), enc.decode(v));
            assert!(a == b || (a.is_nan() && b.is_nan()), "{v}");
            let (a, b) = (lut.encode(v), enc.encode(v));
            assert!(a == b || (a.is_nan() && b.is_nan()), "{v}");
        }
        assert!((lut.decode(1.0) - 1.0).abs() < 1e-6 && (lut.encode(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn light_curves_are_clamped_and_close() {
        let c = LightCurve::new(|v| v * 0.5);
        assert_eq!(c.at(-1.0), 0.0);
        assert!((c.at(2.0) - 0.5).abs() < 1e-6);
        assert!((c.at(0.3) - 0.15).abs() < 1e-6);
        assert_eq!(c.at(f32::NAN), 0.0);
    }
}
