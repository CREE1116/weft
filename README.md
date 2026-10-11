# Weft

Weft (the crosswise thread woven on a loom) is the file-finding companion to Loom. The installer also provides `myvec` as a compatibility command. Existing storage paths (`~/.local/share/myvec` and the `myvec` model cache) are deliberately retained: this rename does not copy weights, rebuild vectors, or change the database format. `WEFT_*` environment settings take precedence over their legacy `MYVEC_*` equivalents. Historical experiment reports retain the name used at measurement time.

A local file explorer for agents: locate relevant folders, choose files, and retrieve verified passages using semantic or filename search. A portable agent skill guides exploration and prepares the Rust engine on first use. Multi-criterion vector distances let an agent refine results with requirements and positive/negative file examples.

The engine combines in-process EmbeddingGemma 2 inference, incremental indexing and exact cosine vector search. Candle supplies CPU, CUDA and Metal kernels. The index stores vectors, paths, hashes and line ranges without copying source text; native text inference needs no Python or server.

See the [MacBook Pro M4 Pro (24 GB) performance measurements](eval/M4PRO_PERFORMANCE_2026-10-12.md) for a seven-run, old-vs-optimized benchmark of model loading, memory, and exact mmap retrieval.

## Skill-first installation

Install the complete `.agents/skills/weft-semantic-search` directory in your agent's skill directory, including `scripts` and `references`. From this checkout:

```sh
mkdir -p "$HOME/.agents/skills"
cp -R .agents/skills/weft-semantic-search "$HOME/.agents/skills/"
```

On first use, the skill directs the agent to its `scripts/bootstrap.sh` if weft is absent or older than 0.7.0. This prepares the native engine and cached text model, then uses the existing installation on later requests. Copying the skill is not an installation execution hook. The source repository is currently private: a new machine needs GitHub repository access, or an existing checkout supplied with `bootstrap.sh --source /path/to/weft`. Rust Cargo is required for the current source-build distribution. Downloads/builds are setup costs, not ordinary search costs; prebuilt release binaries are not distributed yet.

Bootstrap starts no scan by default. For an explicitly requested initial folder, run `bootstrap.sh --scan-root /absolute/folder`. The agent checks `weft stats --under /absolute/folder --kind text --json` before a scoped content search, so a missing index is distinguished from no relevant match. Index only the requested scope when coverage is missing. Folder overview → file selection → chunk evidence is an exploration workflow, not an automatic approximate-search cutoff. See [agent explorer verification](eval/AGENT_EXPLORER_RESULTS.md) for setup and coverage checks.

## Desktop install and agent use

```sh
./scripts/install.sh
weft stats --json
weft search "재시도 간격을 설정한 문서" --top 10 --json
```

The installer downloads/reuses the text model, installs the CLI under `$CARGO_HOME/bin` (normally `~/.cargo/bin`), installs the agent skill at `~/.agents/skills/weft-semantic-search`, and starts the first whole-computer scan in the background. On macOS it builds with Metal. Use `--cpu` or `--cuda` to select another build, or `--no-index` to install without starting a scan. `--scan-root PATH` starts a scan limited to that root. These are installer options; search/device options belong to `weft`.

```sh
weft index --all --background     # independent background desktop scan
weft index --all --dry-run        # preview without embeddings
weft index --all --exclude ~/Downloads
weft index ~/Documents           # content-only update for supported text
weft index --all ~/Documents      # also include filenames of other formats
weft similar ~/Documents/plan.md --json
```

On Unix/macOS, the default computer scan starts at `/`, including user folders and mounted volumes; on Windows the default is the system drive (other drives can be scanned explicitly). It excludes OS directories, per-user application data/caches, generated dependency/build trees, credentials/private keys, and its own database/model data. It honors ignore files and does not follow symlinks. Custom omissions use repeated `--exclude PATH`. Exclusions and denied filesystem permissions mean "whole computer" covers accessible, non-excluded files, not literally every file.

Supported UTF-8 text up to 2 MiB is embedded by overlapping chunks. Supported PDF/Office/OpenDocument text is also parsed under the limits below. Unsupported formats, binary input and oversized files receive one **filename/folder/type embedding** in `--all` mode and return `kind=path` with zero line numbers. This makes those files discoverable without implying their contents were parsed. Native media-content search is still pending.

