use crate::vector::{Encoding, pack, similarity};
use anyhow::{Context, Result, bail};
use memmap2::Mmap;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

const HANGUL_INITIALS: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ',
    'ㅌ', 'ㅍ', 'ㅎ',
];

fn initial_consonant(c: char) -> Option<char> {
    let code = c as u32;
    if (0xAC00..=0xD7A3).contains(&code) {
        Some(HANGUL_INITIALS[((code - 0xAC00) / 588) as usize])
    } else if (0x1100..=0x1112).contains(&code) {
        Some(HANGUL_INITIALS[(code - 0x1100) as usize])
    } else if HANGUL_INITIALS.contains(&c) {
        Some(c)
    } else {
        None
    }
}

/// NFC makes macOS decomposed filenames searchable. Only consonants in the
/// query act as initials; literal syllables, vowels, punctuation and extensions
/// retain their exact meaning. Initials never match a file's parent directory.
struct FilenameMatcher {
    terms: Vec<(String, Vec<char>, bool)>,
}
impl FilenameMatcher {
    fn new(query: &str) -> Self {
        let terms = query
            .split_whitespace()
            .map(|term| {
                let literal: String = term.nfc().flat_map(char::to_lowercase).collect();
                let chars: Vec<char> = literal.chars().collect();
                let initials = chars.iter().any(|&c| {
                    HANGUL_INITIALS.contains(&c) || (0x1100..=0x1112).contains(&(c as u32))
                });
                (literal, chars, initials)
            })
            .collect();
        Self { terms }
    }
    fn normalize(path: &str) -> String {
        path.nfc().flat_map(char::to_lowercase).collect()
    }
    fn matches_normalized(&self, normalized: &str, name_only: bool) -> bool {
        let name = Path::new(normalized)
            .file_name()
            .unwrap_or_default()
            .to_str()
            .unwrap_or("");
        let needs_initials = self.terms.iter().any(|(_, _, initials)| *initials);
        let name_chars: Vec<char> = if needs_initials {
            name.chars().collect()
        } else {
            Vec::new()
        };
        self.terms.iter().all(|(literal, term, initials)| {
            let candidate = if *initials || name_only {
                name
            } else {
                normalized
            };
            if !initials {
                return candidate.contains(literal);
            }
            name_chars.windows(term.len()).any(|window| {
                window.iter().zip(term).all(|(&actual, &query)| {
                    if HANGUL_INITIALS.contains(&query)
                        || (0x1100..=0x1112).contains(&(query as u32))
                    {
                        initial_consonant(actual) == initial_consonant(query)
                    } else {
                        actual == query
                    }
                })
            })
        })
    }
}

pub type PackedChunk = (usize, usize, Vec<u8>, Option<[u8; 32]>);

