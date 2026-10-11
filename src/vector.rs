use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    F32,
    Q8,
}

impl Encoding {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "f32" => Ok(Self::F32),
            "q8" => Ok(Self::Q8),
            _ => bail!("encoding must be f32 or q8"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::Q8 => "q8",
        }
    }
}

pub fn normalized_prefix(v: &[f32], dim: usize) -> Result<Vec<f32>> {
    if v.len() < dim || dim == 0 {
        bail!("embedding has {} dimensions; need {dim}", v.len());
    }
    let mut a = v[..dim].to_vec();
    let norm = a
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt();
    if norm <= 1e-12 || !norm.is_finite() {
        bail!("invalid embedding norm");
    }
    for x in &mut a {
        *x = (*x as f64 / norm) as f32;
    }
    Ok(a)
}

pub fn pack(v: &[f32], encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::F32 => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
        Encoding::Q8 => {
            let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
            let scale = if max > 0.0 { max / 127.0 } else { 1.0 };
            let mut out = Vec::with_capacity(v.len() + 4);
            out.extend_from_slice(&scale.to_le_bytes());
            out.extend(
                v.iter()
                    .map(|x| (x / scale).round().clamp(-127., 127.) as i8 as u8),
            );
            out
        }
    }
}

/// Normalize reconstructed vectors so composed distances are actual cosine
/// distances even when quantization changed their norm slightly.
pub fn unpack_normalized(bytes: &[u8], encoding: Encoding, dim: usize) -> Result<Vec<f32>> {
    let values = match encoding {
        Encoding::F32 => {
            if bytes.len() != dim * 4 {
                bail!("invalid f32 blob length");
            }
            bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect::<Vec<_>>()
        }
        Encoding::Q8 => {
            if bytes.len() != dim + 4 {
                bail!("invalid q8 blob length");
            }
            let scale = f32::from_le_bytes(bytes[..4].try_into().unwrap());
            bytes[4..]
                .iter()
                .map(|b| *b as i8 as f32 * scale)
                .collect::<Vec<_>>()
        }
    };
    normalized_prefix(&values, dim)
}

pub fn similarity(query: &[f32], bytes: &[u8], encoding: Encoding) -> Result<f32> {
    match encoding {
        Encoding::F32 => {
            if bytes.len() != 4 * query.len() {
                bail!("invalid f32 blob length");
            }
            Ok(query
                .iter()
                .zip(bytes.chunks_exact(4))
                .map(|(a, b)| *a * f32::from_le_bytes(b.try_into().unwrap()))
                .sum())
        }
        Encoding::Q8 => {
            if bytes.len() != query.len() + 4 {
                bail!("invalid q8 blob length");
            }
            let scale = f32::from_le_bytes(bytes[..4].try_into().unwrap());
            Ok(query
                .iter()
                .zip(&bytes[4..])
                .map(|(a, b)| *a * (*b as i8 as f32))
                .sum::<f32>()
                * scale)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantized_cosine_is_close() {
        let v = normalized_prefix(&[0.12, -0.4, 0.3, -0.7, 0.03], 5).unwrap();
        let f = similarity(&v, &pack(&v, Encoding::F32), Encoding::F32).unwrap();
        let q = similarity(&v, &pack(&v, Encoding::Q8), Encoding::Q8).unwrap();
        assert!((f - q).abs() < 0.01);
    }
    #[test]
    fn truncated_vector_is_renormalized() {
        let v = normalized_prefix(&[0.3, 0.4, 0.9], 2).unwrap();
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.).abs() < 1e-5);
    }
}
