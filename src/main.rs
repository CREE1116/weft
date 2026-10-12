mod chunk;
mod daemon;
mod document;
mod embed;
mod hierarchy;
mod native;
mod prepared;
mod query;
mod resource;
mod retrieve;
mod scan;
mod scoring;
mod simd;
mod store;
mod vector;
mod wql;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use embed::{Embedder, Input};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;
use store::{Hit, Store};
use vector::{Encoding, normalized_prefix};

#[derive(Parser)]
#[command(
    version,
    about = "Local file explorer for agents with EmbeddingGemma 2"
)]
struct Cli {
    #[arg(long, global = true, default_value_os_t = default_db())]
    db: PathBuf,
    #[arg(long, global = true, default_value = "native")]
    backend: String,
    #[arg(
        long,
        global = true,
        default_value = "http://127.0.0.1:8080/v1/embeddings"
    )]
    endpoint: String,
    #[arg(long, global = true, default_value = "embeddinggemma-2")]
    model: String,
    #[arg(long, global = true, default_value_os_t = default_model_file())]
    model_file: PathBuf,
    /// Native runtime: auto, cpu, cuda[:N], metal[:N], mps[:N] (Metal alias).
    #[arg(long, global = true, default_value = "auto")]
    device: String,
    /// Optional custom Python embedding bridge; the default is bundled into the binary.
    #[arg(long, global = true)]
    bridge: Option<PathBuf>,
    /// Python full-model identifier for native indexing of images, audio, and video.
    #[arg(long, global = true, default_value = "google/embeddinggemma-2")]
    media_model: String,
    /// Load image/audio encoders as well as text; uses substantially more memory.
    #[arg(long, global = true)]
    full_modal: bool,
    #[command(subcommand)]
    command: Cmd,
}

fn default_db() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/myvec/index.sqlite")
}

fn default_model_file() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("myvec/embeddinggemma-2-Q8_0.gguf")
}

#[derive(Subcommand)]
enum Cmd {
    /// Prepare EmbeddingGemma 2 tensor layout and tokenizer metadata for faster cold starts.
    Prepare,
    /// Incrementally index a directory or a single file; respects gitignore.
    Index {
        path: Option<PathBuf>,
        /// Index accessible computer files; unsupported formats get filename embeddings.
        #[arg(long)]
        all: bool,
        /// Exclude a file or directory (repeatable).
        #[arg(long)]
        exclude: Vec<PathBuf>,
        /// Discover files and report coverage without loading the model or changing the index.
        #[arg(long)]
        dry_run: bool,
        /// Start an independent background scan and write progress to a log.
        #[arg(long, conflicts_with = "dry_run")]
        background: bool,
        /// Inference time budget, 1..100%; background default 25, foreground default 100.
        #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
        duty_cycle: Option<u8>,
        #[arg(long, default_value_t = 768)]
        dim: usize,
        #[arg(long, default_value = "q8")]
        encoding: String,
        /// Index visual/audio/video content with the optional full-model Python encoder.
        /// Native text inference stays fast and unchanged; media loads lazily.
        #[arg(long)]
        media: bool,
        #[arg(long)]
        prune: bool,
        #[arg(long, default_value_t = 1100)]
        chunk_chars: usize,
        #[arg(long, default_value_t = 2)]
        overlap_lines: usize,
    },
    /// Rank indexed files by a natural-language question.
    Search {
        query: String,
        /// semantic, path (filename/folder terms), or hybrid (rank fusion).
        #[arg(long, default_value = "semantic", value_parser = ["semantic", "path", "hybrid"])]
        mode: String,
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long)]
        json: bool,
    },
    /// Find files similar to an already indexed file, with no model startup.
    #[command(alias = "file2file")]
    Similar {
        file: PathBuf,
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long)]
        json: bool,
    },
    /// Combine text/file criteria, negative examples and bounded evidence in one scan.
    Retrieve(retrieve::Options),
    /// Execute JSON retrieval leaves and file-set union/intersection/difference.
    Query {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long)]
        json: bool,
    },
    /// Cooperatively pause/resume/stop or change an active indexer's time budget.
    Control {
        #[arg(value_parser = ["pause", "resume", "stop", "limit"])]
        action: String,
        #[arg(long, required_if_eq("action", "limit"), value_parser = clap::value_parser!(u8).range(1..=100))]
        duty_cycle: Option<u8>,
    },
    /// Index status, optionally including folder-scoped coverage without inference.
    Stats {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        under: Option<PathBuf>,
        #[arg(long, requires = "under", value_parser = ["text", "path", "image", "audio", "video"])]
        kind: Option<String>,
    },
    /// Extract document text and optionally export embedded images, without inference.
    Extract {
        file: PathBuf,
        #[arg(long)]
        images_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Execute a Weft Query Language (WQL) expression.
    #[command(name = "q", alias = "wql")]
    Q {
        /// WQL expression, e.g. 'all("A","B") in loom top 10'
        query: String,
        #[arg(long)]
        json: bool,
    },
    /// Manage named scoped views.
    View {
        #[command(subcommand)]
        action: ViewCmd,
    },
    /// Benchmark inference latency, WQL retrieval throughput, candidates/sec and peak memory.
    Bench {
        /// Optional labeled queries file for precision benchmarking; omit for in-situ system benchmark.
        #[arg(long)]
        queries: Option<PathBuf>,
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long)]
        json: bool,
    },
    /// Flush/rebuild the high-speed mmap vector snapshot (vectors.bin).
    Flush,
    /// Run the smart daemon (auto-exits after idle timeout and runs post-session indexing).
    Daemon {
        /// Idle timeout in seconds before auto-indexing and exiting.
        #[arg(long, default_value_t = 180)]
        idle_timeout: u64,
        /// Socket path for IPC.
        #[arg(long, default_value = daemon::DEFAULT_SOCKET_PATH)]
        socket: PathBuf,
    },
}

