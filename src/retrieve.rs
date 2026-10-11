//! Multi-basis distance ranking and bounded, verified evidence for agents.
use crate::{
    chunk,
    store::{Hit, Store},
    vector::{Encoding, normalized_prefix, unpack_normalized},
};
use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use serde::Serialize;
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Combine {
    All,
    Any,
    Strict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Chunk,
    File,
    Folder,
}

#[derive(Args)]
pub struct Options {
    /// First positive text criterion; omit for file-only retrieval.
    pub query: Option<String>,
    /// Additional positive text criteria (repeatable).
    #[arg(long)]
    pub also: Vec<String>,
    /// Positive indexed-file examples (repeatable); does not load a model.
    #[arg(long = "file")]
    pub files: Vec<PathBuf>,
    /// Negative text criteria (repeatable).
    #[arg(long)]
    pub avoid: Vec<String>,
    /// Negative indexed-file examples (repeatable).
    #[arg(long)]
    pub avoid_file: Vec<PathBuf>,
    /// all=sum, any=min, strict=max of positive cosine distances.
    #[arg(long, value_enum, default_value = "all")]
    pub combine: Combine,
    #[arg(long, default_value_t = 1.0)]
    pub avoid_weight: f32,
    /// Hard folder restriction applied before scoring.
    #[arg(long)]
    pub under: Option<PathBuf>,
    #[arg(long, value_parser = ["text", "path", "image", "audio", "video"])]
    pub kind: Option<String>,
    /// Hard path/subtree exclusions (repeatable).
    #[arg(long)]
    pub exclude: Vec<PathBuf>,
    #[arg(long, default_value_t = 10)]
    pub top: usize,
    /// Exact composite-score pool for optional diversity/deduplication.
    #[arg(long, default_value_t = 100)]
    pub pool: usize,
    /// Penalize similarity to already selected evidence, in distance units.
    #[arg(long, default_value_t = 0.0)]
    pub diversity: f32,
    /// Suppress near-identical vectors in the pool; disabled by default.
    #[arg(long)]
    pub dedup_cosine: Option<f32>,
    /// Allow multiple matching chunks from the same file.
    #[arg(long)]
    pub chunks: bool,
    /// Rank chunks, whole-file centroids, or immediate-parent folder centroids.
    #[arg(long, value_enum, conflicts_with = "chunks")]
    pub level: Option<Level>,
    /// Optional upper bound on final composite distance before diversity.
    #[arg(long)]
    pub max_distance: Option<f32>,
    /// Total Unicode-character budget for verified source excerpts; 0 disables reads.
    #[arg(long, default_value_t = 0)]
    pub context_chars: usize,
    #[arg(long)]
    pub json: bool,
}