`index --background` detaches its worker from the terminal/installer process group and writes to the database's `.log` sibling file. This also works for a requested folder.

The initial scan's progress is in `weft stats --json`, with details in `~/.local/share/myvec/index.log`. A scan reports `complete` or `partial` after it exits; a `running` last scan with `indexer_running=false` was interrupted. Results already written are searchable while indexing continues. SQLite WAL supports concurrent searches; only one indexer can update a database at once. Rerun indexing to pick up changes; no persistent watcher is installed. Each run hashes supported text and uses size/modified time for filename-only records. Unchanged files are not re-embedded. `--prune` removes disappeared records only after an error-free scan.

GPU use during the initial scan is expected with the Metal/CUDA build: files are embedded locally in the background. The indexing worker exits when the scan finishes. The optional WQL query daemon keeps the model loaded for an idle period (default 180 seconds) and then exits. On Unix, stop an indexing worker with `kill -INT PID`, using `last_scan.pid` from stats after confirming `indexer_running=true`. Restart with `weft --device cpu index --all --background` to use CPU instead, or omit `--device cpu` to resume GPU indexing. Committed files are reused.

Reload your agent session after installing the skill if it does not appear yet. The skill lets the agent query the same CLI and read matching source lines. It does not grant permission to expand a future user's indexing scope.

## Document text and embedded images

PDF, DOCX/DOCM, PPTX/PPTM, XLSX/XLSM and ODT/ODP/ODS are parsed inside Rust. Ordinary `index FOLDER` includes their text; existing filename-only records are upgraded on the next scan. Word paragraphs/tables and headers/footnotes, presentation slides in actual presentation order, worksheet string/numeric/cached values and OpenDocument text feed the same chunk/file/folder search. Macros are never executed. No LibreOffice, Python or external PDF converter is required.

```sh
weft index ~/Documents
weft retrieve "retry policy" --under ~/Documents --kind text --level chunk --context-chars 4000 --json
weft extract ./report.pdf --json
weft extract ./presentation.pptx --images-dir ./extracted-images --json
```

Extraction runs without model inference or opening an index. Image inventory includes original Office media parts and PDF image streams, including nested resources. Office images retain their bytes. PDF JPEG/JPX streams are retained; supported raw/Flate 8-bit RGB/gray rasters become lossless PNG. Unsupported PDF encodings are marked, not exported as valid images. PDF page association is supplied where directly known; otherwise object IDs identify assets. Inline images, mask compositing and rendered page layout are not reconstructed. Images are exported only with `--images-dir`; content-derived names deduplicate exports and refuse overwriting conflicting files. This is extraction, not visual-semantic indexing or OCR.

Only vectors and locations are persisted during indexing, with no document text/image copies. Evidence rereads the source, verifies its binary content hash, then reconstructs the extracted chunk. `line_kind=extracted_text` distinguishes extracted line coordinates; `source_locations` identifies PDF pages, slides, sheets or Word/OpenDocument parts on verified evidence. DOCX page numbers depend on layout and are not inferred. Document input is limited to 32 MiB, extracted UTF-8 text to 2 MiB, ZIP parts to 8 MiB, ZIP entries to 8,192, declared ZIP expansion to 64 MiB and image output to 32 MiB per request. PDF parser working memory is not guaranteed to stay within the input limit. Encrypted/corrupt/oversized documents are skipped in ordinary indexing; `--all` can fall back to filename-only records while still reporting a partial scan/nonzero exit for parser failures. Image-only PDFs need OCR to become content-searchable. Legacy DOC/PPT/XLS and HWP are still filename-only; rich layout, charts and spreadsheet formatting/calculation are not reconstructed. See [document verification](eval/DOCUMENT_RESULTS.md).

## Agent retrieval: multiple criteria in one search

```sh
weft retrieve "database migration" --also "expand contract rollout" --avoid "backup restoration" --json
weft retrieve --file ./design.md --file ./requirements.md --avoid-file ./rejected.md --json
weft retrieve "retry policy" --under ~/Documents --kind text --chunks --top 5 --context-chars 4000 --json
weft retrieve "database" --diversity 0.5 --dedup-cosine 0.98 --pool 100 --json
```

