//! File-identity set operations over explicitly bounded retrieval sets.
use crate::{
    store::{Hit, Store},
    vector::{Encoding, normalized_prefix},
};
use anyhow::{Result, bail};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

fn default_top() -> usize {
    20
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Text {
        query: String,
        #[serde(default = "default_top")]
        top: usize,
        min_score: Option<f32>,
    },
    File {
        path: PathBuf,
        #[serde(default = "default_top")]
        top: usize,
        min_score: Option<f32>,
    },
    Path {
        #[serde(default)]
        query: String,
        under: Option<PathBuf>,
        kind: Option<String>,
    },
    Union {
        args: Vec<Query>,
    },
    Intersection {
        args: Vec<Query>,
    },
    Difference {
        args: Vec<Query>,
    },
}

impl Query {
    pub fn validate(&self) -> Result<()> {
        fn visit(q: &Query, depth: usize, count: &mut usize) -> Result<()> {
            *count += 1;
            if depth > 16 || *count > 128 {
                bail!("query exceeds depth 16 or 128 nodes");
            }
            match q {
                Query::Text {
                    query,
                    top,
                    min_score,
                } => {
                    if query.trim().is_empty() {
                        bail!("text query must not be empty");
                    }
                    limits(*top, *min_score)?;
                }
                Query::File { top, min_score, .. } => limits(*top, *min_score)?,
                Query::Path { under, kind, .. } => {
                    if under.as_ref().is_some_and(|p| !p.is_absolute()) {
                        bail!("path under must be absolute");
                    }
                    if kind.as_ref().is_some_and(|k| {
                        !matches!(k.as_str(), "text" | "path" | "image" | "audio" | "video")
                    }) {
                        bail!("unknown record kind");
                    }
                }
                Query::Union { args }
                | Query::Intersection { args }
                | Query::Difference { args } => {
                    if args.len() < 2 || matches!(q, Query::Difference { .. }) && args.len() != 2 {
                        bail!(
                            "union/intersection need at least two operands; difference needs exactly two"
                        );
                    }
                    for arg in args {
                        visit(arg, depth + 1, count)?;
                    }
                }
            }
            Ok(())
        }
        visit(self, 0, &mut 0)
    }
}

fn limits(top: usize, min_score: Option<f32>) -> Result<()> {
    if !(1..=10_000).contains(&top) {
        bail!("leaf top must be 1..10000");
    }
    if min_score.is_some_and(|s| !s.is_finite() || !(-1.0..=1.0).contains(&s)) {
        bail!("min_score must be finite and between -1 and 1");
    }
    Ok(())
}

pub fn evaluate(
    q: &Query,
    store: &Store,
    dim: usize,
    enc: Encoding,
    embed: &mut impl FnMut(&str) -> Result<Vec<f32>>,
) -> Result<Vec<Hit>> {
    Ok(match q {
        Query::Text {
            query,
            top,
            min_score,
        } => {
            let v = normalized_prefix(&embed(query)?, dim)?;
            threshold(store.hits(&v, enc, *top)?, *min_score)
        }
        Query::File {
            path,
            top,
            min_score,
        } => {
            let path = path.canonicalize()?.to_string_lossy().into_owned();
            let v = store.document_vector(&path, enc, dim)?;
            threshold(
                store
                    .hits(&v, enc, top.saturating_add(1))?
                    .into_iter()
                    .filter(|h| h.path != path)
                    .take(*top)
                    .collect(),
                *min_score,
            )
        }
        Query::Path { query, under, kind } => {
            store.paths(query, under.as_deref(), kind.as_deref())?
        }
        Query::Union { args } | Query::Intersection { args } | Query::Difference { args } => {
            let sets = args
                .iter()
                .map(|arg| evaluate(arg, store, dim, enc, embed))
                .collect::<Result<Vec<_>>>()?;
            if matches!(q, Query::Union { .. }) {
                union(sets)
            } else {
                let mut sets = sets.into_iter();
                let mut left = sets.next().unwrap();
                for right in sets {
                    let paths: HashSet<_> = right.into_iter().map(|h| h.path).collect();
                    left.retain(|h| {
                        paths.contains(&h.path) == matches!(q, Query::Intersection { .. })
                    });
                }
                left
            }
        }
    })
}

