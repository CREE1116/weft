# Agent-oriented multi-basis retrieval

Verified on the development Apple Silicon Mac on 2026-10-11.

## Evidence

- Seventeen unit tests pass with Metal and CPU-only builds; two existing model-reference tests remain opt-in. The inference graph itself was not changed.
- A constructed three-vector example demonstrates that the sum/max distance search finds a compromise candidate outside either individual criterion's top-1. This passes for FP32 and reconstructed Q8 vectors. Min-distance and positive/negative distance ordering also pass.
- Optional diversity and near-duplicate suppression reduce repeated identical-vector hits in a controlled fixture. They are disabled by default; this is not a relevance benchmark.
- File-only criteria do not invoke the embedding callback. Native CLI retrieval also succeeds with a deliberately nonexistent model file when only positive/negative reference files are used.
- Multiple chunks from the same file retain ordinal identity. A Unicode single-line fixture has two chunks with identical line coordinates; verified context comes from the correct second chunk, fits a five-character total budget, and reports truncation. Modified/missing source files return no context.
- The native end-to-end fixture scans all eligible chunks against three text criteria and obeys a shared 120-character context budget. File-reference retrieval returns verified context, then withholds that context after the source is modified without reindexing.
- Existing incremental indexing, exact bounded-memory single-vector ranking, literal file-set queries and cooperative background controls remain covered by the prior fixture. No destructive index migration was needed for this feature.

Reproduce after building/caching the native text model:

```sh
cargo test --features metal
cargo test --no-default-features
cargo build --release --features metal
python3 eval/verify_features.py
```

## Scope

`retrieve` evaluates positive/negative cosine-distance composition before a global top-K selection. `all` sums positive distances, `any` takes their minimum, and `strict` their maximum. Negative preference subtracts the weighted minimum distance to a negative example. These are ranking functions, not literal file-set membership operations. Existing `query --spec` list algebra retains its original semantics.

Default retrieval uses all eligible reconstructed vectors without a per-criterion candidate cutoff. Optional diversity/deduplication acts only within a bounded exact-score pool; it is not an exhaustive optimization of the diversity objective. File criteria are indexed centroids, not current-file re-embeddings. Context reads only selected text files, verifies the full hash against a SQLite snapshot, and enforces a character budget, not a token budget. Verification describes the contents observed at retrieval time.

These tests demonstrate functional ranking, cache compatibility and evidence handling. They do not prove improved agent task success, general search accuracy, speed superiority, power consumption or performance versus LiteRT/Foresight/QMD. No generation model, native media encoder, automatic watcher, battery/thermal policy or persistent inference service was added.
