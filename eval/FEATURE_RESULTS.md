# Search sets and cooperative resource controls

Verified on the development Apple Silicon Mac on 2026-10-11.

## Functional and preservation evidence

- Twelve unit tests pass; two existing model-reference tests are opt-in. New tests cover set membership, empty sets, invalid query specifications, file deduplication, chunk evidence, path-component boundaries, and pause/stop budgeting logic.
- Exact bounded-memory ranking agrees with exhaustive ranking on 80 files with three chunks each, for FP32 and Q8, including ties and top-K values 0, 1, 7, 80 and 100. SQLite's real query plan used no temporary ordering tree in the end-to-end fixture.
- The additive nullable 32-byte input-hash column preserves legacy vectors and permits legacy writers to omit the new column. Reopening is idempotent; migration is serialized with an immediate transaction. Existing files are not forcibly reindexed.
- A native fixture initially stored three documents and nine chunks. Updating one document reused six unchanged chunks. Its resulting vector blobs exactly matched a separate fresh index of the updated document. This checks cache correctness, not full-precision model quality.
- Filename matching and file-to-file ranking worked with a deliberately absent model file. A file-similarity/folder intersection found the expected document; hybrid search returned deduplicated paths.
- A detached native worker paused, resumed, accepted a changed inference budget, and stopped. Stopping while processing a large file preserved all previously committed files/chunks and skipped requested pruning; no partial file became visible.
- A detached unchanged-file scan used the default 25% budget and completed without new embeddings.

Reproduce with `cargo test --features metal` and, after building the release binary and caching its GGUF, `python3 eval/verify_features.py`. CPU-only builds use `cargo test --no-default-features`; the end-to-end fixture also works with a CPU-built release binary, but the reported execution here used Metal.

## Scope and interpretation

Path matching searches filenames/folders, not source text. Hybrid search combines semantic and path ranks, not a BM25 content index. Set operations apply to stored absolute file identities; semantic/file operands are top-K/threshold-bounded retrieved sets. File-to-file queries currently use the source file's centroid.

The 25% duty cycle means resting for three times measured inference duration. It is not a measured 25% GPU utilization, wattage cap, thermal guarantee, or IO budget. A running inference/model load finishes before a cooperative pause/stop takes effect. Native weights are dropped when paused, so resume can incur loading latency. Foreground search is independent; automatic priority coordination is not implemented.

No filesystem watcher, battery/thermal-aware scheduling, persistent idle inference service, full-text BM25 index, native multimodal encoders, CUDA-device test or competitive runtime/energy benchmark was added. This is a controlled background scan plus expanded retrieval API, not proof of an always-on service or performance superiority.
