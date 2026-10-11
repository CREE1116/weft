use crate::vector::{Encoding, pack, similarity};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub type PackedChunk = (usize, usize, Vec<u8>, Option<[u8; 32]>);

pub struct Store {
    pub conn: Connection,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Hit {
    pub line_kind: &'static str,
    pub path: String,
    pub kind: String,
    pub score: f32,
    pub start_line: usize,
    pub end_line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk_index: Option<usize>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn =
            Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS settings (name TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS documents (
                id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, hash TEXT NOT NULL,
                kind TEXT NOT NULL, bytes INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS chunks (
                doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                embedding BLOB NOT NULL, PRIMARY KEY (doc_id, ordinal)
            );",
        )?;
        // Additive migration: legacy vectors remain readable and legacy writers
        // can omit this nullable column. Reopening is idempotent.
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let has_hash = {
            let mut stmt = tx.prepare("PRAGMA table_info(chunks)")?;
            let names = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            names.iter().any(|name| name == "input_hash")
        };
        if !has_hash {
            tx.execute("ALTER TABLE chunks ADD COLUMN input_hash BLOB", [])?;
        }
        tx.commit()?;
        Ok(Self { conn })
    }
    pub fn set_setting(&self, name: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(name,value) VALUES (?1,?2)
            ON CONFLICT(name) DO UPDATE SET value=excluded.value",
            params![name, value],
        )?;
        Ok(())
    }
    pub fn setting(&self, name: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE name=?1", [name], |r| {
                r.get(0)
            })
            .optional()
            .map_err(Into::into)
    }
    pub fn ensure_settings(&self, values: &[(&str, String)]) -> Result<()> {
        for (name, expected) in values {
            if let Some(existing) = self.setting(name)? {
                if &existing != expected {
                    bail!(
                        "index setting {name}={existing}; requested {expected}. Use a new --db or rebuild the index"
                    );
                }
            }
        }
        for (name, value) in values {
            self.conn.execute(
                "INSERT OR IGNORE INTO settings(name,value) VALUES (?1,?2)",
                params![name, value],
            )?;
        }
        Ok(())
    }
    pub fn unchanged(&self, path: &str, hash: &str) -> Result<bool> {
        let found: Option<String> = self
            .conn
            .query_row("SELECT hash FROM documents WHERE path=?1", [path], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(found.as_deref() == Some(hash))
    }
    pub fn replace(
        &mut self,
        path: &str,
        hash: &str,
        kind: &str,
        bytes: usize,
        vectors: &[PackedChunk],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO documents(path,hash,kind,bytes) VALUES (?1,?2,?3,?4)
                    ON CONFLICT(path) DO UPDATE SET hash=excluded.hash,kind=excluded.kind,bytes=excluded.bytes",
                    params![path, hash, kind, bytes as i64])?;
        let id: i64 = tx.query_row("SELECT id FROM documents WHERE path=?1", [path], |r| {
            r.get(0)
        })?;
        tx.execute("DELETE FROM chunks WHERE doc_id=?1", [id])?;
        {
            let mut statement = tx.prepare("INSERT INTO chunks(doc_id,ordinal,start_line,end_line,embedding,input_hash) VALUES (?1,?2,?3,?4,?5,?6)")?;
            for (idx, (start, end, vec, hash)) in vectors.iter().enumerate() {
                statement.execute(params![
                    id,
                    idx as i64,
                    *start as i64,
                    *end as i64,
                    vec,
                    hash.as_ref().map(|h| &h[..])
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    pub fn prune(
        &mut self,
        visited: &std::collections::HashSet<String>,
        root: &Path,
    ) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT path FROM documents")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let mut deleted = 0;
        for path in paths {
            let p = Path::new(&path);
            if p.starts_with(root) && !visited.contains(&path) {
                self.conn
                    .execute("DELETE FROM documents WHERE path=?1", [&path])?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }
    pub fn cached_chunks(&self, path: &str) -> Result<std::collections::HashMap<Vec<u8>, Vec<u8>>> {
        let mut stmt = self.conn.prepare("SELECT c.input_hash,c.embedding FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.path=?1 AND c.input_hash IS NOT NULL")?;
        let rows = stmt.query_map([path], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn hits(&self, query: &[f32], encoding: Encoding, limit: usize) -> Result<Vec<Hit>> {
        self.rank_by(
            limit,
            true,
            |_, _| true,
            |blob| similarity(query, blob, encoding),
        )
    }
    /// Exact scoring seam shared by single-vector and composed retrieval.
    pub fn rank_by(
        &self,
        limit: usize,
        per_file: bool,
        mut accept: impl FnMut(&str, &str) -> bool,
        mut score_blob: impl FnMut(&[u8]) -> Result<f32>,
    ) -> Result<Vec<Hit>> {
        use std::{cmp::Reverse, collections::BinaryHeap};
        if limit == 0 {
            return Ok(Vec::new());
        }
        #[derive(Debug)]
        struct Ranked(Hit);
        impl PartialEq for Ranked {
            fn eq(&self, other: &Self) -> bool {
                self.cmp(other).is_eq()
            }
        }
        impl Eq for Ranked {}
        impl PartialOrd for Ranked {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for Ranked {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.0
                    .score
                    .total_cmp(&other.0.score)
                    .then(other.0.path.cmp(&self.0.path))
                    .then(other.0.chunk_index.cmp(&self.0.chunk_index))
            }
        }
        let mut heap: BinaryHeap<Reverse<Ranked>> = BinaryHeap::new();
        let mut keep = |hit: Hit| {
            let item = Reverse(Ranked(hit));
            if heap.len() < limit {
                heap.push(item);
            } else if heap.peek().is_some_and(|worst| item.0 > worst.0) {
                heap.pop();
                heap.push(item);
            }
        };
        // The chunks primary key already orders document IDs/ordinals. Keep only
        // the best current-document chunk and K candidates, not every file.
        let mut stmt = self.conn.prepare(
            "SELECT d.id,d.path,d.kind,c.start_line,c.end_line,c.embedding,c.ordinal
            FROM chunks c JOIN documents d ON d.id=c.doc_id ORDER BY c.doc_id,c.ordinal",
        )?;
        let mut rows = stmt.query([])?;
        let mut current_id = None;
        let mut best: Option<Hit> = None;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            if current_id != Some(id) {
                if let Some(hit) = best.take() {
                    keep(hit);
                }
                current_id = Some(id);
            }
            let path: String = row.get(1)?;
            let kind: String = row.get(2)?;
            if !accept(&path, &kind) {
                continue;
            }
            let blob: Vec<u8> = row.get(5)?;
            let score = score_blob(&blob)?;
            if !score.is_finite() {
                bail!("nonfinite retrieval score");
            }
            let hit = Hit {
                line_kind: crate::document::line_kind(&path, &kind),
                path,
                kind,
                score,
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                chunk_index: Some(row.get::<_, i64>(6)? as usize),
            };
            if !per_file {
                keep(hit);
            } else if best.as_ref().is_none_or(|old| score > old.score) {
                best = Some(hit);
            }
        }
        if let Some(hit) = best {
            keep(hit);
        }
        let mut hits: Vec<_> = heap.into_iter().map(|item| item.0.0).collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.path.cmp(&b.path))
                .then(a.chunk_index.cmp(&b.chunk_index))
        });
        Ok(hits)
    }
    /// Metadata-only matching does not read source files or start inference.
    pub fn paths(&self, query: &str, under: Option<&Path>, kind: Option<&str>) -> Result<Vec<Hit>> {
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut stmt = self
            .conn
            .prepare("SELECT path,kind FROM documents ORDER BY path")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let record_kind: String = row.get(1)?;
            if under.is_some_and(|root| !Path::new(&path).starts_with(root))
                || kind.is_some_and(|k| k != record_kind)
                || !terms.iter().all(|term| path.to_lowercase().contains(term))
            {
                continue;
            }
            out.push(Hit {
                line_kind: "none",
                path,
                kind: record_kind,
                score: 1.0,
                start_line: 0,
                end_line: 0,
                chunk_index: None,
            });
        }
        Ok(out)
    }
    /// Indexed coverage only: does not walk source files or start inference.
    pub fn coverage(&self, under: &Path, kind: Option<&str>) -> Result<(usize, usize, usize)> {
        let mut stmt = self.conn.prepare(
            "SELECT d.path,d.kind,COUNT(c.ordinal),COALESCE(SUM(LENGTH(c.embedding)),0)
             FROM documents d LEFT JOIN chunks c ON c.doc_id=d.id GROUP BY d.id ORDER BY d.id",
        )?;
        let mut rows = stmt.query([])?;
        let (mut files, mut chunks, mut bytes) = (0, 0, 0);
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let record_kind: String = row.get(1)?;
            if !Path::new(&path).starts_with(under) || kind.is_some_and(|k| k != record_kind) {
                continue;
            }
            files += 1;
            chunks += row.get::<_, i64>(2)? as usize;
            bytes += row.get::<_, i64>(3)? as usize;
        }
        Ok((files, chunks, bytes))
    }
    pub fn stats(&self) -> Result<(usize, usize, usize)> {
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))?;
        let chunks: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        let bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(length(embedding)),0) FROM chunks",
            [],
            |r| r.get(0),
        )?;
        Ok((files as usize, chunks as usize, bytes as usize))
    }
    pub fn chunk_record(&self, path: &str, ordinal: usize) -> Result<(Vec<u8>, String)> {
        Ok(self.conn.query_row("SELECT c.embedding,d.hash FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.path=?1 AND c.ordinal=?2",params![path,ordinal as i64],|r| Ok((r.get(0)?,r.get(1)?)))?)
    }
    pub fn document_vector(&self, path: &str, encoding: Encoding, dim: usize) -> Result<Vec<f32>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.embedding FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.path=?1",
        )?;
        let rows = stmt.query_map([path], |r| r.get::<_, Vec<u8>>(0))?;
        let mut sum = vec![0.0f32; dim];
        let mut count = 0;
        for row in rows {
            let blob = row?;
            match encoding {
                Encoding::F32 => {
                    if blob.len() != dim * 4 {
                        bail!("invalid f32 vector length");
                    }
                    for (x, b) in sum.iter_mut().zip(blob.chunks_exact(4)) {
                        *x += f32::from_le_bytes(b.try_into().unwrap());
                    }
                }
                Encoding::Q8 => {
                    if blob.len() != dim + 4 {
                        bail!("invalid q8 vector length");
                    }
                    let scale = f32::from_le_bytes(blob[..4].try_into().unwrap());
                    for (x, b) in sum.iter_mut().zip(&blob[4..]) {
                        *x += *b as i8 as f32 * scale;
                    }
                }
            }
            count += 1;
        }
        if count == 0 {
            bail!("file not indexed: {path}");
        }
        crate::vector::normalized_prefix(&sum, dim)
    }
    pub fn vectors(&self) -> Result<Vec<(String, Vec<f32>)>> {
        if self.setting("encoding")?.as_deref() != Some("f32") {
            bail!("benchmark needs a --encoding f32 baseline index");
        }
        let mut stmt = self.conn.prepare("SELECT d.path,c.embedding FROM chunks c JOIN documents d ON d.id=c.doc_id ORDER BY d.path,c.ordinal")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (path, raw) = row?;
            let vec = raw
                .chunks_exact(4)
                .map(|x| f32::from_le_bytes(x.try_into().unwrap()))
                .collect();
            out.push((path, vec));
        }
        Ok(out)
    }
}

