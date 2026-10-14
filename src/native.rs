//! Building blocks for the in-process EmbeddingGemma 2 runtime.
//!
//! The tokenizer is reconstructed from GGUF metadata so the runtime does not
//! need to keep Hugging Face's much larger `tokenizer.json` beside the model.

use anyhow::{Context, Result, bail};
use candle_core::{
    D, Device, Module, Tensor,
    quantized::{GgmlDType, QMatMul, QTensor, ggml_file, gguf_file},
};
use std::{collections::HashMap, fs::File, path::Path, sync::Mutex, time::Instant};

// Most interactive searches fit within this limit. Long document embedding
// still uses the original 2048-position tables, initialized on first use.
const SHORT_ROPE_LEN: usize = 128;
const MAX_LEN: usize = 2048;
use tokenizers::{
    AddedToken, SplitDelimiterBehavior, Tokenizer as HfTokenizer, models::bpe::BPE,
    normalizers::Replace, pre_tokenizers::split::Split,
};

pub struct NativeTokenizer {
    tokenizer: HfTokenizer,
}

struct BlockWeights {
    attn_norm: Tensor,
    attn_q: QMatMul,
    attn_k: QMatMul,
    attn_v: QMatMul,
    attn_q_norm: Tensor,
    attn_k_norm: Tensor,
    attn_output: QMatMul,
    post_attention_norm: Tensor,
    ffn_norm: Tensor,
    ffn_gate: QMatMul,
    ffn_up: QMatMul,
    ffn_down: QMatMul,
    post_ffw_norm: Tensor,
    inp_gate: QMatMul,
    proj: QMatMul,
    post_norm: Tensor,
    layer_output_scale: Tensor,
}

pub struct NativeModel {
    device: Device,
    token_embd: QMatMul,
    per_layer_model_proj: QMatMul,
    per_layer_proj_norm: Tensor,
    output_norm: Tensor,
    output: QMatMul,
    layers: Vec<BlockWeights>,
    local_rope: (Tensor, Tensor),
    full_rope: (Tensor, Tensor),
    long_rope: Mutex<Option<((Tensor, Tensor), (Tensor, Tensor))>>,
    long_mask: Mutex<Option<Tensor>>,
    tokenizer: NativeTokenizer,
}

impl NativeModel {
    pub fn load(path: &Path, device_name: &str) -> Result<Self> {
        Self::load_with_prepared(
            path,
            device_name,
            std::env::var_os("WEFT_DISABLE_PREPARED").is_none(),
        )
    }