pub struct Store {
    pub conn: Connection,
    /// Path to the companion mmap vector cache file (empty = disabled, e.g. :memory:).
    pub vec_path: PathBuf,
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
            PRAGMA cache_size = -16000; PRAGMA mmap_size = 268435456;
            PRAGMA temp_store = MEMORY;
            CREATE TABLE IF NOT EXISTS settings (name TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS documents (
                id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, hash TEXT NOT NULL,
                kind TEXT NOT NULL, bytes INTEGER NOT NULL, stat_sig TEXT
            );
            CREATE TABLE IF NOT EXISTS chunks (
                doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL, start_line INTEGER NOT NULL, end_line INTEGER NOT NULL,
                embedding BLOB NOT NULL, PRIMARY KEY (doc_id, ordinal)
            );
            CREATE TABLE IF NOT EXISTS views (
                name TEXT PRIMARY KEY, path TEXT NOT NULL, kind TEXT
            );
            CREATE TABLE IF NOT EXISTS embedding_cache (
                input_hash BLOB PRIMARY KEY, embedding BLOB NOT NULL
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
        let has_stat = {
            let mut stmt = tx.prepare("PRAGMA table_info(documents)")?;
            let names = stmt
                .query_map([], |r| r.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            names.iter().any(|name| name == "stat_sig")
        };
        if !has_stat {
            tx.execute("ALTER TABLE documents ADD COLUMN stat_sig TEXT", [])?;
        }
        tx.commit()?;
        // Derive the companion mmap vector cache path next to the sqlite file.
        // e.g. ~/.local/share/myvec/index.sqlite → index.sqlite.vectors.bin
        // For :memory: or empty paths the cache is disabled (vec_path stays empty).
        let vec_path = if path.as_os_str() == ":memory:" || path.as_os_str().is_empty() {
            PathBuf::new()
        } else {
            let mut s = path.as_os_str().to_owned();
            s.push(".vectors.bin");
            PathBuf::from(s)
        };
        Ok(Self { conn, vec_path })
    }

    /// Open in read-only mode with zero write locks. Never blocks on concurrent indexers.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        if !path.exists() && path.as_os_str() != ":memory:" {
            return Self::open(path);
        }
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening read-only {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_millis(500))?;
        let _ = conn.execute_batch(
            "PRAGMA cache_size = -16000; PRAGMA mmap_size = 268435456;
             PRAGMA query_only = ON; PRAGMA read_uncommitted = ON;",
        );

        let vec_path = if path.as_os_str() == ":memory:" || path.as_os_str().is_empty() {
            PathBuf::new()
        } else {
            let mut s = path.as_os_str().to_owned();
            s.push(".vectors.bin");
            PathBuf::from(s)
        };
        Ok(Self { conn, vec_path })
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
    /// Fast path for unchanged files. An optional full-hash scan can override it.
    pub fn unchanged_stat(&self, path: &str, kind: &str, stat: &str) -> Result<bool> {
        let found: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT kind,stat_sig FROM documents WHERE path=?1",
                [path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(found.is_some_and(|(stored_kind, stored_stat)| {
            stored_kind == kind && stored_stat.as_deref() == Some(stat)
        }))
    }
    pub fn set_stat(&self, path: &str, stat: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE documents SET stat_sig=?2 WHERE path=?1",
            params![path, stat],
        )?;
        Ok(())
    }

    /// Persistent content-addressed reuse across files, moves and later scans.
    pub fn cached_embedding(&self, hash: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        self.conn
            .query_row(
                "SELECT embedding FROM embedding_cache WHERE input_hash=?1",
                [&hash[..]],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
    /// Avoid downgrading already indexed media to filename-only during a
    /// subsequent default `index --all` that did not request media inference.
    pub fn has_media_embedding(&self, path: &str) -> Result<bool> {
        let kind: Option<String> = self
            .conn
            .query_row("SELECT kind FROM documents WHERE path=?1", [path], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(matches!(kind.as_deref(), Some("image" | "audio" | "video")))
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
                    ON CONFLICT(path) DO UPDATE SET hash=excluded.hash,kind=excluded.kind,bytes=excluded.bytes,stat_sig=NULL",
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
                if let Some(hash) = hash {
                    tx.execute("INSERT OR IGNORE INTO embedding_cache(input_hash,embedding) VALUES (?1,?2)",
                        params![&hash[..], vec])?;
                }
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
    /// Delete a removed path or a removed subtree; chunk vectors remain in the
    /// global cache so a subsequent rename can reuse them.
    pub fn remove_missing(&mut self, path: &Path) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT path FROM documents")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        for candidate in paths {
            if Path::new(&candidate).starts_with(path) {
                removed += tx.execute("DELETE FROM documents WHERE path=?1", [candidate])?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }
    pub fn cached_chunks(&self, path: &str) -> Result<std::collections::HashMap<Vec<u8>, Vec<u8>>> {
        let mut stmt = self.conn.prepare("SELECT c.input_hash,c.embedding FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.path=?1 AND c.input_hash IS NOT NULL")?;
        let rows = stmt.query_map([path], |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn hits(&self, query: &[f32], encoding: Encoding, limit: usize) -> Result<Vec<Hit>> {
        // The mmap snapshot is the hot path for the normal semantic search.
        // SQLite remains the source of truth and fallback for legacy indexes.
        if encoding == Encoding::Q8 {
            if let Some(dim) = self.setting("dim")?.and_then(|v| v.parse::<usize>().ok()) {
                if let Ok(cache) = self.load_mmap_cache(dim) {
                    return Ok(cache.scan_single(query, limit, true));
                }
            }
        }
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
        accept: impl FnMut(&str, &str) -> bool,
        mut score_blob: impl FnMut(&[u8]) -> Result<f32>,
    ) -> Result<Vec<Hit>> {
        self.rank_bounded(limit, per_file, accept, |blob, _| {
            score_blob(blob).map(Some)
        })
    }
    /// The cutoff is a monotone exact-score lower bound. A scorer may return
    /// None only when its outward upper bound is strictly below this cutoff.
    pub fn rank_bounded(
        &self,
        limit: usize,
        per_file: bool,
        mut accept: impl FnMut(&str, &str) -> bool,
        mut score_blob: impl FnMut(&[u8], Option<f32>) -> Result<Option<f32>>,
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
        let keep = |heap: &mut BinaryHeap<Reverse<Ranked>>, hit: Hit| {
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
                    keep(&mut heap, hit);
                }
                current_id = Some(id);
            }
            let path_ref = row.get_ref(1)?.as_str()?;
            let kind_ref = row.get_ref(2)?.as_str()?;
            if !accept(path_ref, kind_ref) {
                continue;
            }
            let blob_ref = row.get_ref(5)?.as_blob()?;
            let cutoff = if heap.len() == limit {
                heap.peek().map(|worst| worst.0.0.score)
            } else {
                None
            };
            let cutoff = if per_file {
                best.as_ref()
                    .map(|old| cutoff.map_or(old.score, |c| c.max(old.score)))
                    .or(cutoff)
            } else {
                cutoff
            };
            let Some(score) = score_blob(blob_ref, cutoff)? else {
                continue;
            };
            if !score.is_finite() {
                bail!("nonfinite retrieval score");
            }
            if per_file && best.as_ref().is_some_and(|old| score <= old.score) {
                continue;
            }
            let hit = Hit {
                line_kind: crate::document::line_kind(path_ref, kind_ref),
                path: path_ref.to_owned(),
                kind: kind_ref.to_owned(),
                score,
                start_line: row.get::<_, i64>(3)? as usize,
                end_line: row.get::<_, i64>(4)? as usize,
                chunk_index: Some(row.get::<_, i64>(6)? as usize),
            };
            if !per_file {
                keep(&mut heap, hit);
            } else {
                best = Some(hit);
            }
        }
        if let Some(hit) = best {
            keep(&mut heap, hit);
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
    /// Metadata-only paths at all three retrieval levels. No vectors or source
    /// contents are read; folder results are deduplicated indexed ancestors.
    pub fn locate(&self, query: &str, level: &str, limit: usize) -> Result<Vec<Hit>> {
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        if level == "file" {
            let matcher = FilenameMatcher::new(query);
            let mut statement = self
                .conn
                .prepare("SELECT path,kind FROM documents ORDER BY path")?;
            let mut rows = statement.query([])?;
            let mut hits = Vec::new();
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                let normalized = FilenameMatcher::normalize(&path);
                let name_match = matcher.matches_normalized(&normalized, true);
                if name_match || matcher.matches_normalized(&normalized, false) {
                    hits.push((
                        !name_match,
                        Hit {
                            path,
                            kind: row.get(1)?,
                            line_kind: "none",
                            score: 1.0,
                            start_line: 0,
                            end_line: 0,
                            chunk_index: None,
                        },
                    ));
                }
            }
            hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
            hits.truncate(limit);
            return Ok(hits.into_iter().map(|(_, hit)| hit).collect());
        }
        let mut out = Vec::new();
        if level == "folder" {
            let mut folders = std::collections::BTreeSet::new();
            let mut statement = self.conn.prepare("SELECT path FROM documents")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                for parent in Path::new(&path).ancestors().skip(1) {
                    let value = parent.to_string_lossy();
                    if terms.iter().all(|term| value.to_lowercase().contains(term)) {
                        folders.insert(value.into_owned());
                    }
                }
            }
            for path in folders {
                out.push(Hit {
                    path,
                    kind: "folder".into(),
                    score: 1.0,
                    line_kind: "none",
                    start_line: 0,
                    end_line: 0,
                    chunk_index: None,
                });
            }
            out.sort_by_key(|hit| {
                let name = Path::new(&hit.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                (
                    !terms.iter().all(|term| name.contains(term)),
                    hit.path.clone(),
                )
            });
            out.truncate(limit);
        } else if level == "chunk" {
            let mut statement = self.conn.prepare("SELECT d.path,d.kind,c.start_line,c.end_line,c.ordinal FROM documents d JOIN chunks c ON c.doc_id=d.id WHERE d.kind='text' ORDER BY d.path,c.ordinal")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path: String = row.get(0)?;
                if !terms.iter().all(|term| path.to_lowercase().contains(term)) {
                    continue;
                }
                out.push(Hit {
                    line_kind: crate::document::line_kind(&path, "text"),
                    path,
                    kind: row.get(1)?,
                    score: 1.0,
                    start_line: row.get::<_, i64>(2)? as usize,
                    end_line: row.get::<_, i64>(3)? as usize,
                    chunk_index: Some(row.get::<_, i64>(4)? as usize),
                });
                if out.len() == limit {
                    break;
                }
            }
        } else {
            bail!("unknown path level: {level}");
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

    pub fn set_view(&self, name: &str, path: &str, kind: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO views (name, path, kind) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET path=excluded.path, kind=excluded.kind",
            params![name, path, kind],
        )?;
        Ok(())
    }

    pub fn get_view(&self, name: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM views WHERE name = ?1")?;
        let mut rows = stmt.query([name])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_views(&self) -> Result<Vec<(String, String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, path, kind FROM views ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        let mut views = Vec::new();
        for r in rows {
            views.push(r?);
        }
        Ok(views)
    }

    pub fn delete_view(&self, name: &str) -> Result<bool> {
        let count = self
            .conn
            .execute("DELETE FROM views WHERE name = ?1", [name])?;
        Ok(count > 0)
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

    pub fn load_flat_cache(&self, dim: usize) -> Result<FlatVectorCache> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.path, d.kind, c.start_line, c.end_line, c.embedding, c.ordinal
             FROM chunks c JOIN documents d ON d.id=c.doc_id ORDER BY c.doc_id, c.ordinal",
        )?;
        let mut rows = stmt.query([])?;
        let mut cache = FlatVectorCache {
            count: 0,
            dim,
            doc_ids: Vec::new(),
            ordinals: Vec::new(),
            scales: Vec::new(),
            paths: Vec::new(),
            kinds: Vec::new(),
            start_lines: Vec::new(),
            end_lines: Vec::new(),
            quantized_bytes: Vec::new(),
        };

        while let Some(row) = rows.next()? {
            let blob_ref = row.get_ref(5)?.as_blob()?;
            if blob_ref.len() != dim + 4 {
                continue;
            }
            let scale = f32::from_le_bytes(blob_ref[..4].try_into().unwrap());
            let q8_slice = &blob_ref[4..];

            cache.doc_ids.push(row.get(0)?);
            cache.paths.push(row.get(1)?);
            cache.kinds.push(row.get(2)?);
            cache.start_lines.push(row.get::<_, i64>(3)? as u32);
            cache.end_lines.push(row.get::<_, i64>(4)? as u32);
            cache.scales.push(scale);
            cache.ordinals.push(row.get::<_, i64>(6)? as u32);
            cache.quantized_bytes.extend_from_slice(q8_slice);
            cache.count += 1;
        }

        Ok(cache)
    }

    // -----------------------------------------------------------------------
    // Persistent mmap vector cache (vectors.bin)
    // -----------------------------------------------------------------------

    /// Write a compact binary snapshot of all Q8 embeddings to `vec_path`
    /// (adjacent to the sqlite file as `<db>.vectors.bin`).
    ///
    /// Format:
    ///   Header  24 B: magic(8) + count(4) + dim(4) + string_pool_size(4) + pad(4)
    ///   Rows  N×32 B: doc_id(8) + ordinal(4) + start_line(4) + end_line(4)
    ///                  + scale(4) + path_off(4) + kind_off(4)
    ///   String pool: null-terminated path and kind strings packed together
    ///   Vectors N×dim B: raw Q8 bytes for SIMD direct access
    ///
    /// The file is written atomically via a temp-file + rename.
    pub fn flush_vectors_bin(&self, dim: usize) -> Result<usize> {
        if self.vec_path.as_os_str().is_empty() {
            return Ok(0); // disabled for :memory:
        }
        use std::io::Write;

        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.path, d.kind, c.start_line, c.end_line, c.embedding, c.ordinal
             FROM chunks c JOIN documents d ON d.id=c.doc_id ORDER BY c.doc_id, c.ordinal",
        )?;
        let mut rows = stmt.query([])?;

        struct Row {
            doc_id: i64,
            path: String,
            kind: String,
            start_line: u32,
            end_line: u32,
            ordinal: u32,
            scale: f32,
        }

        let mut meta: Vec<Row> = Vec::new();
        // Collect raw Q8 bytes in one contiguous buffer to avoid many small allocations.
        let mut all_q8: Vec<u8> = Vec::new();

        while let Some(row) = rows.next()? {
            let blob = row.get_ref(5)?.as_blob()?;
            if blob.len() != dim + 4 {
                continue;
            }
            let scale = f32::from_le_bytes(blob[..4].try_into().unwrap());
            all_q8.extend_from_slice(&blob[4..]);
            meta.push(Row {
                doc_id: row.get(0)?,
                path: row.get(1)?,
                kind: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as u32,
                end_line: row.get::<_, i64>(4)? as u32,
                ordinal: row.get::<_, i64>(6)? as u32,
                scale,
            });
        }

        let count = meta.len();

        // Build interleaved string pool (path0\0kind0\0path1\0kind1\0…)
        let mut pool: Vec<u8> = Vec::new();
        let mut path_offs: Vec<u32> = Vec::with_capacity(count);
        let mut kind_offs: Vec<u32> = Vec::with_capacity(count);
        for r in &meta {
            path_offs.push(pool.len() as u32);
            pool.extend_from_slice(r.path.as_bytes());
            pool.push(0);
            kind_offs.push(pool.len() as u32);
            pool.extend_from_slice(r.kind.as_bytes());
            pool.push(0);
        }
        let pool_size = pool.len() as u32;

        // Write to a temp file first, then rename for atomicity.
        let mut tmp = self.vec_path.clone();
        tmp.as_mut_os_string().push(".tmp");
        {
            let mut f =
                std::fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;

            // Header (24 bytes)
            f.write_all(b"WEFTVEC1")?;
            f.write_all(&(count as u32).to_le_bytes())?;
            f.write_all(&(dim as u32).to_le_bytes())?;
            f.write_all(&pool_size.to_le_bytes())?;
            f.write_all(&0u32.to_le_bytes())?; // pad

            // Row table (count × 32 bytes)
            for (i, r) in meta.iter().enumerate() {
                f.write_all(&r.doc_id.to_le_bytes())?;
                f.write_all(&r.ordinal.to_le_bytes())?;
                f.write_all(&r.start_line.to_le_bytes())?;
                f.write_all(&r.end_line.to_le_bytes())?;
                f.write_all(&r.scale.to_le_bytes())?;
                f.write_all(&path_offs[i].to_le_bytes())?;
                f.write_all(&kind_offs[i].to_le_bytes())?;
            }

            // String pool
            f.write_all(&pool)?;

            // Vectors (count × dim bytes, Q8 raw)
            f.write_all(&all_q8)?;
            f.flush()?;
        }
        std::fs::rename(&tmp, &self.vec_path)
            .with_context(|| format!("rename vectors.bin: {}", self.vec_path.display()))?;

        Ok(count)
    }

    /// Open the companion `vectors.bin` as a memory-mapped read-only view.
    /// Returns an error if the file does not exist, is stale (wrong dim), or is corrupt.
    pub fn load_mmap_cache(&self, dim: usize) -> Result<MmapVectorCache> {
        if self.vec_path.as_os_str().is_empty() {
            bail!("mmap cache disabled for in-memory store");
        }
        let file = std::fs::File::open(&self.vec_path)
            .with_context(|| format!("open {}", self.vec_path.display()))?;
        let mmap = unsafe { Mmap::map(&file) }?;
        MmapVectorCache::parse(mmap, dim)
    }
}

// ---------------------------------------------------------------------------
// Persistent mmap vector cache implementation
// ---------------------------------------------------------------------------

const MMAP_MAGIC: &[u8; 8] = b"WEFTVEC1";
const MMAP_HEADER_SIZE: usize = 24;
const MMAP_ROW_SIZE: usize = 32;

/// Zero-copy view into `vectors.bin` via OS memory mapping.
///
/// Keeps the `Mmap` alive (so the OS page mapping stays live) and exposes
/// per-chunk accessors that read directly from the mapped pages — no heap
/// copying. When the file is in the kernel's page cache (common after the
/// first search), reads are essentially L3/RAM speed.
pub struct MmapVectorCache {
    _mmap: Mmap,
    pub count: usize,
    pub dim: usize,
    rows_off: usize,
    pool_off: usize,
    vecs_off: usize,
}

impl MmapVectorCache {
    fn parse(mmap: Mmap, dim: usize) -> Result<Self> {
        if mmap.len() < MMAP_HEADER_SIZE {
            bail!("vectors.bin too small ({} bytes)", mmap.len());
        }
        if &mmap[..8] != MMAP_MAGIC {
            bail!("vectors.bin: bad magic — rebuild with `weft index`");
        }
        let count = u32::from_le_bytes(mmap[8..12].try_into().unwrap()) as usize;
        let file_dim = u32::from_le_bytes(mmap[12..16].try_into().unwrap()) as usize;
        let pool_size = u32::from_le_bytes(mmap[16..20].try_into().unwrap()) as usize;

        if file_dim != dim {
            bail!("vectors.bin dim={file_dim} != index dim={dim} — rebuild with `weft index`");
        }

        let rows_off = MMAP_HEADER_SIZE;
        let pool_off = rows_off + count * MMAP_ROW_SIZE;
        let vecs_off = pool_off + pool_size;
        let need = vecs_off + count * dim;
        if mmap.len() < need {
            bail!(
                "vectors.bin truncated: need {need} B, have {} B — rebuild with `weft index`",
                mmap.len()
            );
        }

        Ok(Self {
            _mmap: mmap,
            count,
            dim,
            rows_off,
            pool_off,
            vecs_off,
        })
    }

    // --- zero-copy per-row accessors ----------------------------------------

    #[inline]
    fn row(&self, i: usize) -> &[u8] {
        let b = self.rows_off + i * MMAP_ROW_SIZE;
        &self._mmap[b..b + MMAP_ROW_SIZE]
    }

    #[inline]
    fn row_i64_fast(&self, i: usize) -> i64 {
        #[cfg(target_endian = "big")]
        {
            return self.doc_id(i);
        }
        #[cfg(target_endian = "little")]
        unsafe {
            std::ptr::read_unaligned(
                self._mmap.as_ptr().add(self.rows_off + i * MMAP_ROW_SIZE) as *const i64
            )
        }
    }

    #[inline]
    fn row_f32_fast(&self, i: usize) -> f32 {
        #[cfg(target_endian = "big")]
        {
            return self.scale(i);
        }
        #[cfg(target_endian = "little")]
        unsafe {
            std::ptr::read_unaligned(
                self._mmap
                    .as_ptr()
                    .add(self.rows_off + i * MMAP_ROW_SIZE + 20) as *const f32,
            )
        }
    }

    #[inline]
    pub fn doc_id(&self, i: usize) -> i64 {
        i64::from_le_bytes(self.row(i)[0..8].try_into().unwrap())
    }
    #[inline]
    pub fn ordinal(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.row(i)[8..12].try_into().unwrap())
    }
    #[inline]
    pub fn start_line(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.row(i)[12..16].try_into().unwrap())
    }
    #[inline]
    pub fn end_line(&self, i: usize) -> u32 {
        u32::from_le_bytes(self.row(i)[16..20].try_into().unwrap())
    }
    #[inline]
    pub fn scale(&self, i: usize) -> f32 {
        f32::from_le_bytes(self.row(i)[20..24].try_into().unwrap())
    }

    #[inline]
    fn str_at(&self, off: u32) -> &str {
        let s = self.pool_off + off as usize;
        let bytes = &self._mmap[s..];
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        std::str::from_utf8(&bytes[..len]).unwrap_or("")
    }

    #[inline]
    pub fn path(&self, i: usize) -> &str {
        let off = u32::from_le_bytes(self.row(i)[24..28].try_into().unwrap());
        self.str_at(off)
    }

    #[inline]
    pub fn kind(&self, i: usize) -> &str {
        let off = u32::from_le_bytes(self.row(i)[28..32].try_into().unwrap());
        self.str_at(off)
    }

    #[inline]
    pub fn q8(&self, i: usize) -> &[u8] {
        let b = self.vecs_off + i * self.dim;
        &self._mmap[b..b + self.dim]
    }

    // --- WQL SIMD scan (same logic as FlatVectorCache::scan_wql) -----------

    pub fn scan_wql(
        &self,
        compiled: &crate::wql::CompiledQuery,
        limit: usize,
        per_file: bool,
        accept: impl FnMut(&str, &str) -> bool,
    ) -> Vec<Hit> {
        self.scan_wql_impl::<true, _>(compiled, limit, per_file, accept)
    }

    /// Unscoped searches do not need to fetch and decode paths for every
    /// document. Keep the filter decision outside the hot vector loop.
    pub fn scan_wql_unfiltered(
        &self,
        compiled: &crate::wql::CompiledQuery,
        limit: usize,
        per_file: bool,
    ) -> Vec<Hit> {
        self.scan_wql_impl::<false, _>(compiled, limit, per_file, |_, _| true)
    }

    #[inline]
    fn scan_wql_impl<const FILTERED: bool, F: FnMut(&str, &str) -> bool>(
        &self,
        compiled: &crate::wql::CompiledQuery,
        limit: usize,
        per_file: bool,
        mut accept: F,
    ) -> Vec<Hit> {
        use std::collections::BinaryHeap;
        if limit == 0 || compiled.branches.is_empty() {
            return Vec::new();
        }
        struct Ri {
            idx: usize,
            dist: f32,
        }
        impl PartialEq for Ri {
            fn eq(&self, o: &Self) -> bool {
                self.dist == o.dist
            }
        }
        impl Eq for Ri {}
        impl PartialOrd for Ri {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                self.dist.partial_cmp(&o.dist)
            }
        }
        impl Ord for Ri {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                self.dist.total_cmp(&o.dist)
            }
        }

        let mut heap: BinaryHeap<Ri> = BinaryHeap::new();
        let mut cur_id: Option<i64> = None;
        let mut best_file: Option<(usize, f32)> = None;
        let mut allowed = false;

        let push = |heap: &mut BinaryHeap<Ri>, item: Ri| {
            if heap.len() < limit {
                heap.push(item);
            } else if let Some(w) = heap.peek() {
                if item.dist < w.dist {
                    heap.pop();
                    heap.push(item);
                }
            }
        };

        for i in 0..self.count {
            let did = self.row_i64_fast(i);
            if cur_id != Some(did) {
                if let Some((bi, bd)) = best_file.take() {
                    push(&mut heap, Ri { idx: bi, dist: bd });
                }
                cur_id = Some(did);
                // A document's path and kind are constant for all of its chunks.
                // Avoid decoding the same NUL-terminated strings for every vector.
                if FILTERED {
                    allowed = accept(self.path(i), self.kind(i));
                }
            }
            if FILTERED && !allowed {
                continue;
            }

            let q8 = self.q8(i);
            let scale = self.row_f32_fast(i);
            let mut min_dist = f32::INFINITY;
            if let [b0, b1] = compiled.branches.as_slice() {
                let (s0, s1) = crate::simd::q8_dot2(q8, &b0.vector, &b1.vector);
                min_dist = (b0.bias - s0 * scale).min(b1.bias - s1 * scale);
            } else {
                for branch in &compiled.branches {
                    let dot = crate::simd::q8_dot(q8, &branch.vector) * scale;
                    let s = branch.bias - dot;
                    if s < min_dist {
                        min_dist = s;
                    }
                }
            }
            if !min_dist.is_finite() {
                continue;
            }

            if per_file {
                if best_file.as_ref().is_none_or(|(_, d)| min_dist < *d) {
                    best_file = Some((i, min_dist));
                }
            } else {
                push(
                    &mut heap,
                    Ri {
                        idx: i,
                        dist: min_dist,
                    },
                );
            }
        }
        if let Some((bi, bd)) = best_file {
            push(&mut heap, Ri { idx: bi, dist: bd });
        }

        let mut results: Vec<Ri> = heap.into_iter().collect();
        results.sort_by(|a, b| a.dist.total_cmp(&b.dist));
        results
            .into_iter()
            .map(|item| {
                let i = item.idx;
                Hit {
                    line_kind: crate::document::line_kind(self.path(i), self.kind(i)),
                    path: self.path(i).to_owned(),
                    kind: self.kind(i).to_owned(),
                    score: -item.dist,
                    start_line: self.start_line(i) as usize,
                    end_line: self.end_line(i) as usize,
                    chunk_index: Some(self.ordinal(i) as usize),
                }
            })
            .collect()
    }

    pub fn scan_single(&self, query: &[f32], limit: usize, per_file: bool) -> Vec<Hit> {
        use std::{cmp::Reverse, collections::BinaryHeap};
        if limit == 0 || query.len() != self.dim {
            return Vec::new();
        }
        #[derive(Debug)]
        struct Item<'a> {
            idx: usize,
            score: f32,
            path: &'a str,
            ordinal: u32,
        }
        impl PartialEq for Item<'_> {
            fn eq(&self, o: &Self) -> bool {
                self.cmp(o).is_eq()
            }
        }
        impl Eq for Item<'_> {}
        impl PartialOrd for Item<'_> {
            fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(o))
            }
        }
        impl Ord for Item<'_> {
            fn cmp(&self, o: &Self) -> std::cmp::Ordering {
                self.score
                    .total_cmp(&o.score)
                    .then_with(|| o.path.cmp(self.path))
                    .then_with(|| o.ordinal.cmp(&self.ordinal))
            }
        }

        let mut heap = BinaryHeap::new();
        let mut current = None;
        let mut best: Option<(usize, f32)> = None;
        let mut current_path = "";
        fn push<'a>(heap: &mut BinaryHeap<Reverse<Item<'a>>>, item: Item<'a>, limit: usize) {
            if heap.len() < limit {
                heap.push(Reverse(item));
            } else if heap.peek().is_some_and(|w| item > w.0) {
                heap.pop();
                heap.push(Reverse(item));
            }
        }
        for i in 0..self.count {
            let doc_id = self.row_i64_fast(i);
            if current != Some(doc_id) {
                if let Some((idx, score)) = best.take() {
                    push(
                        &mut heap,
                        Item {
                            idx,
                            score,
                            path: current_path,
                            ordinal: self.ordinal(idx),
                        },
                        limit,
                    );
                }
                current = Some(doc_id);
                current_path = self.path(i);
            }
            let score = crate::simd::q8_dot(self.q8(i), query) * self.row_f32_fast(i);
            if !score.is_finite() {
                continue;
            }
            if per_file {
                if best.as_ref().is_none_or(|x| score > x.1) {
                    best = Some((i, score));
                }
            } else {
                push(
                    &mut heap,
                    Item {
                        idx: i,
                        score,
                        path: current_path,
                        ordinal: self.ordinal(i),
                    },
                    limit,
                );
            }
        }
        if let Some((idx, score)) = best {
            push(
                &mut heap,
                Item {
                    idx,
                    score,
                    path: current_path,
                    ordinal: self.ordinal(idx),
                },
                limit,
            );
        }
        let mut out: Vec<_> = heap.into_iter().map(|x| x.0).collect();
        out.sort_by(|a, b| b.cmp(a));
        out.into_iter()
            .map(|x| Hit {
                line_kind: crate::document::line_kind(self.path(x.idx), self.kind(x.idx)),
                path: self.path(x.idx).to_owned(),
                kind: self.kind(x.idx).to_owned(),
                score: x.score,
                start_line: self.start_line(x.idx) as usize,
                end_line: self.end_line(x.idx) as usize,
                chunk_index: Some(self.ordinal(x.idx) as usize),
            })
            .collect()
    }
}

