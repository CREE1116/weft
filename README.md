# myvec

A compact Rust CLI combining an in-process EmbeddingGemma 2 inference engine, incremental file indexer, and exact cosine vector search. The dedicated model graph runs in Rust; Candle supplies CPU, CUDA and Metal tensor/quantized kernels. The database stores paths, hashes, line ranges, and vectors without copying source files. Text inference requires no Python or server.

## Desktop install and agent use

```sh
./scripts/install.sh
myvec stats --json
myvec search "재시도 간격을 설정한 문서" --top 10 --json
```

The installer downloads/reuses the text model, installs the CLI under `$CARGO_HOME/bin` (normally `~/.cargo/bin`), installs the agent skill at `~/.agents/skills/myvec-semantic-search`, and starts the first whole-computer scan in the background. On macOS it builds with Metal. Use `--cpu` or `--cuda` to select another build, or `--no-index` to install without starting a scan. `--scan-root PATH` starts a scan limited to that root. These are installer options; search/device options belong to `myvec`.

```sh
myvec index --all --background     # independent background desktop scan
myvec index --all --dry-run        # preview without embeddings
myvec index --all --exclude ~/Downloads
myvec index ~/Documents           # content-only update for supported text
myvec index --all ~/Documents      # also include filenames of other formats
myvec similar ~/Documents/plan.md --json
```

On Unix/macOS, the default computer scan starts at `/`, including user folders and mounted volumes; on Windows the default is the system drive (other drives can be scanned explicitly). It excludes OS directories, per-user application data/caches, generated dependency/build trees, credentials/private keys, and its own database/model data. It honors ignore files and does not follow symlinks. Custom omissions use repeated `--exclude PATH`. Exclusions and denied filesystem permissions mean "whole computer" covers accessible, non-excluded files, not literally every file.

Supported UTF-8 text up to 2 MiB is embedded by overlapping chunks. Other formats, binary input and larger files receive one **filename/folder/type embedding** and return `kind=path` with zero line numbers. This makes those files discoverable without pretending their PDF/image/audio/video contents have been parsed. Native media-content search is still pending.

`index --background` detaches its worker from the terminal/installer process group and writes to the database's `.log` sibling file. This also works for a requested folder.

The initial scan's progress is in `myvec stats --json`, with details in `~/.local/share/myvec/index.log`. A scan reports `complete` or `partial` after it exits; a `running` last scan with `indexer_running=false` was interrupted. Results already written are searchable while indexing continues. SQLite WAL supports concurrent searches; only one indexer can update a database at once. Rerun indexing to pick up changes; no persistent watcher is installed. Each run hashes supported text and uses size/modified time for filename-only records. Unchanged files are not re-embedded. `--prune` removes disappeared records only after an error-free scan.

GPU use during the initial scan is expected with the Metal/CUDA build: files are embedded locally in the background. The worker exits when the scan finishes; there is no always-running GPU service. On Unix, stop a worker with `kill -INT PID`, using `last_scan.pid` from stats after confirming `indexer_running=true`. Restart with `myvec --device cpu index --all --background` to use CPU instead, or omit `--device cpu` to resume GPU indexing. Committed files are reused.

Reload your agent session after installing the skill if it does not appear yet. The skill lets the agent query the same CLI and read matching source lines. It does not grant permission to expand a future user's indexing scope.

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

Then index and search:

```sh
target/release/myvec index ~/Documents --dim 768 --encoding q8
target/release/myvec search "meeting notes about database migration"
target/release/myvec similar ~/Documents/plan.md
target/release/myvec stats
```

The default database is `~/.local/share/myvec/index.sqlite`; use `--db` to select another. Indexing respects ignore files and includes eligible hidden text files, hashes files to avoid re-embedding unchanged content, and splits text into line-aware chunks. Binary files are skipped in content-only scans and get filename embeddings with `--all`. `--prune` removes deleted paths only after an error-free scan. Common source, markup, configuration, text, and notebook formats are supported. Scanned PDFs and Office documents are not parsed.

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
MYVEC_PYTHON="$PWD/.venv/bin/python" target/release/myvec --backend python --model google/embeddinggemma-2 --full-modal index ~/Media --media --dim 768 --encoding q8
```

The Python backend can also index text without the multimodal encoders:

```sh
MYVEC_PYTHON="$PWD/.venv/bin/python" target/release/myvec --backend python --model google/embeddinggemma-2 index ~/Documents
MYVEC_PYTHON="$PWD/.venv/bin/python" target/release/myvec --backend python --model google/embeddinggemma-2 search "meeting notes"
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
MYVEC_PYTHON="$PWD/.venv/bin/python" target/release/myvec --backend python --model google/embeddinggemma-2 --db /tmp/myvec-reference.sqlite index ~/Documents --dim 768 --encoding f32
MYVEC_PYTHON="$PWD/.venv/bin/python" target/release/myvec --backend python --model google/embeddinggemma-2 --db /tmp/myvec-reference.sqlite bench --queries eval.json --top 1
```

The benchmark reports labeled Recall@K, agreement with 768d FP32 ranking, vector bytes, and exact scan time for 768/512/256 with FP32/Q8. A 24-file, 16-query smoke evaluation is recorded in [`eval/RESULTS.md`](eval/RESULTS.md). It checks storage and ranking consistency on a small synthetic corpus; it is not a quality guarantee for your files.

## Agent skill

See [`.agents/skills/myvec-semantic-search/SKILL.md`](.agents/skills/myvec-semantic-search/SKILL.md) for safe indexing and search steps for coding agents.

## Legacy HTTP backend

For comparison with llama.cpp, the existing setup/server scripts remain available. Select `--backend http` explicitly; native inference does not use these scripts. Existing HTTP indexes retain their backend setting, so use `--backend http` to search them or create a separate native index with `--db`.

See [native runtime measurements](eval/NATIVE_RESULTS.md) for accuracy, cold/warm latency, memory and unverified capabilities.

## Engine verification

Run unit tests with `cargo test --features metal` or `cargo test --no-default-features`. To compare the dedicated graph against a local llama.cpp reference server using the same GGUF, give the server a physical batch/context of at least 2,048, then run:

```sh
MYVEC_TEST_MODEL="$HOME/.cache/myvec/embeddinggemma-2-Q8_0.gguf" cargo test --release --features metal native_reference_suite -- --ignored --nocapture
MYVEC_DEVICE=cpu MYVEC_TEST_MODEL="$HOME/.cache/myvec/embeddinggemma-2-Q8_0.gguf" cargo test --release --no-default-features native_reference_suite -- --ignored --nocapture
```

These comparisons check implementation equivalence for the same Q8 weights, not Q8 model quality against the original full-precision checkpoint.