    fn load_with_prepared(path: &Path, device_name: &str, use_prepared: bool) -> Result<Self> {
        let load_start = Instant::now();
        let device = select_device(device_name)?;
        let device_ms = load_start.elapsed().as_secs_f64() * 1e3;
        let file = File::open(path).with_context(|| format!("open model {}", path.display()))?;
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .with_context(|| format!("mmap model {}", path.display()))?;
        let content = if use_prepared {
            crate::prepared::load(path, &mmap)?
        } else {
            None
        };
        let content = if let Some(prepared) = content {
            prepared
        } else {
            let mut cursor = std::io::Cursor::new(&mmap[..]);
            gguf_file::Content::read(&mut cursor).context("read GGUF model")?
        };
        if !matches!(content.metadata.get("general.architecture"), Some(gguf_file::Value::String(arch)) if arch == "gemma-embedding2")
        {
            bail!("model is not EmbeddingGemma 2 GGUF")
        }
        let parse_ms = load_start.elapsed().as_secs_f64() * 1e3 - device_ms;
        let gguf_file::Content {
            metadata,
            tensor_infos,
            tensor_data_offset: data_offset,
            ..
        } = content;
        // Tokenizer construction is independent of device uploads. Build it
        // concurrently with the tensor transfer to reduce cold-query latency.
        let tokenizer_start = Instant::now();
        let tokenizer_job = std::thread::spawn(move || NativeTokenizer::from_metadata(metadata));

        // Directly route tensors without intermediate 413-entry HashMap allocations
        #[derive(Default)]
        struct RawBlock {
            attn_norm: Option<Tensor>,
            attn_q: Option<QMatMul>,
            attn_k: Option<QMatMul>,
            attn_v: Option<QMatMul>,
            attn_q_norm: Option<Tensor>,
            attn_k_norm: Option<Tensor>,
            attn_output: Option<QMatMul>,
            post_attention_norm: Option<Tensor>,
            ffn_norm: Option<Tensor>,
            ffn_gate: Option<QMatMul>,
            ffn_up: Option<QMatMul>,
            ffn_down: Option<QMatMul>,
            post_ffw_norm: Option<Tensor>,
            inp_gate: Option<QMatMul>,
            proj: Option<QMatMul>,
            post_norm: Option<Tensor>,
            layer_output_scale: Option<Tensor>,
        }

        let mut raw_blocks: Vec<RawBlock> = (0..24).map(|_| RawBlock::default()).collect();
        let mut token_embd = None;
        let mut per_layer_model_proj = None;
        let mut per_layer_proj_norm = None;
        let mut output_norm = None;
        let mut output = None;

        let fast_norms = std::env::var_os("WEFT_DISABLE_FAST_NORMS").is_none();
        for (name, info) in &tensor_infos {
            // Candle's QTensor::dequantize(Metal) copies each small norm to
            // Metal, blits it back to CPU and synchronizes, then uploads the
            // resulting f32 tensor again. Decode those floats directly from
            // the mapped GGUF bytes to avoid hundreds of GPU round trips.
            let is_norm =
                name.ends_with("norm.weight") || name.ends_with("layer_output_scale.weight");
            let mut norm = if fast_norms && is_norm {
                read_mapped_float_tensor(info, &mmap, data_offset, &device)?
            } else {
                None
            };
            let mut tensor = if norm.is_none() {
                Some(
                    read_mapped_gguf_tensor(info, &mmap, data_offset, &device)
                        .with_context(|| format!("load model tensor {name}"))?,
                )
            } else {
                None
            };

            if name.starts_with("blk.") {
                let rest = &name[4..];
                if let Some((idx_str, field)) = rest.split_once('.') {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        if idx < 24 {
                            let blk = &mut raw_blocks[idx];
                            match field {
                                "attn_norm.weight" => {
                                    blk.attn_norm =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                "attn_q.weight" => {
                                    blk.attn_q = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing attn_q")?,
                                    )?)
                                }
                                "attn_k.weight" => {
                                    blk.attn_k = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing attn_k")?,
                                    )?)
                                }
                                "attn_v.weight" => {
                                    blk.attn_v = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing attn_v")?,
                                    )?)
                                }
                                "attn_q_norm.weight" => {
                                    blk.attn_q_norm =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                "attn_k_norm.weight" => {
                                    blk.attn_k_norm =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                "attn_output.weight" => {
                                    blk.attn_output = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing attn_output")?,
                                    )?)
                                }
                                "post_attention_norm.weight" => {
                                    blk.post_attention_norm =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                "ffn_norm.weight" => {
                                    blk.ffn_norm =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                "ffn_gate.weight" => {
                                    blk.ffn_gate = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing ffn_gate")?,
                                    )?)
                                }
                                "ffn_up.weight" => {
                                    blk.ffn_up = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing ffn_up")?,
                                    )?)
                                }
                                "ffn_down.weight" => {
                                    blk.ffn_down = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing ffn_down")?,
                                    )?)
                                }
                                "post_ffw_norm.weight" => {
                                    blk.post_ffw_norm =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                "inp_gate.weight" => {
                                    blk.inp_gate = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing inp_gate")?,
                                    )?)
                                }
                                "proj.weight" => {
                                    blk.proj = Some(QMatMul::from_qtensor(
                                        tensor.take().context("missing proj")?,
                                    )?)
                                }
                                "post_norm.weight" => {
                                    blk.post_norm =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                "layer_output_scale.weight" => {
                                    blk.layer_output_scale =
                                        Some(take_float(&mut norm, &mut tensor, &device)?)
                                }
                                _ => {}
                            }
                            continue;
                        }
                    }
                }
            }

            match name.as_str() {
                "token_embd.weight" => {
                    token_embd = Some(QMatMul::from_qtensor(
                        tensor.take().context("missing token_embd")?,
                    )?)
                }
                "per_layer_model_proj.weight" => {
                    per_layer_model_proj = Some(QMatMul::from_qtensor(
                        tensor.take().context("missing per_layer_model_proj")?,
                    )?)
                }
                "per_layer_proj_norm.weight" => {
                    per_layer_proj_norm = Some(take_float(&mut norm, &mut tensor, &device)?)
                }
                "output_norm.weight" => {
                    output_norm = Some(take_float(&mut norm, &mut tensor, &device)?)
                }
                "output.weight" => {
                    output = Some(QMatMul::from_qtensor(
                        tensor.take().context("missing output")?,
                    )?)
                }
                _ => {}
            }
        }

        let token_embd = token_embd.context("missing token_embd.weight")?;
        let per_layer_model_proj =
            per_layer_model_proj.context("missing per_layer_model_proj.weight")?;
        let per_layer_proj_norm =
            per_layer_proj_norm.context("missing per_layer_proj_norm.weight")?;
        let output_norm = output_norm.context("missing output_norm.weight")?;
        let output = output.context("missing output.weight")?;

        let mut layers = Vec::with_capacity(24);
        for (i, b) in raw_blocks.into_iter().enumerate() {
            layers.push(BlockWeights {
                attn_norm: b.attn_norm.with_context(|| format!("blk.{i}.attn_norm"))?,
                attn_q: b.attn_q.with_context(|| format!("blk.{i}.attn_q"))?,
                attn_k: b.attn_k.with_context(|| format!("blk.{i}.attn_k"))?,
                attn_v: b.attn_v.with_context(|| format!("blk.{i}.attn_v"))?,
                attn_q_norm: b
                    .attn_q_norm
                    .with_context(|| format!("blk.{i}.attn_q_norm"))?,
                attn_k_norm: b
                    .attn_k_norm
                    .with_context(|| format!("blk.{i}.attn_k_norm"))?,
                attn_output: b
                    .attn_output
                    .with_context(|| format!("blk.{i}.attn_output"))?,
                post_attention_norm: b
                    .post_attention_norm
                    .with_context(|| format!("blk.{i}.post_attention_norm"))?,
                ffn_norm: b.ffn_norm.with_context(|| format!("blk.{i}.ffn_norm"))?,
                ffn_gate: b.ffn_gate.with_context(|| format!("blk.{i}.ffn_gate"))?,
                ffn_up: b.ffn_up.with_context(|| format!("blk.{i}.ffn_up"))?,
                ffn_down: b.ffn_down.with_context(|| format!("blk.{i}.ffn_down"))?,
                post_ffw_norm: b
                    .post_ffw_norm
                    .with_context(|| format!("blk.{i}.post_ffw_norm"))?,
                inp_gate: b.inp_gate.with_context(|| format!("blk.{i}.inp_gate"))?,
                proj: b.proj.with_context(|| format!("blk.{i}.proj"))?,
                post_norm: b.post_norm.with_context(|| format!("blk.{i}.post_norm"))?,
                layer_output_scale: b
                    .layer_output_scale
                    .with_context(|| format!("blk.{i}.layer_output_scale"))?,
            });
        }

        let weights_ms = load_start.elapsed().as_secs_f64() * 1e3 - device_ms - parse_ms;
        let local_rope = precompute_rope(&device, 256, SHORT_ROPE_LEN, 10_000.)?;
        let full_rope = precompute_rope(&device, 512, SHORT_ROPE_LEN, 1_000_000.)?;
        let prep_ms = load_start.elapsed().as_secs_f64() * 1e3 - device_ms - parse_ms - weights_ms;
        let tokenizer = tokenizer_job
            .join()
            .map_err(|_| anyhow::anyhow!("GGUF tokenizer worker panicked"))??;
        let tokenizer_overlap_ms = tokenizer_start.elapsed().as_secs_f64() * 1e3;

        if std::env::var_os("WEFT_PROFILE_LOAD").is_some() {
            eprintln!(
                "model load ms: device={device_ms:.2} parse={parse_ms:.2} parallel_tokenizer_span={tokenizer_overlap_ms:.2} weights={weights_ms:.2} rope={prep_ms:.2} total={:.2}",
                load_start.elapsed().as_secs_f64() * 1e3
            );
        }

        // Release host CPU mmap pages back to OS immediately since all tensors are on device
        #[cfg(unix)]
        unsafe {
            libc::madvise(
                mmap.as_ptr() as *mut libc::c_void,
                mmap.len(),
                libc::MADV_DONTNEED,
            );
        }
        drop(mmap);

        eprintln!(
            "Loaded EmbeddingGemma 2 on {:?} (24 layers, precomputed RoPE caches)",
            device.location()
        );
        Ok(Self {
            device,
            token_embd,
            per_layer_model_proj,
            per_layer_proj_norm,
            output_norm,
            output,
            layers,
            local_rope,
            full_rope,
            long_rope: Mutex::new(None),
            long_mask: Mutex::new(None),
            tokenizer,
        })
    }

    pub fn embed(&self, text: &str, role: &str) -> Result<Vec<f32>> {
        let prompt = if role == "query" {
            format!("task: search result | query: {text}")
        } else {
            format!("title: none | text: {text}")
        };
        let ids = self.tokenizer.encode(&prompt)?;
        if ids.len() > MAX_LEN {
            bail!(
                "text tokenizes to {} tokens; maximum supported is 2046",
                ids.len() - 2
            )
        }
        let seq = ids.len();
        let token_ids = Tensor::from_vec(ids, (seq,), &self.device)?;
        let mut x = self
            .token_embd
            .embedding(&token_ids)?
            .affine(512f64.sqrt(), 0.)?;
        let per_layer = self
            .linear(&self.per_layer_model_proj, &x)?
            .affine(512f64.sqrt().recip(), 0.)?
            .reshape((seq, 24, 512))?;
        let per_layer =
            candle_nn::ops::rms_norm(&per_layer.contiguous()?, &self.per_layer_proj_norm, 1.0e-6)?;

        // Interactive queries only allocate short RoPE tables; longer indexing
        // inputs initialize and retain the full tables on their first use.
        let expanded_rope = if seq > SHORT_ROPE_LEN {
            let mut cache = self.long_rope.lock().unwrap();
            if cache.is_none() {
                *cache = Some((
                    precompute_rope(&self.device, 256, MAX_LEN, 10_000.)?,
                    precompute_rope(&self.device, 512, MAX_LEN, 1_000_000.)?,
                ));
            }
            cache.clone()
        } else {
            None
        };
        let (local_table, full_table) = expanded_rope
            .as_ref()
            .map(|(local, full)| (local, full))
            .unwrap_or((&self.local_rope, &self.full_rope));
        let local_rope = (
            local_table.0.narrow(1, 0, seq)?,
            local_table.1.narrow(1, 0, seq)?,
        );
        let full_rope = (
            full_table.0.narrow(1, 0, seq)?,
            full_table.1.narrow(1, 0, seq)?,
        );
        // For seq <= 513 every relative position is inside the 512-token
        // sliding window; adding an all-zero mask is mathematically redundant.
        let mask = cached_mask(&mut self.long_mask.lock().unwrap(), &self.device, seq)?;

        for layer in 0..24 {
            let ple = per_layer.narrow(1, layer, 1)?.squeeze(1)?;
            let rope = if layer % 6 == 5 {
                &full_rope
            } else {
                &local_rope
            };
            x = self.layer_forward(layer, &x, &ple, seq, rope, mask.as_ref())?;
        }
        let x = candle_nn::ops::rms_norm(&x.contiguous()?, &self.output_norm, 1.0e-6)?;
        // Linear projection commutes with mean pooling: project once, not per token.
        let pooled = self
            .linear(&self.output, &x.mean_keepdim(0)?)?
            .squeeze(0)?
            .contiguous()?;
        normalize_embedding(&pooled)
    }

    fn layer_forward(
        &self,
        i: usize,
        x: &Tensor,
        ple: &Tensor,
        seq: usize,
        rope: &(Tensor, Tensor),
        mask: Option<&Tensor>,
    ) -> Result<Tensor> {
        let b = &self.layers[i];
        let mut residual = x.clone();
        let h = candle_nn::ops::rms_norm(&x.contiguous()?, &b.attn_norm, 1.0e-6)?;
        let qdim = if i % 6 == 5 { 512 } else { 256 };
        let kvheads = if qdim == 512 { 1 } else { 2 };
        let q = self
            .linear(&b.attn_q, &h)?
            .reshape((seq, 4, qdim))?
            .transpose(0, 1)?;
        let k = self
            .linear(&b.attn_k, &h)?
            .reshape((seq, kvheads, qdim))?
            .transpose(0, 1)?;
        let v = self
            .linear(&b.attn_v, &h)?
            .reshape((seq, kvheads, qdim))?
            .transpose(0, 1)?;
        let q = candle_nn::ops::rms_norm(&q.contiguous()?, &b.attn_q_norm, 1.0e-6)?;
        let k = candle_nn::ops::rms_norm(&k.contiguous()?, &b.attn_k_norm, 1.0e-6)?;
        let v = self.rms_nonorm(&v)?;
        let (q, k) = self.apply_rope(&q, &k, qdim, rope)?;
        let groups = 4 / kvheads;
        let k = k
            .unsqueeze(1)?
            .expand((kvheads, groups, seq, qdim))?
            .reshape((4, seq, qdim))?;
        let v = v
            .unsqueeze(1)?
            .expand((kvheads, groups, seq, qdim))?
            .reshape((4, seq, qdim))?;
        let mut scores = q.matmul(&k.transpose(1, 2)?)?;
        if qdim == 256 {
            if let Some(mask) = mask {
                scores = scores.broadcast_add(mask)?;
            }
        }
        let attended = candle_nn::ops::softmax_last_dim(&scores.contiguous()?)?
            .matmul(&v)?
            .transpose(0, 1)?
            .contiguous()?
            .reshape((seq, 4 * qdim))?;
        let h = self.linear(&b.attn_output, &attended)?;
        residual = residual.add(&candle_nn::ops::rms_norm(
            &h.contiguous()?,
            &b.post_attention_norm,
            1.0e-6,
        )?)?;
        let h = candle_nn::ops::rms_norm(&residual.contiguous()?, &b.ffn_norm, 1.0e-6)?;
        let gate = gelu_tanh(&self.linear(&b.ffn_gate, &h)?)?;
        let up = self.linear(&b.ffn_up, &h)?;
        let h = self.linear(&b.ffn_down, &gate.mul(&up)?)?;
        residual = residual.add(&candle_nn::ops::rms_norm(
            &h.contiguous()?,
            &b.post_ffw_norm,
            1.0e-6,
        )?)?;
        let gate = gelu_tanh(&self.linear(&b.inp_gate, &residual)?)?;
        let h = self.linear(&b.proj, &gate.mul(ple)?)?;
        let h = candle_nn::ops::rms_norm(&h.contiguous()?, &b.post_norm, 1.0e-6)?;
        Ok(residual.add(&h)?.broadcast_mul(&b.layer_output_scale)?)
    }

    fn apply_rope(
        &self,
        q: &Tensor,
        k: &Tensor,
        dim: usize,
        tables: &(Tensor, Tensor),
    ) -> Result<(Tensor, Tensor)> {
        let (cos, sin) = tables;
        let rotate = |x: &Tensor| -> Result<Tensor> {
            Ok(Tensor::cat(
                &[
                    &x.narrow(2, dim / 2, dim / 2)?.neg()?,
                    &x.narrow(2, 0, dim / 2)?,
                ],
                2,
            )?)
        };
        let apply = |x: &Tensor| -> Result<Tensor> {
            Ok(x.broadcast_mul(cos)?.add(&rotate(x)?.broadcast_mul(sin)?)?)
        };
        Ok((apply(q)?, apply(k)?))
    }

    fn rms_nonorm(&self, x: &Tensor) -> Result<Tensor> {
        let x = x.contiguous()?;
        let inv = (x.sqr()?.mean_keepdim(D::Minus1)? + 1.0e-6)?
            .sqrt()?
            .recip()?;
        Ok(x.broadcast_mul(&inv)?)
    }

    fn linear(&self, matrix: &QMatMul, x: &Tensor) -> Result<Tensor> {
        if matches!(self.device, Device::Cpu) {
            if let QMatMul::QTensor(weights) = matrix {
                return Ok(x.matmul(&weights.dequantize(&self.device)?.t()?)?);
            }
        }
        matrix.forward(x).context("matmul")
    }
}