#[derive(Clone, Default)]
pub struct FlatVectorCache {
    pub count: usize,
    pub dim: usize,
    pub doc_ids: Vec<i64>,
    pub ordinals: Vec<u32>,
    pub scales: Vec<f32>,
    pub paths: Vec<String>,
    pub kinds: Vec<String>,
    pub start_lines: Vec<u32>,
    pub end_lines: Vec<u32>,
    pub quantized_bytes: Vec<u8>,
}

impl FlatVectorCache {
    pub fn scan_wql(
        &self,
        compiled: &crate::wql::CompiledQuery,
        limit: usize,
        per_file: bool,
        mut accept: impl FnMut(&str, &str) -> bool,
    ) -> Vec<Hit> {
        use std::collections::BinaryHeap;
        #[derive(Debug)]
        struct RankedItem {
            idx: usize,
            dist: f32,
        }
        impl PartialEq for RankedItem {
            fn eq(&self, other: &Self) -> bool {
                self.dist == other.dist
            }
        }
        impl Eq for RankedItem {}
        impl PartialOrd for RankedItem {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                self.dist.partial_cmp(&other.dist)
            }
        }
        impl Ord for RankedItem {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.dist.total_cmp(&other.dist)
            }
        }

        let mut heap: BinaryHeap<RankedItem> = BinaryHeap::new();
        let mut current_id = None;
        let mut best_in_file: Option<(usize, f32)> = None;

        let push_heap = |heap: &mut BinaryHeap<RankedItem>, item: RankedItem| {
            if heap.len() < limit {
                heap.push(item);
            } else if let Some(worst) = heap.peek() {
                if item.dist < worst.dist {
                    heap.pop();
                    heap.push(item);
                }
            }
        };

        let dim = self.dim;
        for i in 0..self.count {
            let doc_id = self.doc_ids[i];
            if current_id != Some(doc_id) {
                if let Some((b_idx, b_dist)) = best_in_file.take() {
                    push_heap(
                        &mut heap,
                        RankedItem {
                            idx: b_idx,
                            dist: b_dist,
                        },
                    );
                }
                current_id = Some(doc_id);
            }

            if !accept(&self.paths[i], &self.kinds[i]) {
                continue;
            }

            let q8_slice = &self.quantized_bytes[i * dim..(i + 1) * dim];
            let scale = self.scales[i];

            let mut min_dist = f32::INFINITY;
            for branch in &compiled.branches {
                let dot = crate::simd::q8_dot(q8_slice, &branch.vector) * scale;
                let s = branch.bias - dot;
                if s < min_dist {
                    min_dist = s;
                }
            }

            if !min_dist.is_finite() {
                continue;
            }

            if per_file {
                if best_in_file.as_ref().is_none_or(|(_, d)| min_dist < *d) {
                    best_in_file = Some((i, min_dist));
                }
            } else {
                push_heap(
                    &mut heap,
                    RankedItem {
                        idx: i,
                        dist: min_dist,
                    },
                );
            }
        }

        if let Some((b_idx, b_dist)) = best_in_file {
            push_heap(
                &mut heap,
                RankedItem {
                    idx: b_idx,
                    dist: b_dist,
                },
            );
        }

        let mut results: Vec<RankedItem> = heap.into_iter().collect();
        results.sort_by(|a, b| a.dist.total_cmp(&b.dist));

        results
            .into_iter()
            .map(|item| {
                let idx = item.idx;
                Hit {
                    line_kind: crate::document::line_kind(&self.paths[idx], &self.kinds[idx]),
                    path: self.paths[idx].clone(),
                    kind: self.kinds[idx].clone(),
                    score: -item.dist,
                    start_line: self.start_lines[idx] as usize,
                    end_line: self.end_lines[idx] as usize,
                    chunk_index: Some(self.ordinals[idx] as usize),
                }
            })
            .collect()
    }
}

