# Weft performance on MacBook Pro M4 Pro (24 GB)

Measured **2026-10-12**, on the developer's physical **MacBook Pro, Apple M4 Pro (12 CPU cores: 8 performance + 4 efficiency), 24 GB unified memory**, using the Metal backend and an EmbeddingGemma 2 Q8_0 GGUF model (~296 MiB). Both executables were run on the same machine against an **isolated read-only snapshot** of the same index; the personal live index was not used or rebuilt.

The snapshot held **32,636 indexed files and 89,894 chunks**, with **66,661 eligible vectors** scanned in the WQL workload, **768 dimensions**, **Q8 vector encoding** (~72 MiB of vectors). The index and mmap vector sidecar were ~95 MiB and ~57 MiB on disk, respectively. The executable sizes were about 14 MiB; the prepared model-layout cache was ~7.9 MiB in addition to the original weights.

Each version was executed **seven times**, alternating older installed and optimized builds to reduce run-order effects. The table shows **the median of the seven per-process measurements** for each field, rounded to three decimals. Short warm-query latency is the median across per-process short-query p50 values. Measurements use the operating system's current file/cache state; they are **not cold disk boot** or fully uncached GPU timings. The first trial for the old binary had a large outlier, so medians rather than averages are used. All latencies are lower-is-better.

| Metric | Previously installed | Optimized | Change |
| --- | ---: | ---: | ---: |
| Model load | 324.096 ms | **176.155 ms** | **45.6% faster** |
| First inference after load | 25.020 ms | **20.659 ms** | 17.4% faster |
| Warm short query (p50) | **11.458 ms** | 11.594 ms | 1.2% slower |
| Peak process RSS | 953.609 MiB | **873.109 MiB** | **80.500 MiB (8.4%) less** |
| Warm mmap SIMD scan + Top-K | 8.217 ms | **3.701 ms** | **55.0% faster** |
| Single-text WQL retrieval E2E | 22.002 ms | **16.813 ms** | 23.6% faster |
| Two-branch `all()` retrieval E2E | 32.620 ms | **27.086 ms** | 17.0% faster |
| Nested `any(all(), avoid())` E2E | 56.776 ms | **49.263 ms** | 13.2% faster |

The mmap scan result includes SIMD scoring and file Top-K selection, but **excludes model inference and SQL/metadata work**. WQL retrieval E2E includes query computation and DB/search overhead after the benchmark has loaded the model. Peak RSS reflects the entire benchmark process on unified memory; the benchmark holds a model as well as the index. The old/new values are separate process measurements, not matched samples within one process.

The optimization includes a validated offline GGUF metadata layout, mmap weight access with reduced intermediate copies/allocations, overlapping tokenizer preparation and weight uploads, short-context RoPE preparation, a specialized SIMD Q8 scanner, and bounded exact result selection. A 180-second idle WQL embedding daemon is also available for successive CLI invocations, but **daemon reuse is not included in these benchmark figures**. Weights are kept as a separate GGUF file: embedding them in the executable would not remove GPU uploads or model initialization.

## Verification and reproduction

The optimized build passed **36 release tests** (0 failed; six ignored), including query/parser/storage checks. Separate ignored model tests passed tokenizer/layout round-trip and five Metal embedding parity cases (minimum cosine ≥ 0.999999924 against the baseline and no observed component difference in those cases). These are limited equivalence checks, not proof of identical search rankings for every query or universally improved retrieval accuracy. The WQL query compiler uses conservative screening followed by original exact scoring for admitted candidates, with explicit reference-mode verification in tests.

An additional run after the final build showed **581.725 ms model load**, **3.889 ms mmap scan p50**, and **826.641 MiB peak RSS**. A further run using the optimized binary **installed on PATH** showed **417.506 ms model load**, **3.782 ms mmap scan p50**, and **791.109 MiB peak RSS**. This illustrates substantial run-to-run loading variance even though the optimized scan rate reproduced. The seven-run median above is a description of those recorded trials, not a guaranteed cold start. A repeated WQL CLI invocation with an existing daemon completed in about **0.30 seconds wall time**, including process startup and IPC; it is not comparable with the in-process scan benchmark.

```sh
cargo build --release
cargo test --release
WEFT_TEST_MODEL="$HOME/.cache/myvec/embeddinggemma-2-Q8_0.gguf" WEFT_DEVICE=metal cargo test --release native::tests::prepared_embedding_parity -- --ignored --nocapture
WEFT_TEST_MODEL="$HOME/.cache/myvec/embeddinggemma-2-Q8_0.gguf" cargo test --release native::tests::prepared_layout_roundtrip -- --ignored --nocapture
weft prepare
weft --device metal --db /path/to/isolated-snapshot.sqlite bench --json
```

The baseline measurements were collected using the previously installed executable before replacement; reproduce a *comparison* only if you preserve that older executable. The historical per-run observations (seven for each executable) are in [`M4PRO_PERFORMANCE_2026-10-12.json`](M4PRO_PERFORMANCE_2026-10-12.json). OS caching, process scheduling, thermal state and actual dataset affect results. The benchmark does not measure indexing throughput, energy use, CPU-only performance, or the time to generate a first-ever prepared layout file; these should be separately measured before claiming improvements there.