fn precompute_rope(
    device: &Device,
    dim: usize,
    max_len: usize,
    theta: f64,
) -> Result<(Tensor, Tensor)> {
    // Compute frequency denominators only once (the old implementation
    // repeated the expensive powf for every position and every frequency).
    let denominators: Vec<f64> = (0..dim / 2)
        .map(|j| theta.powf((2 * j) as f64 / dim as f64))
        .collect();
    let mut cos = vec![0f32; max_len * dim];
    let mut sin = vec![0f32; max_len * dim];
    for pos in 0..max_len {
        for j in 0..dim / 2 {
            let angle = pos as f64 / denominators[j];
            let idx = pos * dim + j;
            cos[idx] = angle.cos() as f32;
            sin[idx] = angle.sin() as f32;
            cos[idx + dim / 2] = cos[idx];
            sin[idx + dim / 2] = sin[idx];
        }
    }
    let cos = Tensor::from_vec(cos, (max_len, dim), device)?.unsqueeze(0)?;
    let sin = Tensor::from_vec(sin, (max_len, dim), device)?.unsqueeze(0)?;
    Ok((cos, sin))
}

/// Read tensor bytes directly from the page-aligned GGUF mapping. Candle's
/// TensorInfo::read allocates and fills an extra Vec<u8> for every weight.
/// This path transfers the mapped slice directly to Metal, and for CPU makes
/// only the final owned quantized storage copy.
fn mapped_tensor_bytes<'a>(
    info: &gguf_file::TensorInfo,
    mmap: &'a [u8],
    data_offset: u64,
) -> Result<&'a [u8]> {
    let elements = info.shape.elem_count();
    let block_size = info.ggml_dtype.block_size();
    if !elements.is_multiple_of(block_size) {
        bail!("invalid quantized tensor length {elements} for block size {block_size}");
    }
    let length = (elements / block_size)
        .checked_mul(info.ggml_dtype.type_size())
        .context("quantized tensor size overflow")?;
    let start = usize::try_from(
        data_offset
            .checked_add(info.offset)
            .context("tensor offset overflow")?,
    )?;
    let end = start
        .checked_add(length)
        .context("tensor offset overflow")?;
    mmap.get(start..end)
        .context("GGUF tensor extends beyond mapped file")
}

