use crate::vector::{Encoding, pack, similarity};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub struct Store {
    pub conn: Connection,
}

#[derive(Debug, serde::Serialize)]
pub struct Hit {
    pub path: String,
    pub kind: String,
    pub score: f32,
    pub start_line: usize,
    pub end_line: usize,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
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
        vectors: &[(usize, usize, Vec<u8>)],
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
            let mut statement = tx.prepare("INSERT INTO chunks(doc_id,ordinal,start_line,end_line,embedding) VALUES (?1,?2,?3,?4,?5)")?;
            for (idx, (start, end, vec)) in vectors.iter().enumerate() {
                statement.execute(params![id, idx as i64, *start as i64, *end as i64, vec])?;
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
    pub fn hits(&self, query: &[f32], encoding: Encoding, limit: usize) -> Result<Vec<Hit>> {
        use std::collections::HashMap;
        let mut stmt = self.conn.prepare(
            "SELECT d.path,d.kind,c.start_line,c.end_line,c.embedding
            FROM chunks c JOIN documents d ON d.id=c.doc_id",
        )?;
        let mut rows = stmt.query([])?;
        let mut best: HashMap<String, Hit> = HashMap::new();
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let start_line: usize = row.get::<_, i64>(2)? as usize;
            let end_line: usize = row.get::<_, i64>(3)? as usize;
            let blob: Vec<u8> = row.get(4)?;
            let score = similarity(query, &blob, encoding)?;
            if best.get(&path).is_none_or(|old| score > old.score) {
                best.insert(
                    path.clone(),
                    Hit {
                        path,
                        kind,
                        score,
                        start_line,
                        end_line,
                    },
                );
            }
        }
        let mut hits: Vec<Hit> = best.into_values().collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.path.cmp(&b.path)));
        hits.truncate(limit);
        Ok(hits)
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
