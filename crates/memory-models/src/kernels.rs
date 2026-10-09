//! Small row kernels used by the fused ops. Each public function dispatches at runtime to a copy
//! compiled with AVX-512 or AVX2+FMA enabled (the bodies are plain loops written so that LLVM can
//! vectorise them: reductions use 16 independent accumulators, `exp`/`erf` are branch-free
//! polynomial/rational approximations). The reduction order is fixed, so every code path
//! produces bit-identical results.

#[inline(always)]
fn sum_impl(x: &[f32]) -> f32 {
    let mut acc = [0f32; 16];
    let chunks = x.chunks_exact(16);
    let rem = chunks.remainder();
    for c in chunks {
        for i in 0..16 {
            acc[i] += c[i];
        }
    }
    let mut s = 0f32;
    for a in acc {
        s += a;
    }
    for r in rem {
        s += r;
    }
    s
}

#[inline(always)]
fn sq_dev_sum_impl(x: &[f32], mean: f32) -> f32 {
    let mut acc = [0f32; 16];
    let chunks = x.chunks_exact(16);
    let rem = chunks.remainder();
    for c in chunks {
        for i in 0..16 {
            let d = c[i] - mean;
            acc[i] += d * d;
        }
    }
    let mut s = 0f32;
    for a in acc {
        s += a;
    }
    for r in rem {
        let d = r - mean;
        s += d * d;
    }
    s
}

#[inline(always)]
fn max_impl(x: &[f32]) -> f32 {
    let mut acc = [f32::NEG_INFINITY; 16];
    let chunks = x.chunks_exact(16);
    let rem = chunks.remainder();
    for c in chunks {
        for i in 0..16 {
            acc[i] = if c[i] > acc[i] { c[i] } else { acc[i] };
        }
    }
    let mut m = f32::NEG_INFINITY;
    for a in acc {
        m = if a > m { a } else { m };
    }
    for &r in rem {
        m = if r > m { r } else { m };
    }
    m
}

/// exp(x) for x <= ~88; Cephes-style range reduction + degree-7 polynomial (rel. err ~1e-7).
/// Inputs below -87.3 return ~1e-38 instead of 0 (irrelevant for softmax).
#[inline(always)]
pub fn exp_fast(x: f32) -> f32 {
    let x = if x < -87.3 { -87.3 } else { x };
    let x = if x > 88.3 { 88.3 } else { x };
    let fx = x * std::f32::consts::LOG2_E;
    // Round to nearest integer (valid for |fx| < 2^22).
    let fx = (fx + 12582912.0) - 12582912.0;
    let r = x - fx * 0.693_359_4 + fx * 2.121_944_4e-4;
    let r2 = r * r;
    let mut y = 1.987_569_1e-4f32;
    y = y * r + 1.398_199_9e-3;
    y = y * r + 8.333_452e-3;
    y = y * r + 4.166_579_6e-2;
    y = y * r + 1.666_666_5e-1;
    y = y * r + 5.000_000_1e-1;
    y = y * r2 + r + 1.0;
    let n = fx as i32;
    let pow2n = f32::from_bits(((n + 127) as u32) << 23);
    y * pow2n
}

/// erf(x), rational approximation (as used by Eigen/XLA for f32); |err| < ~2e-7.
#[inline(always)]
pub fn erf_fast(x: f32) -> f32 {
    let x = if x > 4.0 { 4.0 } else { x };
    let x = if x < -4.0 { -4.0 } else { x };
    let x2 = x * x;
    let mut p = x2 * -2.726_142_3e-10 + 2.770_681_4e-8;
    p = x2 * p + -2.101_024e-6;
    p = x2 * p + -5.692_506_4e-5;
    p = x2 * p + -7.349_906_3e-4;
    p = x2 * p + -2.954_6e-3;
    p = x2 * p + -1.609_603_3e-2;
    p *= x;
    let mut q = x2 * -1.456_607_2e-5 + -2.133_740_6e-4;
    q = x2 * q + -1.682_827e-3;
    q = x2 * q + -7.373_329e-3;
    q = x2 * q + -1.426_474e-2;
    p / q
}

#[inline(always)]
pub fn gelu_erf(x: f32) -> f32 {
    0.5 * x * (1.0 + erf_fast(x * std::f32::consts::FRAC_1_SQRT_2))
}

#[inline(always)]
fn layer_norm_row_impl(x: &mut [f32], g: &[f32], b: &[f32], eps: f32) {
    let n = x.len() as f32;
    let mean = sum_impl(x) / n;
    let var = sq_dev_sum_impl(x, mean) / n;
    let inv = 1.0 / (var + eps).sqrt();
    for ((v, &gi), &bi) in x.iter_mut().zip(g).zip(b) {
        *v = (*v - mean) * inv * gi + bi;
    }
}

#[inline(always)]
fn bias_row_impl(x: &mut [f32], bias: &[f32]) {
    for (v, &bi) in x.iter_mut().zip(bias) {
        *v += bi;
    }
}

#[inline(always)]
fn bias_gelu_row_impl(x: &mut [f32], bias: &[f32]) {
    for (v, &bi) in x.iter_mut().zip(bias) {
        *v = gelu_erf(*v + bi);
    }
}