`retrieve` is the primary multi-basis vector search interface. It combines text and indexed-file criteria directly, without requiring set definitions or intermediate top-K lists. Each eligible chunk is scored against **every** criterion before selecting candidates. File criteria use stored chunk centroids, exclude reference files from results, and need no embedding model. Text criteria share one model instance per call. File criteria represent the indexed version; reindex references if they have changed.

For normalized reconstructed candidate vectors, let `d(C,A) = 1 - cosine(C,A)`. Positive combination options are `--combine all` (default, sum of distances), `any` (minimum distance), `strict` (maximum distance), and `bridge` (an explicit name for maximum positive distance, i.e. maximum weakest similarity). With negative examples, subtract `--avoid-weight` times the minimum distance to a negative example, default weight 1. A candidate far from **all** negative examples gets a larger subtraction. These are ranking preferences, not Boolean exclusions or entailment tests. Composite distance may be negative, and smaller is better. More positive criteria increase the sum's scale; tune the negative weight accordingly. `--exclude PATH` and `--under PATH` are hard path filters applied before scoring. `--max-distance` optionally caps composite distance.

The default keeps one best matching chunk per file. `--chunks` allows several evidence chunks from the same file. Exact composite-score top-K selection scans all eligible chunks and uses bounded candidate memory. There is no per-criterion top-K cutoff. The stored vectors are still Q8 by default; "exact" refers to ranking those reconstructed vectors, not equivalence to an unquantized model.

Choose the result unit explicitly:

```sh
weft retrieve "retry policy" --level folder --under ~/Documents --kind text --json
weft retrieve "retry policy" --level file --under ~/Documents/projects --kind text --json
weft retrieve "retry policy" --level chunk --under ~/Documents/projects --kind text --context-chars 4000 --json
```

`--level chunk` scores individual chunks (`--chunks` is the compatible older spelling). `--level file` scores the normalized sum of stored chunk vectors, which represents the whole document rather than its best passage. `--level folder` scores the normalized sum of normalized file representatives with equal file weight, grouping **immediate parent directories**, not entire recursive subtrees. Nested directories are separate results. All hard filters and reference-file exclusions apply to member files before aggregation. These modes use the same multi-criterion distance formula and optional diversity. Whole-document representatives may dilute a small relevant passage; use chunk search for precise evidence.

JSON adds `level`, `aggregation`, `evaluated_candidates` and per-hit `member_files`/`member_chunks`. Default best-chunk hits report one selected file/chunk, not full document membership. Aggregate hits have line coordinates zero, no chunk index, no excerpt and `source_state=aggregate_not_checked`, even when a context budget is requested. Their stored hashes identify the indexed file version or folder membership snapshot, not a live source verification. Drill down with `--under FOLDER --level chunk --context-chars N` for verified evidence. Representatives are computed from the existing index: no extra model inference for file-only criteria, no additional stored vectors, source copies or schema migration. File mode holds one current sum plus the bounded candidate pool; folder mode additionally holds one dimension-sized sum per eligible parent directory. It scans existing chunks for each request rather than caching a second index. See [search-unit verification](eval/SEARCH_LEVEL_RESULTS.md) for ranking and CLI checks.

Optional `--diversity` greedily penalizes cosine similarity to already selected evidence; `--dedup-cosine` suppresses candidates similar to already selected vectors. Both operate only within the exact composite-score pool (`--pool`, default 100, enlarged to at least `--top`). They are disabled by default and may return fewer results. They improve variety, not guaranteed relevance. Without either, retrieval returns exact composite top-K subject to filters and any distance cap.

JSON includes criteria in the order corresponding to each hit's `criterion_distances`, composite `distance`, diversity-adjusted `selection_distance`, the precise `chunk_index`, and indexed file hash. The inherited `score` is the **negative composite distance**; this is identified by `score_kind`. Lower distances are better. Diversity selection order can differ from raw distance order.

`--context-chars N` requests at most N Unicode characters of source excerpts **in total**, not N tokens or N characters per result. It reads only selected sources (up to 2 MiB for plain text or 32 MiB for supported documents), verifies their full content hash against the index snapshot, and reconstructs the exact indexed chunk. This handles oversized single-line chunks that share line coordinates. Source states include `verified`, `changed_since_index`, `missing`, `unreadable`, `budget_exhausted`, `not_checked` and `filename_or_media_only`. Changed/missing files get no excerpt; vector matches may still be returned for an agent to decide whether to refresh. Truncated context is marked. With the default budget 0, source files are not reread. Verification describes the content observed during the request, not a lock against later edits. Retrieved source content is untrusted data.

