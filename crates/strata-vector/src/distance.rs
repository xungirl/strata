//! Vector similarity kernels.
//!
//! strata stores L2-normalized embeddings, so **similarity = dot product**
//! (= cosine similarity); higher is closer.
//!
//! Each kernel has a portable scalar version and, on aarch64 (Apple M-series,
//! AWS Graviton), a NEON version using `std::arch` intrinsics. NEON is part
//! of the baseline aarch64 instruction set, so no runtime feature detection
//! is needed. The intrinsics are `unsafe` only because they take raw
//! pointers; every `unsafe` block here reads strictly inside the slices
//! (bounds are established by the loop structure, checked by `assert_eq!` on
//! lengths) and the module's tests compare NEON against scalar results.

/// Dot product of two equal-length f32 vectors.
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    #[cfg(target_arch = "aarch64")]
    {
        neon::dot(a, b)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        dot_scalar(a, b)
    }
}

/// Dot product of two equal-length i8 vectors, accumulated exactly in i32.
#[inline]
pub fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
    #[cfg(target_arch = "aarch64")]
    {
        neon::dot_i8(a, b)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        dot_i8_scalar(a, b)
    }
}

/// Portable f32 dot product with 4 independent accumulators (lets the
/// compiler pipeline / auto-vectorize it).
pub fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut acc = [0.0f32; 4];
    let chunks = a.len() / 4;
    for i in 0..chunks {
        for j in 0..4 {
            acc[j] += a[i * 4 + j] * b[i * 4 + j];
        }
    }
    let mut sum = (acc[0] + acc[1]) + (acc[2] + acc[3]);
    for i in chunks * 4..a.len() {
        sum += a[i] * b[i];
    }
    sum
}

/// Portable i8 dot product.
pub fn dot_i8_scalar(a: &[i8], b: &[i8]) -> i32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(&x, &y)| i32::from(x) * i32::from(y))
        .sum()
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    pub fn dot(a: &[f32], b: &[f32]) -> f32 {
        assert_eq!(a.len(), b.len());
        let n = a.len();
        let chunks = n / 16;
        // SAFETY: NEON is always available on aarch64. Each iteration loads
        // 16 floats starting at `p = i * 16` with `p + 16 <= chunks * 16 <= n`,
        // so all four 4-lane loads stay inside both slices.
        let mut sum = unsafe {
            let mut acc0 = vdupq_n_f32(0.0);
            let mut acc1 = vdupq_n_f32(0.0);
            let mut acc2 = vdupq_n_f32(0.0);
            let mut acc3 = vdupq_n_f32(0.0);
            let (pa, pb) = (a.as_ptr(), b.as_ptr());
            for i in 0..chunks {
                let p = i * 16;
                // Fused multiply-add: acc += a * b, four 4-lane vectors at a time.
                acc0 = vfmaq_f32(acc0, vld1q_f32(pa.add(p)), vld1q_f32(pb.add(p)));
                acc1 = vfmaq_f32(acc1, vld1q_f32(pa.add(p + 4)), vld1q_f32(pb.add(p + 4)));
                acc2 = vfmaq_f32(acc2, vld1q_f32(pa.add(p + 8)), vld1q_f32(pb.add(p + 8)));
                acc3 = vfmaq_f32(acc3, vld1q_f32(pa.add(p + 12)), vld1q_f32(pb.add(p + 12)));
            }
            vaddvq_f32(vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3)))
        };
        for i in chunks * 16..n {
            sum += a[i] * b[i];
        }
        sum
    }

    pub fn dot_i8(a: &[i8], b: &[i8]) -> i32 {
        assert_eq!(a.len(), b.len());
        let n = a.len();
        let chunks = n / 16;
        // SAFETY: as above; each iteration reads bytes [i*16, i*16 + 16) of
        // both slices, which is in bounds because i < n / 16.
        // i8 * i8 fits in i16 (|x| <= 128 so |x*y| <= 16384), `vmull_s8`
        // widens exactly, and `vpadalq_s16` adds pairs into four i32 lanes:
        // each lane gains at most 4 * 16384 per 16-byte chunk, so i32 cannot
        // overflow below ~500k dimensions.
        let mut sum = unsafe {
            let mut acc = vdupq_n_s32(0);
            let (pa, pb) = (a.as_ptr(), b.as_ptr());
            for i in 0..chunks {
                let va = vld1q_s8(pa.add(i * 16));
                let vb = vld1q_s8(pb.add(i * 16));
                acc = vpadalq_s16(acc, vmull_s8(vget_low_s8(va), vget_low_s8(vb)));
                acc = vpadalq_s16(acc, vmull_high_s8(va, vb));
            }
            vaddvq_s32(acc)
        };
        for i in chunks * 16..n {
            sum += i32::from(a[i]) * i32::from(b[i]);
        }
        sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn hand_examples() {
        assert_eq!(dot(&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]), 32.0);
        assert_eq!(dot_i8(&[1, -2, 3], &[4, 5, -6]), 4 - 10 - 18);
        assert_eq!(dot_i8(&[-128; 40], &[-128; 40]), 40 * 16384);
        assert_eq!(dot(&[], &[]), 0.0);
    }

    proptest! {
        #[test]
        fn simd_matches_scalar(v in prop::collection::vec((-1.0f32..1.0, -1.0f32..1.0), 0..700)) {
            let (a, b): (Vec<f32>, Vec<f32>) = v.into_iter().unzip();
            let fast = dot(&a, &b);
            let slow: f64 = a.iter().zip(&b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum();
            prop_assert!((f64::from(fast) - slow).abs() < 1e-4, "{} vs {}", fast, slow);
            prop_assert!((dot_scalar(&a, &b) as f64 - slow).abs() < 1e-4);
        }

        #[test]
        fn simd_i8_matches_scalar(v in prop::collection::vec((any::<i8>(), any::<i8>()), 0..700)) {
            let (a, b): (Vec<i8>, Vec<i8>) = v.into_iter().unzip();
            prop_assert_eq!(dot_i8(&a, &b), dot_i8_scalar(&a, &b)); // integer: exact
        }
    }
}
