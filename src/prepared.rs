//! Optional offline layout cache for the fixed EmbeddingGemma 2 GGUF model.
//! The cache duplicates only tokenizer metadata and tensor descriptors, never weights.
//! Its identity check avoids hashing 296 MiB on every cold query.

use anyhow::{Context, Result, bail};
use candle_core::{
    Shape,
    quantized::{GgmlDType, gguf_file},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const VERSION: u32 = 2;
const MAX_CACHE: u64 = 48 * 1024 * 1024;
const SAMPLE: usize = 64 * 1024;
const CHECKSUM_LEN: usize = 32;

#[derive(Serialize, Deserialize)]
struct Layout {
    version: u32,
    source_size: u64,
    source_modified_ns: u128,
    source_fingerprint: [u8; 32],
    data_offset: u64,
    tensors: Vec<TensorDesc>,
    tokens: Vec<String>,
    merges: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct TensorDesc {
    name: String,
    dtype: u8,
    dims: Vec<usize>,
    offset: u64,
}

fn dtype_code(dtype: GgmlDType) -> u8 {
    match dtype {
        GgmlDType::F32 => 0,
        GgmlDType::F16 => 1,
        GgmlDType::BF16 => 30,
        GgmlDType::Q4_0 => 2,
        GgmlDType::Q4_1 => 3,
        GgmlDType::Q5_0 => 6,
        GgmlDType::Q5_1 => 7,
        GgmlDType::Q8_0 => 8,
        GgmlDType::Q8_1 => 9,
        GgmlDType::Q2K => 10,
        GgmlDType::Q3K => 11,
        GgmlDType::Q4K => 12,
        GgmlDType::Q5K => 13,
        GgmlDType::Q6K => 14,
        GgmlDType::Q8K => 15,
    }
}

fn decode_dtype(code: u8) -> Result<GgmlDType> {
    Ok(match code {
        0 => GgmlDType::F32,
        1 => GgmlDType::F16,
        30 => GgmlDType::BF16,
        2 => GgmlDType::Q4_0,
        3 => GgmlDType::Q4_1,
        6 => GgmlDType::Q5_0,
        7 => GgmlDType::Q5_1,
        8 => GgmlDType::Q8_0,
        9 => GgmlDType::Q8_1,
        10 => GgmlDType::Q2K,
        11 => GgmlDType::Q3K,
        12 => GgmlDType::Q4K,
        13 => GgmlDType::Q5K,
        14 => GgmlDType::Q6K,
        15 => GgmlDType::Q8K,
        _ => bail!("unknown prepared tensor dtype {code}"),
    })
}

pub fn cache_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".weft-layout");
    PathBuf::from(name)
}

fn strings(metadata: &HashMap<String, gguf_file::Value>, key: &str) -> Result<Vec<String>> {
    let Some(gguf_file::Value::Array(values)) = metadata.get(key) else {
        bail!("missing GGUF tokenizer array {key}")
    };
    values
        .iter()
        .map(|v| match v {
            gguf_file::Value::String(s) => Ok(s.clone()),
            _ => bail!("invalid GGUF tokenizer array {key}"),
        })
        .collect()
}

fn source_id(path: &Path, mapped: &[u8]) -> Result<(u64, u128, [u8; 32])> {
    let metadata = fs::metadata(path)?;
    let size = metadata.len();
    if size != mapped.len() as u64 {
        bail!("model size changed during preparation");
    }
    let modified_ns = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&size.to_le_bytes());
    let count = mapped.len().min(SAMPLE);
    hasher.update(&mapped[..count]);
    hasher.update(&mapped[mapped.len() - count..]);
    Ok((size, modified_ns, *hasher.finalize().as_bytes()))
}