/// Decode the small floating-point norms on the host before moving the final
/// f32 data to Metal. QTensor::dequantize(Metal) synchronizes a GPU→CPU blit
/// for each tensor, even if the source GGUF already stores plain floats.
fn read_mapped_float_tensor(
    info: &gguf_file::TensorInfo,
    mmap: &[u8],
    data_offset: u64,
    device: &Device,
) -> Result<Option<Tensor>> {
    let bytes = mapped_tensor_bytes(info, mmap, data_offset)?;
    let vals: Vec<f32> = match info.ggml_dtype {
        GgmlDType::F32 => bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
        GgmlDType::F16 => bytes
            .chunks_exact(2)
            .map(|b| half::f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32())
            .collect(),
        GgmlDType::BF16 => bytes
            .chunks_exact(2)
            .map(|b| half::bf16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32())
            .collect(),
        _ => return Ok(None),
    };
    Ok(Some(Tensor::from_vec(vals, info.shape.clone(), device)?))
}

fn take_float(
    norm: &mut Option<Tensor>,
    tensor: &mut Option<QTensor>,
    device: &Device,
) -> Result<Tensor> {
    if let Some(value) = norm.take() {
        return Ok(value);
    }
    tensor
        .take()
        .context("missing quantized norm")?
        .dequantize(device)
        .map_err(Into::into)
}