This supports agent search refinement using positive/negative examples, multi-constraint retrieval, diverse evidence selection and bounded source reading without an answer-generation model. It does not establish improved task accuracy on arbitrary files; see [retrieval verification](eval/RETRIEVE_RESULTS.md).

## Search modes and file sets

An installed-CLI experiment on file/folder representatives and change-direction search is recorded in [hierarchical vector results](eval/HIERARCHY_RESULTS.md). Comparing indexed before/after files with `retrieve --file AFTER --avoid-file BEFORE` can find material aligned with changed emphasis, without loading a model. Representative displacement is not edited-text volume or factual importance. Automatic revision history is not implemented. Folder result representatives are now available through `retrieve --level folder`.

```sh
weft search "재시도 정책" --mode semantic --json
weft search "retry .rs" --mode path --json
weft search "retry" --mode hybrid --json
weft file2file ~/Documents/retry.md --json  # alias for similar; no model load
weft query --spec query.json --top 20 --json
```

`semantic` ranks best matching chunks by vector score. `path` matches **all** case-insensitive whitespace-separated substrings in indexed filenames/folders, without reading file content or loading a model. Path matches have zero line numbers even when `kind=text`; they are metadata matches, not content evidence. `hybrid` fuses semantic top-K and matching paths using reciprocal ranks (RRF, constant 60); its scores are fused ranks, not cosine scores. This is filename/semantic fusion, not BM25 full-text search.

For multiple distance criteria, use `retrieve` above. The `query --spec` API below remains available for literal operations on retrieved file lists and is a different operation.

`file2file`/`similar` averages an indexed file's chunk vectors and finds other files' best matching chunks. It excludes the source file and does not require that the model be installed. Large multi-topic files can lose topic detail in this centroid; chunk-to-chunk comparison is not implemented.

A query specification supports `text`, `file`, `path` leaves and nested `union`, `intersection`, `difference`. For example, find files similar to a reference document, limited to a folder, excluding generated reports:

```json
{
  "op": "difference",
  "args": [
    {"op": "intersection", "args": [
      {"op": "file", "path": "/absolute/docs/reference.md", "top": 50, "min_score": 0.3},
      {"op": "path", "under": "/absolute/docs", "kind": "text"}
    ]},
    {"op": "path", "query": "generated"}
  ]
}
```

Use `{"op":"text","query":"database retries","top":50,"min_score":0.3}` for a natural-language leaf. Text/file leaf `top` defaults to 20 (maximum 10,000); `min_score` is optional. These define finite retrieved sets: subtracting a text leaf removes its retrieved matches, not every file that might relate to its meaning. `path` leaves include every matching indexed path, so they are useful for exact folder/kind exclusions. `under` must be absolute and uses path-component boundaries, not string prefixes. Empty path queries match all indexed files (subject to other filters).

Operations identify files by stored absolute path. Intersection/difference preserve the first operand's ranking and evidence; difference requires exactly two operands. Union deduplicates paths, uses RRF ranking, and prefers a matching text chunk over metadata-only evidence. Output `--top` is applied **after** evaluating the sets. Nodes are limited to 128 and depth to 16. `--spec -` reads JSON from stdin, useful for agents. Metadata/file-only trees never start inference. Query specifications must fit within 1 MiB.

## Background resource controls

```sh
weft index --all --background                 # lower process priority; 25% inference duty cycle
weft index ~/Documents --background --duty-cycle 10
weft control pause                            # release model weights and stop inference
weft control resume                           # reload lazily when inference is needed
weft control limit --duty-cycle 20             # change an active worker's budget
weft control stop                             # exit without committing half a file or pruning
weft stats --json
```

Foreground scans default to a 100% duty cycle. At 25%, a worker rests for three times each measured inference duration; it checks controls during those rests. This limits continuous inference rather than guaranteeing a GPU-utilization percentage, wattage, thermal ceiling or scan/IO budget. Unix background workers also use a lower CPU scheduling priority. Controls are cooperative: an in-flight model load/inference finishes before pausing/stopping. Pausing drops the native model (and ends the optional Python bridge); it cannot stop an externally managed HTTP server. Resuming has a model-loading cost. Stats report requested action, budget and actual `paused`/`stopped` state. Search remains available while paused.