#[derive(Subcommand)]
enum ViewCmd {
    /// Save a scoped directory view.
    Set {
        name: String,
        path: PathBuf,
        #[arg(long, value_parser = ["text", "path", "image", "audio", "video"])]
        kind: Option<String>,
    },
    /// List all defined views.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a defined view.
    Rm { name: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(&cli.command, Cmd::Prepare) {
        let started = Instant::now();
        let (cache, bytes) = prepared::prepare(&cli.model_file)?;
        println!(
            "Prepared {} ({} bytes, {:.1} ms)",
            cache.display(),
            bytes,
            started.elapsed().as_secs_f64() * 1000.
        );
        return Ok(());
    }
    if let Cmd::Extract {
        file,
        images_dir,
        json,
    } = &cli.command
    {
        let mut bytes = Vec::new();
        std::fs::File::open(file)?
            .take(document::INPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        let mut extracted = document::extract(file, &bytes, true)?;
        if let Some(dir) = images_dir {
            extracted.export_images(dir)?;
        }
        if *json {
            println!("{}", serde_json::to_string_pretty(&extracted)?);
        } else {
            println!("{}", extracted.text);
            eprintln!("embedded images: {}", extracted.images.len());
            for warning in extracted.warnings {
                eprintln!("{warning}");
            }
        }
        return Ok(());
    }
    let is_mutation_cmd = matches!(
        &cli.command,
        Cmd::Index { .. } | Cmd::Control { .. } | Cmd::View { .. } | Cmd::Flush
    );
    let mut store = if is_mutation_cmd {
        Store::open(&cli.db)?
    } else {
        Store::open_read_only(&cli.db)?
    };
    match &cli.command {
        Cmd::Prepare => unreachable!("handled before opening database"),
        Cmd::Index {
            path,
            all,
            exclude,
            dry_run,
            background,
            duty_cycle,
            dim,
            encoding,
            media,
            prune,
            chunk_chars,
            overlap_lines,
        } => {
            if !matches!(*dim, 256 | 512 | 768) {
                bail!("--dim must be 256, 512, or 768; 128d has significant quality loss");
            }
            if *chunk_chars < 100 {
                bail!("--chunk-chars must be at least 100");
            }
            if *media && cli.backend == "http" {
                bail!(
                    "HTTP backend cannot embed media; use native --media or python --full-modal --media"
                );
            }
            if *media && cli.backend == "python" && !cli.full_modal {
                bail!("Python --media requires --full-modal to load the image/audio encoders");
            }
            let enc = Encoding::parse(encoding)?;
            let root = path
                .clone()
                .unwrap_or_else(|| {
                    if *all {
                        scan::computer_root()
                    } else {
                        PathBuf::from(".")
                    }
                })
                .canonicalize()
                .context("cannot open scan root")?;
            let settings = [
                ("dim", dim.to_string()),
                ("encoding", enc.name().to_string()),
                ("backend", cli.backend.clone()),
                ("model", cli.model.clone()),
                ("model_file", cli.model_file.to_string_lossy().into_owned()),
                (
                    "endpoint",
                    if cli.backend == "http" {
                        cli.endpoint.clone()
                    } else {
                        String::new()
                    },
                ),
                ("chunk_chars", chunk_chars.to_string()),
                ("overlap_lines", overlap_lines.to_string()),
            ];
            if !dry_run {
                store.ensure_settings(&settings)?;
                if *media && cli.backend == "native" {
                    store.ensure_settings(&[("media_model", cli.media_model.clone())])?;
                }
            }
            if *background {
                if indexing_running(&cli.db)? {
                    bail!("another indexer is already running for this database");
                }
                start_background(&cli.db, *duty_cycle)?;
                return Ok(());
            }
            index(
                &cli,
                &mut store,
                &root,
                *dim,
                enc,
                *media,
                *prune,
                *chunk_chars,
                *overlap_lines,
                *all,
                *all && path.is_none(),
                exclude,
                *dry_run,
                duty_cycle.unwrap_or(100),
            )?;
        }
        Cmd::Search {
            query,
            mode,
            top,
            json,
        } => {
            if query.trim().is_empty() {
                bail!("query must not be empty");
            }
            let paths = if mode != "semantic" {
                store.paths(query, None, None)?
            } else {
                Vec::new()
            };
            let mut hits = if mode == "path" {
                paths
            } else {
                let (dim, enc) = config(&store)?;
                check_backend(&store, &cli)?;
                let mut engine = new_embedder(&cli)?;
                let q = engine.embed(
                    &[Input {
                        kind: "text",
                        value: query.clone(),
                    }],
                    "query",
                )?;
                let semantic = store.hits(&normalized_prefix(&q[0], dim)?, enc, *top)?;
                if mode == "hybrid" {
                    query::union(vec![semantic, paths])
                } else {
                    semantic
                }
            };
            hits.truncate(*top);
            show(hits, *json)?;
        }
        Cmd::Retrieve(options) => {
            options.validate()?;
            let (dim, enc) = config(&store)?;
            let mut engine = None;
            let response = retrieve::run(options, &store, dim, enc, &mut |text| {
                check_backend(&store, &cli)?;
                if engine.is_none() {
                    engine = Some(new_embedder(&cli)?);
                }
                Ok(engine
                    .as_mut()
                    .unwrap()
                    .embed(
                        &[Input {
                            kind: "text",
                            value: text.into(),
                        }],
                        "query",
                    )?
                    .remove(0))
            })?;
            if options.json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                for evidence in response.hits {
                    if evidence.hit.chunk_index.is_none() {
                        println!(
                            "{:.4}\t{}\t{} files / {} chunks\t{}",
                            evidence.distance,
                            evidence.hit.path,
                            evidence.member_files,
                            evidence.member_chunks,
                            evidence.source_state
                        );
                    } else {
                        println!(
                            "{:.4}\t{}:{}-{}\t{}",
                            evidence.distance,
                            evidence.hit.path,
                            evidence.hit.start_line,
                            evidence.hit.end_line,
                            evidence.source_state
                        );
                    }
                    if evidence.hit.line_kind == "extracted_text" {
                        println!(
                            "extracted text lines; source: {}",
                            evidence
                                .source_locations
                                .iter()
                                .map(|l| l.label.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                    if let Some(context) = evidence.context {
                        println!("{context}");
                    }
                }
            }
        }
        Cmd::Query { spec, top, json } => {
            use std::io::Read;
            let mut raw = Vec::new();
            if spec == Path::new("-") {
                std::io::stdin()
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut raw)?;
            } else {
                std::fs::File::open(spec)
                    .context("read query specification")?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut raw)?;
            }
            if raw.len() > 1024 * 1024 {
                bail!("query specification exceeds 1 MiB");
            }
            let spec: query::Query = serde_json::from_slice(&raw)?;
            spec.validate()?;
            // Metadata-only leaves still work on an empty index, without model loading.
            let dim = store
                .setting("dim")?
                .map(|s| s.parse::<usize>())
                .transpose()?
                .unwrap_or(768);
            let enc = Encoding::parse(&store.setting("encoding")?.unwrap_or_else(|| "q8".into()))?;
            let mut engine = None;
            let mut hits = query::evaluate(&spec, &store, dim, enc, &mut |text| {
                check_backend(&store, &cli)?;
                if engine.is_none() {
                    engine = Some(new_embedder(&cli)?);
                }
                Ok(engine
                    .as_mut()
                    .unwrap()
                    .embed(
                        &[Input {
                            kind: "text",
                            value: text.into(),
                        }],
                        "query",
                    )?
                    .remove(0))
            })?;
            hits.truncate(*top);
            show(hits, *json)?;
        }
        Cmd::Control { action, duty_cycle } => {
            if !indexing_running(&cli.db)? {
                bail!("no indexer is running for this database");
            }
            if let Some(duty) = duty_cycle {
                store.set_setting("index_duty_cycle", &duty.to_string())?;
            }
            if action != "limit" {
                store.set_setting("index_action", action)?;
            }
            println!(
                "{}",
                serde_json::json!({"requested":action,"duty_cycle":store.setting("index_duty_cycle")?})
            );
        }
        Cmd::Similar { file, top, json } => {
            let (dim, enc) = config(&store)?;
            let canonical = file
                .canonicalize()
                .with_context(|| format!("file does not exist: {}", file.display()))?;
            let path = canonical.to_string_lossy().into_owned();
            let q = store.document_vector(&path, enc, dim)?;
            let hits = store
                .hits(&q, enc, top.saturating_add(1))?
                .into_iter()
                .filter(|h| h.path != path)
                .take(*top)
                .collect();
            show(hits, *json)?;
        }
        Cmd::Stats { json, under, kind } => {
            let scope = under.as_ref().map(|path| -> Result<serde_json::Value> {
                let path = path.canonicalize().with_context(|| format!("cannot open scope {}", path.display()))?;
                let (files, chunks, bytes) = store.coverage(&path, kind.as_deref())?;
                Ok(serde_json::json!({
                    "under": path, "kind": kind, "files": files, "chunks": chunks,
                    "vector_bytes": bytes, "source_state": "not_checked",
                    "state": if chunks == 0 { "no_indexed_chunks" } else { "indexed_candidates_available" },
                }))
            }).transpose()?;
            let (files, chunks, bytes) = store.stats()?;
            let disk_bytes = database_disk_bytes(&cli.db)?;
            let indexer_running = indexing_running(&cli.db)?;
            let last_scan = store
                .setting("last_scan")?
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "db": cli.db, "files": files, "chunks": chunks, "vector_bytes": bytes,
                    "database_bytes": disk_bytes,
                    "backend": store.setting("backend")?, "last_scan": last_scan,
                    "indexer_running": indexer_running,
                    "index_action": store.setting("index_action")?,
                    "index_duty_cycle": store.setting("index_duty_cycle")?,
                    "scope": scope,
                    }))?
                );
                return Ok(());
            }
            println!(
                "files: {files}\nchunks: {chunks}\nembedding bytes: {bytes}\ndatabase bytes: {}",
                disk_bytes
            );
            for name in [
                "dim",
                "encoding",
                "model",
                "model_file",
                "backend",
                "chunk_chars",
            ] {
                if let Some(v) = store.setting(name)? {
                    println!("{name}: {v}");
                }
            }
            if let Some(last_scan) = last_scan {
                println!("last scan: {last_scan}");
            }
            println!("indexer running: {indexer_running}");
            if let Some(scope) = scope {
                println!("scope: {scope}");
            }
        }
        Cmd::Extract { .. } => unreachable!(),
        Cmd::Q { query, json } => {
            let parsed = wql::parse_wql(query)?;
            let (dim, enc) = config(&store)?;
            check_backend(&store, &cli)?;
            let socket = daemon_socket(&cli);

            // File-only WQL needs no model or daemon. Each DB/model/device
            // combination gets its own socket to avoid mixing embeddings.
            if parsed.expr.has_text() && !daemon::DaemonClient::is_alive(&socket) {
                spawn_daemon_silent(&cli, &socket);
            }

            let mut fallback_engine = None;
            let response = wql::execute_wql(&parsed, &store, dim, enc, &mut |text| {
                // 1. Try smart daemon IPC.
                //    try_embed_wait polls for up to 15s while daemon loads the model.
                //    Once the socket is bound (< 5ms after spawn), it connects immediately
                //    and waits for the model-ready reply.
                if let Some(res) = daemon::DaemonClient::try_embed(&socket, text) {
                    return res;
                }

                // 2. Fallback: local in-process embedding (daemon unavailable / timed-out)
                check_backend(&store, &cli)?;
                if fallback_engine.is_none() {
                    fallback_engine = Some(new_embedder(&cli)?);
                }
                Ok(fallback_engine
                    .as_mut()
                    .unwrap()
                    .embed(
                        &[Input {
                            kind: "text",
                            value: text.into(),
                        }],
                        "query",
                    )?
                    .remove(0))
            })?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                for hit in response.hits {
                    if hit.hit.chunk_index.is_none() {
                        println!("{:.4}\t{}", hit.distance, hit.hit.path);
                    } else {
                        println!(
                            "{:.4}\t{}:{}-{}",
                            hit.distance, hit.hit.path, hit.hit.start_line, hit.hit.end_line
                        );
                    }
                }
            }
        }
        Cmd::View { action } => match action {
            ViewCmd::Set { name, path, kind } => {
                let canonical = path
                    .canonicalize()
                    .with_context(|| format!("path does not exist: {}", path.display()))?;
                store.set_view(name, &canonical.to_string_lossy(), kind.as_deref())?;
                println!("saved view '{}' -> {}", name, canonical.display());
            }
            ViewCmd::List { json } => {
                let views = store.list_views()?;
                if *json {
                    let items: Vec<_> = views
                        .into_iter()
                        .map(|(name, path, kind)| {
                            serde_json::json!({
                                "name": name,
                                "path": path,
                                "kind": kind,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&items)?);
                } else {
                    for (name, path, kind) in views {
                        println!(
                            "{}\t{}{}",
                            name,
                            path,
                            kind.map(|k| format!("\t(kind: {k})")).unwrap_or_default()
                        );
                    }
                }
            }
            ViewCmd::Rm { name } => {
                if store.delete_view(name)? {
                    println!("removed view '{name}'");
                } else {
                    bail!("view '{name}' not found");
                }
            }
        },
        Cmd::Bench { queries, top, json } => bench(&cli, &store, queries.as_deref(), *top, *json)?,
        Cmd::Flush => {
            let (dim, enc) = config(&store)?;
            if enc != Encoding::Q8 {
                bail!("flush only supported for Q8 indexes");
            }
            let n = store.flush_vectors_bin(dim)?;
            println!("flushed {n} chunks to {}", store.vec_path.display());
        }
        Cmd::Daemon {
            idle_timeout,
            socket,
        } => {
            let (dim, enc) = config(&store)?;
            check_backend(&store, &cli)?;

            // ── Phase 1: bind socket BEFORE model loading so clients can connect immediately ──
            let server = daemon::DaemonServer::new(
                socket.clone(),
                std::time::Duration::from_secs(*idle_timeout),
            );
            let listener = server.bind()?;

            // ── Phase 2: load model (clients are queued in the OS accept backlog) ──
            let mut engine = new_embedder(&cli)?;

            // ── Phase 3: serve (drains backlog + enters normal loop) ──
            let store_db = cli.db.clone();
            server.serve(
                listener,
                |text| {
                    Ok(engine
                        .embed(
                            &[Input {
                                kind: "text",
                                value: text.into(),
                            }],
                            "query",
                        )?
                        .remove(0))
                },
                move || {
                    let mut s = Store::open(&store_db)?;
                    let views = s.list_views()?;
                    let mut indexed_any = false;
                    for (name, path_str, _) in views {
                        let path = PathBuf::from(path_str);
                        if path.exists() {
                            eprintln!(
                                "weft daemon: idle auto-indexing view '{name}' ({})",
                                path.display()
                            );
                            if let Ok(()) = index(
                                &cli,
                                &mut s,
                                &path,
                                dim,
                                enc,
                                false,
                                true,
                                1100,
                                2,
                                false,
                                false,
                                &[],
                                false,
                                100,
                            ) {
                                indexed_any = true;
                            }
                        }
                    }
                    if indexed_any && enc == Encoding::Q8 {
                        let _ = s.flush_vectors_bin(dim);
                    }
                    Ok(())
                },
            )?;
        }
    }
    Ok(())
}