pub fn encode(v: &[f32], enc: Encoding) -> Vec<u8> {
    pack(v, enc)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_initial_search_handles_nfc_nfd_mixed_queries_and_parent_scoping() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        for path in [
            "/docs/데이터베이스.pdf",
            "/docs/회의자료.PDF",
            "/docs/회의록.pdf",
            "/docs/까치.md",
            "/회의자료/plain.pdf",
        ] {
            store.replace(path, "hash", "text", 1, &[]).unwrap();
        }
        assert_eq!(
            store.locate("ㄷㅇㅌㅂㅇㅅ", "file", 30).unwrap()[0].path,
            "/docs/데이터베이스.pdf"
        );
        assert_eq!(store.locate("ㅎㅇ.pdf", "file", 30).unwrap().len(), 0); // initials remain contiguous
        assert_eq!(store.locate("ㅎㅇㅈㄹ.pdf", "file", 30).unwrap().len(), 1);
        assert_eq!(store.locate("회ㅇ", "file", 30).unwrap().len(), 2);
        assert_eq!(store.locate("ㅎㅇ .pdf", "file", 30).unwrap().len(), 2);
        assert_eq!(store.locate("회의록", "file", 30).unwrap().len(), 1);
        assert_eq!(store.locate("회의록", "file", 30).unwrap().len(), 1);
        assert_eq!(store.locate("ㄲㅊ", "file", 30).unwrap().len(), 1);
        assert!(store.locate("ㄱㅊ", "file", 30).unwrap().is_empty());
        assert_eq!(store.locate("ㅎㅇ", "file", 1).unwrap().len(), 1);
        assert_eq!(store.locate("plain.pdf", "file", 30).unwrap().len(), 1);
    }
    #[test]
    fn locate_returns_files_folders_and_chunks_without_inference() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let chunks = vec![
            (2, 5, encode(&[1.0, 0.0], Encoding::Q8), None),
            (6, 10, encode(&[0.0, 1.0], Encoding::Q8), None),
        ];
        store
            .replace("/docs/budget/report.md", "hash", "text", 10, &chunks)
            .unwrap();
        store
            .replace("/docs/budget/notes.md", "hash", "text", 10, &chunks)
            .unwrap();
        assert_eq!(store.locate("report", "file", 30).unwrap().len(), 1);
        let folders = store.locate("budget", "folder", 30).unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(
            (&*folders[0].path, &*folders[0].kind),
            ("/docs/budget", "folder")
        );
        let hits = store.locate("report", "chunk", 30).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            (hits[1].start_line, hits[1].end_line, hits[1].chunk_index),
            (6, 10, Some(1))
        );
        assert!(store.locate("", "file", 30).unwrap().is_empty());
        assert_eq!(store.locate("report", "chunk", 1).unwrap().len(), 1);
    }
    #[test]
    fn metadata_and_global_embedding_caches_survive_replacement_and_removal() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let input_hash = [7u8; 32];
        let embedding = encode(&[1.0, 0.0], Encoding::Q8);
        let vectors = vec![(1, 1, embedding.clone(), Some(input_hash))];
        store
            .replace("/docs/a.txt", "file-hash", "text", 12, &vectors)
            .unwrap();
        store.set_stat("/docs/a.txt", "stamp-1").unwrap();
        assert!(
            store
                .unchanged_stat("/docs/a.txt", "text", "stamp-1")
                .unwrap()
        );
        assert!(
            !store
                .unchanged_stat("/docs/a.txt", "text", "stamp-2")
                .unwrap()
        );
        assert_eq!(
            store.cached_embedding(&input_hash).unwrap().unwrap(),
            embedding
        );
        assert_eq!(store.remove_missing(Path::new("/docs/a.txt")).unwrap(), 1);
        assert!(store.cached_embedding(&input_hash).unwrap().is_some());
    }
    #[test]
    #[ignore = "local flush microbenchmark on an isolated DB copy; WEFT_FLUSH_BENCH_DB"]
    fn benchmark_snapshot_flush() {
        let db = std::env::var("WEFT_FLUSH_BENCH_DB").expect("WEFT_FLUSH_BENCH_DB required");
        let store = Store::open(Path::new(&db)).unwrap();
        let dim: usize = store.setting("dim").unwrap().unwrap().parse().unwrap();
        let start = std::time::Instant::now();
        let rows = store.flush_vectors_bin(dim).unwrap();
        let ms = start.elapsed().as_secs_f64() * 1000.;
        let file_bytes = std::fs::metadata(&store.vec_path).unwrap().len();
        let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
        unsafe {
            libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        }
        #[cfg(target_os = "macos")]
        let peak_mib = usage.ru_maxrss as f64 / (1024. * 1024.);
        #[cfg(not(target_os = "macos"))]
        let peak_mib = usage.ru_maxrss as f64 / 1024.;
        println!(
            "flush rows={rows} elapsed_ms={ms:.2} snapshot_bytes={file_bytes} peak_rss_mib={peak_mib:.1}"
        );
        assert_eq!(store.load_mmap_cache(dim).unwrap().count, rows);
    }
    #[test]
    #[ignore = "local scan microbenchmark; set WEFT_BENCH_DB"]
    fn benchmark_mmap_scan() {
        let db = std::env::var("WEFT_BENCH_DB").expect("WEFT_BENCH_DB required");
        let store = Store::open_read_only(Path::new(&db)).unwrap();
        let dim: usize = store.setting("dim").unwrap().unwrap().parse().unwrap();
        let cache = store.load_mmap_cache(dim).unwrap();
        let query: Vec<f32> = (0..dim)
            .map(|i| ((i * 97 % 313) as f32 - 155.0) / (dim as f32))
            .collect();
        let compiled = crate::wql::CompiledQuery::new(vec![
            crate::wql::Affine::new(0.5, query.clone()),
            crate::wql::Affine::new(0.5, query.iter().rev().copied().collect()),
        ]);
        fn sample(mut task: impl FnMut()) -> f64 {
            let mut samples = Vec::new();
            for _ in 0..12 {
                let t = std::time::Instant::now();
                task();
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(|a, b| a.total_cmp(b));
            samples[6]
        }
        let a = sample(|| {
            std::hint::black_box(cache.scan_single(&query, 10, true));
        });
        let b = sample(|| {
            std::hint::black_box(cache.scan_wql(&compiled, 10, true, |_, _| true));
        });
        let unfiltered = sample(|| {
            std::hint::black_box(cache.scan_wql_unfiltered(&compiled, 10, true));
        });
        let c = sample(|| {
            std::hint::black_box(
                cache.scan_wql(&compiled, 10, true, |p, _| p.starts_with("/Users/")),
            );
        });
        let d = sample(|| {
            let mut total = 0f32;
            for i in 0..cache.count {
                total += crate::simd::q8_dot(cache.q8(i), &query) * cache.row_f32_fast(i);
            }
            std::hint::black_box(total);
        });
        let one = crate::wql::CompiledQuery::new(vec![crate::wql::Affine::new(0.5, query.clone())]);
        let e = sample(|| {
            std::hint::black_box(cache.scan_wql(&one, 10, true, |_, _| true));
        });
        let fast_one = sample(|| {
            std::hint::black_box(cache.scan_wql_unfiltered(&one, 10, true));
        });
        println!(
            "mmap count={} dim={} single_ms={a:.3} wql_2branch_ms={b:.3} wql_unfiltered_2branch_ms={unfiltered:.3} wql_filtered_ms={c:.3} raw_dot_ms={d:.3} wql_1branch_ms={e:.3} wql_unfiltered_1branch_ms={fast_one:.3}",
            cache.count, dim
        );

        // Interleave kernels on the exact same mapped vectors. This avoids
        // mistaking warmed pages or CPU frequency changes for a SIMD win.
        let kernels: [(&str, fn(&[u8], &[f32]) -> f32); 3] = [
            ("prior-8b", crate::simd::q8_dot_prior),
            ("current-16b", crate::simd::q8_dot),
            ("scalar", crate::simd::q8_dot_scalar),
        ];
        let mut timings: [Vec<f64>; 3] = std::array::from_fn(|_| Vec::new());
        let mut totals = [0f64; 3];
        for round in 0..27 {
            for k in 0..3 {
                let idx = (round + k) % 3;
                let before = std::time::Instant::now();
                let mut total = 0.0f64;
                for i in 0..cache.count {
                    total +=
                        (kernels[idx].1)(cache.q8(i), &query) as f64 * cache.row_f32_fast(i) as f64;
                }
                let elapsed_ms = before.elapsed().as_secs_f64() * 1000.0;
                timings[idx].push(elapsed_ms);
                totals[idx] = std::hint::black_box(total);
            }
        }
        for idx in 0..3 {
            timings[idx].sort_by(f64::total_cmp);
            let median = timings[idx][timings[idx].len() / 2];
            let p10 = timings[idx][2];
            let p90 = timings[idx][24];
            println!(
                "paired_kernel={} p10_ms={p10:.3} p50_ms={median:.3} p90_ms={p90:.3} checksum={:.6}",
                kernels[idx].0, totals[idx]
            );
        }
        assert!(
            (totals[0] - totals[1]).abs() < 0.01,
            "NEON variants diverged"
        );

        let other: Vec<f32> = query.iter().rev().copied().collect();
        let two_kernels: [(&str, fn(&[u8], &[f32], &[f32]) -> (f32, f32)); 2] = [
            ("separate-2pass", |bytes, q0, q1| {
                (
                    crate::simd::q8_dot(bytes, q0),
                    crate::simd::q8_dot(bytes, q1),
                )
            }),
            ("fused-1pass", crate::simd::q8_dot2),
        ];
        let mut paired: [Vec<f64>; 2] = std::array::from_fn(|_| Vec::new());
        let mut check = [(0f64, 0f64); 2];
        for round in 0..32 {
            for k in 0..2 {
                let idx = (round + k) % 2;
                let start = std::time::Instant::now();
                let mut accum = (0f64, 0f64);
                for i in 0..cache.count {
                    let (x, y) = two_kernels[idx].1(cache.q8(i), &query, &other);
                    let scale = cache.row_f32_fast(i) as f64;
                    accum.0 += x as f64 * scale;
                    accum.1 += y as f64 * scale;
                }
                paired[idx].push(start.elapsed().as_secs_f64() * 1000.0);
                check[idx] = std::hint::black_box(accum);
            }
        }
        for (idx, (name, _)) in two_kernels.iter().enumerate() {
            paired[idx].sort_by(f64::total_cmp);
            println!(
                "paired_2branch={name} p50_ms={:.3} p90_ms={:.3}",
                paired[idx][16], paired[idx][29]
            );
        }
        assert!((check[0].0 - check[1].0).abs() < 0.01);
        assert!((check[0].1 - check[1].1).abs() < 0.01);
    }

    #[test]
    fn mmap_hits_match_sqlite_ranking() {
        let root = std::env::temp_dir().join(format!(
            "weft-mmap-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("index.sqlite");
        let mut store = Store::open(&db).unwrap();
        store.set_setting("dim", "3").unwrap();
        for (path, vectors) in [
            ("/z.md", vec![[0.2, 0.5, 0.9], [0.4, 0.3, 0.9]]),
            ("/a.md", vec![[0.8, 0.1, 0.2], [0.7, 0.5, 0.3]]),
            ("/b.md", vec![[0.3, 0.3, 0.9], [0.3, 0.3, 0.9]]),
        ] {
            let chunks = vectors
                .iter()
                .enumerate()
                .map(|(i, v)| (i + 1, i + 1, pack(v, Encoding::Q8), None))
                .collect::<Vec<_>>();
            store.replace(path, "hash", "text", 1, &chunks).unwrap();
        }
        let q = [1.0, 0.0, 0.0];
        let sql = store
            .rank_by(2, true, |_, _| true, |b| similarity(&q, b, Encoding::Q8))
            .unwrap();
        store.flush_vectors_bin(3).unwrap();
        let mmap = store.hits(&q, Encoding::Q8, 2).unwrap();
        assert_eq!(
            mmap.iter().map(|h| &h.path).collect::<Vec<_>>(),
            sql.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        for (a, b) in mmap.iter().zip(&sql) {
            assert!((a.score - b.score).abs() < 1e-5);
        }
        drop(store);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn fused_mmap_two_branch_matches_unfused_flat_ranking() {
        let root = std::env::temp_dir().join(format!("weft-fused-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("fused.sqlite");
        let mut store = Store::open(&db).unwrap();
        let dim = 768;
        store.set_setting("dim", &dim.to_string()).unwrap();
        for file in 0..80 {
            let mut chunks = Vec::new();
            for sub in 0..3 {
                let v: Vec<f32> = (0..dim)
                    .map(|i| (((i * 19 + file * 73 + sub * 23) % 257) as f32 - 128.) / 151.)
                    .collect();
                chunks.push((sub + 1, sub + 1, pack(&v, Encoding::Q8), None));
            }
            store
                .replace(&format!("/fused/{file:03}.md"), "h", "text", 2, &chunks)
                .unwrap();
        }
        store.flush_vectors_bin(dim).unwrap();
        let mapped = store.load_mmap_cache(dim).unwrap();
        let flat = store.load_flat_cache(dim).unwrap();
        let qa: Vec<f32> = (0..dim)
            .map(|i| ((i * 79 % 211) as f32 - 105.) / 309.)
            .collect();
        let qb: Vec<f32> = (0..dim)
            .map(|i| ((i * 47 % 197) as f32 - 98.) / 217.)
            .collect();
        let compiled = crate::wql::CompiledQuery::new(vec![
            crate::wql::Affine::new(0.25, qa),
            crate::wql::Affine::new(-0.15, qb),
        ]);
        for per_file in [true, false] {
            let got = mapped.scan_wql(&compiled, 35, per_file, |_, _| true);
            let fast = mapped.scan_wql_unfiltered(&compiled, 35, per_file);
            let want = flat.scan_wql(&compiled, 35, per_file, |_, _| true);
            assert_eq!(got.len(), want.len());
            assert_eq!(got.len(), fast.len());
            for (a, b) in got.iter().zip(&fast) {
                assert_eq!((&a.path, a.chunk_index), (&b.path, b.chunk_index));
                assert_eq!(a.score, b.score);
            }
            for (a, b) in got.iter().zip(&want) {
                assert_eq!((&a.path, a.chunk_index), (&b.path, b.chunk_index));
                assert!(
                    (a.score - b.score).abs() < 1e-5,
                    "{}: {} vs {}",
                    a.path,
                    a.score,
                    b.score
                );
            }
        }
        drop(mapped);
        drop(flat);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
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
            "weft-migration-{}-{}.sqlite",
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
