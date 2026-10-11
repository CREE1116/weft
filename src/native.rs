//! Building blocks for the in-process EmbeddingGemma 2 runtime.
//!
//! The tokenizer is reconstructed from GGUF metadata so the runtime does not
//! need to keep Hugging Face's much larger `tokenizer.json` beside the model.

use anyhow::{Context, Result, bail};
use candle_core::{
    D, Device, Module, Tensor,
    quantized::{QMatMul, gguf_file},
};
use std::{collections::HashMap, fs::File, path::Path};
use tokenizers::{
    AddedToken, SplitDelimiterBehavior, Tokenizer as HfTokenizer, models::bpe::BPE,
    normalizers::Replace, pre_tokenizers::split::Split,
};

pub struct NativeTokenizer {
    tokenizer: HfTokenizer,
}

pub struct NativeModel {
    device: Device,
    matrices: HashMap<String, QMatMul>,
    vectors: HashMap<String, Tensor>,
    tokenizer: NativeTokenizer,
}

impl NativeModel {
    pub fn load(path: &Path, device_name: &str) -> Result<Self> {
        let device = select_device(device_name)?;
        let mut file =
            File::open(path).with_context(|| format!("open model {}", path.display()))?;
        let content = gguf_file::Content::read(&mut file).context("read GGUF model")?;
        if !matches!(content.metadata.get("general.architecture"), Some(gguf_file::Value::String(arch)) if arch == "gemma-embedding2")
        {
            bail!("model is not EmbeddingGemma 2 GGUF")
        }
        let tokenizer = NativeTokenizer::from_content(&content)?;
        let data_offset = content.tensor_data_offset;
        let tensor_infos = content.tensor_infos;
        drop(content.metadata); // release vocabulary metadata before loading weights
        let mut matrices = HashMap::new();
        let mut vectors = HashMap::new();
        for (name, info) in &tensor_infos {
            let tensor = info
                .read(&mut file, data_offset, &device)
                .with_context(|| format!("load model tensor {name}"))?;
            if info.shape.rank() == 1 {
                vectors.insert(name.clone(), tensor.dequantize(&device)?);
            } else {
                matrices.insert(name.clone(), QMatMul::from_qtensor(tensor)?);
            }
        }
        eprintln!(
            "Loaded EmbeddingGemma 2 on {:?} ({} tensors)",
            device.location(),
            matrices.len() + vectors.len()
        );
        Ok(Self {
            device,
            matrices,
            vectors,
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
        if ids.len() > 2048 {
            bail!(
                "text tokenizes to {} tokens; maximum supported is 2046",
                ids.len() - 2
            )
        }
        let seq = ids.len();
        let token_ids = Tensor::from_vec(ids, (seq,), &self.device)?;
        let mut x = self
            .matrix("token_embd.weight")?
            .embedding(&token_ids)?
            .affine(512f64.sqrt(), 0.)?;
        let per_layer = self
            .linear("per_layer_model_proj.weight", &x)?
            .affine(512f64.sqrt().recip(), 0.)?
            .reshape((seq, 24, 512))?;
        let per_layer = self.rms(&per_layer, "per_layer_proj_norm.weight")?;
        let local_rope = self.rope_tables(256, seq, 10_000.)?;
        let full_rope = self.rope_tables(512, seq, 1_000_000.)?;
        let mut mask = vec![0f32; seq * seq];
        for qi in 0..seq {
            for ki in 0..seq {
                if qi.abs_diff(ki) > 512 {
                    mask[qi * seq + ki] = f32::NEG_INFINITY;
                }
            }
        }
        let mask = Tensor::from_vec(mask, (seq, seq), &self.device)?.unsqueeze(0)?;
        for layer in 0..24 {
            x = self.layer(
                layer,
                &x,
                &per_layer.narrow(1, layer, 1)?.squeeze(1)?,
                seq,
                if layer % 6 == 5 {
                    &full_rope
                } else {
                    &local_rope
                },
                &mask,
            )?;
        }
        x = self.rms(&x, "output_norm.weight")?;
        // Linear projection commutes with mean pooling: project once, not per token.
        let pooled = self
            .linear("output.weight", &x.mean_keepdim(0)?)?
            .squeeze(0)?
            .contiguous()?;
        let norm = pooled.sqr()?.sum_all()?.sqrt()?.to_scalar::<f32>()?;
        if norm == 0. || !norm.is_finite() {
            bail!("invalid zero or non-finite embedding norm")
        }
        Ok(pooled.affine(1. / norm as f64, 0.)?.to_vec1::<f32>()?)
    }

    fn layer(
        &self,
        i: usize,
        x: &Tensor,
        ple: &Tensor,
        seq: usize,
        rope: &(Tensor, Tensor),
        mask: &Tensor,
    ) -> Result<Tensor> {
        let p = format!("blk.{i}.");
        let mut residual = x.clone();
        let h = self.rms(x, &format!("{p}attn_norm.weight"))?;
        let qdim = if i % 6 == 5 { 512 } else { 256 };
        let kvheads = if qdim == 512 { 1 } else { 2 };
        let q = self
            .linear(&format!("{p}attn_q.weight"), &h)?
            .reshape((seq, 4, qdim))?
            .transpose(0, 1)?;
        let k = self
            .linear(&format!("{p}attn_k.weight"), &h)?
            .reshape((seq, kvheads, qdim))?
            .transpose(0, 1)?;
        let v = self
            .linear(&format!("{p}attn_v.weight"), &h)?
            .reshape((seq, kvheads, qdim))?
            .transpose(0, 1)?;
        let q = self.rms(&q, &format!("{p}attn_q_norm.weight"))?;
        let k = self.rms(&k, &format!("{p}attn_k_norm.weight"))?;
        let v = self.rms(&v, "")?;
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
            scores = scores.broadcast_add(mask)?;
        }
        let attended = candle_nn::ops::softmax_last_dim(&scores.contiguous()?)?
            .matmul(&v)?
            .transpose(0, 1)?
            .contiguous()?
            .reshape((seq, 4 * qdim))?;
        let h = self.linear(&format!("{p}attn_output.weight"), &attended)?;
        residual = residual.add(&self.rms(&h, &format!("{p}post_attention_norm.weight"))?)?;
        let h = self.rms(&residual, &format!("{p}ffn_norm.weight"))?;
        let gate = gelu_tanh(&self.linear(&format!("{p}ffn_gate.weight"), &h)?)?;
        let up = self.linear(&format!("{p}ffn_up.weight"), &h)?;
        let h = self.linear(&format!("{p}ffn_down.weight"), &gate.mul(&up)?)?;
        residual = residual.add(&self.rms(&h, &format!("{p}post_ffw_norm.weight"))?)?;
        let gate = gelu_tanh(&self.linear(&format!("{p}inp_gate.weight"), &residual)?)?;
        let h = self.linear(&format!("{p}proj.weight"), &gate.mul(ple)?)?;
        let h = self.rms(&h, &format!("{p}post_norm.weight"))?;
        Ok(residual
            .add(&h)?
            .broadcast_mul(self.vector(&format!("{p}layer_output_scale.weight"))?)?)
    }

    fn rope_tables(&self, dim: usize, seq: usize, theta: f64) -> Result<(Tensor, Tensor)> {
        let mut cos = Vec::with_capacity(seq * dim);
        let mut sin = Vec::with_capacity(seq * dim);
        for pos in 0..seq {
            let mut row_cos = Vec::with_capacity(dim / 2);
            let mut row_sin = Vec::with_capacity(dim / 2);
            for j in 0..dim / 2 {
                let angle = pos as f64 / theta.powf((2 * j) as f64 / dim as f64);
                row_cos.push(angle.cos() as f32);
                row_sin.push(angle.sin() as f32);
            }
            cos.extend_from_slice(&row_cos);
            cos.extend_from_slice(&row_cos);
            sin.extend_from_slice(&row_sin);
            sin.extend_from_slice(&row_sin);
        }
        let cos = Tensor::from_vec(cos, (seq, dim), &self.device)?.unsqueeze(0)?;
        let sin = Tensor::from_vec(sin, (seq, dim), &self.device)?.unsqueeze(0)?;
        Ok((cos, sin))
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

    fn rms(&self, x: &Tensor, name: &str) -> Result<Tensor> {
        let x = x.contiguous()?;
        if !name.is_empty() {
            return Ok(candle_nn::ops::rms_norm(&x, self.vector(name)?, 1.0e-6)?);
        }
        let inv = (x.sqr()?.mean_keepdim(D::Minus1)? + 1.0e-6)?
            .sqrt()?
            .recip()?;
        Ok(x.broadcast_mul(&inv)?)
    }

    fn linear(&self, name: &str, x: &Tensor) -> Result<Tensor> {
        let matrix = self.matrix(name)?;
        // CPU quantized matmul would also quantize activations. Expand just the
        // current matrix to preserve FP32 activations, releasing it after use.
        // Keep the large token embedding table quantized, with direct row lookup.
        if matches!(self.device, Device::Cpu) {
            if let QMatMul::QTensor(weights) = matrix {
                return Ok(x.matmul(&weights.dequantize(&self.device)?.t()?)?);
            }
        }
        matrix.forward(x).with_context(|| format!("matmul {name}"))
    }
    fn matrix(&self, name: &str) -> Result<&QMatMul> {
        self.matrices
            .get(name)
            .with_context(|| format!("missing matrix {name}"))
    }
    fn vector(&self, name: &str) -> Result<&Tensor> {
        self.vectors
            .get(name)
            .with_context(|| format!("missing vector {name}"))
    }
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
        let tokens = string_array(content, "tokenizer.ggml.tokens")?;
        let merges = string_array(content, "tokenizer.ggml.merges")?;
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
        let bpe = BPE::builder()
            .vocab_and_merges(vocab, merges)
            .unk_token("<unk>".to_owned())
            .byte_fallback(true)
            .build()
            .map_err(|e| anyhow::anyhow!("build GGUF BPE tokenizer: {e}"))?;
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

fn string_array(content: &gguf_file::Content, key: &str) -> Result<Vec<String>> {
    let value = content
        .metadata
        .get(key)
        .with_context(|| format!("GGUF is missing {key}"))?;
    let gguf_file::Value::Array(values) = value else {
        bail!("GGUF metadata {key} is not an array")
    };
    values
        .iter()
        .map(|value| match value {
            gguf_file::Value::String(text) => Ok(text.clone()),
            other => bail!("GGUF metadata {key} contains non-string {other:?}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let path = std::env::var("MYVEC_TEST_MODEL")?;
        let model = NativeModel::load(
            Path::new(&path),
            &std::env::var("MYVEC_DEVICE").unwrap_or_else(|_| "auto".into()),
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
        let path = std::env::var("MYVEC_TEST_MODEL")?;
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
        if let Ok(reference_path) = std::env::var("MYVEC_TEST_TOKENIZER") {
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
        if let Ok(reference_path) = std::env::var("MYVEC_REFERENCE_EMBEDDING") {
            let model = NativeModel::load(
                Path::new(&path),
                &std::env::var("MYVEC_DEVICE").unwrap_or_else(|_| "auto".into()),
            )?;
            let output = model.embed("where is retry configured?", "query")?;
            if let Ok(output_path) = std::env::var("MYVEC_NATIVE_OUTPUT") {
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
