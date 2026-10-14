//! Contentless lexical postings. The caller owns schema/replacement transactions;
//! vector-only updates must leave this index and its coverage markers alone.

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;

const MAX_TERMS: usize = 16;
const MAX_CANDIDATES: usize = 200;

#[derive(Clone, Debug)]
pub struct LexicalHit {
    pub hit: crate::store::Hit,
    /// Fraction of distinct original query units matched anywhere in this file.
    pub coverage: f32,
}

pub fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts
             USING fts5(tokens, content='', contentless_delete=1);
         CREATE TABLE IF NOT EXISTS lexical_chunks (
             rowid INTEGER PRIMARY KEY,
             doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
             ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
             start_line INTEGER NOT NULL CHECK(start_line >= 0),
             end_line INTEGER NOT NULL CHECK(end_line >= start_line),
             UNIQUE(doc_id, ordinal)
         );
         CREATE TABLE IF NOT EXISTS lexical_documents (
             doc_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE,
             source_hash TEXT
         );
         CREATE TRIGGER IF NOT EXISTS lexical_chunks_delete
             AFTER DELETE ON lexical_chunks BEGIN
                 DELETE FROM chunk_fts WHERE rowid = old.rowid;
             END;",
    )?;
    let has_source_hash = conn
        .prepare("PRAGMA table_info(lexical_documents)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "source_hash");
    if !has_source_hash {
        // An old marker cannot prove its postings describe the current hash.
        // NULL leaves it uncovered so the normal indexing path backfills it.
        conn.execute(
            "ALTER TABLE lexical_documents ADD COLUMN source_hash TEXT",
            [],
        )?;
    }
    Ok(())
}

/// Replace lexical chunks, even for an empty/non-text document. Pass the caller's
/// transaction as `conn` to make this atomic with the document/vector update.
pub fn replace(conn: &Connection, doc_id: i64, texts: &[(usize, usize, String)]) -> Result<()> {
    conn.execute("DELETE FROM lexical_chunks WHERE doc_id=?1", [doc_id])?;
    conn.execute("DELETE FROM lexical_documents WHERE doc_id=?1", [doc_id])?;
    let mut mapping = conn.prepare(
        "INSERT INTO lexical_chunks(doc_id,ordinal,start_line,end_line) VALUES(?1,?2,?3,?4)",
    )?;
    let mut posting = conn.prepare("INSERT INTO chunk_fts(rowid,tokens) VALUES(?1,?2)")?;
    for (ordinal, (start, end, text)) in texts.iter().enumerate() {
        mapping.execute(params![doc_id, ordinal as i64, *start as i64, *end as i64])?;
        let rowid = conn.last_insert_rowid();
        posting.execute(params![rowid, index_tokens(text)])?;
    }
    conn.execute(
        "INSERT INTO lexical_documents(doc_id,source_hash)
         VALUES(?1,(SELECT hash FROM documents WHERE id=?1))",
        [doc_id],
    )?;
    Ok(())
}

fn schema_exists(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT count(*)=3 AND EXISTS(
             SELECT 1 FROM pragma_table_info('lexical_documents') WHERE name='source_hash'
         ) FROM sqlite_schema WHERE type='table'
         AND name IN ('chunk_fts','lexical_chunks','lexical_documents')",
        [],
        |r| r.get(0),
    )?)
}

/// Legacy read-only databases are uncovered; this function never creates tables.
pub fn has_document(conn: &Connection, path: &str) -> Result<bool> {
    if !schema_exists(conn)? {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM lexical_documents l
         JOIN documents d ON d.id=l.doc_id AND l.source_hash=d.hash WHERE d.path=?1)",
        [path],
        |r| r.get(0),
    )?)
}