fn config(store: &Store) -> Result<(usize, Encoding)> {
    let dim = store
        .setting("dim")?
        .context("index empty; run index first")?
        .parse()?;
    let enc = Encoding::parse(
        &store
            .setting("encoding")?
            .context("index missing encoding")?,
    )?;
    Ok((dim, enc))
}
fn check_backend(store: &Store, cli: &Cli) -> Result<()> {
    for (name, current) in [("backend", &cli.backend), ("model", &cli.model)] {
        if let Some(v) = store.setting(name)? {
            if v != *current {
                bail!("index {name}={v}, requested {current}; specify the same model and backend");
            }
        }
    }
    if let Some(v) = store
        .setting("model_file")?
        .filter(|_| cli.backend == "native")
    {
        if v != cli.model_file.to_string_lossy() {
            bail!(
                "index model file={v}, requested {}; specify the same model file",
                cli.model_file.display()
            );
        }
    }
    if cli.backend == "http" && store.setting("endpoint")?.as_deref() != Some(&cli.endpoint) {
        bail!("embedding endpoint differs from indexed endpoint");
    }
    Ok(())
}
fn new_embedder(cli: &Cli) -> Result<Embedder> {
    Embedder::new(
        &cli.backend,
        &cli.endpoint,
        &cli.model,
        &cli.model_file,
        &cli.device,
        cli.bridge.as_deref(),
        !cli.full_modal,
    )
}

