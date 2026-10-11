//! Query compilation with outward floating-point guards. Compiled expressions
//! only reject impossible contenders; all admitted scores use the old formula.
use crate::{
    retrieve::Combine,
    vector::{Encoding, unpack_normalized},
};
use anyhow::{Result, bail};
use serde::Serialize;

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x * y)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

pub fn composite(d: &[f32], positives: usize, mode: Combine, weight: f32) -> f32 {
    let positive = match mode {
        Combine::All => d[..positives].iter().sum(),
        Combine::Any => d[..positives].iter().copied().fold(f32::INFINITY, f32::min),
        Combine::Strict => d[..positives].iter().copied().fold(0.0, f32::max),
        Combine::Bridge => d[..positives]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max),
    };
    positive
        - weight
            * d[positives..]
                .iter()
                .copied()
                .reduce(f32::min)
                .unwrap_or(0.0)
}

#[derive(Default, Serialize, Clone)]
pub struct Stats {
    pub executor: &'static str,
    pub plan: &'static str,
    pub screening_backend: &'static str,
    pub screened_candidates: usize,
    pub pruned_candidates: usize,
    pub reference_evaluations: usize,
    pub screening_dot_products: usize,
    pub reference_dot_products: usize,
}

pub struct Query<'a> {
    bases: &'a [Vec<f32>],
    positives: usize,
    mode: Combine,
    weight: f32,
    compiled: Vec<f32>,
    offset: f64,
    margin: f64,
    fused_avoid: bool,
    pub stats: Stats,
}

fn norm(v: &[f32]) -> f64 {
    v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt()
}
fn gamma(n: usize) -> f64 {
    let nu = n as f64 * (f32::EPSILON as f64 / 2.0);
    if nu >= 0.5 {
        f64::INFINITY
    } else {
        nu / (1.0 - nu)
    }
}

impl<'a> Query<'a> {
    pub fn new(
        bases: &'a [Vec<f32>],
        positives: usize,
        mode: Combine,
        weight: f32,
        reference: bool,
    ) -> Self {
        let dim = bases[0].len();
        let fused_avoid = bases.len() == positives + 1 && !matches!(mode, Combine::Any);
        let mean = matches!(mode, Combine::Strict);
        let count = if mean { positives as f64 } else { 1.0 };
        let exact: Vec<f64> = (0..dim)
            .map(|i| {
                let positive = bases[..positives].iter().map(|v| v[i] as f64).sum::<f64>() / count;
                positive
                    - if fused_avoid {
                        weight as f64 * bases[positives][i] as f64
                    } else {
                        0.0
                    }
            })
            .collect();
        let compiled: Vec<f32> = exact.iter().map(|x| *x as f32).collect();
        let coefficient_error = exact
            .iter()
            .zip(&compiled)
            .map(|(a, b)| (a - *b as f64).powi(2))
            .sum::<f64>()
            .sqrt();
        let positive_norm = bases[..positives].iter().map(|v| norm(v)).sum::<f64>();
        let negative_norm = bases[positives..]
            .iter()
            .map(|v| norm(v))
            .fold(0.0, f64::max);
        // Covers scalar and SIMD dot reductions, 1-dot subtraction, positive
        // summation, negative multiplication, coefficient rounding and Q8
        // scale-cancellation rounding. Inputs to score_vector are normalized.
        // Clamp is 1-Lipschitz; near +/-1 we keep the outward guard, not a clamp
        // on a fused query. Larger dimensions make this bound less selective.
        let arithmetic_norm = positive_norm + weight as f64 * negative_norm;
        let screening_norm = norm(&compiled) + weight as f64 * negative_norm;
        let margin = 4.0 * gamma(2 * dim + 32) * (arithmetic_norm + screening_norm + 1.0)
            + 4.0 * gamma(2 * bases.len() + 16) * (positives as f64 + weight as f64 + 1.0)
            + 2.0 * coefficient_error;
        let has_avoid = bases.len() > positives;
        let offset = if matches!(mode, Combine::All) {
            -(positives as f64)
        } else {
            -1.0
        } + if has_avoid { weight as f64 } else { 0.0 };
        let plan = match mode {
            Combine::All if fused_avoid => "all_linear_single_avoid",
            Combine::All if has_avoid => "all_linear_max_avoid",
            Combine::All => "all_linear",
            Combine::Strict => "strict_mean_upper_bound",
            Combine::Any => "any_max",
            Combine::Bridge => "bridge_reference",
        };
        Self {
            bases,
            positives,
            mode,
            weight,
            compiled,
            offset,
            margin,
            fused_avoid,
            stats: Stats {
                executor: if reference { "reference" } else { "exact" },
                plan,
                screening_backend: crate::simd::backend(),
                ..Stats::default()
            },
        }
    }