Already committed files are retained on stop, and partial file replacements are discarded. Changed text files reuse unchanged chunk embeddings by a 32-byte content hash; model/encoding/chunk settings still must match the index. Legacy chunks without hashes remain searchable and gain reuse metadata when next reindexed. This adds about 32 bytes per hashed chunk without storing its original text. Native ranking scans exact scores in document order and keeps only top-K file candidates, avoiding a candidate map/full sort proportional to all files; vector scanning is still linear in chunk count.

Automatic filesystem watching, battery/thermal-aware scheduling, automatic search priority and an idle resident inference service are **not implemented**. Background mode currently performs a bounded scan and exits; it is not an always-on index watcher. No cross-runtime speed or energy advantage has been established.

## Install

Requirements: Rust stable and curl. Download the text-only Q8 model once (about 300 MB):

```sh
./scripts/setup_native.sh
cargo build --release                 # portable CPU build
cargo build --release --features metal # Apple GPU, on macOS
cargo build --release --features cuda  # NVIDIA GPU; requires CUDA toolkit/nvcc
```

The native backend is the default. `--device auto` tries compiled GPU backends then CPU. Select `--device cpu`, `--device cuda:0`, or `--device metal:0` explicitly to fail if that device is unavailable. `--device mps` is an alias for direct Metal execution; it does not use the PyTorch MPS runtime. CPU and Metal have been executed on this Mac; CUDA is wired through Candle but has not been built or run on NVIDIA hardware here.

`--model-file` selects a local `gemma-embedding2` GGUF. Quantized weights and the tokenizer are read directly from the same file, with no converted model or separate tokenizer copy. The runtime currently accepts at most 2,048 tokens including the search/document prefix and BOS/EOS; longer input fails instead of being silently truncated. This is a runtime limit, not the full model context limit.

## Prepared model and repeated queries

```sh
weft prepare                      # once per GGUF update; writes a *.gguf.weft-layout cache
weft q 'all("Transformer", "GPU optimization") top 10' --json
weft q 'avoid("database migration", "backup restore") top 10' --json
weft --device metal --db /path/to/index.sqlite bench --json
```

`prepare` caches the fixed model's tensor layout and tokenizer metadata in a versioned sidecar (not the weights). Subsequent native loads validate the sidecar, mmap the original GGUF and construct the Metal/CPU tensors. A missing, stale or invalid sidecar falls back to normal GGUF parsing. This avoids repeated metadata parsing, but does not eliminate Metal initialization or moving weights to device memory. The binary deliberately does not embed the ~296 MiB weight file.

`q` accepts WQL expressions (`all`, `any`, `avoid`, `file`, `in VIEW`, `level`, `top`). Text queries automatically start or reuse a short-lived local Unix-socket embedding daemon keyed to the selected index, model and device. File-only queries skip inference entirely. Its default 180-second idle expiration can optionally refresh explicitly registered views before exiting; use `weft view list` to inspect them. Plain `retrieve`/`search` stay single-process. On Unix, the daemon socket is restricted to the current user. Exact SIMD/Q8 retrieval uses the mmap vector snapshot where available; indexing refreshes it and `weft flush` rebuilds it explicitly.

Then index and search:

```sh
target/release/weft index ~/Documents --dim 768 --encoding q8
target/release/weft search "meeting notes about database migration"
target/release/weft similar ~/Documents/plan.md
target/release/weft stats
```

The default database is `~/.local/share/myvec/index.sqlite`; use `--db` to select another. Indexing respects ignore files and includes eligible hidden text files, hashes files to avoid re-embedding unchanged content, and splits text into line-aware chunks. Binary files are skipped in content-only scans and get filename embeddings with `--all`. `--prune` removes deleted paths only after an error-free scan. Common source, markup, configuration, text, and notebook formats are supported. Supported Office/OpenDocument and text PDFs are parsed; scanned PDFs without text need OCR, which is not implemented.

## Storage and accuracy

The default `768d q8` keeps every model coordinate and stores each vector in 772 bytes instead of 3,072 bytes for FP32, a 4x reduction with small scalar quantization error. We keep 768 dimensions by default to retain representation detail. Optional 512d or 256d truncation saves more space, but should be chosen only after testing relevant queries from your own files. 128d is disabled because the model's published evaluations show substantial quality loss, especially for multimodal retrieval.