/// Return one representative chunk per file, ordered by document coverage. All
/// postings are combined inside SQLite before LIMIT, so a full match cannot be
/// lost behind an early batch of partial matches. Rust retains at most 200 files.
pub fn search(conn: &Connection, query: &str, limit: usize) -> Result<Vec<LexicalHit>> {
    let units = query_units(query);
    if limit == 0 || units.is_empty() || !schema_exists(conn)? {
        return Ok(Vec::new());
    }
    let mut expressions = Vec::with_capacity(units.len());
    let mut exact = conn.prepare_cached(
        "SELECT 1 FROM chunk_fts JOIN lexical_chunks l ON l.rowid=chunk_fts.rowid
         JOIN documents d ON d.id=l.doc_id
         JOIN lexical_documents cover ON cover.doc_id=d.id AND cover.source_hash=d.hash
         WHERE chunk_fts MATCH ?1 LIMIT 1",
    )?;
    for unit in &units {
        let expression = unit.expression();
        let exists = exact
            .query_row([&expression], |_| Ok(()))
            .optional()?
            .is_some();
        expressions.push(if exists || unit.korean {
            expression
        } else {
            format!("{expression}*")
        });
    }

    // Do not cap each term separately: that would discard late full-coverage
    // documents. UNION ALL + distinct unit counts also handles cross-chunk hits.
    let union = (0..units.len())
        .map(|i| {
            format!(
                "SELECT l.doc_id,l.rowid,{i} AS unit FROM chunk_fts
             JOIN lexical_chunks l ON l.rowid=chunk_fts.rowid
             JOIN documents d ON d.id=l.doc_id
             JOIN lexical_documents cover ON cover.doc_id=d.id AND cover.source_hash=d.hash
             WHERE chunk_fts MATCH ?{}",
                i + 1
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    // Materialize once: both document coverage and best-chunk selection reuse
    // these postings instead of rescanning FTS for every candidate document.
    let sql = format!(
        "WITH matches(doc_id,rowid,unit) AS MATERIALIZED ({union}),
         doc_scores AS MATERIALIZED (
             SELECT doc_id,count(DISTINCT unit) AS matched FROM matches
             GROUP BY doc_id ORDER BY matched DESC,doc_id LIMIT ?{}
         ),
         chunk_scores AS (
             SELECT m.doc_id,l.ordinal,l.start_line,l.end_line,
                    count(DISTINCT m.unit) AS matched
             FROM matches m JOIN doc_scores s ON s.doc_id=m.doc_id
             JOIN lexical_chunks l ON l.rowid=m.rowid
             GROUP BY m.doc_id,m.rowid
         ),
         ranked_chunks AS (
             SELECT *,row_number() OVER (
                 PARTITION BY doc_id ORDER BY matched DESC,ordinal
             ) AS position FROM chunk_scores
         )
         SELECT d.path,d.kind,CAST(s.matched AS REAL)/?{},
                c.ordinal,c.start_line,c.end_line
         FROM doc_scores s JOIN ranked_chunks c ON c.doc_id=s.doc_id AND c.position=1
         JOIN documents d ON d.id=s.doc_id ORDER BY s.matched DESC,s.doc_id",
        units.len() + 1,
        units.len() + 2
    );
    let mut bindings: Vec<rusqlite::types::Value> = expressions
        .into_iter()
        .map(rusqlite::types::Value::Text)
        .collect();
    bindings.push(rusqlite::types::Value::Integer(
        limit.min(MAX_CANDIDATES) as i64
    ));
    bindings.push(rusqlite::types::Value::Integer(units.len() as i64));
    let mut stmt = conn.prepare_cached(&sql)?;
    Ok(stmt
        .query_map(params_from_iter(&bindings), |r| {
            let path: String = r.get(0)?;
            let kind: String = r.get(1)?;
            let coverage = r.get::<_, f64>(2)? as f32;
            Ok(LexicalHit {
                hit: crate::store::Hit {
                    line_kind: crate::document::line_kind(&path, &kind),
                    path,
                    kind,
                    score: coverage,
                    chunk_index: Some(r.get::<_, i64>(3)? as usize),
                    start_line: r.get::<_, i64>(4)? as usize,
                    end_line: r.get::<_, i64>(5)? as usize,
                },
                coverage,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

struct Unit {
    normalized: String,
    korean: bool,
}

impl Unit {
    fn expression(&self) -> String {
        if self.korean {
            korean_tokens(&self.normalized)
                .into_iter()
                .map(|token| format!("\"{token}\""))
                .collect::<Vec<_>>()
                .join(" AND ")
        } else {
            format!("\"{}\"", encoded('w', &self.normalized))
        }
    }
}

fn hangul(c: char) -> bool {
    matches!(c as u32, 0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F)
}

fn normalize(text: &str) -> String {
    text.nfc().flat_map(char::to_lowercase).collect()
}

/// Punctuation is always data, never MATCH syntax. Preserve ASCII snake and
/// hyphenated identifiers as one original query unit (INV-2041 != INV-2042).
fn raw_units(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.nfc().collect();
    let mut units = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let connector = matches!(c, '_' | '-')
            && i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_alphanumeric()
            && chars[i + 1].is_ascii_alphanumeric();
        if c.is_alphanumeric() || connector {
            current.push(c);
        } else if !current.is_empty() {
            units.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        units.push(current);
    }
    units
}

fn query_units(query: &str) -> Vec<Unit> {
    let mut seen = HashSet::new();
    let mut units: Vec<Unit> = raw_units(query)
        .into_iter()
        .filter_map(|raw| {
            let normalized = normalize(&raw);
            if !seen.insert(normalized.clone()) {
                return None;
            }
            let korean = normalized.chars().all(hangul);
            Some(Unit { normalized, korean })
        })
        .collect();
    // Mixed natural-language queries should spend their coverage denominator
    // and 16-unit budget on meaningful words. Keep literal single-word and
    // all-stopword queries searchable; compound identifiers never match this list.
    if units.len() > 1 && units.iter().any(|unit| !stopword(&unit.normalized)) {
        units.retain(|unit| !stopword(&unit.normalized));
    }
    units.truncate(MAX_TERMS);
    units
}

fn stopword(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "been"
            | "being"
            | "by"
            | "can"
            | "could"
            | "did"
            | "do"
            | "does"
            | "for"
            | "from"
            | "had"
            | "has"
            | "have"
            | "how"
            | "i"
            | "if"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "me"
            | "my"
            | "of"
            | "on"
            | "or"
            | "our"
            | "please"
            | "should"
            | "so"
            | "some"
            | "that"
            | "the"
            | "their"
            | "them"
            | "there"
            | "these"
            | "they"
            | "this"
            | "those"
            | "to"
            | "us"
            | "was"
            | "we"
            | "were"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "why"
            | "will"
            | "with"
            | "would"
            | "you"
            | "your"
    )
}

/// Hex keeps every token inside FTS5's ASCII alphabet while preserving prefix
/// relationships and punctuation in combined identifiers. Only postings persist.
fn encoded(namespace: char, word: &str) -> String {
    use std::fmt::Write;
    let mut token = String::with_capacity(1 + word.len() * 2);
    token.push(namespace);
    for byte in word.bytes() {
        let _ = write!(token, "{byte:02x}");
    }
    token
}

fn korean_tokens(word: &str) -> Vec<String> {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() == 1 {
        return vec![encoded('k', word)];
    }
    let mut seen = HashSet::new();
    chars
        .windows(2)
        .map(|pair| encoded('k', &pair.iter().collect::<String>()))
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

fn index_tokens(text: &str) -> String {
    let mut tokens = HashSet::new();
    for raw in raw_units(text) {
        let word = normalize(&raw);
        tokens.insert(encoded('w', &word));
        // Natural-language queries can find snake_case, camelCase and acronyms
        // without weakening an identifier query into independently matched parts.
        let chars: Vec<char> = raw.chars().collect();
        let mut segment = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = i > 0
                && c.is_ascii_uppercase()
                && (chars[i - 1].is_ascii_lowercase()
                    || (chars[i - 1].is_ascii_uppercase()
                        && chars.get(i + 1).is_some_and(char::is_ascii_lowercase)));
            if boundary || matches!(c, '_' | '-') {
                if !segment.is_empty() {
                    tokens.insert(encoded('w', &normalize(&segment)));
                    segment.clear();
                }
            }
            if !matches!(c, '_' | '-') {
                segment.push(c);
            }
        }
        if !segment.is_empty() {
            tokens.insert(encoded('w', &normalize(&segment)));
        }
        // Index Hangul runs in mixed text too; compose decomposed syllables first.
        for run in word
            .split(|c: char| !hangul(c))
            .filter(|run| !run.is_empty())
        {
            tokens.extend(korean_tokens(run));
            tokens.extend(run.chars().map(|c| encoded('k', &c.to_string())));
        }
    }
    // Stable insertion also makes debugging and byte-for-byte rebuilds predictable.
    let mut tokens: Vec<_> = tokens.into_iter().collect();
    tokens.sort_unstable();
    tokens.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE documents(id INTEGER PRIMARY KEY,path TEXT UNIQUE,kind TEXT,hash TEXT NOT NULL);
             CREATE TABLE chunks(doc_id INTEGER REFERENCES documents(id),embedding BLOB);",
        )
        .unwrap();
        create_schema(&conn).unwrap();
        conn
    }

    fn document(conn: &Connection, path: &str, texts: &[&str]) -> i64 {
        conn.execute(
            "INSERT INTO documents(path,kind,hash) VALUES(?1,'text','initial-hash')",
            [path],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        let texts = texts
            .iter()
            .enumerate()
            .map(|(i, text)| (i * 10 + 1, i * 10 + 5, text.to_string()))
            .collect::<Vec<_>>();
        replace(conn, id, &texts).unwrap();
        id
    }

    fn paths(conn: &Connection, query: &str) -> Vec<String> {
        search(conn, query, 200)
            .unwrap()
            .into_iter()
            .map(|h| h.hit.path)
            .collect()
    }

    #[test]
    fn contentless_schema_has_no_body_and_is_idempotent() {
        let conn = database();
        create_schema(&conn).unwrap();
        document(&conn, "secret.txt", &["Unique private body sentence"]);
        assert_eq!(paths(&conn, "private"), ["secret.txt"]);
        let body: Option<String> = conn
            .query_row("SELECT tokens FROM chunk_fts", [], |r| r.get(0))
            .unwrap();
        assert!(body.is_none());
        let content_table: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='chunk_fts_content')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!content_table);
        let columns = conn
            .prepare("PRAGMA table_info(lexical_chunks)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            columns,
            ["rowid", "doc_id", "ordinal", "start_line", "end_line"]
        );
    }

    #[test]
    fn replacement_deletion_and_cascade_remove_postings() {
        let conn = database();
        let id = document(&conn, "file", &["oldvalue", "secondold"]);
        replace(&conn, id, &[(7, 9, "newvalue".into())]).unwrap();
        assert!(paths(&conn, "oldvalue secondold").is_empty());
        let hit = search(&conn, "newvalue", 1).unwrap().remove(0);
        assert_eq!(
            (hit.hit.start_line, hit.hit.end_line, hit.hit.chunk_index),
            (7, 9, Some(0))
        );
        assert_eq!(hit.coverage, 1.0);
        conn.execute("DELETE FROM documents WHERE id=?1", [id])
            .unwrap();
        assert!(paths(&conn, "newvalue").is_empty());
        for table in ["chunk_fts", "lexical_chunks", "lexical_documents"] {
            let count: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
    }

    #[test]
    fn direct_mapping_delete_triggers_fts_delete() {
        let conn = database();
        document(&conn, "file", &["disappears"]);
        conn.execute("DELETE FROM lexical_chunks", []).unwrap();
        assert!(paths(&conn, "disappears").is_empty());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM chunk_fts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn empty_and_nontext_documents_are_covered() {
        let conn = database();
        let id = document(&conn, "image.png", &[]);
        conn.execute("UPDATE documents SET kind='image' WHERE id=?1", [id])
            .unwrap();
        assert!(has_document(&conn, "image.png").unwrap());
        assert!(!has_document(&conn, "missing").unwrap());
        replace(&conn, id, &[]).unwrap();
        assert!(has_document(&conn, "image.png").unwrap());
        assert!(search(&conn, "image", 10).unwrap().is_empty());
        conn.execute(
            "UPDATE documents SET hash='updated-image' WHERE id=?1",
            [id],
        )
        .unwrap();
        assert!(!has_document(&conn, "image.png").unwrap());
        replace(&conn, id, &[]).unwrap();
        assert!(has_document(&conn, "image.png").unwrap());
    }

    #[test]
    fn transactions_are_owned_by_caller_and_vectors_are_independent() {
        let mut conn = database();
        let id = document(&conn, "file", &["original"]);
        conn.execute(
            "INSERT INTO chunks(doc_id,embedding) VALUES(?1,x'1234')",
            [id],
        )
        .unwrap();
        {
            let tx = conn.transaction().unwrap();
            tx.execute(
                "UPDATE documents SET hash='replacement-hash' WHERE id=?1",
                [id],
            )
            .unwrap();
            replace(&tx, id, &[(1, 2, "replacement".into())]).unwrap();
            assert!(has_document(&tx, "file").unwrap());
            assert_eq!(paths(&tx, "replacement"), ["file"]);
            tx.rollback().unwrap();
        }
        assert_eq!(paths(&conn, "original"), ["file"]);
        assert!(has_document(&conn, "file").unwrap());
        assert!(paths(&conn, "replacement").is_empty());
        replace(&conn, id, &[(1, 2, "next".into())]).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        conn.execute("DELETE FROM chunks", []).unwrap();
        assert_eq!(paths(&conn, "next"), ["file"]);
        assert!(has_document(&conn, "file").unwrap());
    }

    #[test]
    fn prefix_only_applies_when_exact_is_globally_absent() {
        let conn = database();
        document(&conn, "short", &["plan alpha"]);
        document(&conn, "long", &["planning alphabet"]);
        assert_eq!(paths(&conn, "plan"), ["short"]);
        assert_eq!(paths(&conn, "plann"), ["long"]);
        let hits = search(&conn, "plan alphab", 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.coverage == 0.5));
    }

    #[test]
    fn identifiers_remain_whole_and_symbols_have_natural_segments() {
        let conn = database();
        document(
            &conn,
            "right",
            &["INV-2041 K7Q2LM snake_case HTTPServer getUserName"],
        );
        document(
            &conn,
            "wrong",
            &["INV-2042 K7Q2LN snake_other getOtherName"],
        );
        document(&conn, "separate", &["INV 2041 snake case"]);
        for query in ["INV-2041", "K7Q2LM", "snake_case", "getUserName"] {
            assert_eq!(paths(&conn, query), ["right"], "{query}");
        }
        assert!(paths(&conn, "INV-2043").is_empty());
        for query in ["user", "http", "server", "USER"] {
            assert_eq!(paths(&conn, query), ["right"], "{query}");
        }
    }

    #[test]
    fn korean_nfc_partial_and_and_bigrams_count_as_one_unit() {
        let conn = database();
        let decomposed: String = "대한민국 검색검색".nfd().collect();
        document(&conn, "korean", &[&decomposed]);
        document(&conn, "partial", &["대한 사람"]);
        assert_eq!(paths(&conn, "한민"), ["korean"]);
        assert_eq!(paths(&conn, "대한민국"), ["korean"]);
        assert_eq!(paths(&conn, "검색검색"), ["korean"]);
        assert_eq!(paths(&conn, "민"), ["korean"]);
        let query: String = "한민".nfd().collect();
        assert_eq!(paths(&conn, &query), ["korean"]);
        let hits = search(&conn, "한민국 absent", 10).unwrap();
        assert_eq!(hits[0].coverage, 0.5);
    }

    #[test]
    fn coverage_combines_chunks_and_late_full_matches_survive_bound() {
        let conn = database();
        for i in 0..250 {
            document(&conn, &format!("early{i}"), &["alpha"]);
        }
        document(&conn, "late", &["alpha", "beta"]);
        document(&conn, "bestchunk", &["alpha", "alpha beta"]);
        let hits = search(&conn, "alpha beta", usize::MAX).unwrap();
        assert_eq!(hits.len(), MAX_CANDIDATES);
        assert_eq!(hits[0].hit.path, "late");
        assert_eq!(hits[0].coverage, 1.0);
        assert_eq!(hits[1].hit.path, "bestchunk");
        assert_eq!(hits[1].hit.chunk_index, Some(1));
        assert_eq!(hits[2].coverage, 0.5);
        assert_eq!(search(&conn, "alpha beta", 1).unwrap()[0].hit.path, "late");
    }

    #[test]
    fn query_units_are_unique_capped_and_sql_syntax_is_inert() {
        let conn = database();
        document(&conn, "file", &["alpha"]);
        let hit = search(&conn, "ALPHA alpha missing", 10).unwrap().remove(0);
        assert_eq!(hit.coverage, 0.5);
        let query = std::iter::once("alpha".to_owned())
            .chain((0..20).map(|i| format!("absent{i}")))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(search(&conn, &query, 10).unwrap()[0].coverage, 1.0 / 16.0);
        for query in [
            "\"",
            "*",
            "() : - _ +",
            "alpha\" OR *",
            "tokens:alpha",
            "NEAR(alpha, 2)",
            "'); DROP TABLE documents; --",
            "\0",
            "",
        ] {
            search(&conn, query, 10).unwrap();
        }
        assert_eq!(paths(&conn, "\"alpha\"").len(), 1);
        assert!(search(&conn, "alpha", 0).unwrap().is_empty());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn mixed_queries_drop_stopwords_before_coverage_and_term_cap() {
        let conn = database();
        document(
            &conn,
            "meaningful",
            &["alpha beta INV-2041 snake_case 한민국"],
        );
        document(&conn, "stopwords", &["the and for"]);
        for query in [
            "the alpha and beta",
            "THE alpha ALPHA",
            "where is INV-2041",
            "the snake_case",
            "한민국 and the",
        ] {
            let hits = search(&conn, query, 200).unwrap();
            assert_eq!(hits.len(), 1, "{query}");
            assert_eq!(hits[0].hit.path, "meaningful", "{query}");
            assert_eq!(hits[0].coverage, 1.0, "{query}");
        }
        assert_eq!(paths(&conn, "the"), ["stopwords"]);
        assert_eq!(search(&conn, "the and", 10).unwrap()[0].coverage, 1.0);
        let query =
            "a an and are as at be been being by can could did do does for from had has have alpha";
        assert_eq!(search(&conn, query, 10).unwrap()[0].coverage, 1.0);
        assert!(query_units("() : - _ +").is_empty());
    }

    #[test]
    fn best_chunk_counts_distinct_units_and_breaks_ties_by_ordinal() {
        let conn = database();
        document(
            &conn,
            "file",
            &[
                "irrelevant",
                "alpha alpha alpha beta",
                "alpha beta",
                "gamma",
            ],
        );
        let hits = search(&conn, "alpha beta gamma", 10).unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(hit.coverage, 1.0);
        assert_eq!(hit.hit.chunk_index, Some(1));
        assert_eq!((hit.hit.start_line, hit.hit.end_line), (11, 15));
    }

    #[test]
    fn missing_schema_is_safe_in_read_only_legacy_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE documents(id INTEGER PRIMARY KEY,path TEXT,kind TEXT); PRAGMA query_only=ON;").unwrap();
        assert!(!has_document(&conn, "file").unwrap());
        assert!(search(&conn, "anything", 10).unwrap().is_empty());
        assert_eq!(conn.query_row("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'lexical_%' OR name='chunk_fts'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn old_vector_writer_hash_changes_hide_stale_postings_and_allow_prefix_fallback() {
        let conn = database();
        let id = document(&conn, "stale", &["plan obsolete 대한민국"]);
        document(&conn, "current", &["planning"]);
        assert_eq!(paths(&conn, "plan"), ["stale"]);
        // The older writer keeps the document id and lexical marker intact.
        conn.execute(
            "UPDATE documents SET hash='new-vector-hash' WHERE id=?1",
            [id],
        )
        .unwrap();
        let source_hash: String = conn
            .query_row(
                "SELECT source_hash FROM lexical_documents WHERE doc_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source_hash, "initial-hash");
        assert!(!has_document(&conn, "stale").unwrap());
        assert!(paths(&conn, "obsolete").is_empty());
        assert!(paths(&conn, "한민").is_empty());
        assert_eq!(paths(&conn, "plan"), ["current"]);
        // Normal indexing backfills the now-uncovered file, without manual SQL.
        replace(&conn, id, &[(1, 2, "refreshed".into())]).unwrap();
        assert!(has_document(&conn, "stale").unwrap());
        assert_eq!(paths(&conn, "refreshed"), ["stale"]);
        let source_hash: String = conn
            .query_row(
                "SELECT source_hash FROM lexical_documents WHERE doc_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(source_hash, "new-vector-hash");
    }

    #[test]
    fn source_hash_migration_preserves_unknown_markers_for_lazy_backfill() {
        let conn = database();
        let id = document(&conn, "legacy", &["obsolete"]);
        conn.execute_batch(
            "DROP TABLE lexical_documents;
             CREATE TABLE lexical_documents (
                 doc_id INTEGER PRIMARY KEY REFERENCES documents(id) ON DELETE CASCADE
             );",
        )
        .unwrap();
        conn.execute("INSERT INTO lexical_documents(doc_id) VALUES(?1)", [id])
            .unwrap();
        // Pre-migration read-only readers also treat unknown coverage as absent.
        conn.execute_batch("PRAGMA query_only=ON").unwrap();
        assert!(!has_document(&conn, "legacy").unwrap());
        assert!(paths(&conn, "obsolete").is_empty());
        conn.execute_batch("PRAGMA query_only=OFF").unwrap();
        create_schema(&conn).unwrap();
        create_schema(&conn).unwrap();
        let source_hash: Option<String> = conn
            .query_row(
                "SELECT source_hash FROM lexical_documents WHERE doc_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(source_hash.is_none());
        assert!(!has_document(&conn, "legacy").unwrap());
        assert!(paths(&conn, "obsolete").is_empty());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM chunk_fts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        replace(&conn, id, &[(1, 2, "backfilled".into())]).unwrap();
        assert!(has_document(&conn, "legacy").unwrap());
        assert_eq!(paths(&conn, "backfilled"), ["legacy"]);
    }

    #[test]
    fn marker_only_null_hash_never_certifies_existing_postings() {
        let conn = database();
        let id = document(&conn, "unknown", &["plan"]);
        document(&conn, "current", &["planning"]);
        conn.execute("DELETE FROM lexical_documents WHERE doc_id=?1", [id])
            .unwrap();
        conn.execute("INSERT INTO lexical_documents(doc_id) VALUES(?1)", [id])
            .unwrap();
        assert!(!has_document(&conn, "unknown").unwrap());
        assert_eq!(paths(&conn, "plan"), ["current"]);
    }

    #[test]
    #[ignore = "manual lexical latency benchmark"]
    fn benchmark_search_latency() {
        use std::time::Instant;
        for count in [26, 200] {
            let conn = database();
            for i in 0..count {
                document(
                    &conn,
                    &format!("doc{i}"),
                    &["alpha beta gamma", "alpha gamma delta"],
                );
            }
            for _ in 0..20 {
                std::hint::black_box(search(&conn, "alpha beta gamma", 200).unwrap());
            }
            let started = Instant::now();
            for _ in 0..400 {
                std::hint::black_box(search(&conn, "alpha beta gamma", 200).unwrap());
            }
            eprintln!(
                "{count} documents: {:.3} ms/search",
                started.elapsed().as_secs_f64() * 1_000.0 / 400.0
            );
        }
    }
}