    pub fn reference(&mut self, v: &[f32]) -> f32 {
        self.stats.reference_evaluations += 1;
        self.stats.reference_dot_products += self.bases.len();
        let d: Vec<_> = self.bases.iter().map(|b| 1.0 - cosine(v, b)).collect();
        -composite(&d, self.positives, self.mode, self.weight)
    }

    fn upper_bound(&mut self, mut dot: impl FnMut(&[f32]) -> f32) -> f64 {
        self.stats.screened_candidates += 1;
        let positive = if matches!(self.mode, Combine::Any) {
            self.stats.screening_dot_products += self.positives;
            self.bases[..self.positives]
                .iter()
                .map(|b| dot(b) as f64)
                .fold(f64::NEG_INFINITY, f64::max)
        } else {
            self.stats.screening_dot_products += 1;
            dot(&self.compiled) as f64
        };
        let negative = if self.fused_avoid || self.weight == 0.0 {
            0.0
        } else {
            self.stats.screening_dot_products += self.bases.len() - self.positives;
            self.bases[self.positives..]
                .iter()
                .map(|b| dot(b) as f64)
                .reduce(f64::max)
                .unwrap_or(0.0)
                * self.weight as f64
        };
        self.offset + positive - negative + self.margin
    }

    pub fn score_vector(&mut self, v: &[f32], cutoff: Option<f32>) -> Option<f32> {
        if self.stats.executor != "reference"
            && !matches!(self.mode, Combine::Bridge)
            && let Some(cutoff) = cutoff
        {
            let upper = self.upper_bound(|q| crate::simd::dot(v, q));
            // Strict comparison preserves all score ties and path/ordinal order.
            if upper < cutoff as f64 {
                self.stats.pruned_candidates += 1;
                return None;
            }
        }
        Some(self.reference(v))
    }

    pub fn score_blob(
        &mut self,
        blob: &[u8],
        enc: Encoding,
        cutoff: Option<f32>,
    ) -> Result<Option<f32>> {
        let dim = self.compiled.len();
        if self.stats.executor != "reference"
            && !matches!(self.mode, Combine::Bridge)
            && enc == Encoding::Q8
            && let Some(cutoff) = cutoff
        {
            if blob.len() != dim + 4 {
                bail!("invalid q8 blob length");
            }
            let scale = f32::from_le_bytes(blob[..4].try_into().unwrap());
            let ints = &blob[4..];
            let squared: u64 = ints
                .iter()
                .map(|b| {
                    let n = *b as i8 as i64;
                    (n * n) as u64
                })
                .sum();
            // Avoid using scale cancellation outside ordinary finite Q8 data.
            // Extreme/invalid scales follow the original decode/error path.
            if scale.is_finite() && scale > 1e-30 && scale < 1e30 && squared > 0 {
                let inverse = (1.0 / (squared as f64).sqrt()) as f32;
                let upper = self.upper_bound(|q| crate::simd::q8_dot(ints, q) * inverse);
                if upper < cutoff as f64 {
                    self.stats.pruned_candidates += 1;
                    return Ok(None);
                }
                return Ok(Some(self.reference(&unpack_normalized(blob, enc, dim)?)));
            }
        }
        Ok(self.score_vector(&unpack_normalized(blob, enc, dim)?, cutoff))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_q8_never_prunes_a_candidate_above_cutoff() {
        // With two negative similarities, their sum is below the weakest
        // similarity, so treating a sum as an upper bound would lose a hit.
        let bases = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.0, 1.0, 0.0, 0.0]];
        let candidate = vec![-std::f32::consts::FRAC_1_SQRT_2; 2]
            .into_iter()
            .chain([0.0, 0.0])
            .collect::<Vec<f32>>();
        let packed = crate::store::encode(&candidate, Encoding::Q8);
        let mut query = Query::new(&bases, 2, Combine::Bridge, 1.0, false);
        let score = query.score_blob(&packed, Encoding::Q8, Some(-2.0)).unwrap();
        assert!(score.unwrap() > -2.0);
        assert_eq!(query.stats.pruned_candidates, 0);
    }
}
