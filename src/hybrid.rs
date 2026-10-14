//! Query-adaptive fusion; WQL and composed retrieval keep their exact semantics.
use crate::{
    lexical::LexicalHit,
    store::{Hit, Store},
    vector::Encoding,
};
use anyhow::Result;
use std::collections::HashMap;

const CANDIDATES: usize = 200;
const SCALE_CHUNKS: usize = 8;
const DEFAULT_MIN_SIMILARITY: f32 = 0.30;
const DEFAULT_WEAK_RATIO: f32 = 0.50;

#[derive(Clone, Copy, Default)]
struct Moments {
    n: usize,
    mean: f64,
    m2: f64,
}
impl Moments {
    fn add(&mut self, score: f32) {
        self.n += 1;
        let delta = f64::from(score) - self.mean;
        self.mean += delta / self.n as f64;
        self.m2 += delta * (f64::from(score) - self.mean);
    }
}
pub(crate) fn modality(kind: &str) -> usize {
    match kind {
        "image" => 1,
        "audio" => 2,
        "video" => 3,
        _ => 0,
    }
}
#[derive(Default)]
pub struct Distribution {
    groups: [Moments; 4],
}
impl Distribution {
    pub fn add(&mut self, kind: &str, ordinal: usize, score: f32) {
        if ordinal < SCALE_CHUNKS && score.is_finite() {
            self.groups[modality(kind)].add(score);
        }
    }
    pub fn z(&self, kind: &str, score: f32) -> f32 {
        let n: usize = self.groups.iter().map(|g| g.n).sum();
        if n == 0 {
            return 0.;
        }
        let variance = self.groups.iter().map(|g| g.m2).sum::<f64>() / n as f64;
        ((f64::from(score) - self.groups[modality(kind)].mean) / variance.sqrt().max(0.02)) as f32
    }
    fn relevant(&self, kind: &str, raw: f32, min_similarity: f32) -> bool {
        let min_z = if self.groups[modality(kind)].n >= 20 {
            2.
        } else {
            1.
        };
        raw >= min_similarity && self.z(kind, raw) >= min_z
    }
}

fn filename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn os_metadata(path: &str) -> bool {
    matches!(
        filename(path).to_ascii_lowercase().as_str(),
        ".ds_store" | ".localized" | "thumbs.db" | "desktop.ini"
    )
}

/// An exact, allowlisted format query reads document metadata without inference
/// or lexical tables, including on legacy indexes with no chunk postings.
pub fn format_lookup(store: &Store, query: &str, top: usize) -> Result<Option<Vec<Hit>>> {
    let normalized = query.trim().to_ascii_lowercase();
    let extension = match normalized.as_str() {
        "markdown" => "md",
        name => name.strip_prefix('.').unwrap_or(name),
    };
    if !matches!(
        extension,
        "md" | "pdf"
            | "docx"
            | "pptx"
            | "xlsx"
            | "txt"
            | "csv"
            | "tsv"
            | "odt"
            | "odp"
            | "ods"
            | "epub"
            | "hwpx"
            | "html"
            | "htm"
            | "rtf"
            | "json"
            | "yaml"
            | "yml"
            | "xml"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "tiff"
            | "tif"
            | "heic"
            | "mp3"
            | "wav"
            | "m4a"
            | "flac"
            | "ogg"
            | "mp4"
            | "mov"
            | "mkv"
            | "webm"
            | "avi"
    ) {
        return Ok(None);
    }
    let mut hits = Vec::new();
    if top > 0 {
        let mut stmt = store
            .conn
            .prepare("SELECT path,kind FROM documents ORDER BY path")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let matches = std::path::Path::new(filename(&path))
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(extension));
            if matches && !os_metadata(&path) {
                hits.push(Hit {
                    line_kind: "none",
                    path,
                    kind: row.get(1)?,
                    score: 1.,
                    start_line: 0,
                    end_line: 0,
                    chunk_index: None,
                });
                if hits.len() == top {
                    break;
                }
            }
        }
    }
    Ok(Some(hits))
}
pub fn is_lookup(query: &str) -> bool {
    let words: Vec<_> = query.split_whitespace().collect();
    !words.is_empty()
        && words.len() <= 3
        && words.iter().all(|w| {
            let digit = w.chars().any(|c| c.is_ascii_digit());
            let letter = w.chars().any(|c| c.is_ascii_alphabetic());
            w.is_ascii()
                && ((digit && (letter || w.len() >= 4))
                    || w.contains('_')
                    || w.contains("::")
                    || w.chars()
                        .zip(w.chars().skip(1))
                        .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase()))
        })
}
fn kind_match(query: &str, hit: &Hit) -> bool {
    let words: Vec<_> = query
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .collect();
    let extension = std::path::Path::new(&hit.path)
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    words.iter().any(|w| match w.as_str() {
        "pdf" => extension == "pdf",
        "image" | "photo" | "이미지" | "사진" => hit.kind == "image",
        "audio" | "음성" | "녹음" => hit.kind == "audio",
        "video" | "영상" | "동영상" => hit.kind == "video",
        _ => false,
    })
}
pub fn score(z: f32, coverage: f32, kind: bool, lookup: bool) -> f32 {
    let (alpha, beta) = if lookup { (0.5, 4.) } else { (1., 2.) };
    alpha * z.max(0.) + beta * coverage.clamp(0., 1.).powf(1.5) + if kind { 1. } else { 0. }
}