fn read_mapped_gguf_tensor(
    info: &gguf_file::TensorInfo,
    mmap: &[u8],
    data_offset: u64,
    device: &Device,
) -> Result<QTensor> {
    let bytes = mapped_tensor_bytes(info, mmap, data_offset)?;
    Ok(ggml_file::qtensor_from_ggml(
        info.ggml_dtype,
        bytes,
        info.shape.dims().to_vec(),
        device,
    )?)
}

fn normalize_embedding(pooled: &Tensor) -> Result<Vec<f32>> {
    let norm = pooled.sqr()?.sum_all()?.sqrt()?;
    // Normalize before the single device-to-host transfer. Invalid norms
    // produce non-finite values or an all-zero vector; reject both on the host.
    let output = pooled.broadcast_div(&norm)?.to_vec1::<f32>()?;
    if output.iter().any(|value| !value.is_finite()) || !output.iter().any(|value| *value != 0.) {
        bail!("invalid zero or non-finite embedding norm")
    }
    Ok(output)
}

fn cached_mask(cache: &mut Option<Tensor>, device: &Device, seq: usize) -> Result<Option<Tensor>> {
    if seq <= 513 {
        // A short query also releases a previously retained long-document mask.
        *cache = None;
    } else if cache
        .as_ref()
        .is_none_or(|mask| mask.dims() != [1, seq, seq])
    {
        // Retain only one exact-size mask, reusing it for equal-length batches.
        *cache = None;
        *cache = Some(precompute_mask(device, seq)?);
    }
    Ok(cache.clone())
}

fn precompute_mask(device: &Device, max_len: usize) -> Result<Tensor> {
    let mut mask = vec![0f32; max_len * max_len];
    for qi in 0..max_len {
        for ki in 0..max_len {
            if qi.abs_diff(ki) > 512 {
                mask[qi * max_len + ki] = f32::NEG_INFINITY;
            }
        }
    }
    let mask = Tensor::from_vec(mask, (max_len, max_len), device)?.unsqueeze(0)?;
    Ok(mask)
}

/// MPS is a CLI alias for direct Metal execution; no PyTorch/MPS process is used.
fn select_device(name: &str) -> Result<Device> {
    if name == "cpu" {
        return Ok(Device::Cpu);
    }
    if name == "auto" {
        #[cfg(feature = "cuda")]
        if let Ok(device) = Device::new_cuda(0) {
            return Ok(device);
        }
        #[cfg(feature = "metal")]
        if let Ok(device) = Device::new_metal(0) {
            return Ok(device);
        }
        return Ok(Device::Cpu);
    }
    let (backend, ordinal) = name.split_once(':').unwrap_or((name, "0"));
    let ordinal: usize = ordinal
        .parse()
        .context("device index must be a non-negative integer")?;
    match backend {
        "cuda" => {
            #[cfg(feature = "cuda")]
            {
                return Device::new_cuda(ordinal).context("initialize CUDA device");
            }
            #[cfg(not(feature = "cuda"))]
            {
                let _ = ordinal;
                bail!("CUDA is not compiled in; rebuild with --features cuda");
            }
        }
        "metal" | "mps" => {
            #[cfg(feature = "metal")]
            {
                return Device::new_metal(ordinal).context("initialize Apple Metal device");
            }
            #[cfg(not(feature = "metal"))]
            {
                let _ = ordinal;
                bail!("Metal is not compiled in; rebuild on macOS with --features metal");
            }
        }
        _ => bail!("device must be auto, cpu, cuda[:N], metal[:N], or mps[:N]"),
    }
}

fn gelu_tanh(x: &Tensor) -> Result<Tensor> {
    Ok(x.gelu()?)
}

impl NativeTokenizer {
    #[cfg(test)]
    pub fn from_gguf(path: &Path) -> Result<Self> {
        let mut file =
            File::open(path).with_context(|| format!("open model {}", path.display()))?;
        let content = gguf_file::Content::read(&mut file).context("read GGUF metadata")?;
        Self::from_content(&content)
    }

    fn from_content(content: &gguf_file::Content) -> Result<Self> {
        Self::from_metadata(content.metadata.clone())
    }