/// The optional Python process starts only for changed media files. Native text
/// and full-model media embeddings target the same EmbeddingGemma 2 space;
/// cross-backend ranking quality still needs comparison against a real model.
fn new_media_embedder(cli: &Cli) -> Result<Embedder> {
    if cli.backend == "python" {
        return new_embedder(cli);
    }
    Embedder::new(
        "python",
        &cli.endpoint,
        &cli.media_model,
        &cli.model_file,
        &cli.device,
        cli.bridge.as_deref(),
        false,
    )
}

fn daemon_socket(cli: &Cli) -> PathBuf {
    let mut h = blake3::Hasher::new();
    for part in [cli.db.as_path(), cli.model_file.as_path()] {
        let absolute = part
            .canonicalize()
            .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(part));
        h.update(absolute.to_string_lossy().as_bytes());
        h.update(&[0]);
    }
    for part in [&cli.backend, &cli.model, &cli.device] {
        h.update(part.as_bytes());
        h.update(&[0]);
    }
    let hash = h.finalize().to_hex();
    std::env::temp_dir().join(format!(
        "weft-{}-{}.sock",
        unsafe { libc::geteuid() },
        &hash[..16]
    ))
}

fn spawn_daemon_silent(cli: &Cli, socket: &Path) {
    if let Ok(exe) = std::env::current_exe() {
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--db")
            .arg(&cli.db)
            .arg("--backend")
            .arg(&cli.backend)
            .arg("--model")
            .arg(&cli.model)
            .arg("--model-file")
            .arg(&cli.model_file)
            .arg("--device")
            .arg(&cli.device)
            .arg("daemon")
            .arg("--socket")
            .arg(socket)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    let _ = libc::setsid();
                    Ok(())
                });
            }
        }
        let _ = cmd.spawn();
    }
}
fn show(hits: Vec<Hit>, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&hits)?);
    } else {
        for h in hits {
            println!(
                "{:.4}\t{}:{}-{}\t{}",
                h.score, h.path, h.start_line, h.end_line, h.kind
            );
            if h.line_kind == "extracted_text" {
                println!("coordinates: extracted text lines");
            }
        }
    }
    Ok(())
}

fn indexing_running(db: &Path) -> Result<bool> {
    let path = db.with_extension("index.lock");
    if !path.exists() {
        return Ok(false);
    }
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(false),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(err) => Err(err.into()),
    }
}

fn database_disk_bytes(db: &Path) -> Result<u64> {
    let mut bytes = std::fs::metadata(db)?.len();
    for suffix in ["-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", db.display()));
        match std::fs::metadata(path) {
            Ok(metadata) => bytes += metadata.len(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (),
            Err(err) => return Err(err.into()),
        }
    }
    Ok(bytes)
}