impl Options {
    pub fn validate(&self) -> Result<()> {
        let positives = usize::from(self.query.is_some()) + self.also.len() + self.files.len();
        let count = positives + self.avoid.len() + self.avoid_file.len();
        if positives == 0 || count > 32 {
            bail!("provide 1..32 total criteria, including at least one positive");
        }
        if self
            .query
            .iter()
            .chain(self.also.iter())
            .chain(self.avoid.iter())
            .any(|s| s.trim().is_empty())
        {
            bail!("text criteria must not be empty");
        }
        if !(1..=1000).contains(&self.top) || !(1..=10_000).contains(&self.pool) {
            bail!("top must be 1..1000 and pool 1..10000");
        }
        if !self.avoid_weight.is_finite()
            || !(0.0..=10.0).contains(&self.avoid_weight)
            || !self.diversity.is_finite()
            || !(0.0..=10.0).contains(&self.diversity)
            || self
                .dedup_cosine
                .is_some_and(|s| !s.is_finite() || !(0.0..=1.0).contains(&s))
            || self.max_distance.is_some_and(|s| !s.is_finite())
            || self.context_chars > 1_000_000
        {
            bail!("weights must be finite 0..10, dedup cosine 0..1, context budget <=1000000");
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub struct Criterion {
    pub source: String,
    pub negative: bool,
}

#[derive(Serialize)]
pub struct Evidence {
    #[serde(flatten)]
    pub hit: Hit,
    pub distance: f32,
    pub criterion_distances: Vec<f32>,
    pub selection_distance: f32,
    pub indexed_hash: String,
    pub member_files: usize,
    pub member_chunks: usize,
    pub source_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub context_truncated: bool,
}

#[derive(Serialize)]
pub struct Response {
    pub score_kind: &'static str,
    pub level: &'static str,
    pub aggregation: &'static str,
    pub evaluated_candidates: usize,
    pub combine: Combine,
    pub criteria: Vec<Criterion>,
    pub avoid_weight: f32,
    pub evaluated_chunks: usize,
    pub pool_size: usize,
    pub pool_limit: usize,
    pub selection_uses_pool: bool,
    pub context_chars_used: usize,
    pub hits: Vec<Evidence>,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x * y)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

fn composite(distances: &[f32], positives: usize, mode: Combine, weight: f32) -> f32 {
    let positive = match mode {
        Combine::All => distances[..positives].iter().sum(),
        Combine::Any => distances[..positives]
            .iter()
            .copied()
            .fold(f32::INFINITY, f32::min),
        Combine::Strict => distances[..positives].iter().copied().fold(0.0, f32::max),
    };
    let negative = distances[positives..]
        .iter()
        .copied()
        .reduce(f32::min)
        .unwrap_or(0.0);
    positive - weight * negative
}

pub fn run(
    options: &Options,
    store: &Store,
    dim: usize,
    enc: Encoding,
    embed: &mut impl FnMut(&str) -> Result<Vec<f32>>,
) -> Result<Response> {
    options.validate()?;
    let under = options
        .under
        .as_ref()
        .map(|p| p.canonicalize())
        .transpose()?;
    let excluded = options
        .exclude
        .iter()
        .map(|p| {
            p.canonicalize().or_else(|_| {
                if p.is_absolute() {
                    Ok(p.clone())
                } else {
                    std::env::current_dir().map(|cwd| cwd.join(p))
                }
            })
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    // Embed text before taking the SQLite snapshot, so model startup cannot hold
    // an old read snapshot while the background worker grows its WAL.
    let texts: Vec<_> = options
        .query
        .iter()
        .chain(options.also.iter())
        .chain(options.avoid.iter())
        .collect();
    let mut text_vectors = Vec::new();
    for text in &texts {
        text_vectors.push(normalized_prefix(&embed(text)?, dim)?);
    }
    let tx = store.conn.unchecked_transaction()?;
    let mut criteria = Vec::new();
    let mut vectors = Vec::new();
    let mut source_files = HashSet::new();
    let positive_texts = usize::from(options.query.is_some()) + options.also.len();
    for (text, v) in texts[..positive_texts]
        .iter()
        .zip(&text_vectors[..positive_texts])
    {
        criteria.push(Criterion {
            source: format!("text:{text}"),
            negative: false,
        });
        vectors.push(v.clone());
    }
    let mut add_file = |path: &Path, negative: bool| -> Result<()> {
        let path = path
            .canonicalize()
            .with_context(|| format!("cannot open reference {}", path.display()))?;
        let name = path.to_string_lossy().into_owned();
        vectors.push(store.document_vector(&name, enc, dim)?);
        criteria.push(Criterion {
            source: format!("file:{name}"),
            negative,
        });
        source_files.insert(name);
        Ok(())
    };
    for path in &options.files {
        add_file(path, false)?;
    }
    let positives = positive_texts + options.files.len();
    for path in &options.avoid_file {
        add_file(path, true)?;
    }
    for (text, v) in texts[positive_texts..]
        .iter()
        .zip(&text_vectors[positive_texts..])
    {
        criteria.push(Criterion {
            source: format!("text:{text}"),
            negative: true,
        });
        vectors.push(v.clone());
    }
    let mut evaluated = 0;
    let pool_limit = options.pool.max(options.top);
    let mut accept = |path: &str, kind: &str| {
        !source_files.contains(path)
            && under
                .as_ref()
                .is_none_or(|p| Path::new(path).starts_with(p))
            && options.kind.as_ref().is_none_or(|k| k == kind)
            && !excluded.iter().any(|p| Path::new(path).starts_with(p))
    };
    let score = |v: &[f32]| {
        let d: Vec<_> = vectors.iter().map(|basis| 1.0 - cosine(v, basis)).collect();
        -composite(&d, positives, options.combine, options.avoid_weight)
    };
    let aggregate = matches!(options.level, Some(Level::File | Level::Folder));
    let (records, evaluated_candidates) = if aggregate {
        let (records, chunks, count) = crate::hierarchy::rank(
            store,
            enc,
            dim,
            options.level == Some(Level::Folder),
            pool_limit,
            &mut accept,
            score,
        )?;
        evaluated = chunks;
        (records, count)
    } else {
        let mut last_path = String::new();
        let mut file_count = 0;
        let hits = store.rank_by(
            pool_limit,
            !options.chunks && options.level != Some(Level::Chunk),
            |path, kind| {
                let accepted = accept(path, kind);
                if accepted && last_path != path {
                    last_path = path.to_owned();
                    file_count += 1;
                }
                accepted
            },
            |blob| {
                evaluated += 1;
                Ok(score(&unpack_normalized(blob, enc, dim)?))
            },
        )?;
        let count = if options.chunks || options.level == Some(Level::Chunk) {
            evaluated
        } else {
            file_count
        };
        let mut records = Vec::new();
        for hit in hits {
            let (blob, hash) = store.chunk_record(&hit.path, hit.chunk_index.unwrap())?;
            records.push(crate::hierarchy::Representative {
                hit,
                vector: unpack_normalized(&blob, enc, dim)?,
                hash,
                files: 1,
                chunks: 1,
            });
        }
        (records, count)
    };
    let pool_size = records.len();
    let mut candidates = Vec::new();
    for record in records {
        let crate::hierarchy::Representative {
            hit,
            vector: v,
            hash,
            files,
            chunks,
        } = record;
        let distances: Vec<_> = vectors
            .iter()
            .map(|basis| 1.0 - cosine(&v, basis))
            .collect();
        let distance = composite(&distances, positives, options.combine, options.avoid_weight);
        if options.max_distance.is_some_and(|max| distance > max) {
            continue;
        }
        candidates.push((
            Evidence {
                hit,
                distance,
                criterion_distances: distances,
                selection_distance: distance,
                indexed_hash: hash,
                member_files: files,
                member_chunks: chunks,
                source_state: "not_checked".into(),
                context: None,
                context_truncated: false,
            },
            v,
        ));
    }
    let mut selected: Vec<(Evidence, Vec<f32>)> = Vec::new();
    while !candidates.is_empty() && selected.len() < options.top {
        for (e, v) in &mut candidates {
            let redundancy = selected
                .iter()
                .map(|(_, old)| cosine(v, old))
                .fold(0.0, f32::max);
            e.selection_distance = e.distance + options.diversity * redundancy;
        }
        candidates.retain(|(_, v)| {
            options
                .dedup_cosine
                .is_none_or(|cutoff| !selected.iter().any(|(_, old)| cosine(v, old) >= cutoff))
        });
        let index = candidates
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.0.selection_distance
                    .total_cmp(&b.0.selection_distance)
                    .then(a.0.hit.path.cmp(&b.0.hit.path))
                    .then(a.0.hit.chunk_index.cmp(&b.0.hit.chunk_index))
            })
            .map(|(i, _)| i);
        let Some(index) = index else {
            break;
        };
        selected.push(candidates.remove(index));
    }
    let mut remaining = options.context_chars;
    let chunk_chars = store
        .setting("chunk_chars")?
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(1100);
    let overlap = store
        .setting("overlap_lines")?
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(2);
    for (e, _) in &mut selected {
        if aggregate {
            e.source_state = "aggregate_not_checked".into();
            continue;
        }
        if e.hit.kind != "text" {
            e.source_state = "filename_or_media_only".into();
            continue;
        }
        if remaining == 0 {
            if options.context_chars > 0 {
                e.source_state = "budget_exhausted".into();
            }
            continue;
        }
        match excerpt(e, chunk_chars, overlap, remaining) {
            Ok(Some((text, truncated))) => {
                remaining -= text.chars().count();
                e.context = Some(text);
                e.context_truncated = truncated;
                e.source_state = "verified".into();
            }
            Ok(None) => e.source_state = "changed_since_index".into(),
            Err(error) => {
                e.source_state = if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    "missing"
                } else {
                    "unreadable"
                }
                .into()
            }
        }
    }
    tx.commit()?;
    Ok(Response {
        score_kind: "negative_composite_cosine_distance",
        level: match options.level {
            Some(Level::File) => "file",
            Some(Level::Folder) => "folder",
            Some(Level::Chunk) => "chunk",
            None if options.chunks => "chunk",
            None => "file_best_chunk",
        },
        aggregation: match options.level {
            Some(Level::File) => "normalized_chunk_sum",
            Some(Level::Folder) => "equal_file_weight_immediate_parent",
            _ => "none",
        },
        evaluated_candidates,
        combine: options.combine,
        criteria,
        avoid_weight: options.avoid_weight,
        evaluated_chunks: evaluated,
        pool_size,
        pool_limit,
        selection_uses_pool: options.diversity > 0.0 || options.dedup_cosine.is_some(),
        context_chars_used: options.context_chars - remaining,
        hits: selected.into_iter().map(|(e, _)| e).collect(),
    })
}

fn excerpt(
    e: &Evidence,
    chars: usize,
    overlap: usize,
    budget: usize,
) -> Result<Option<(String, bool)>> {
    let mut bytes = Vec::new();
    std::fs::File::open(&e.hit.path)?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 2 * 1024 * 1024 || blake3::hash(&bytes).to_hex().as_str() != e.indexed_hash {
        return Ok(None);
    }
    let text = std::str::from_utf8(&bytes)?;
    let chunks = chunk::split(text, chars, overlap);
    let Some(chunk) = e.hit.chunk_index.and_then(|i| chunks.get(i)) else {
        return Ok(None);
    };
    if chunk.start != e.hit.start_line || chunk.end != e.hit.end_line {
        return Ok(None);
    }
    let truncated = chunk.text.chars().count() > budget;
    Ok(Some((chunk.text.chars().take(budget).collect(), truncated)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        options: Options,
    }
    fn options(args: &[&str]) -> Options {
        Cli::parse_from(std::iter::once("test").chain(args.iter().copied())).options
    }
    fn fixture(enc: Encoding) -> Store {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        for (name, v) in [
            ("/a.md", vec![1., 0.]),
            ("/b.md", vec![0., 1.]),
            ("/compromise.md", vec![1., 1.]),
        ] {
            let v = normalized_prefix(&v, 2).unwrap();
            store
                .replace(
                    name,
                    "hash",
                    "text",
                    1,
                    &[(1, 1, crate::store::encode(&v, enc), None)],
                )
                .unwrap();
        }
        store
    }
    fn embed(text: &str) -> Result<Vec<f32>> {
        Ok(match text {
            "A" => vec![1., 0.],
            "B" => vec![0., 1.],
            _ => vec![-1., 0.],
        })
    }
    #[test]
    fn levels_rank_different_units_and_equal_weight_files() {
        for enc in [Encoding::F32, Encoding::Q8] {
            let mut store = Store::open(Path::new(":memory:")).unwrap();
            for (path, values) in [
                ("/mixed/long.md", vec![vec![1., 0.]; 10]),
                ("/mixed/short.md", vec![vec![0., 1.]]),
                ("/focused/a.md", vec![vec![0.8, 0.6]]),
                ("/focused/nested/a.md", vec![vec![0., 1.]]),
                (
                    "/passages/a.md",
                    vec![vec![1., 0.], vec![0., 1.], vec![0., 1.]],
                ),
            ] {
                let chunks: Vec<_> = values
                    .into_iter()
                    .map(|v| (1, 1, crate::vector::pack(&v, enc), None))
                    .collect();
                store.replace(path, "hash", "text", 1, &chunks).unwrap();
            }
            let search = |args: &[&str]| run(&options(args), &store, 2, enc, &mut embed).unwrap();
            let chunks = search(&["A", "--level", "chunk"]);
            assert_eq!(chunks.evaluated_chunks, 16);
            assert_eq!(chunks.evaluated_candidates, 16);
            let files = search(&["A", "--level", "file", "--context-chars", "100"]);
            assert_eq!(files.evaluated_candidates, 5);
            let mixed = files
                .hits
                .iter()
                .find(|e| e.hit.path == "/mixed/long.md")
                .unwrap();
            assert_eq!(mixed.member_chunks, 10);
            assert!(mixed.hit.chunk_index.is_none());
            assert!(mixed.context.is_none());
            assert_eq!(mixed.source_state, "aggregate_not_checked");
            let passage = files
                .hits
                .iter()
                .find(|e| e.hit.path == "/passages/a.md")
                .unwrap();
            assert!(passage.distance > 0.5);
            let best = search(&["A"]);
            assert!(
                best.hits
                    .iter()
                    .find(|e| e.hit.path == "/passages/a.md")
                    .unwrap()
                    .distance
                    < 0.001
            );
            let folders = search(&["A", "--level", "folder"]);
            assert_eq!(folders.evaluated_candidates, 4);
            assert_eq!(folders.hits[0].hit.path, "/focused");
            let bounded = search(&["A", "--level", "folder", "--pool", "1", "--top", "1"]);
            assert_eq!(bounded.hits[0].hit.path, folders.hits[0].hit.path);
            assert_eq!(bounded.pool_size, 1);
            assert_eq!(bounded.evaluated_candidates, 4);
            let mixed = folders
                .hits
                .iter()
                .find(|e| e.hit.path == "/mixed")
                .unwrap();
            assert_eq!((mixed.member_files, mixed.member_chunks), (2, 11));
            assert!((mixed.distance - (1. - 0.5f32.sqrt())).abs() < 0.005);
            let filtered = search(&["A", "--level", "folder", "--exclude", "/mixed/long.md"]);
            let mixed = filtered
                .hits
                .iter()
                .find(|e| e.hit.path == "/mixed")
                .unwrap();
            assert_eq!(mixed.member_files, 1);
            assert!((mixed.distance - 1.).abs() < 0.001);
            assert_eq!(filtered.evaluated_chunks, 6);
            assert_ne!(
                mixed.indexed_hash,
                folders
                    .hits
                    .iter()
                    .find(|e| e.hit.path == "/mixed")
                    .unwrap()
                    .indexed_hash
            );
        }
    }

    #[test]
    fn full_scan_finds_compromise_outside_each_leaf_top_one() {
        for enc in [Encoding::F32, Encoding::Q8] {
            let store = fixture(enc);
            assert_eq!(store.hits(&[1., 0.], enc, 1).unwrap()[0].path, "/a.md");
            assert_eq!(store.hits(&[0., 1.], enc, 1).unwrap()[0].path, "/b.md");
            for mode in ["all", "strict"] {
                let result = run(
                    &options(&["A", "--also", "B", "--combine", mode, "--top", "1"]),
                    &store,
                    2,
                    enc,
                    &mut embed,
                )
                .unwrap();
                assert_eq!(result.hits[0].hit.path, "/compromise.md");
                assert_eq!(result.evaluated_chunks, 3);
            }
            let union = run(
                &options(&["A", "--also", "B", "--combine", "any"]),
                &store,
                2,
                enc,
                &mut embed,
            )
            .unwrap();
            assert_eq!(union.hits[0].hit.path, "/a.md");
            let difference =
                run(&options(&["A", "--avoid", "B"]), &store, 2, enc, &mut embed).unwrap();
            assert_eq!(difference.hits[0].hit.path, "/a.md");
            assert!((difference.hits[0].distance + 1.).abs() < 1e-5);
            assert_eq!(difference.hits[0].criterion_distances.len(), 2);
        }
    }
    #[test]
    fn optional_diversity_and_dedup_reduce_redundant_evidence() {
        let mut store = fixture(Encoding::F32);
        store
            .replace(
                "/aa-copy.md",
                "hash",
                "text",
                1,
                &[(1, 1, crate::store::encode(&[1., 0.], Encoding::F32), None)],
            )
            .unwrap();
        let baseline = run(
            &options(&["A", "--top", "2"]),
            &store,
            2,
            Encoding::F32,
            &mut embed,
        )
        .unwrap();
        assert_eq!(baseline.hits[1].hit.path, "/aa-copy.md");
        for args in [
            vec!["A", "--top", "2", "--dedup-cosine", "0.99"],
            vec!["A", "--top", "2", "--diversity", "2"],
        ] {
            let result = run(&options(&args), &store, 2, Encoding::F32, &mut embed).unwrap();
            assert_ne!(result.hits[1].hit.path, "/aa-copy.md");
            assert!(result.selection_uses_pool);
        }
        let excluded = run(
            &options(&["A", "--exclude", "/a.md", "--kind", "text"]),
            &store,
            2,
            Encoding::F32,
            &mut embed,
        )
        .unwrap();
        assert!(excluded.hits.iter().all(|e| e.hit.path != "/a.md"));
    }
    #[test]
    fn verified_budgeted_context_uses_exact_chunk_and_detects_stale_sources() {
        let path = std::env::temp_dir().join(format!(
            "myvec-context-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let text = format!("{}{}", "가".repeat(100), "나".repeat(100));
        std::fs::write(&path, &text).unwrap();
        let name = path.to_string_lossy();
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        store.set_setting("chunk_chars", "100").unwrap();
        store.set_setting("overlap_lines", "0").unwrap();
        store
            .replace(
                &name,
                &blake3::hash(text.as_bytes()).to_hex(),
                "text",
                text.len(),
                &[
                    (1, 1, crate::store::encode(&[0., 1.], Encoding::F32), None),
                    (1, 1, crate::store::encode(&[1., 0.], Encoding::F32), None),
                ],
            )
            .unwrap();
        let request = options(&["A", "--chunks", "--top", "2", "--context-chars", "5"]);
        let result = run(&request, &store, 2, Encoding::F32, &mut embed).unwrap();
        assert_eq!(result.hits.len(), 2);
        assert_eq!(result.hits[0].hit.chunk_index, Some(1));
        assert_eq!(result.hits[0].context.as_deref(), Some("나나나나나"));
        assert_eq!(result.hits[0].source_state, "verified");
        assert!(result.hits[0].context_truncated);
        assert_eq!(result.context_chars_used, 5);
        assert_eq!(result.hits[1].source_state, "budget_exhausted");
        std::fs::write(&path, "changed").unwrap();
        let result = run(&request, &store, 2, Encoding::F32, &mut embed).unwrap();
        assert!(
            result
                .hits
                .iter()
                .all(|e| e.context.is_none() && e.source_state == "changed_since_index")
        );
        std::fs::remove_file(&path).unwrap();
        let result = run(&request, &store, 2, Encoding::F32, &mut embed).unwrap();
        assert!(result.hits.iter().all(|e| e.source_state == "missing"));
    }
    #[test]
    fn file_only_criteria_do_not_start_inference() {
        let path = std::env::temp_dir().join(format!("myvec-reference-{}.txt", std::process::id()));
        std::fs::write(&path, "reference").unwrap();
        let canonical = path.canonicalize().unwrap();
        let mut store = fixture(Encoding::F32);
        store
            .replace(
                &canonical.to_string_lossy(),
                "hash",
                "text",
                1,
                &[(1, 1, crate::store::encode(&[1., 0.], Encoding::F32), None)],
            )
            .unwrap();
        let name = path.to_str().unwrap();
        let result = run(
            &options(&["--file", name]),
            &store,
            2,
            Encoding::F32,
            &mut |_| panic!("inference must not start"),
        )
        .unwrap();
        assert_eq!(result.hits[0].hit.path, "/a.md");
        assert!(
            result
                .hits
                .iter()
                .all(|e| e.hit.path != canonical.to_string_lossy())
        );
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn malformed_requests_fail_before_inference() {
        for args in [
            vec![],
            vec!["A", "--also", ""],
            vec!["A", "--top", "0"],
            vec!["A", "--diversity", "NaN"],
            vec!["A", "--dedup-cosine", "2"],
        ] {
            let request = options(&args);
            assert!(
                run(
                    &request,
                    &fixture(Encoding::F32),
                    2,
                    Encoding::F32,
                    &mut |_| panic!("invalid request")
                )
                .is_err()
            );
        }
    }
}