#[inline(always)]
fn add2_row_impl(x: &mut [f32], a: &[f32], b: &[f32]) {
    for ((v, &ai), &bi) in x.iter_mut().zip(a).zip(b) {
        *v = *v + ai + bi;
    }
}

#[inline(always)]
fn softmax_row_impl(x: &mut [f32]) {
    if x.is_empty() {
        return;
    }
    let m = max_impl(x);
    for v in x.iter_mut() {
        *v = exp_fast(*v - m);
    }
    let inv = 1.0 / sum_impl(x);
    for v in x.iter_mut() {
        *v *= inv;
    }
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Avx512,
    Avx2,
    Generic,
}

#[cfg(target_arch = "x86_64")]
fn level() -> Level {
    use std::sync::OnceLock;
    static L: OnceLock<Level> = OnceLock::new();
    *L.get_or_init(|| {
        if std::env::var_os("MEMORY_NO_SIMD").is_some() {
            Level::Generic
        } else if is_x86_feature_detected!("avx512f")
            && is_x86_feature_detected!("avx512vl")
            && is_x86_feature_detected!("avx512dq")
            && is_x86_feature_detected!("fma")
        {
            Level::Avx512
        } else if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            Level::Avx2
        } else {
            Level::Generic
        }
    })
}

macro_rules! dispatch {
    ($(#[$m:meta])* pub fn $name:ident($($arg:ident : $ty:ty),*) => $impl:ident;) => {
        $(#[$m])*
        pub fn $name($($arg: $ty),*) {
            #[cfg(target_arch = "x86_64")]
            {
                #[target_feature(enable = "avx512f,avx512vl,avx512dq,avx2,fma")]
                unsafe fn avx512($($arg: $ty),*) { $impl($($arg),*) }
                #[target_feature(enable = "avx2,fma")]
                unsafe fn avx2($($arg: $ty),*) { $impl($($arg),*) }
                match level() {
                    // SAFETY: the CPU features were detected at runtime.
                    Level::Avx512 => return unsafe { avx512($($arg),*) },
                    Level::Avx2 => return unsafe { avx2($($arg),*) },
                    Level::Generic => {}
                }
            }
            $impl($($arg),*)
        }
    };
}

dispatch! {
    /// In-place LayerNorm of one row.
    pub fn layer_norm_row(x: &mut [f32], g: &[f32], b: &[f32], eps: f32) => layer_norm_row_impl;
}
dispatch! {
    /// x += bias
    pub fn bias_row(x: &mut [f32], bias: &[f32]) => bias_row_impl;
}
dispatch! {
    /// x = gelu(x + bias)
    pub fn bias_gelu_row(x: &mut [f32], bias: &[f32]) => bias_gelu_row_impl;
}
dispatch! {
    /// x = x + a + b
    pub fn add2_row(x: &mut [f32], a: &[f32], b: &[f32]) => add2_row_impl;
}
dispatch! {
    /// In-place softmax of one row.
    pub fn softmax_row(x: &mut [f32]) => softmax_row_impl;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erf_matches_libm() {
        let mut max_err = 0f32;
        let mut x = -6.0f32;
        while x <= 6.0 {
            let e = (erf_fast(x) - candle_core::cpu::erf::erf_f32(x)).abs();
            max_err = max_err.max(e);
            x += 1e-3;
        }
        assert!(max_err < 1e-6, "erf max err {max_err}");
    }

    #[test]
    fn exp_matches_std() {
        let mut max_rel = 0f32;
        let mut x = -80.0f32;
        while x <= 80.0 {
            let r = ((exp_fast(x) - x.exp()) / x.exp()).abs();
            max_rel = max_rel.max(r);
            x += 1e-3;
        }
        assert!(max_rel < 5e-7, "exp max rel err {max_rel}");
    }

    #[test]
    fn simd_paths_match_generic() {
        let x: Vec<f32> = (0..1000)
            .map(|i| ((i * 7919 % 1000) as f32 / 100.0) - 5.0)
            .collect();
        let g: Vec<f32> = (0..1000).map(|i| 1.0 + (i % 3) as f32 * 0.1).collect();
        let b: Vec<f32> = (0..1000).map(|i| (i % 5) as f32 * 0.01).collect();
        let mut a1 = x.clone();
        layer_norm_row(&mut a1, &g, &b, 1e-5);
        let mut a2 = x.clone();
        layer_norm_row_impl(&mut a2, &g, &b, 1e-5);
        assert_eq!(a1, a2);
        let mut s1 = x.clone();
        softmax_row(&mut s1);
        let mut s2 = x.clone();
        softmax_row_impl(&mut s2);
        let max_diff = s1
            .iter()
            .zip(&s2)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(max_diff < 1e-7);
        let tot: f32 = s1.iter().sum();
        assert!((tot - 1.0).abs() < 1e-4);
        let mut g1 = x.clone();
        bias_gelu_row(&mut g1, &b);
        for (i, v) in g1.iter().enumerate() {
            let z = x[i] + b[i];
            let r = 0.5 * z * (1.0 + candle_core::cpu::erf::erf_f32(z / 2f32.sqrt()));
            assert!((v - r).abs() < 1e-5, "{v} vs {r}");
        }
    }
}