fn threshold(hits: Vec<Hit>, minimum: Option<f32>) -> Vec<Hit> {
    hits.into_iter()
        .filter(|h| minimum.is_none_or(|s| h.score >= s))
        .collect()
}

/// Reciprocal-rank fusion avoids comparing cosine and boolean metadata scores.
pub fn union(sets: Vec<Vec<Hit>>) -> Vec<Hit> {
    let mut results: HashMap<String, (Hit, f32)> = HashMap::new();
    for set in sets {
        for (rank, hit) in set.into_iter().enumerate() {
            let weight = 1.0 / (60.0 + (rank + 1) as f32);
            let entry = results
                .entry(hit.path.clone())
                .or_insert((hit.clone(), 0.0));
            // Prefer actual chunk evidence over a metadata-only occurrence.
            if entry.0.start_line == 0 && hit.start_line > 0 {
                entry.0 = hit;
            }
            entry.1 += weight;
        }
    }
    let mut out: Vec<_> = results
        .into_values()
        .map(|(mut hit, score)| {
            hit.score = score;
            hit
        })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.path.cmp(&b.path)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::encode;
    use std::path::Path;
    fn store() -> Store {
        let mut s = Store::open(Path::new(":memory:")).unwrap();
        for (path, v) in [
            ("/docs/retry.md", vec![1.0, 0.0]),
            ("/docs/timeout.md", vec![0.8, 0.6]),
            ("/photos/retry.jpg", vec![0.0, 1.0]),
        ] {
            s.replace(
                path,
                "hash",
                if path.ends_with(".jpg") {
                    "path"
                } else {
                    "text"
                },
                1,
                &[(1, 2, encode(&v, Encoding::F32), None)],
            )
            .unwrap();
        }
        s
    }
    fn run(spec: &str) -> Vec<Hit> {
        let q: Query = serde_json::from_str(spec).unwrap();
        q.validate().unwrap();
        evaluate(&q, &store(), 2, Encoding::F32, &mut |_| Ok(vec![1.0, 0.0])).unwrap()
    }
    #[test]
    fn sets_apply_to_paths_with_explicit_leaf_boundaries() {
        let hits = run(
            r#"{"op":"intersection","args":[{"op":"text","query":"retry","top":3,"min_score":0.7},{"op":"path","query":"retry","under":"/docs"}]}"#,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "/docs/retry.md");
        assert_eq!(hits[0].start_line, 1);
        let hits = run(
            r#"{"op":"difference","args":[{"op":"path","query":"retry"},{"op":"path","kind":"path"}]}"#,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "/docs/retry.md");
        let hits =
            run(r#"{"op":"intersection","args":[{"op":"path","query":"missing"},{"op":"path"}]}"#);
        assert!(hits.is_empty());
        assert!(run(r#"{"op":"path","under":"/doc"}"#).is_empty());
    }
    #[test]
    fn union_deduplicates_and_keeps_chunk_evidence() {
        let hits = run(
            r#"{"op":"union","args":[{"op":"path","query":"retry"},{"op":"text","query":"retry","top":1}]}"#,
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "/docs/retry.md");
        assert_eq!(hits[0].start_line, 1);
        assert!((hits[0].score - 2.0 / 61.0).abs() < 1e-6);
    }
    #[test]
    fn invalid_specs_fail_before_inference() {
        for spec in [
            r#"{"op":"union","args":[]}"#,
            r#"{"op":"text","query":"x","top":0}"#,
            r#"{"op":"text","query":"x","min_score":2}"#,
            r#"{"op":"path","under":"relative"}"#,
            r#"{"op":"difference","args":[{"op":"path"},{"op":"path"},{"op":"path"}]}"#,
        ] {
            assert!(
                serde_json::from_str::<Query>(spec)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        assert!(serde_json::from_str::<Query>(r#"{"op":"path","typo":1}"#).is_err());
    }
}