/// Exact lookups with complete lexical matches bypass inference entirely.
pub fn lexical_lookup(query: &str, lexical: &[LexicalHit], top: usize) -> Option<Vec<Hit>> {
    if !is_lookup(query)
        || !lexical
            .iter()
            .any(|h| h.coverage >= 1. && !os_metadata(&h.hit.path))
    {
        return None;
    }
    let mut hits: Vec<_> = lexical
        .iter()
        .filter(|h| h.coverage >= 1. && !os_metadata(&h.hit.path))
        .map(|h| {
            let mut hit = h.hit.clone();
            hit.score = score(0., h.coverage, kind_match(query, &hit), true);
            hit
        })
        .collect();
    sort(&mut hits, top);
    Some(hits)
}
pub fn search(
    store: &Store,
    query: &str,
    vector: &[f32],
    enc: Encoding,
    top: usize,
) -> Result<Vec<Hit>> {
    if top == 0 {
        return Ok(Vec::new());
    }
    let lexical = crate::lexical::search(&store.conn, query, CANDIDATES.max(top))?;
    search_with_lexical(store, query, vector, enc, top, lexical)
}
pub fn search_variant(
    store: &Store,
    query: &str,
    vector: &[f32],
    enc: Encoding,
    top: usize,
    intent: bool,
    z_score: bool,
) -> Result<Vec<Hit>> {
    if top == 0 {
        return Ok(Vec::new());
    }
    let lexical = crate::lexical::search(&store.conn, query, CANDIDATES.max(top))?;
    let cutoff = (intent && z_score).then_some((DEFAULT_MIN_SIMILARITY, DEFAULT_WEAK_RATIO));
    fuse(
        store, query, vector, enc, top, intent, z_score, lexical, cutoff,
    )
}
/// Reuse postings already fetched for the inference-free lookup decision.
pub fn search_with_lexical(
    store: &Store,
    query: &str,
    vector: &[f32],
    enc: Encoding,
    top: usize,
    lexical: Vec<LexicalHit>,
) -> Result<Vec<Hit>> {
    search_with_lexical_cutoff(
        store,
        query,
        vector,
        enc,
        top,
        lexical,
        DEFAULT_MIN_SIMILARITY,
        DEFAULT_WEAK_RATIO,
    )
}

