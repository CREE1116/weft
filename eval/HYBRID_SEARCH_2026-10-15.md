# Hybrid search and runtime verification — 2026-10-15

Apple M4 Pro, macOS 27.2, release Metal build, EmbeddingGemma 2 Q8 GGUF, 768-dimensional Q8 index. Small constructed fixture: 26 files/chunks, 16 existing natural-language labels plus five identifiers/symbols/Korean labels. Search timings use seven warm samples per query and exclude model loading, embedding and process startup. Query embeddings are shared across all variants. JSON hits are relative paths in the combined fixture corpus.

| Variant | Recall@10 | MRR@10 | NDCG@10 | p50 search ms | p95 search ms |
| --- | ---: | ---: | ---: | ---: | ---: |
| semantic | 1.000 | 0.913 | 0.935 | 0.033 | 0.038 |
| rrf | 1.000 | 0.913 | 0.935 | 0.046 | 0.058 |
| lexical | 0.857 | 0.724 | 0.758 | 0.336 | 0.525 |
| fts_cosine | 1.000 | 0.905 | 0.930 | 0.411 | 0.554 |
| intent_cosine | 1.000 | 0.905 | 0.930 | 0.415 | 0.569 |
| intent_zscore | 1.000 | 0.976 | 0.982 | 0.415 | 0.570 |

Intent+Z-score improves ordering on this fixture while retaining Recall@10. Legacy RRF is cheaper in the ranking stage alone; the content index adds keyword matching. This small corpus does not establish general relevance gains or large-index latency. Weights should be recalibrated on representative personal search labels before drawing wider conclusions. Identifiers with full lexical coverage bypass inference in production; the benchmark deliberately embeds every query once to compare the ranking formulas fairly.

A dedicated snapshot-flush test on an isolated backup of 5,236 chunks took 21.59 ms, wrote 4,283,812 bytes, and peaked at 19.2 MiB process RSS. There is no old-build comparison for that sample. The structural memory saving removes the `N * dimension` Q8 staging buffer and repeated per-chunk path/kind strings. For 600 tokens, the retained sliding mask is 600²×4 bytes (1.37 MiB), versus the former 2048²×4 bytes (16 MiB); this concerns mask storage, not total model memory. Short queries release that mask. Normalization stays on-device and transfers one vector to CPU. CPU Q8 SIMD search keeps the index mapped, avoiding an extra GPU index copy.

Verification passed: CPU/Metal Rust suites; actual-GGUF prepared/unprepared embedding parity including long input; CPU/Metal normalization parity; isolated mock-HTTP hybrid CLI integration; native incremental-cache, pause/resume/stop and source-verification integration; bootstrap checks; and the native Mac app harness including ordinary hybrid, negative, WQL and folder routing. The mock hybrid integration runs in CI. Reference-server comparisons and manual benchmarks remain opt-in.

Existing indexes gain postings on the next explicit indexing pass without recomputing unchanged vectors. Postings are checked against document hashes so older vector writers cannot certify stale keyword data. Background/watch indexing defaults to a 25% compute duty budget, yields between inference slices and releases index models during sustained searches. Idle serving retires its socket before background indexing, allowing a new query daemon to start without waiting for indexing. This is cooperative pacing, not a hard GPU-utilization or memory ceiling. Keyword tokens persist even though full source text is not stored.

Reproduce from the repository root:

```sh
fixture=$(mktemp -d /tmp/weft-hybrid.XXXXXX)
cp -R eval/corpus "$fixture/corpus"
cp eval/hybrid-corpus/*.md "$fixture/corpus/"
target/release/weft --db "$fixture/index.sqlite" index "$fixture/corpus"
target/release/weft --db "$fixture/index.sqlite" bench-search --queries eval/hybrid-queries.json --top 10 --json
python3 eval/verify_hybrid.py --binary target/release/weft
```

[Full scores and predictions](HYBRID_SEARCH_2026-10-15.json). Formula inspiration: [DigUp Searcher.swift](https://github.com/ARahim3/DigUp/blob/main/Sources/DigUpKit/Searcher.swift). Storage reference: [SQLite contentless-delete FTS5](https://www.sqlite.org/fts5.html#contentless_delete_tables).

## Relevance cutoff follow-up

The same 26-file/21-query fixture was rerun after adding semantic evidence and relative-score gates. Recall@10 remained 1.000, MRR@10 0.976, and NDCG@10 0.982. Ordinary fusion returned 1.57 results per query rather than always filling ten slots; all expected files stayed in the returned sets. Search-stage p50 was 0.431 ms and p95 0.669 ms. These are warm ranking timings, with the same exclusions and small-fixture limits as above. [Raw cutoff comparison](HYBRID_SEARCH_CUTOFF_2026-10-15.json). Exact format queries and identifier inference bypass are production routes outside this formula comparison.