fn start_background(db: &Path, duty_cycle: Option<u8>) -> Result<()> {
    use std::process::{Command, Stdio};
    let log_path = db.with_extension("log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let position = args
        .iter()
        .position(|arg| arg == "--background")
        .context("missing background flag")?;
    args.remove(position);
    if duty_cycle.is_none() {
        args.push("--duty-cycle".into());
        args.push("25".into());
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach from the installer's/terminal's process group. Only async-signal-
        // safe setsid is used between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setpriority(libc::PRIO_PROCESS, 0, 10) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x00000200 | 0x00000008); // NEW_PROCESS_GROUP | DETACHED_PROCESS
    }
    let child = command.spawn().context("start background indexer")?;
    println!(
        "{}",
        serde_json::json!({"started":true,"pid":child.id(),"db":db,"log":log_path})
    );
    Ok(())
}

fn index(
    cli: &Cli,
    store: &mut Store,
    root: &Path,
    dim: usize,
    enc: Encoding,
    media: bool,
    prune: bool,
    chunk_chars: usize,
    overlap: usize,
    all_files: bool,
    computer: bool,
    exclusions: &[PathBuf],
    dry_run: bool,
    duty_cycle: u8,
) -> Result<()> {
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(cli.db.with_extension("index.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)
        .context("another indexer is already running for this database")?;
    if !dry_run {
        store.set_setting("index_action", "resume")?;
        store.set_setting("index_duty_cycle", &duty_cycle.to_string())?;
    }
    let started = Instant::now();
    let mut engine: Option<Embedder> = None;
    let mut media_engine: Option<Embedder> = None;
    let mut visited = HashSet::new();
    let (mut added, mut skipped, mut errors) = (0usize, 0usize, 0usize);
    let (mut discovered, mut filename_only) = (0usize, 0usize);
    let mut reused_chunks = 0usize;
    let mut excluded = exclusions
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
    let db = cli.db.canonicalize()?;
    excluded.push(db.clone());
    for suffix in ["-wal", "-shm", "-journal"] {
        excluded.push(PathBuf::from(format!("{}{suffix}", db.display())));
    }
    excluded.push(db.with_extension("index.lock"));
    excluded.push(db.with_extension("log"));
    let report = |status: &str,
                  discovered: usize,
                  added: usize,
                  skipped: usize,
                  filename_only: usize,
                  errors: usize| {
        serde_json::json!({"status":status,"root":root,"pid":std::process::id(),
            "duty_cycle":duty_cycle,"discovered":discovered,"indexed":added,"unchanged_or_skipped":skipped,
            "filename_only_discovered":filename_only,"errors":errors,"elapsed_seconds":started.elapsed().as_secs_f64()})
    };
    if !dry_run {
        store.set_setting("last_scan", &report("running", 0, 0, 0, 0, 0).to_string())?;
    }
    let mut walker = ignore::WalkBuilder::new(root);
    walker
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .ignore(true)
        .follow_links(false)
        .filter_entry(move |entry| !scan::excluded(entry.path(), computer, &excluded));
    for entry in walker.build() {
        if !dry_run && !resource::gate_pair(store, &mut engine, &mut media_engine)? {
            return Ok(());
        }
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("walk: {e}");
                errors += 1;
                continue;
            }
        };
        let path = entry.path();
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let mut kind = if chunk::supported(path) || document::format(path).is_some() {
            "text"
        } else if media {
            match chunk::media_kind(path) {
                Some(s) => s,
                None if all_files => "path",
                None => continue,
            }
        } else if all_files {
            "path"
        } else {
            continue;
        };
        let path_str = path.to_string_lossy().into_owned();
        if kind == "path"
            && !media
            && chunk::media_kind(path).is_some()
            && store.has_media_embedding(&path_str)?
        {
            // Default whole-computer rescans preserve optional media vectors.
            if prune {
                visited.insert(path_str);
            }
            skipped += 1;
            continue;
        }
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                errors += 1;
                continue;
            }
        };
        let max_size = if kind == "text" && document::format(path).is_some() {
            document::INPUT_LIMIT
        } else if kind == "text" {
            2 * 1024 * 1024
        } else if matches!(kind, "image" | "audio" | "video") {
            // Media is hashed by streaming; decoding happens in the bridge.
            2 * 1024 * 1024 * 1024
        } else {
            64 * 1024 * 1024
        };
        if metadata.len() > max_size as u64 && kind != "path" && !all_files {
            eprintln!("skip large file: {}", path.display());
            skipped += 1;
            continue;
        }
        if metadata.len() > max_size as u64 {
            kind = "path";
        }
        discovered += 1;
        if kind == "path" {
            filename_only += 1;
        }
        if prune {
            visited.insert(path_str.clone());
        }
        if dry_run {
            continue;
        }
        if discovered % 25 == 0 {
            store.set_setting(
                "last_scan",
                &report("running", discovered, added, skipped, filename_only, errors).to_string(),
            )?;
            eprintln!(
                "scanned={discovered} indexed={added} unchanged/skipped={skipped} errors={errors}"
            );
        }
        let mut data = if kind == "path" || matches!(kind, "image" | "audio" | "video") {
            Vec::new()
        } else {
            match std::fs::File::open(path).and_then(|file| {
                use std::io::Read;
                let mut bytes = Vec::new();
                file.take(max_size as u64 + 1).read_to_end(&mut bytes)?;
                Ok(bytes)
            }) {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("{}: {e}", path.display());
                    errors += 1;
                    continue;
                }
            }
        };
        if data.len() > max_size {
            if !all_files {
                skipped += 1;
                continue;
            }
            kind = "path";
            filename_only += 1;
            data.clear();
        }
        if kind == "text"
            && document::format(path).is_none()
            && (data.contains(&0) || std::str::from_utf8(&data).is_err())
        {
            if !all_files {
                skipped += 1;
                continue;
            }
            kind = "path";
            filename_only += 1;
            data.clear();
        }
        let mut hash = if kind == "path" {
            let signature = format!(
                "filename-v1:{path_str}:{}:{:?}",
                metadata.len(),
                metadata.modified().ok()
            );
            blake3::hash(signature.as_bytes()).to_hex().to_string()
        } else if matches!(kind, "image" | "audio" | "video") {
            // Do not allocate/read a 2 GiB movie into RAM just to detect
            // whether it changed. Reuse persisted vectors for exact matches.
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0u8; 64 * 1024];
            let hash_result = (|| -> std::io::Result<()> {
                let mut file = std::fs::File::open(path)?;
                loop {
                    let len = file.read(&mut buffer)?;
                    if len == 0 {
                        break;
                    }
                    hasher.update(&buffer[..len]);
                }
                Ok(())
            })();
            if let Err(err) = hash_result {
                eprintln!("{}: media hash failed: {err}", path.display());
                errors += 1;
                continue;
            }
            hasher.finalize().to_hex().to_string()
        } else {
            blake3::hash(&data).to_hex().to_string()
        };
        if store.unchanged(&path_str, &hash)? {
            skipped += 1;
            continue;
        }
        let extracted = if kind == "text" {
            match document::extract(path, &data, false) {
                Ok(doc) if !doc.text.trim().is_empty() => Some(doc),
                outcome => {
                    if let Err(error) = outcome {
                        eprintln!("{}: extraction failed: {error}", path.display());
                        errors += 1;
                    }
                    if !all_files {
                        skipped += 1;
                        continue;
                    }
                    kind = "path";
                    filename_only += 1;
                    let signature = format!(
                        "filename-v1:{path_str}:{}:{:?}",
                        metadata.len(),
                        metadata.modified().ok()
                    );
                    hash = blake3::hash(signature.as_bytes()).to_hex().to_string();
                    None
                }
            }
        } else {
            None
        };
        let segments: Vec<(usize, usize, Input)> = if let Some(extracted) = &extracted {
            let text = &extracted.text;
            chunk::split(text, chunk_chars, overlap)
                .into_iter()
                .map(|part| {
                    (
                        part.start,
                        part.end,
                        Input {
                            kind: "text",
                            value: part.text,
                        },
                    )
                })
                .collect()
        } else if kind == "path" {
            vec![(
                0,
                0,
                Input {
                    kind: "text",
                    value: scan::filename_text(path),
                },
            )]
        } else {
            vec![(
                0,
                0,
                Input {
                    kind,
                    value: path_str.clone(),
                },
            )]
        };
        if segments.is_empty() {
            skipped += 1;
            continue;
        }
        let mut cache = store.cached_chunks(&path_str)?;
        if segments.len() > 32 {
            let mut progress = report("running", discovered, added, skipped, filename_only, errors);
            progress["active_file"] = serde_json::json!(path);
            progress["chunks_total"] = serde_json::json!(segments.len());
            progress["chunks_completed"] = serde_json::json!(0);
            store.set_setting("last_scan", &progress.to_string())?;
        }
        let mut packed = Vec::with_capacity(segments.len());
        let mut success = true;
        for batch in segments.chunks(1) {
            if !resource::gate_pair(store, &mut engine, &mut media_engine)? {
                return Ok(());
            }
            let item = &batch[0];
            let input_hash = if item.2.kind == "text" {
                Some(*blake3::hash(item.2.value.as_bytes()).as_bytes())
            } else {
                None
            };
            if let Some(bytes) = input_hash.as_ref().and_then(|hash| cache.get(&hash[..])) {
                packed.push((item.0, item.1, bytes.clone(), input_hash));
                reused_chunks += 1;
                continue;
            }
            let selected_engine = if item.2.kind == "text" || cli.backend == "python" {
                &mut engine
            } else {
                &mut media_engine
            };
            if selected_engine.is_none() {
                *selected_engine = Some(if item.2.kind == "text" {
                    new_embedder(cli)?
                } else {
                    new_media_embedder(cli)?
                });
            }
            let compute_started = Instant::now();
            let input: Vec<Input> = batch.iter().map(|item| item.2.clone()).collect();
            match selected_engine.as_mut().unwrap().embed(&input, "document") {
                Ok(vectors) => {
                    for (item, v) in batch.iter().zip(vectors.iter()) {
                        let norm = normalized_prefix(v, dim)?;
                        let bytes = store::encode(&norm, enc);
                        if let Some(hash) = input_hash {
                            cache.insert(hash.to_vec(), bytes.clone());
                        }
                        packed.push((item.0, item.1, bytes, input_hash));
                    }
                    if segments.len() > 32 && packed.len() % 64 == 0 {
                        let mut progress =
                            report("running", discovered, added, skipped, filename_only, errors);
                        progress["active_file"] = serde_json::json!(path);
                        progress["chunks_total"] = serde_json::json!(segments.len());
                        progress["chunks_completed"] = serde_json::json!(packed.len());
                        store.set_setting("last_scan", &progress.to_string())?;
                    }
                }
                Err(err) => {
                    eprintln!("{}: {err:#}", path.display());
                    errors += 1;
                    success = false;
                    break;
                }
            }
            if !resource::throttle_pair(
                store,
                &mut engine,
                &mut media_engine,
                compute_started.elapsed(),
            )? {
                return Ok(());
            }
        }
        if !success {
            continue;
        }
        store.replace(&path_str, &hash, kind, metadata.len() as usize, &packed)?;
        added += 1;
    }
    if dry_run {
        println!(
            "{}",
            report("preview", discovered, 0, 0, filename_only, errors)
        );
        return Ok(());
    }
    let deleted = if prune && errors == 0 {
        store.prune(&visited, root)?
    } else {
        0
    };
    if prune && errors != 0 {
        eprintln!("prune skipped due to scan/embedding errors");
    }
    let (files, chunks, bytes) = store.stats()?;
    let mut final_report = report(
        if errors == 0 { "complete" } else { "partial" },
        discovered,
        added,
        skipped,
        filename_only,
        errors,
    );
    final_report["reused_chunks"] = serde_json::json!(reused_chunks);
    store.set_setting("last_scan", &final_report.to_string())?;
    println!(
        "indexed={added} reused_chunks={reused_chunks} unchanged/skipped={skipped} errors={errors} pruned={deleted} files={files} chunks={chunks} vector_bytes={bytes} elapsed={:.2}s",
        started.elapsed().as_secs_f32()
    );
    if errors > 0 {
        bail!("{errors} files failed to index");
    }
    // Flush the mmap vector snapshot (Q8 only). This is a background-safe
    // atomic rename, so concurrent readers of the old file are unaffected.
    if enc == Encoding::Q8 {
        match store.flush_vectors_bin(dim) {
            Ok(n) => eprintln!(
                "vectors.bin: wrote {n} chunks → {}",
                store.vec_path.display()
            ),
            Err(e) => eprintln!(
                "warning: could not write vectors.bin ({e}); search will fall back to SQLite"
            ),
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct BenchmarkQuery {
    query: String,
    relevant: Vec<String>,
}

fn bench(cli: &Cli, store: &Store, path: Option<&Path>, top: usize, json: bool) -> Result<()> {
    if let Some(path) = path {
        return bench_reference_queries(cli, store, path, top, json);
    }
    bench_comprehensive(cli, store, top, json)
}

fn get_peak_rss_mb() -> f64 {
    #[cfg(unix)]
    unsafe {
        let mut rusage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut rusage) == 0 {
            #[cfg(target_os = "macos")]
            return rusage.ru_maxrss as f64 / (1024.0 * 1024.0);
            #[cfg(not(target_os = "macos"))]
            return rusage.ru_maxrss as f64 / 1024.0;
        }
    }
    0.0
}

fn bench_comprehensive(cli: &Cli, store: &Store, top: usize, json: bool) -> Result<()> {
    let (dim, enc) = config(store)?;
    check_backend(store, cli)?;

    let (indexed_files, indexed_chunks, vector_bytes) = store.stats()?;

    // 1. Model Loading / Initialization
    let load_start = Instant::now();
    let mut engine = new_embedder(cli)?;
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

    // 2. Cold Inference
    let cold_start = Instant::now();
    let _ = engine.embed(
        &[Input {
            kind: "text",
            value: "콜드 스타트 웜업 질의".into(),
        }],
        "query",
    )?;
    let cold_infer_ms = cold_start.elapsed().as_secs_f64() * 1000.0;

    // 3. Short Warm Queries (5 runs for stable p50)
    let short_text = "재시도 백오프 알고리즘 구현";
    let mut short_times = Vec::with_capacity(5);
    for _ in 0..5 {
        let s = Instant::now();
        let _ = engine.embed(
            &[Input {
                kind: "text",
                value: short_text.into(),
            }],
            "query",
        )?;
        short_times.push(s.elapsed().as_secs_f64() * 1000.0);
    }
    short_times.sort_by(|a, b| a.total_cmp(b));
    let short_p50 = short_times[2];
    let short_min = short_times[0];
    let short_max = short_times[4];

    // 4. Medium Query (~150 chars)
    let medium_text = "소프트웨어 아키텍처에서 대규모 분산 시스템의 네트워크 단절 및 일시적 장애를 복구하기 위한 지수 백오프 기반의 재시도 정책 및 서킷 브레이커 패턴";
    let med_start = Instant::now();
    let _ = engine.embed(
        &[Input {
            kind: "text",
            value: medium_text.into(),
        }],
        "query",
    )?;
    let med_ms = med_start.elapsed().as_secs_f64() * 1000.0;

    // 5. WQL Engine Retrieval Benchmarks
    // Match the bounded, exact-text cache of the long-lived WQL daemon.
    // The first occurrence incurs inference; subsequent queries reuse it.
    let mut embedding_cache = daemon::EmbeddingCache::new(128);
    let mut embed_fn = |text: &str| -> Result<Vec<f32>> {
        embedding_cache.embed(text, &mut |text| {
            Ok(engine
                .embed(
                    &[Input {
                        kind: "text",
                        value: text.into(),
                    }],
                    "query",
                )?
                .remove(0))
        })
    };

    // Query A: Single text
    let q_a = wql::parse_wql(&format!(r#""재시도" top {top}"#))?;
    let start_a = Instant::now();
    let res_a = wql::execute_wql(&q_a, store, dim, enc, &mut embed_fn)?;
    let time_a_ms = start_a.elapsed().as_secs_f64() * 1000.0;
    let rate_a = (res_a.evaluated_candidates as f64) / (time_a_ms / 1000.0).max(1e-6);

    // Query B: Compound All (2 branches)
    let q_b = wql::parse_wql(&format!(r#"all("재시도", "백오프") top {top}"#))?;
    let start_b = Instant::now();
    let res_b = wql::execute_wql(&q_b, store, dim, enc, &mut embed_fn)?;
    let time_b_ms = start_b.elapsed().as_secs_f64() * 1000.0;
    let rate_b = (res_b.evaluated_candidates as f64) / (time_b_ms / 1000.0).max(1e-6);

    // Query C: Complex Nested (Any + All + Avoid)
    let q_c = wql::parse_wql(&format!(
        r#"any( all("재시도","백오프"), avoid("모델 라우팅","테스트 구현") ) top {top}"#
    ))?;
    let start_c = Instant::now();
    let res_c = wql::execute_wql(&q_c, store, dim, enc, &mut embed_fn)?;
    let time_c_ms = start_c.elapsed().as_secs_f64() * 1000.0;
    let rate_c = (res_c.evaluated_candidates as f64) / (time_c_ms / 1000.0).max(1e-6);

    // Same query, same score/ranking, all four embeddings already cached.
    let repeat_start = Instant::now();
    let res_repeat = wql::execute_wql(&q_c, store, dim, enc, &mut embed_fn)?;
    let time_repeat_ms = repeat_start.elapsed().as_secs_f64() * 1000.0;
    if serde_json::to_value(&res_c.hits)? != serde_json::to_value(&res_repeat.hits)? {
        bail!("cached WQL query returned different search results");
    }

    // Benchmark the same memory-mapped path as live retrieval. Avoid loading
    // a duplicate heap copy of every vector, which skewed latency and peak RSS.
    let warm_scan_a = if enc == Encoding::Q8 {
        if let Ok(cache) = store.load_mmap_cache(dim) {
            let compiled_a = wql::compile_wql(&q_a.expr, dim, &mut embed_fn, &mut |p| {
                store.document_vector(&p.to_string_lossy(), enc, dim)
            })?;
            // The mapped pages and CPU clock can be cold after Metal inference.
            // Report a warmed, repeatable scan + per-file Top-K measurement.
            for _ in 0..2 {
                std::hint::black_box(cache.scan_wql_unfiltered(&compiled_a, top, true));
            }
            let mut samples = [0f64; 11];
            for sample in &mut samples {
                let s = Instant::now();
                std::hint::black_box(cache.scan_wql_unfiltered(&compiled_a, top, true));
                *sample = s.elapsed().as_secs_f64() * 1000.0;
            }
            samples.sort_by(f64::total_cmp);
            Some((samples[5], samples[9], samples[0], cache.count))
        } else {
            None
        }
    } else {
        None
    };

    let peak_rss_mb = get_peak_rss_mb();

    if json {
        let out = serde_json::json!({
            "device": cli.device,
            "backend": cli.backend,
            "dim": dim,
            "encoding": enc.name(),
            "indexed_files": indexed_files,
            "indexed_chunks": indexed_chunks,
            "vector_bytes": vector_bytes,
            "peak_rss_mb": peak_rss_mb,
            "inference": {
                "model_load_ms": load_ms,
                "cold_infer_ms": cold_infer_ms,
                "short_query_p50_ms": short_p50,
                "short_query_min_ms": short_min,
                "short_query_max_ms": short_max,
                "medium_query_ms": med_ms,
            },
            "wql_retrieval": {
                "single_text": {
                    "branches": res_a.compiled_branches,
                    "elapsed_ms": time_a_ms,
                    "timings_ms": res_a.timings_ms,
                    "pure_simd_ms": warm_scan_a.map(|(p50, _, _, _)| p50),
                    "mmap_scan_p50_ms": warm_scan_a.map(|(p50, _, _, _)| p50),
                    "mmap_scan_p90_ms": warm_scan_a.map(|(_, p90, _, _)| p90),
                    "mmap_scan_min_ms": warm_scan_a.map(|(_, _, min, _)| min),
                    "mmap_scan_candidates": warm_scan_a.map(|(_, _, _, count)| count),
                    "evaluated_candidates": res_a.evaluated_candidates,
                    "candidates_per_sec": rate_a,
                    "top_hits": res_a.hits.len(),
                },
                "all_2branch": {
                    "branches": res_b.compiled_branches,
                    "elapsed_ms": time_b_ms,
                    "timings_ms": res_b.timings_ms,
                    "evaluated_candidates": res_b.evaluated_candidates,
                    "candidates_per_sec": rate_b,
                    "top_hits": res_b.hits.len(),
                },
                "nested_any_avoid": {
                    "branches": res_c.compiled_branches,
                    "elapsed_ms": time_c_ms,
                    "timings_ms": res_c.timings_ms,
                    "evaluated_candidates": res_c.evaluated_candidates,
                    "candidates_per_sec": rate_c,
                    "top_hits": res_c.hits.len(),
                },
                "nested_any_avoid_cached_repeat": {
                    "branches": res_repeat.compiled_branches,
                    "elapsed_ms": time_repeat_ms,
                    "timings_ms": res_repeat.timings_ms,
                    "evaluated_candidates": res_repeat.evaluated_candidates,
                    "top_hits": res_repeat.hits.len(),
                }
            }
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("============================================================");
        println!("              WEFT ENGINE PERFORMANCE BENCHMARK             ");
        println!("============================================================");
        println!(
            "Device: {:<12} Backend: {:<8} Encoding: {} ({}d)",
            cli.device,
            cli.backend,
            enc.name(),
            dim
        );
        println!(
            "Dataset: {} files / {} chunks (Q8: {:.2} MB)",
            indexed_files,
            indexed_chunks,
            vector_bytes as f64 / (1024.0 * 1024.0)
        );
        println!("Memory Peak RSS: {:.2} MiB", peak_rss_mb);
        println!("------------------------------------------------------------");
        println!("▶ [Embedding Inference Latency]");
        println!("  Model Load:              {:>8.2} ms", load_ms);
        println!("  Cold Inference:          {:>8.2} ms", cold_infer_ms);
        println!(
            "  Short Query (Warm p50):  {:>8.2} ms  (min: {:.2}ms, max: {:.2}ms)",
            short_p50, short_min, short_max
        );
        println!("  Medium Query (Warm):     {:>8.2} ms", med_ms);
        println!("------------------------------------------------------------");
        let has_mmap = store.load_mmap_cache(dim).is_ok();
        println!(
            "▶ [WQL Scan & Retrieval Throughput]  (mmap: {})",
            if has_mmap {
                "active"
            } else {
                "none (fallback)"
            }
        );
        println!("  1. Single Text (\"재시도\"):");
        if let Some((p50, p90, min, scanned)) = warm_scan_a {
            let scan_rate = (scanned as f64) / (p50 / 1000.0).max(1e-6);
            println!(
                "     Warm mmap scan+TopK:  {:>7.2} ms p50 (min: {:.2}, p90: {:.2}) | Scanned: {} | {:.0} cands/sec",
                p50, min, p90, scanned, scan_rate
            );
        }
        println!(
            "     E2E WQL (incl. embed): {:>7.2} ms | Scanned: {:>6} | {:>9.0} cands/sec",
            time_a_ms, res_a.evaluated_candidates, rate_a
        );
        println!("  2. all(\"재시도\",\"백오프\"):");
        println!(
            "     E2E WQL (incl. embed): {:>7.2} ms | Scanned: {:>6} | {:>9.0} cands/sec",
            time_b_ms, res_b.evaluated_candidates, rate_b
        );
        println!("  3. any(all(), avoid()):");
        println!(
            "     E2E WQL (incl. embed): {:>7.2} ms | Scanned: {:>6} | {:>9.0} cands/sec",
            time_c_ms, res_c.evaluated_candidates, rate_c
        );
        println!(
            "  4. any(all(), avoid()), repeated (cached embeddings): {:.2} ms | Scanned: {}",
            time_repeat_ms, res_repeat.evaluated_candidates
        );
        println!("------------------------------------------------------------");
        println!("▶ [WQL Phase Breakdown] (ms; compile excludes embedding/lookup)");
        for (name, t) in [
            ("single", &res_a.timings_ms),
            ("all", &res_b.timings_ms),
            ("nested", &res_c.timings_ms),
            ("repeat", &res_repeat.timings_ms),
        ] {
            println!(
                "  {:<7} embed {:>6.2} compile {:>5.2} scope {:>5.2} load {:>5.2} scan+TopK {:>6.2} result {:>5.2} | terms {}/{} (reused {})",
                name,
                t.embedding_ms,
                t.compile_ms,
                t.scope_ms,
                t.cache_load_ms,
                t.scan_ms,
                t.result_ms,
                t.unique_text_terms,
                t.text_terms,
                t.reused_text_terms
            );
        }
        println!("============================================================");
    }
    Ok(())
}

fn bench_reference_queries(
    cli: &Cli,
    store: &Store,
    path: &Path,
    top: usize,
    json: bool,
) -> Result<()> {
    let (dim, enc) = config(store)?;
    check_backend(store, cli)?;
    if dim != 768 || enc != Encoding::F32 {
        bail!("bench requires a 768-dimensional f32 reference index");
    }
    let queries: Vec<BenchmarkQuery> = serde_json::from_slice(&std::fs::read(path)?)?;
    if queries.is_empty() || top == 0 {
        bail!("benchmark needs >=1 query and --top > 0");
    }
    if queries.iter().any(|q| q.relevant.is_empty()) {
        bail!("each benchmark query needs relevant file names");
    }
    let doc_vectors = store.vectors()?;
    if doc_vectors.is_empty() {
        bail!("benchmark index is empty");
    }
    let mut engine = new_embedder(cli)?;
    let inputs: Vec<Input> = queries
        .iter()
        .map(|q| Input {
            kind: "text",
            value: q.query.clone(),
        })
        .collect();
    let mut query_vectors = Vec::new();
    for batch in inputs.chunks(8) {
        query_vectors.extend(engine.embed(batch, "query")?);
    }
    let variants = [
        (768, Encoding::F32),
        (768, Encoding::Q8),
        (512, Encoding::F32),
        (512, Encoding::Q8),
        (256, Encoding::F32),
        (256, Encoding::Q8),
    ];
    let mut reports = Vec::new();
    let full_docs: Vec<_> = doc_vectors
        .iter()
        .map(|(p, v)| (p.clone(), store::encode(v, Encoding::F32)))
        .collect();
    let baseline: Vec<_> = query_vectors
        .iter()
        .map(|v| rank(&normalized_prefix(v, 768)?, &full_docs, Encoding::F32, top))
        .collect::<Result<_>>()?;
    for (n, format) in variants {
        let docs = doc_vectors
            .iter()
            .map(|(p, v)| Ok((p.clone(), store::encode(&normalized_prefix(v, n)?, format))))
            .collect::<Result<Vec<_>>>()?;
        let stored_bytes: usize = docs.iter().map(|(_, b)| b.len()).sum();
        let (mut recall, mut agreement, mut latency) = (0f32, 0f32, 0f64);
        let mut misses = Vec::new();
        for ((query, labeled), reference) in query_vectors.iter().zip(&queries).zip(&baseline) {
            let qt = normalized_prefix(query, n)?;
            let timer = Instant::now();
            let predicted = rank(&qt, &docs, format, top)?;
            latency += timer.elapsed().as_secs_f64();
            let correct = predicted
                .iter()
                .filter(|p| labeled.relevant.iter().any(|r| p.ends_with(r)))
                .count();
            if correct == 0 {
                misses.push(serde_json::json!({
                    "query": labeled.query,
                    "expected": labeled.relevant,
                    "top_result": predicted.first(),
                }));
            }
            recall += correct as f32 / labeled.relevant.len().min(top).max(1) as f32;
            agreement += predicted.iter().filter(|p| reference.contains(p)).count() as f32
                / reference.len().max(1) as f32;
        }
        reports.push(serde_json::json!({
            "dim":n, "encoding":format.name(),"vector_bytes":stored_bytes,
            "recall_at_k":recall/queries.len() as f32,
            "agreement_with_f32_768_at_k":agreement/queries.len() as f32,
            "scan_ms_per_query":latency*1000.0/queries.len() as f64,
            "misses":misses
        }));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else {
        println!(
            "{:>4} {:>4} {:>10} {:>10} {:>12} {:>10}",
            "dim", "type", "bytes", "recall@k", "agree@k", "scan_ms"
        );
        for r in &reports {
            println!(
                "{:>4} {:>4} {:>10} {:>10.3} {:>12.3} {:>10.3}",
                r["dim"],
                r["encoding"].as_str().unwrap_or("?"),
                r["vector_bytes"],
                r["recall_at_k"].as_f64().unwrap_or(0.),
                r["agreement_with_f32_768_at_k"].as_f64().unwrap_or(0.),
                r["scan_ms_per_query"].as_f64().unwrap_or(0.)
            );
        }
    }
    Ok(())
}

fn rank(
    query: &[f32],
    docs: &[(String, Vec<u8>)],
    enc: Encoding,
    top: usize,
) -> Result<Vec<String>> {
    use std::collections::HashMap;
    let mut best = HashMap::<String, f32>::new();
    for (path, vec) in docs {
        let score = vector::similarity(query, vec, enc)?;
        best.entry(path.clone())
            .and_modify(|x| {
                if score > *x {
                    *x = score
                }
            })
            .or_insert(score);
    }
    let mut order: Vec<_> = best.into_iter().collect();
    order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(order.into_iter().take(top).map(|x| x.0).collect())
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    #[test]
    fn bootstrap_can_read_version_without_a_subcommand() {
        let error = Cli::try_parse_from(["weft", "--version"]).err().unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }
}