pub fn encode(v: &[f32], enc: Encoding) -> Vec<u8> {
    pack(v, enc)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_respects_components_kinds_and_empty_documents() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        for (path, kind, count) in [
            ("/docs/a.md", "text", 2),
            ("/docs/nested/b.md", "text", 1),
            ("/docs/image.png", "path", 1),
            ("/docs-other/c.md", "text", 3),
            ("/docs/empty.md", "text", 0),
        ] {
            let chunks = vec![(1, 1, encode(&[1., 0.], Encoding::Q8), None); count];
            store.replace(path, "hash", kind, 1, &chunks).unwrap();
        }
        assert_eq!(
            store.coverage(Path::new("/docs"), None).unwrap(),
            (4, 4, 24)
        );
        assert_eq!(
            store.coverage(Path::new("/docs"), Some("text")).unwrap(),
            (3, 3, 18)
        );
        assert_eq!(
            store.coverage(Path::new("/docs/nested"), None).unwrap(),
            (1, 1, 6)
        );
        assert_eq!(
            store.coverage(Path::new("/missing"), None).unwrap(),
            (0, 0, 0)
        );
    }
    #[test]
    fn bounded_ranking_matches_exhaustive_search_and_ties() {
        for enc in [Encoding::F32, Encoding::Q8] {
            let mut store = Store::open(Path::new(":memory:")).unwrap();
            let query = vec![1.0, 0.0];
            let mut expected = Vec::new();
            for i in 0..80 {
                let path = format!("/fixture/{i:03}.md");
                let mut chunks = Vec::new();
                let mut score = f32::NEG_INFINITY;
                let mut line = 0;
                for j in 0..3 {
                    let v = crate::vector::normalized_prefix(
                        &[((i * 13 + j * 17) % 11) as f32, 1.0],
                        2,
                    )
                    .unwrap();
                    let bytes = encode(&v, enc);
                    let s = similarity(&query, &bytes, enc).unwrap();
                    if s > score {
                        score = s;
                        line = j + 1;
                    }
                    chunks.push((j + 1, j + 1, bytes, None));
                }
                store.replace(&path, "hash", "text", 1, &chunks).unwrap();
                expected.push((path, score, line));
            }
            expected.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            for k in [0, 1, 7, 80, 100] {
                let actual = store.hits(&query, enc, k).unwrap();
                let reference: Vec<_> = expected.iter().take(k).cloned().collect();
                assert_eq!(
                    actual
                        .into_iter()
                        .map(|h| (h.path, h.score, h.start_line))
                        .collect::<Vec<_>>(),
                    reference
                );
            }
        }
    }

    #[test]
    fn additive_hash_migration_preserves_legacy_vectors_and_writers() {
        let path = std::env::temp_dir().join(format!(
            "myvec-migration-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch("CREATE TABLE documents(id INTEGER PRIMARY KEY,path TEXT UNIQUE NOT NULL,hash TEXT NOT NULL,kind TEXT NOT NULL,bytes INTEGER NOT NULL); CREATE TABLE chunks(doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,ordinal INTEGER NOT NULL,start_line INTEGER NOT NULL,end_line INTEGER NOT NULL,embedding BLOB NOT NULL,PRIMARY KEY(doc_id,ordinal)); INSERT INTO documents VALUES(1,'/legacy.md','hash','text',1);").unwrap();
            old.execute(
                "INSERT INTO chunks VALUES(1,0,1,1,?1)",
                [encode(&[1.0, 0.0], Encoding::F32)],
            )
            .unwrap();
            let mut new = Store::open(&path).unwrap();
            assert_eq!(new.stats().unwrap(), (1, 1, 8));
            assert_eq!(
                new.hits(&[1.0, 0.0], Encoding::F32, 1).unwrap()[0].path,
                "/legacy.md"
            );
            assert!(new.cached_chunks("/legacy.md").unwrap().is_empty());
            let hash = *blake3::hash(b"new text").as_bytes();
            new.replace(
                "/new.md",
                "hash",
                "text",
                1,
                &[(1, 1, encode(&[0.0, 1.0], Encoding::F32), Some(hash))],
            )
            .unwrap();
            assert_eq!(new.cached_chunks("/new.md").unwrap().len(), 1);
            // Legacy writers explicitly name columns, so the nullable addition
            // also permits rolling back to the previous binary.
            old.execute("INSERT INTO chunks(doc_id,ordinal,start_line,end_line,embedding) VALUES(1,1,2,2,?1)",[encode(&[0.0,1.0],Encoding::F32)]).unwrap();
            assert_eq!(Store::open(&path).unwrap().stats().unwrap(), (2, 3, 24));
            let columns: i64 = old
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('chunks') WHERE name='input_hash'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(columns, 1);
        }
        std::fs::remove_file(&path).unwrap();
    }
}