pub fn prepare(path: &Path) -> Result<(PathBuf, u64)> {
    let file = File::open(path)?;
    let mapped = unsafe { memmap2::Mmap::map(&file)? };
    let mut cursor = std::io::Cursor::new(&mapped[..]);
    let content = gguf_file::Content::read(&mut cursor).context("parse source GGUF")?;
    if !matches!(content.metadata.get("general.architecture"), Some(gguf_file::Value::String(arch)) if arch == "gemma-embedding2")
    {
        bail!("source is not EmbeddingGemma 2 GGUF")
    }
    let (source_size, source_modified_ns, source_fingerprint) = source_id(path, &mapped)?;
    let layout = Layout {
        version: VERSION,
        source_size,
        source_modified_ns,
        source_fingerprint,
        data_offset: content.tensor_data_offset,
        tensors: content
            .tensor_infos
            .iter()
            .map(|(name, info)| TensorDesc {
                name: name.clone(),
                dtype: dtype_code(info.ggml_dtype),
                dims: info.shape.dims().to_vec(),
                offset: info.offset,
            })
            .collect(),
        tokens: strings(&content.metadata, "tokenizer.ggml.tokens")?,
        merges: strings(&content.metadata, "tokenizer.ggml.merges")?,
    };
    let mut bytes = rmp_serde::to_vec_named(&layout).context("encode prepared layout")?;
    if bytes.len() as u64 + CHECKSUM_LEN as u64 > MAX_CACHE {
        bail!("prepared layout exceeds size cap");
    }
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    let dest = cache_path(path);
    // Use create_new to avoid colliding with another process preparing the model.
    let (tmp_path, mut output) = (0..10)
        .find_map(|i| {
            let mut temp = dest.as_os_str().to_os_string();
            temp.push(format!(".tmp-{}-{i}", std::process::id()));
            let temp = PathBuf::from(temp);
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .ok()
                .map(|f| (temp, f))
        })
        .context("create unique prepared model temporary file")?;
    let result = (|| -> Result<()> {
        output.write_all(&bytes)?;
        output.sync_all()?;
        fs::rename(&tmp_path, &dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result?;
    Ok((dest, bytes.len() as u64))
}

/// Invalid, stale or corrupt caches are ignored; the canonical GGUF remains the source of truth.
pub fn load(path: &Path, mapped: &[u8]) -> Result<Option<gguf_file::Content>> {
    let cache = cache_path(path);
    let Ok(meta) = fs::metadata(&cache) else {
        return Ok(None);
    };
    if meta.len() > MAX_CACHE || meta.len() < CHECKSUM_LEN as u64 {
        return Ok(None);
    }
    let data = match fs::read(&cache) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let Some(body_len) = data.len().checked_sub(CHECKSUM_LEN) else {
        return Ok(None);
    };
    let (body, checksum) = data.split_at(body_len);
    if blake3::hash(body).as_bytes() != checksum {
        return Ok(None);
    }
    let layout: Layout = match rmp_serde::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Ok((size, modified, fingerprint)) = source_id(path, mapped) else {
        return Ok(None);
    };
    if layout.version != VERSION
        || layout.source_size != size
        || layout.source_modified_ns != modified
        || layout.source_fingerprint != fingerprint
        || layout.tensors.len() != 413
        || layout.tokens.is_empty()
        || layout.merges.is_empty()
    {
        return Ok(None);
    }
    if layout.data_offset >= size {
        return Ok(None);
    }
    let mut tensor_infos = HashMap::with_capacity(layout.tensors.len());
    for t in layout.tensors {
        if t.name.is_empty() || t.dims.is_empty() || t.dims.len() > 4 || t.dims.contains(&0) {
            return Ok(None);
        }
        let dtype = match decode_dtype(t.dtype) {
            Ok(dtype) => dtype,
            Err(_) => return Ok(None),
        };
        let Some(elements) = t
            .dims
            .iter()
            .try_fold(1usize, |acc, dim| acc.checked_mul(*dim))
        else {
            return Ok(None);
        };
        if !elements.is_multiple_of(dtype.block_size()) {
            return Ok(None);
        }
        let Some(nbytes) = (elements / dtype.block_size()).checked_mul(dtype.type_size()) else {
            return Ok(None);
        };
        let Some(end) = layout
            .data_offset
            .checked_add(t.offset)
            .and_then(|start| start.checked_add(nbytes as u64))
        else {
            return Ok(None);
        };
        if end > size {
            return Ok(None);
        }
        let info = gguf_file::TensorInfo {
            ggml_dtype: dtype,
            shape: Shape::from(t.dims),
            offset: t.offset,
        };
        if tensor_infos.insert(t.name, info).is_some() {
            return Ok(None);
        }
    }
    let mut metadata = HashMap::with_capacity(3);
    metadata.insert(
        "general.architecture".into(),
        gguf_file::Value::String("gemma-embedding2".into()),
    );
    metadata.insert(
        "tokenizer.ggml.tokens".into(),
        gguf_file::Value::Array(
            layout
                .tokens
                .into_iter()
                .map(gguf_file::Value::String)
                .collect(),
        ),
    );
    metadata.insert(
        "tokenizer.ggml.merges".into(),
        gguf_file::Value::Array(
            layout
                .merges
                .into_iter()
                .map(gguf_file::Value::String)
                .collect(),
        ),
    );
    Ok(Some(gguf_file::Content {
        magic: gguf_file::VersionedMagic::GgufV3,
        metadata,
        tensor_infos,
        tensor_data_offset: layout.data_offset,
    }))
}