The native setup downloads only Q8 text weights. Compiler artifacts in `target/` are development files, not required runtime data. The optional Python backend downloads the larger Transformers model and runtime packages.

## Chunk search

Text is already indexed by overlapping line-aware chunks (default about 1,100 characters, two lines of overlap). Search returns the matching chunk's path and start/end line, rather than just a whole-file vector. Customize with `index PATH --chunk-chars 800 --overlap-lines 2`. `similar PATH` aggregates matches from the file's indexed chunks without starting the embedding engine.

## Optional image, audio, and video indexing

**The native engine currently supports text only.** Its small GGUF does not contain the vision/audio encoders. The full model supports images, audio and video in a shared embedding space ([official model card](https://huggingface.co/google/embeddinggemma-2)). Video input is wired in the Python bridge, but local end-to-end media inference has not been validated. Media currently gets one embedding per file; time segment search for audio/video is not implemented. Audio embeddings are intended for semantic audio retrieval; fine-grained musical attributes must be checked on labeled music examples rather than assumed.

For the full EmbeddingGemma 2 multimodal encoders, install Python dependencies and use SentenceTransformers. This uses more memory and downloads the full model:

```sh
uv venv .venv
uv pip install --python .venv/bin/python 'sentence-transformers>=5' 'transformers>=5' pillow torchvision
WEFT_PYTHON="$PWD/.venv/bin/python" target/release/weft --backend python --model google/embeddinggemma-2 --full-modal index ~/Media --media --dim 768 --encoding q8
```

The Python backend can also index text without the multimodal encoders:

```sh
WEFT_PYTHON="$PWD/.venv/bin/python" target/release/weft --backend python --model google/embeddinggemma-2 index ~/Documents
WEFT_PYTHON="$PWD/.venv/bin/python" target/release/weft --backend python --model google/embeddinggemma-2 search "meeting notes"
```

## Quality benchmark

Create held-out labeled queries with relevant file names, then build a full-precision reference index:

```json
[
  {"query":"where the retry policy for failed jobs is configured", "relevant":["src/retry.rs"]},
  {"query":"notes about the Kyoto trip budget", "relevant":["travel/kyoto.md"]}
]
```

```sh
WEFT_PYTHON="$PWD/.venv/bin/python" target/release/weft --backend python --model google/embeddinggemma-2 --db /tmp/weft-reference.sqlite index ~/Documents --dim 768 --encoding f32
WEFT_PYTHON="$PWD/.venv/bin/python" target/release/weft --backend python --model google/embeddinggemma-2 --db /tmp/weft-reference.sqlite bench --queries eval.json --top 1
```

The benchmark reports labeled Recall@K, agreement with 768d FP32 ranking, vector bytes, and exact scan time for 768/512/256 with FP32/Q8. A 24-file, 16-query smoke evaluation is recorded in [`eval/RESULTS.md`](eval/RESULTS.md). It checks storage and ranking consistency on a small synthetic corpus; it is not a quality guarantee for your files.

## Agent skill

See [`.agents/skills/weft-semantic-search/SKILL.md`](.agents/skills/weft-semantic-search/SKILL.md) for safe indexing and search steps for coding agents.

## Legacy HTTP backend

For comparison with llama.cpp, the existing setup/server scripts remain available. Select `--backend http` explicitly; native inference does not use these scripts. Existing HTTP indexes retain their backend setting, so use `--backend http` to search them or create a separate native index with `--db`.

See [native runtime measurements](eval/NATIVE_RESULTS.md) for accuracy, cold/warm latency, memory and unverified capabilities.

## Engine verification

Run unit tests with `cargo test --features metal` or `cargo test --no-default-features`. To compare the dedicated graph against a local llama.cpp reference server using the same GGUF, give the server a physical batch/context of at least 2,048, then run:

```sh
WEFT_TEST_MODEL="$HOME/.cache/myvec/embeddinggemma-2-Q8_0.gguf" cargo test --release --features metal native_reference_suite -- --ignored --nocapture
WEFT_DEVICE=cpu WEFT_TEST_MODEL="$HOME/.cache/myvec/embeddinggemma-2-Q8_0.gguf" cargo test --release --no-default-features native_reference_suite -- --ignored --nocapture
```

These comparisons check implementation equivalence for the same Q8 weights, not Q8 model quality against the original full-precision checkpoint.