    fn from_metadata(mut metadata: HashMap<String, gguf_file::Value>) -> Result<Self> {
        let profiling = std::env::var_os("WEFT_PROFILE_LOAD").is_some();
        let started = Instant::now();
        // The metadata is exclusively owned by this worker. Moving vocab and
        // merges avoids cloning ~100K strings before building the BPE tables.
        let tokens = take_string_array(&mut metadata, "tokenizer.ggml.tokens")?;
        let merges = take_string_array(&mut metadata, "tokenizer.ggml.merges")?;
        let extract_ms = started.elapsed().as_secs_f64() * 1e3;
        let vocab = tokens
            .into_iter()
            .enumerate()
            .map(|(id, token)| (token, id as u32))
            .collect::<ahash::AHashMap<_, _>>();
        let merges = merges
            .into_iter()
            .map(|merge| {
                let (left, right) = merge
                    .split_once(' ')
                    .with_context(|| format!("invalid tokenizer merge {merge:?}"))?;
                Ok((left.to_owned(), right.to_owned()))
            })
            .collect::<Result<Vec<_>>>()?;
        let prepare_ms = started.elapsed().as_secs_f64() * 1e3 - extract_ms;
        let bpe = BPE::builder()
            .vocab_and_merges(vocab, merges)
            .unk_token("<unk>".to_owned())
            .byte_fallback(true)
            .build()
            .map_err(|e| anyhow::anyhow!("build GGUF BPE tokenizer: {e}"))?;
        let build_ms = started.elapsed().as_secs_f64() * 1e3 - extract_ms - prepare_ms;
        let mut tokenizer = HfTokenizer::new(bpe);
        // The official model's added tokens must bypass ordinary BPE splitting.
        // Their IDs already exist in the GGUF vocabulary, so no extra file is needed.
        tokenizer.add_special_tokens(
            &[
                "<pad>",
                "<eos>",
                "<bos>",
                "<unk>",
                "<mask>",
                "<|tool>",
                "<tool|>",
                "<|tool_call>",
                "<tool_call|>",
                "<|tool_response>",
                "<tool_response|>",
                "<|\"|>",
                "<|think|>",
                "<|channel>",
                "<channel|>",
                "<|turn>",
                "<turn|>",
                "<|image>",
                "<|audio>",
                "<|image|>",
                "<|audio|>",
                "<image|>",
                "<audio|>",
                "<|video|>",
            ]
            .map(|token| AddedToken::from(token, true).normalized(false)),
        );

        tokenizer.with_normalizer(Some(
            Replace::new(" ", "▁").map_err(|e| anyhow::anyhow!("create GGUF normalizer: {e}"))?,
        ));
        tokenizer.with_pre_tokenizer(Some(
            Split::new(" ", SplitDelimiterBehavior::MergedWithPrevious, false)
                .map_err(|e| anyhow::anyhow!("create GGUF pre-tokenizer: {e}"))?,
        ));
        if profiling {
            eprintln!(
                "tokenizer build ms: extract={extract_ms:.2} prepare={prepare_ms:.2} bpe={build_ms:.2} specials={:.2} total={:.2}",
                started.elapsed().as_secs_f64() * 1e3 - extract_ms - prepare_ms - build_ms,
                started.elapsed().as_secs_f64() * 1e3
            );
        }
        Ok(Self { tokenizer })
    }

    /// Apply the model's required BOS/EOS framing and return token IDs.
    pub fn encode(&self, text: &str) -> Result<Vec<u32>> {
        let ids = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow::anyhow!("tokenize text: {e}"))?;
        let mut output = Vec::with_capacity(ids.len() + 2);
        output.push(2); // tokenizer.ggml.bos_token_id
        output.extend_from_slice(ids.get_ids());
        output.push(1); // tokenizer.ggml.eos_token_id
        Ok(output)
    }
}

