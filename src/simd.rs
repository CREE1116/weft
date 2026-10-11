//! Exact Q8 × f32 scoring kernels. SIMD uses a different floating-point
//! accumulation order from a scalar loop; validate scores within rounding error.
pub fn backend() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    {
        return "neon";
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        return "avx2";
    }
    #[allow(unreachable_code)]
    "scalar"
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "aarch64")]
    // SAFETY: NEON is baseline on aarch64; the kernel bounds every load.
    unsafe {
        return neon::dot(a, b);
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: runtime detection and length checks cover the kernel.
        unsafe {
            return avx2::dot(a, b);
        }
    }
    #[allow(unreachable_code)]
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

pub fn q8_dot(bytes: &[u8], q: &[f32]) -> f32 {
    assert_eq!(bytes.len(), q.len());
    #[cfg(target_arch = "aarch64")]
    // SAFETY: see dot; bytes are sign-extended before conversion to float.
    unsafe {
        return neon::q8_dot(bytes, q);
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        unsafe {
            return avx2::q8_dot(bytes, q);
        }
    }
    #[allow(unreachable_code)]
    bytes
        .iter()
        .zip(q)
        .map(|(a, b)| (*a as i8 as f32) * b)
        .sum()
}

/// Score two queries against the same Q8 vector while loading and converting
/// the quantized bytes only once. The query vectors retain their f32 precision.
#[inline]
pub fn q8_dot2(bytes: &[u8], q0: &[f32], q1: &[f32]) -> (f32, f32) {
    assert_eq!(bytes.len(), q0.len());
    assert_eq!(bytes.len(), q1.len());
    #[cfg(target_arch = "aarch64")]
    unsafe {
        return neon::q8_dot2(bytes, q0, q1);
    }
    #[allow(unreachable_code)]
    (q8_dot(bytes, q0), q8_dot(bytes, q1))
}

// Comparison kernel for reproducible local benchmarks. Kept out of production.
#[cfg(test)]
pub fn q8_dot_prior(a: &[u8], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    #[cfg(target_arch = "aarch64")]
    unsafe {
        use std::arch::aarch64::*;
        let mut acc0 = vdupq_n_f32(0.0);
        let mut acc1 = acc0;
        let mut i = 0;
        while i + 8 <= a.len() {
            let bytes = vld1_s8(a.as_ptr().add(i).cast());
            let wide = vmovl_s8(bytes);
            let x0 = vcvtq_f32_s32(vmovl_s16(vget_low_s16(wide)));
            let x1 = vcvtq_f32_s32(vmovl_s16(vget_high_s16(wide)));
            acc0 = vaddq_f32(acc0, vmulq_f32(x0, vld1q_f32(b.as_ptr().add(i))));
            acc1 = vaddq_f32(acc1, vmulq_f32(x1, vld1q_f32(b.as_ptr().add(i + 4))));
            i += 8;
        }
        let mut sum = vaddvq_f32(vaddq_f32(acc0, acc1));
        for j in i..a.len() {
            sum += (a[j] as i8 as f32) * b[j];
        }
        return sum;
    }
    #[allow(unreachable_code)]
    q8_dot(a, b)
}

