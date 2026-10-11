mod chunk;
mod document;
mod embed;
mod hierarchy;
mod native;
mod query;
mod resource;
mod retrieve;
mod scan;
mod store;
mod vector;

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
    #[arg(long, global = true, default_value = "scripts/embed_bridge.py")]
    bridge: PathBuf,
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
    /// Compare truncated/quantized vectors against FP32 768-dim, with labeled queries.
    Bench {
        #[arg(long)]
        queries: PathBuf,
        #[arg(long, default_value_t = 5)]
        top: usize,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
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
    let mut store = Store::open(&cli.db)?;
    match &cli.command {
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
            if *media && cli.backend != "python" {
                bail!(
                    "native/HTTP models contain text weights only; media needs --backend python --full-modal"
                );
            }
            if *media && !cli.full_modal {
                bail!("--media requires --full-modal to load the image/audio encoders");
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
        Cmd::Bench { queries, top, json } => bench(&cli, &store, queries, *top, *json)?,
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
        &cli.bridge,
        !cli.full_modal,
    )
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
        if !dry_run && !resource::gate(store, &mut engine)? {
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
        let mut data = if kind == "path" {
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
            if !resource::gate(store, &mut engine)? {
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
            if engine.is_none() {
                engine = Some(new_embedder(cli)?);
            }
            let compute_started = Instant::now();
            let input: Vec<Input> = batch.iter().map(|item| item.2.clone()).collect();
            match engine.as_mut().unwrap().embed(&input, "document") {
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
            if !resource::throttle(store, &mut engine, compute_started.elapsed())? {
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
    Ok(())
}

#[derive(serde::Deserialize)]
struct BenchmarkQuery {
    query: String,
    relevant: Vec<String>,
}

fn bench(cli: &Cli, store: &Store, path: &Path, top: usize, json: bool) -> Result<()> {
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
        let error = Cli::try_parse_from(["myvec", "--version"]).err().unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }
}