fn take_string_array(
    metadata: &mut HashMap<String, gguf_file::Value>,
    key: &str,
) -> Result<Vec<String>> {
    let value = metadata
        .remove(key)
        .with_context(|| format!("GGUF is missing {key}"))?;
    let gguf_file::Value::Array(values) = value else {
        bail!("GGUF metadata {key} is not an array")
    };
    values
        .into_iter()
        .map(|value| match value {
            gguf_file::Value::String(text) => Ok(text),
            other => bail!("GGUF metadata {key} contains non-string {other:?}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_mask_exact_size_cache_and_parity() -> Result<()> {
        let baseline = precompute_mask(&Device::Cpu, MAX_LEN)?;
        let mut cache = None;
        for seq in [514, 600, 600, 1024, 514, 513, 128] {
            let actual = cached_mask(&mut cache, &Device::Cpu, seq)?;
            if seq <= 513 {
                assert!(actual.is_none());
                assert!(cache.is_none(), "short queries release the long mask");
                let original = baseline.narrow(1, 0, seq)?.narrow(2, 0, seq)?;
                assert!(
                    original
                        .flatten_all()?
                        .to_vec1::<f32>()?
                        .iter()
                        .all(|v| *v == 0.)
                );
            } else {
                let actual = actual.unwrap();
                assert_eq!(actual.dims(), [1, seq, seq]);
                assert!(actual.is_contiguous());
                let original = baseline.narrow(1, 0, seq)?.narrow(2, 0, seq)?;
                assert_eq!(
                    actual.flatten_all()?.to_vec1::<f32>()?,
                    original.flatten_all()?.to_vec1::<f32>()?
                );
                let reused = cached_mask(&mut cache, &Device::Cpu, seq)?.unwrap();
                assert_eq!(actual.id(), reused.id(), "equal lengths reuse the mask");
            }
        }
        Ok(())
    }

    #[test]
    fn device_normalization_matches_host_scalar_and_rejects_invalid() -> Result<()> {
        for values in [
            vec![3f32, 4., -2.],
            vec![0.001, -0.002, 0.003],
            vec![0., 1., 0.],
        ] {
            let pooled = Tensor::from_vec(values.clone(), values.len(), &Device::Cpu)?;
            let norm = pooled.sqr()?.sum_all()?.sqrt()?.to_scalar::<f32>()?;
            let previous = pooled.affine(1. / norm as f64, 0.)?.to_vec1::<f32>()?;
            let actual = normalize_embedding(&pooled)?;
            for (a, b) in actual.iter().zip(previous) {
                assert!((a - b).abs() < 1e-7);
            }
            let norm = actual
                .iter()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>()
                .sqrt();
            assert!((norm - 1.).abs() < 1e-6);
        }
        for values in [
            vec![0f32, 0.],
            vec![f32::NAN, 1.],
            vec![f32::INFINITY, 1.],
            vec![f32::MAX, 1.],
        ] {
            let pooled = Tensor::from_vec(values, 2, &Device::Cpu)?;
            assert_eq!(
                normalize_embedding(&pooled).unwrap_err().to_string(),
                "invalid zero or non-finite embedding norm"
            );
        }
        Ok(())
    }

    #[cfg(feature = "metal")]
    #[test]
    #[ignore = "requires a Metal GPU; checks device normalization parity"]
    fn device_normalization_cpu_metal_parity() -> Result<()> {
        let metal = Device::new_metal(0)?;
        let values = (0..768)
            .map(|i| ((i as f32) - 384.) / 768.)
            .collect::<Vec<_>>();
        let cpu = normalize_embedding(&Tensor::from_vec(values.clone(), 768, &Device::Cpu)?)?;
        let gpu = normalize_embedding(&Tensor::from_vec(values, 768, &metal)?)?;
        for (a, b) in cpu.iter().zip(gpu) {
            assert!((a - b).abs() < 1e-6);
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires local GGUF; verifies prepared layout and tokenizer parity"]
    fn prepared_layout_roundtrip() -> Result<()> {
        let path = std::env::var("WEFT_TEST_MODEL")?;
        let tokenizer = NativeTokenizer::from_gguf(Path::new(&path))?;
        let model_path = Path::new(&path);
        let file = File::open(model_path)?;
        let mapped = unsafe { memmap2::Mmap::map(&file)? };
        let prepared = crate::prepared::load(model_path, &mapped)?
            .context("run weft prepare before this test")?;
        let loaded = NativeTokenizer::from_content(&prepared)?;
        for sample in [
            "task: search result | query: GPU 최적화 / Transformer 2026",
            "title: none | text: 원래 임베딩과 동일한가요?",
            "<bos> test",
            "A long text with spaces and unicode 한글 ♥",
        ] {
            assert_eq!(loaded.encode(sample)?, tokenizer.encode(sample)?);
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires prepared local GGUF; compares native embeddings with and without layout cache"]
    fn prepared_embedding_parity() -> Result<()> {
        let model_path = std::env::var("WEFT_TEST_MODEL")?;
        let device = std::env::var("WEFT_DEVICE").unwrap_or_else(|_| "cpu".to_owned());
        let path = Path::new(&model_path);
        let samples = [
            ("Transformer GPU 최적화와 CUDA attention kernel", "query"),
            ("분산 데이터베이스 트랜잭션과 복제", "query"),
            (
                "Magic school curriculum and spell design: 마법 교육 체계",
                "document",
            ),
            ("Character personality and behavior 설계", "document"),
            // >513 tokens exercises the long RoPE table and sliding attention mask.
            (
                &"database transactions replication recovery ".repeat(125),
                "document",
            ),
        ];
        let baseline = {
            let model = NativeModel::load_with_prepared(path, &device, false)?;
            samples
                .iter()
                .map(|(text, role)| model.embed(text, role))
                .collect::<Result<Vec<_>>>()?
        };
        let prepared = NativeModel::load_with_prepared(path, &device, true)?;
        for ((text, role), original) in samples.iter().zip(baseline.iter()) {
            let actual = prepared.embed(text, role)?;
            assert_eq!(actual.len(), original.len());
            let cosine: f64 = actual
                .iter()
                .zip(original)
                .map(|(a, b)| (*a as f64) * (*b as f64))
                .sum();
            let max_diff = actual
                .iter()
                .zip(original)
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            eprintln!(
                "parity {role}: {} chars, cosine={cosine:.9}, max_diff={max_diff:.8}",
                text.len()
            );
            assert!(
                cosine > 0.99999,
                "prepared layout changed semantic embedding"
            );
            assert!(
                max_diff < 1e-3,
                "prepared layout changed embedding components"
            );
        }
        Ok(())
    }

    #[test]
    fn device_selection_is_explicit() {
        assert!(matches!(select_device("cpu").unwrap(), Device::Cpu));
        assert!(select_device("unknown").is_err());
        assert!(select_device("cuda:bad").is_err());
        #[cfg(not(feature = "cuda"))]
        assert!(select_device("cuda").is_err());
        #[cfg(not(feature = "metal"))]
        assert!(select_device("mps").is_err());
    }

    #[test]
    #[ignore = "requires a local GGUF and running reference server"]
    fn native_reference_suite() -> Result<()> {
        let path =
            std::env::var("WEFT_TEST_MODEL").or_else(|_| std::env::var("MYVEC_TEST_MODEL"))?;
        let model = NativeModel::load(
            Path::new(&path),
            &std::env::var("WEFT_DEVICE")
                .or_else(|_| std::env::var("MYVEC_DEVICE"))
                .unwrap_or_else(|_| "auto".into()),
        )?;
        let cases = [
            ("where is retry configured?".to_string(), "query"),
            (
                "실패한 작업 재시도 간격을 어디에서 설정하나요?".to_string(),
                "query",
            ),
            (
                "Database migration: take a backup, deploy schema, verify rollback.".to_string(),
                "document",
            ),
            (
                "오사카 여행 숙소와 식비 예산을 정리한 문서입니다.".to_string(),
                "document",
            ),
            (
                (0..110)
                    .map(|i| format!("Section {i}: database backup and retry policy. "))
                    .collect::<String>(),
                "document",
            ),
        ];
        for (text, role) in cases {
            let prompt = if role == "query" {
                format!("task: search result | query: {text}")
            } else {
                format!("title: none | text: {text}")
            };
            let response: serde_json::Value = ureq::post("http://127.0.0.1:8080/v1/embeddings")
                .send_json(serde_json::json!({"input": [prompt], "model": "embeddinggemma-2"}))?
                .into_json()?;
            let reference = response["data"][0]["embedding"]
                .as_array()
                .context("missing reference")?;
            let timer = std::time::Instant::now();
            let output = model.embed(&text, role)?;
            let dot: f64 = output
                .iter()
                .zip(reference)
                .map(|(a, b)| *a as f64 * b.as_f64().unwrap())
                .sum();
            let norm: f64 = output
                .iter()
                .map(|x| (*x as f64).powi(2))
                .sum::<f64>()
                .sqrt();
            eprintln!(
                "role={role} tokens={} cosine={dot:.8} norm={norm:.8} forward_ms={:.1}",
                model.tokenizer.encode(&text)?.len(),
                timer.elapsed().as_secs_f64() * 1000.
            );
            assert_eq!(output.len(), reference.len());
            assert!((norm - 1.).abs() < 1e-5);
            assert!(dot > 0.9999, "cosine {dot}");
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires local GGUF; optional official tokenizer/reference embedding"]
    fn embedded_gguf_tokenizer_handles_search_text() -> Result<()> {
        let path =
            std::env::var("WEFT_TEST_MODEL").or_else(|_| std::env::var("MYVEC_TEST_MODEL"))?;
        assert_eq!(check_q8_model(Path::new(&path))?, 413);
        let tokenizer = NativeTokenizer::from_gguf(Path::new(&path))?;
        for text in [
            "task: search result | query: where is retry configured?",
            "title: none | text: 한국어 문서 검색 테스트",
            "Rust/C++ paths: src/search.rs and config.toml",
            "literal <bos> tag, <|image|>, tabs\t and newlines\n한국어 😀",
        ] {
            let ids = tokenizer.encode(text)?;
            assert!(ids.len() > 2);
            assert_eq!(ids.first(), Some(&2));
            assert_eq!(ids.last(), Some(&1));
        }
        if let Ok(reference_path) =
            std::env::var("WEFT_TEST_TOKENIZER").or_else(|_| std::env::var("MYVEC_TEST_TOKENIZER"))
        {
            let reference = HfTokenizer::from_file(reference_path)
                .map_err(|e| anyhow::anyhow!("load reference tokenizer: {e}"))?;
            for text in [
                "task: search result | query: where is retry configured?",
                "title: none | text: 한국어 문서 검색 테스트",
                "Rust/C++ paths: src/search.rs and config.toml",
                "literal <bos> tag, <|image|>, tabs\t and newlines\n한국어 😀",
            ] {
                let expected = reference
                    .encode(text, true)
                    .map_err(|e| anyhow::anyhow!("reference tokenize: {e}"))?;
                assert_eq!(tokenizer.encode(text)?, expected.get_ids(), "{text}");
            }
        }
        if let Ok(reference_path) = std::env::var("WEFT_REFERENCE_EMBEDDING")
            .or_else(|_| std::env::var("MYVEC_REFERENCE_EMBEDDING"))
        {
            let model = NativeModel::load(
                Path::new(&path),
                &std::env::var("WEFT_DEVICE")
                    .or_else(|_| std::env::var("MYVEC_DEVICE"))
                    .unwrap_or_else(|_| "auto".into()),
            )?;
            let output = model.embed("where is retry configured?", "query")?;
            if let Ok(output_path) = std::env::var("WEFT_NATIVE_OUTPUT")
                .or_else(|_| std::env::var("MYVEC_NATIVE_OUTPUT"))
            {
                std::fs::write(output_path, serde_json::to_vec(&output)?)?;
            }
            let reference: serde_json::Value =
                serde_json::from_slice(&std::fs::read(reference_path)?)?;
            let reference = reference["data"][0]["embedding"]
                .as_array()
                .context("reference embedding missing")?
                .iter()
                .map(|v| v.as_f64().unwrap() as f32)
                .collect::<Vec<_>>();
            assert_eq!(output.len(), 768);
            let dot: f32 = output.iter().zip(&reference).map(|(a, b)| a * b).sum();
            eprintln!("native vs llama.cpp cosine: {dot:.6}");
            assert!(dot > 0.99, "native/reference cosine too low: {dot}");
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub fn check_q8_model(path: &Path) -> Result<usize> {
    let mut file = File::open(path).with_context(|| format!("open model {}", path.display()))?;
    let content = gguf_file::Content::read(&mut file).context("read GGUF model")?;
    if !matches!(content.metadata.get("general.architecture"), Some(gguf_file::Value::String(arch)) if arch == "gemma-embedding2")
    {
        bail!("model is not EmbeddingGemma 2 GGUF")
    }
    // Reading one representative quantized matrix verifies that Candle can
    // consume the model directly, without a llama.cpp process or conversion.
    let info = content
        .tensor_infos
        .get("blk.0.attn_q.weight")
        .context("GGUF is missing the first attention matrix")?;
    let _matrix = info.read(&mut file, content.tensor_data_offset, &Device::Cpu)?;
    Ok(content.tensor_infos.len())
}