/// Semantic evidence must clear both the absolute cosine and modality z gates;
/// keyword/path coverage independently proves relevance. Do not pad weak tails.
pub fn search_with_lexical_cutoff(
    store: &Store,
    query: &str,
    vector: &[f32],
    enc: Encoding,
    top: usize,
    lexical: Vec<LexicalHit>,
    min_similarity: f32,
    weak_ratio: f32,
) -> Result<Vec<Hit>> {
    if top == 0 {
        return Ok(Vec::new());
    }
    fuse(
        store,
        query,
        vector,
        enc,
        top,
        true,
        true,
        lexical,
        Some((min_similarity, weak_ratio)),
    )
}
fn fuse(
    store: &Store,
    query: &str,
    vector: &[f32],
    enc: Encoding,
    top: usize,
    intent: bool,
    z_score: bool,
    lexical: Vec<LexicalHit>,
    cutoff: Option<(f32, f32)>,
) -> Result<Vec<Hit>> {
    let paths = store.paths_limited(query, None, None, CANDIDATES.max(top))?;
    let mut extras: HashMap<String, (Hit, f32, Option<f32>)> = HashMap::new();
    for item in lexical {
        extras.insert(item.hit.path.clone(), (item.hit, item.coverage, None));
    }
    for hit in paths.into_iter().take(CANDIDATES.max(top)) {
        extras
            .entry(hit.path.clone())
            .and_modify(|e| e.1 = 1.)
            .or_insert((hit, 1., None));
    }
    let mut distribution = Distribution::default();
    let semantic = store.hits_observed(
        vector,
        enc,
        CANDIDATES.max(top),
        true,
        |path, kind, ordinal, raw| {
            if cutoff.is_some() && os_metadata(path) {
                return;
            }
            distribution.add(kind, ordinal, raw);
            if let Some(extra) = extras.get_mut(path) {
                extra.2 = Some(extra.2.map_or(raw, |old| old.max(raw)));
            }
        },
    )?;
    let lookup = intent && is_lookup(query);
    let mut candidates = HashMap::new();
    for mut hit in semantic {
        let coverage = extras.remove(&hit.path).map_or(0., |e| e.1);
        if let Some((min_similarity, _)) = cutoff {
            if os_metadata(&hit.path)
                || (coverage < 0.5 && !distribution.relevant(&hit.kind, hit.score, min_similarity))
            {
                continue;
            }
        }
        let z = if z_score {
            distribution.z(&hit.kind, hit.score)
        } else {
            hit.score
        };
        hit.score = score(z, coverage, kind_match(query, &hit), lookup);
        candidates.insert(hit.path.clone(), hit);
    }
    for (_, (mut hit, coverage, raw)) in extras {
        if let Some((min_similarity, _)) = cutoff {
            if os_metadata(&hit.path)
                || (coverage < 0.5
                    && !raw.is_some_and(|value| {
                        distribution.relevant(&hit.kind, value, min_similarity)
                    }))
            {
                continue;
            }
        }
        let semantic = raw.map_or(0., |s| {
            if z_score {
                distribution.z(&hit.kind, s)
            } else {
                s
            }
        });
        hit.score = score(semantic, coverage, kind_match(query, &hit), lookup);
        candidates.insert(hit.path.clone(), hit);
    }
    let mut hits: Vec<_> = candidates.into_values().collect();
    if let Some((_, weak_ratio)) = cutoff {
        let top_score = hits.iter().map(|hit| hit.score).fold(0., f32::max);
        hits.retain(|hit| hit.score > 0. && hit.score >= top_score * weak_ratio);
    }
    sort(&mut hits, top);
    Ok(hits)
}
fn sort(hits: &mut Vec<Hit>, top: usize) {
    hits.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.path.cmp(&b.path))
            .then(a.chunk_index.cmp(&b.chunk_index))
    });
    hits.truncate(top);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn add_document(store: &mut Store, path: &str, raw: f32, body: &str) -> Result<()> {
        store.replace_with_lexical(
            path,
            "hash",
            "text",
            1,
            &[(
                1,
                1,
                crate::store::encode(&[raw, (1. - raw * raw).sqrt()], Encoding::F32),
                None,
            )],
            Some(&[(1, 1, body.into())]),
        )
    }

    #[test]
    fn relevance_and_relative_cutoff_remove_weak_tail_without_padding() -> Result<()> {
        let mut store = Store::open(std::path::Path::new(":memory:"))?;
        add_document(&mut store, "/docs/exact.txt", 0.1, "needle orange")?;
        add_document(&mut store, "/docs/partial.txt", 0.1, "needle")?;
        add_document(&mut store, "/docs/needle orange.md", 0.1, "unrelated")?;
        for i in 0..30 {
            add_document(
                &mut store,
                &format!("/docs/filler{i}.txt"),
                0.1,
                "unrelated",
            )?;
        }
        let lexical = crate::lexical::search(&store.conn, "needle orange", 200)?;
        let untrimmed = search_with_lexical_cutoff(
            &store,
            "needle orange",
            &[1., 0.],
            Encoding::F32,
            30,
            lexical.clone(),
            0.3,
            0.,
        )?;
        assert_eq!(untrimmed.len(), 3);
        assert!(untrimmed.iter().any(|hit| hit.path == "/docs/partial.txt"));
        let hits = search_with_lexical(
            &store,
            "needle orange",
            &[1., 0.],
            Encoding::F32,
            30,
            lexical,
        )?;
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|hit| hit.path == "/docs/exact.txt"));
        assert!(hits.iter().any(|hit| hit.path == "/docs/needle orange.md"));
        assert_eq!(
            search(&store, "needle orange", &[1., 0.], Encoding::F32, 30)?.len(),
            2
        );
        Ok(())
    }

    #[test]
    fn all_weak_queries_return_empty_even_with_positive_outlier_z() -> Result<()> {
        let mut store = Store::open(std::path::Path::new(":memory:"))?;
        for i in 0..30 {
            add_document(
                &mut store,
                &format!("/docs/{i}.txt"),
                if i == 29 { 0.29 } else { 0.1 },
                "unrelated",
            )?;
        }
        assert!(search(&store, "absent concept", &[1., 0.], Encoding::F32, 30)?.is_empty());
        // Ablations retain the original scoring behavior for comparisons.
        assert_eq!(
            search_variant(
                &store,
                "absent concept",
                &[1., 0.],
                Encoding::F32,
                30,
                false,
                false
            )?
            .len(),
            30
        );
        Ok(())
    }

    #[test]
    fn small_index_admits_strong_semantic_hit_and_respects_custom_cosine() -> Result<()> {
        let mut store = Store::open(std::path::Path::new(":memory:"))?;
        for (path, raw) in [
            ("/docs/a.txt", 0.1),
            ("/docs/b.txt", 0.1),
            ("/docs/c.txt", 0.8),
        ] {
            add_document(&mut store, path, raw, "unrelated")?;
        }
        let hits = search(&store, "absent concept", &[1., 0.], Encoding::F32, 30)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "/docs/c.txt");
        assert!(
            search_with_lexical_cutoff(
                &store,
                "absent concept",
                &[1., 0.],
                Encoding::F32,
                30,
                vec![],
                0.9,
                0.5
            )?
            .is_empty()
        );
        Ok(())
    }

    #[test]
    fn semantic_gate_requires_z_using_the_candidate_modality_count() {
        let mut d = Distribution::default();
        for ordinal in 0..19 {
            d.add("text", 0, if ordinal % 2 == 0 { 0.2 } else { 0.8 });
        }
        assert!(d.relevant("text", 0.8, 0.3));
        d.add("text", 0, 0.8);
        assert!(!d.relevant("text", 0.8, 0.3));
        d.add("image", 0, 0.1);
        d.add("image", 0, 0.9);
        assert!(d.relevant("image", 0.9, 0.3));
        let mut uniform = Distribution::default();
        for _ in 0..30 {
            uniform.add("text", 0, 0.9);
        }
        assert!(!uniform.relevant("text", 0.9, 0.3));
    }

    #[test]
    fn previously_indexed_os_metadata_cannot_pass_semantic_or_keyword_proof() -> Result<()> {
        let mut store = Store::open(std::path::Path::new(":memory:"))?;
        add_document(
            &mut store,
            "/docs/.DS_Store",
            0.99,
            "needle orange Store_id",
        )?;
        for i in 0..30 {
            add_document(&mut store, &format!("/docs/{i}.txt"), 0.1, "unrelated")?;
        }
        for query in ["needle orange", ".DS_Store", "absent concept"] {
            assert!(
                search(&store, query, &[1., 0.], Encoding::F32, 30)?.is_empty(),
                "{query}"
            );
        }
        let lexical = crate::lexical::search(&store.conn, "Store_id", 200)?;
        assert!(!lexical.is_empty());
        assert!(lexical_lookup("Store_id", &lexical, 30).is_none());
        // An explicitly selected path search can still inspect old metadata.
        assert_eq!(store.paths_limited(".DS_Store", None, None, 30)?.len(), 1);
        Ok(())
    }

    #[test]
    fn exact_format_lookup_reads_only_legacy_metadata_with_robust_paths() -> Result<()> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE documents(path TEXT UNIQUE, kind TEXT)")?;
        for path in [
            "/docs/report.MD",
            "C:\\한글 폴더\\memo.md",
            "/docs/md/notes.txt",
            "/docs/.DS_Store",
            "/docs/.md",
            "/docs/report.md.bak",
            "/docs/guide.markdown",
            "/docs/report.PDF",
            "C:\\md\\notes.TXT",
        ] {
            conn.execute("INSERT INTO documents VALUES (?1,'text')", [path])?;
        }
        let store = Store {
            conn,
            vec_path: Default::default(),
        };
        for query in ["md", ".md", "markdown", " MD ", "MARKDOWN"] {
            let hits = format_lookup(&store, query, 30)?.unwrap();
            assert_eq!(hits.len(), 2, "{query}");
            assert!(hits.iter().all(|hit| hit.chunk_index.is_none()
                && hit.line_kind == "none"
                && hit.start_line == 0
                && hit.end_line == 0));
        }
        assert_eq!(
            format_lookup(&store, "pdf", 30)?.unwrap()[0].path,
            "/docs/report.PDF"
        );
        assert_eq!(format_lookup(&store, ".TXT", 30)?.unwrap().len(), 2);
        assert_eq!(format_lookup(&store, "md", 1)?.unwrap().len(), 1);
        assert!(format_lookup(&store, "md", 0)?.unwrap().is_empty());
        assert!(format_lookup(&store, "docx", 30)?.unwrap().is_empty());
        for query in [
            "",
            "screenshots md",
            "find markdown",
            "format:pdf",
            "unknownext",
            ".DS_Store",
            "md/",
            "md%",
        ] {
            assert!(format_lookup(&store, query, 30)?.is_none(), "{query}");
        }
        Ok(())
    }

    #[test]
    fn streaming_shared_variance_and_long_document_cap() {
        let mut d = Distribution::default();
        d.add("text", 0, 0.1);
        d.add("text", 1, 0.3);
        d.add("image", 0, 0.7);
        d.add("image", 1, 0.9);
        assert!((d.z("text", 0.3) - d.z("image", 0.9)).abs() < 1e-5);
        assert!((d.z("text", 0.3) - 1.).abs() < 1e-5);
        let before = d.z("text", 0.3);
        for ordinal in 8..1000 {
            d.add("text", ordinal, -1.);
        }
        assert_eq!(before, d.z("text", 0.3));
        let mut narrow = Distribution::default();
        narrow.add("text", 0, 0.1);
        narrow.add("text", 1, 0.3);
        narrow.add("image", 0, 0.800);
        narrow.add("image", 1, 0.802);
        assert!(narrow.z("image", 0.802) < 0.02);
    }
    #[test]
    fn modality_candidates_survive_raw_top_k_and_match_mmap() -> Result<()> {
        use crate::vector::normalized_prefix;
        let root = std::env::temp_dir().join(format!("weft-hybrid-modal-{}", std::process::id()));
        std::fs::create_dir_all(&root)?;
        let mut store = Store::open(&root.join("index.sqlite"))?;
        store.set_setting("dim", "2")?;
        for i in 0..250 {
            let raw = normalized_prefix(&[0.99, 0.1], 2)?;
            store.replace(
                &format!("/text/{i}.md"),
                "hash",
                "text",
                1,
                &[(1, 1, crate::store::encode(&raw, Encoding::Q8), None)],
            )?;
        }
        for i in 0..30 {
            let x: f32 = if i == 29 { 0.7 } else { 0.1 };
            let raw = vec![x, (1. - x * x).sqrt()];
            store.replace(
                &format!("/image/{i}.png"),
                "hash",
                "image",
                1,
                &[(0, 0, crate::store::encode(&raw, Encoding::Q8), None)],
            )?;
        }
        let sqlite = search(&store, "unmatched words", &[1., 0.], Encoding::Q8, 10)?;
        assert_eq!(sqlite[0].path, "/image/29.png");
        store.flush_vectors_bin(2)?;
        let mapped = search(&store, "unmatched words", &[1., 0.], Encoding::Q8, 10)?;
        assert_eq!(
            sqlite
                .iter()
                .map(|h| (&h.path, h.chunk_index))
                .collect::<Vec<_>>(),
            mapped
                .iter()
                .map(|h| (&h.path, h.chunk_index))
                .collect::<Vec<_>>()
        );
        for (a, b) in sqlite.iter().zip(&mapped) {
            assert!((a.score - b.score).abs() < 1e-4);
        }
        drop(store);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn lexical_candidates_outside_semantic_top_k_are_scored() -> Result<()> {
        let mut store = Store::open(std::path::Path::new(":memory:"))?;
        for i in 0..220 {
            let x: f32 = if i == 219 { 0.1 } else { 0.9 };
            let vector = crate::store::encode(&[x, (1. - x * x).sqrt()], Encoding::F32);
            let body = if i == 219 {
                "unique lookup term"
            } else {
                "unrelated"
            };
            store.replace_with_lexical(
                &format!("/doc/{i}.md"),
                "hash",
                "text",
                1,
                &[(1, 1, vector, None)],
                Some(&[(1, 1, body.into())]),
            )?;
        }
        let result = search(&store, "unique lookup term", &[1., 0.], Encoding::F32, 10)?;
        assert_eq!(result[0].path, "/doc/219.md");
        assert_eq!(result[0].start_line, 1);
        Ok(())
    }
    #[test]
    fn identifiers_route_and_keyword_weights() {
        for q in [
            "INV-2041",
            "K7Q2LM",
            "4815162342",
            "load_model",
            "loadModel",
            "Store::open",
        ] {
            assert!(is_lookup(q), "{q}");
        }
        for q in [
            "where is retry configured",
            "2026 budget review today",
            "메모리 사용량",
        ] {
            assert!(!is_lookup(q), "{q}");
        }
        assert!(score(0., 1., false, true) > score(4., 0., false, true));
        assert!(score(2., 1., true, false) > score(2., 1., false, false));
    }
}