#[cfg(test)]
pub fn q8_dot_scalar(a: &[u8], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    let mut sum = 0.0;
    for i in 0..a.len() {
        sum += (a[i] as i8 as f32) * b[i];
    }
    sum
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        unsafe {
            let mut acc = vdupq_n_f32(0.0);
            let mut i = 0;
            while i + 4 <= a.len() {
                acc = vaddq_f32(
                    acc,
                    vmulq_f32(vld1q_f32(a.as_ptr().add(i)), vld1q_f32(b.as_ptr().add(i))),
                );
                i += 4;
            }
            let mut sum = vaddvq_f32(acc);
            for j in i..a.len() {
                sum += a[j] * b[j];
            }
            sum
        }
    }
    pub unsafe fn q8_dot(a: &[u8], b: &[f32]) -> f32 {
        unsafe {
            // Keep four independent accumulators to hide SIMD conversion and
            // multiply latency. A 16-byte load feeds four f32 vectors.
            let mut acc0 = vdupq_n_f32(0.0);
            let mut acc1 = acc0;
            let mut acc2 = acc0;
            let mut acc3 = acc0;
            let mut i = 0;
            while i + 16 <= a.len() {
                let bytes = vld1q_s8(a.as_ptr().add(i).cast());
                let lower = vmovl_s8(vget_low_s8(bytes));
                let upper = vmovl_s8(vget_high_s8(bytes));
                let x0 = vcvtq_f32_s32(vmovl_s16(vget_low_s16(lower)));
                let x1 = vcvtq_f32_s32(vmovl_s16(vget_high_s16(lower)));
                let x2 = vcvtq_f32_s32(vmovl_s16(vget_low_s16(upper)));
                let x3 = vcvtq_f32_s32(vmovl_s16(vget_high_s16(upper)));
                acc0 = vaddq_f32(acc0, vmulq_f32(x0, vld1q_f32(b.as_ptr().add(i))));
                acc1 = vaddq_f32(acc1, vmulq_f32(x1, vld1q_f32(b.as_ptr().add(i + 4))));
                acc2 = vaddq_f32(acc2, vmulq_f32(x2, vld1q_f32(b.as_ptr().add(i + 8))));
                acc3 = vaddq_f32(acc3, vmulq_f32(x3, vld1q_f32(b.as_ptr().add(i + 12))));
                i += 16;
            }
            let mut sum = vaddvq_f32(vaddq_f32(vaddq_f32(acc0, acc1), vaddq_f32(acc2, acc3)));
            for j in i..a.len() {
                sum += (a[j] as i8 as f32) * b[j];
            }
            sum
        }
    }

    pub unsafe fn q8_dot2(a: &[u8], b: &[f32], c: &[f32]) -> (f32, f32) {
        unsafe {
            let mut ab0 = vdupq_n_f32(0.0);
            let mut ab1 = ab0;
            let mut ab2 = ab0;
            let mut ab3 = ab0;
            let mut ac0 = ab0;
            let mut ac1 = ab0;
            let mut ac2 = ab0;
            let mut ac3 = ab0;
            let mut i = 0;
            while i + 16 <= a.len() {
                let bytes = vld1q_s8(a.as_ptr().add(i).cast());
                let lo = vmovl_s8(vget_low_s8(bytes));
                let hi = vmovl_s8(vget_high_s8(bytes));
                let x0 = vcvtq_f32_s32(vmovl_s16(vget_low_s16(lo)));
                let x1 = vcvtq_f32_s32(vmovl_s16(vget_high_s16(lo)));
                let x2 = vcvtq_f32_s32(vmovl_s16(vget_low_s16(hi)));
                let x3 = vcvtq_f32_s32(vmovl_s16(vget_high_s16(hi)));
                ab0 = vaddq_f32(ab0, vmulq_f32(x0, vld1q_f32(b.as_ptr().add(i))));
                ac0 = vaddq_f32(ac0, vmulq_f32(x0, vld1q_f32(c.as_ptr().add(i))));
                ab1 = vaddq_f32(ab1, vmulq_f32(x1, vld1q_f32(b.as_ptr().add(i + 4))));
                ac1 = vaddq_f32(ac1, vmulq_f32(x1, vld1q_f32(c.as_ptr().add(i + 4))));
                ab2 = vaddq_f32(ab2, vmulq_f32(x2, vld1q_f32(b.as_ptr().add(i + 8))));
                ac2 = vaddq_f32(ac2, vmulq_f32(x2, vld1q_f32(c.as_ptr().add(i + 8))));
                ab3 = vaddq_f32(ab3, vmulq_f32(x3, vld1q_f32(b.as_ptr().add(i + 12))));
                ac3 = vaddq_f32(ac3, vmulq_f32(x3, vld1q_f32(c.as_ptr().add(i + 12))));
                i += 16;
            }
            let mut sb = vaddvq_f32(vaddq_f32(vaddq_f32(ab0, ab1), vaddq_f32(ab2, ab3)));
            let mut sc = vaddvq_f32(vaddq_f32(vaddq_f32(ac0, ac1), vaddq_f32(ac2, ac3)));
            for j in i..a.len() {
                let x = a[j] as i8 as f32;
                sb += x * b[j];
                sc += x * c[j];
            }
            (sb, sc)
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;
    #[target_feature(enable = "avx2")]
    pub unsafe fn dot(a: &[f32], b: &[f32]) -> f32 {
        unsafe {
            let mut acc = _mm256_setzero_ps();
            let mut i = 0;
            while i + 8 <= a.len() {
                acc = _mm256_add_ps(
                    acc,
                    _mm256_mul_ps(
                        _mm256_loadu_ps(a.as_ptr().add(i)),
                        _mm256_loadu_ps(b.as_ptr().add(i)),
                    ),
                );
                i += 8;
            }
            let mut lanes = [0f32; 8];
            _mm256_storeu_ps(lanes.as_mut_ptr(), acc);
            let mut sum: f32 = lanes.into_iter().sum();
            for j in i..a.len() {
                sum += a[j] * b[j];
            }
            sum
        }
    }
    #[target_feature(enable = "avx2")]
    pub unsafe fn q8_dot(a: &[u8], b: &[f32]) -> f32 {
        unsafe {
            let mut acc = _mm256_setzero_ps();
            let mut i = 0;
            while i + 8 <= a.len() {
                let bytes = _mm_loadl_epi64(a.as_ptr().add(i).cast());
                let x = _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(bytes));
                acc = _mm256_add_ps(acc, _mm256_mul_ps(x, _mm256_loadu_ps(b.as_ptr().add(i))));
                i += 8;
            }
            let mut lanes = [0f32; 8];
            _mm256_storeu_ps(lanes.as_mut_ptr(), acc);
            let mut sum: f32 = lanes.into_iter().sum();
            for j in i..a.len() {
                sum += (a[j] as i8 as f32) * b[j];
            }
            sum
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn fused_two_query_dot_matches_separate_dots() {
        for len in [0, 1, 3, 4, 7, 8, 15, 16, 31, 32, 127, 768, 771] {
            let bytes: Vec<u8> = (0..len).map(|i| ((i * 37 + 117) % 256) as u8).collect();
            let qa: Vec<f32> = (0..len)
                .map(|i| ((i * 19 % 127) as f32 - 63.0) / 4096.0)
                .collect();
            let qb: Vec<f32> = (0..len)
                .map(|i| ((i * 17 % 113) as f32 - 56.0) / 4096.0)
                .collect();
            let (a, b) = super::q8_dot2(&bytes, &qa, &qb);
            let (a0, b0) = (super::q8_dot(&bytes, &qa), super::q8_dot(&bytes, &qb));
            assert!(
                (a - a0).abs() <= 0.00002 * (1.0 + a0.abs()),
                "len={len}: {a} vs {a0}"
            );
            assert!(
                (b - b0).abs() <= 0.00002 * (1.0 + b0.abs()),
                "len={len}: {b} vs {b0}"
            );
        }
    }
}
