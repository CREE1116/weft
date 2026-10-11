//! Search-time representatives; never writes extra vectors to the index.
use crate::{
    store::{Hit, Store},
    vector::{Encoding, normalized_prefix, unpack},
};
use anyhow::Result;
use std::{collections::BTreeMap, path::Path};

pub struct Representative {
    pub hit: Hit,
    pub vector: Vec<f32>,
    pub hash: String,
    pub files: usize,
    pub chunks: usize,
}

struct Group {
    path: String,
    kind: String,
    hash: String,
    sum: Vec<f32>,
    chunks: usize,
}

/// Streams files with O(dim + pool*dim) memory. Folder mode additionally holds
/// one sum and fingerprint per eligible immediate parent, O(folders*dim).
pub fn rank(
    store: &Store,
    enc: Encoding,
    dim: usize,
    folder: bool,
    limit: usize,
    mut accept: impl FnMut(&str, &str) -> bool,
    mut score: impl FnMut(&[f32]) -> f32,
) -> Result<(Vec<Representative>, usize, usize)> {
    let mut pool: Vec<Representative> = Vec::new();
    let mut folders: BTreeMap<String, (Vec<f32>, usize, usize, blake3::Hasher)> = BTreeMap::new();
    let mut evaluated_chunks = 0;
    let mut evaluated_candidates = 0;
    let mut keep = |mut candidate: Representative| {
        candidate.hit.score = score(&candidate.vector);
        evaluated_candidates += 1;
        let position = pool.partition_point(|old| {
            old.hit.score > candidate.hit.score
                || (old.hit.score == candidate.hit.score && old.hit.path < candidate.hit.path)
        });
        if position < limit {
            pool.insert(position, candidate);
            pool.truncate(limit);
        }
    };
    let mut finish = |group: Group| -> Result<()> {
        let vector = normalized_prefix(&group.sum, dim)?;
        if folder {
            let parent = Path::new(&group.path)
                .parent()
                .unwrap_or(Path::new("/"))
                .to_string_lossy()
                .into_owned();
            let entry = folders
                .entry(parent)
                .or_insert_with(|| (vec![0.; dim], 0, 0, blake3::Hasher::new()));
            for (sum, value) in entry.0.iter_mut().zip(vector) {
                *sum += value;
            }
            entry.1 += 1;
            entry.2 += group.chunks;
            // SQL document ID order is stable for a snapshot. Delimited membership
            // plus content hashes distinguishes this from a source-file hash.
            entry.3.update(&(group.path.len() as u64).to_le_bytes());
            entry.3.update(group.path.as_bytes());
            entry.3.update(&(group.hash.len() as u64).to_le_bytes());
            entry.3.update(group.hash.as_bytes());
        } else {
            keep(Representative {
                hit: Hit {
                    path: group.path,
                    kind: group.kind,
                    score: 0.,
                    start_line: 0,
                    end_line: 0,
                    chunk_index: None,
                },
                vector,
                hash: group.hash,
                files: 1,
                chunks: group.chunks,
            });
        }
        Ok(())
    };
    let mut stmt = store.conn.prepare("SELECT d.id,d.path,d.kind,d.hash,c.embedding FROM chunks c JOIN documents d ON d.id=c.doc_id ORDER BY c.doc_id,c.ordinal")?;
    let mut rows = stmt.query([])?;
    let mut id = None;
    let mut current: Option<Group> = None;
    while let Some(row) = rows.next()? {
        let next_id: i64 = row.get(0)?;
        if id != Some(next_id) {
            if let Some(group) = current.take() {
                finish(group)?;
            }
            id = Some(next_id);
            let path: String = row.get(1)?;
            let kind: String = row.get(2)?;
            if accept(&path, &kind) {
                current = Some(Group {
                    path,
                    kind,
                    hash: row.get(3)?,
                    sum: vec![0.; dim],
                    chunks: 0,
                });
            }
        }
        if let Some(group) = &mut current {
            let blob: Vec<u8> = row.get(4)?;
            for (sum, value) in group.sum.iter_mut().zip(unpack(&blob, enc, dim)?) {
                *sum += value;
            }
            group.chunks += 1;
            evaluated_chunks += 1;
        }
    }
    if let Some(group) = current {
        finish(group)?;
    }
    for (path, (sum, files, chunks, hash)) in folders {
        keep(Representative {
            hit: Hit {
                path,
                kind: "folder".into(),
                score: 0.,
                start_line: 0,
                end_line: 0,
                chunk_index: None,
            },
            vector: normalized_prefix(&sum, dim)?,
            hash: hash.finalize().to_hex().to_string(),
            files,
            chunks,
        });
    }
    Ok((pool, evaluated_chunks, evaluated_candidates))
}
