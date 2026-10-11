# Native EmbeddingGemma 2 runtime verification

Measured on 2026-10-11 on this Apple Silicon Mac. The model graph, tokenizer reconstruction, indexing and ranking execute in Rust. Candle provides device kernels; no llama.cpp or Python process is used by the native backend. The reference server was used only for comparison, with the same Q8_0 GGUF weights.

## Equivalence

Five cases: English and Korean queries, English and Korean documents, and a 1,213-token document before the document prefix. The long case crosses the local attention boundary. Both CPU and Metal returned normalized 768-dimensional vectors. Minimum cosine versus the same-model llama.cpp reference: CPU 0.99999933, Metal 0.99999954. Official tokenizer comparisons passed for English, Korean, code paths, special token literals, tabs, newlines and an emoji.

This is equivalence to Q8 weights, not proof that model Q8 quantization preserves every original checkpoint result. CUDA is a compile-time backend option; this Mac has no NVIDIA device or CUDA toolkit, so CUDA compilation and execution are not verified. `mps` is a CLI alias for direct Metal and was exercised by a real search.

## Latency and footprint

Final optimized, already prepared device timings, excluding model load/tokenizer setup/reference requests:

| Device | Short cases (forward) | Long document (forward) |
| --- | ---: | ---: |
| CPU, FP32 activations | 143–183 ms | 2,112 ms |
| Metal, FP32 activations | 14–23 ms | 460 ms |

A separate earlier Metal run took about 12 seconds for its first forward, then 14–20 ms for short inputs. Initial kernel preparation/cache state matters; steady forward time is not CLI startup time.

A complete one-shot `--device mps search` took 2.49 seconds including load, tokenization and ranking; maximum RSS was 670,744,576 bytes (about 640 MiB), and reported peak memory footprint was 708,068,528 bytes. These are process measurements on unified memory, not a guarantee for longer inputs or other GPUs. Metadata is parsed once and released before weights are loaded. Quantized embedding rows are gathered directly; CPU only expands the current linear matrix transiently to avoid an additional activation quantization, without retaining the entire model as FP32.

The release executable is about 11 MiB; the text model is about 296 MiB. There is no separate tokenizer file, converted checkpoint, Python environment or persistent server required for native text inference. Development build artifacts are separate from runtime storage.

## Retrieval and chunks

The 24-document/16-query synthetic benchmark produced Recall@1 = 15/16 (93.75%), with the same schema-backup/migration confusion as the HTTP reference. All tested vector representations agreed on top-1 within the native benchmark. Final report is in `native-bench.json`. A fresh native FP32 index took 3.44 seconds including model loading. Q8 vector storage remains 18,528 bytes for 24 chunks versus 73,728 bytes FP32. The database does not copy source text.

Five unit tests pass, including Unicode-safe long-line chunking with lossless text coverage and preserved line IDs. Two model-dependent tests are explicitly ignored by default and were run separately with local model/reference inputs. CPU-only compilation and CPU/Metal unit suites pass.

Text chunk search is implemented. Native vision/audio encoders, media preprocessing and audio/video time-segment indexing are not implemented. The optional Python bridge has media input routing, but local end-to-end multimodal inference has not been validated.

Reproduce with the README engine verification commands and `eval/corpus`, `eval/queries.json`. These small tests do not establish retrieval quality for an arbitrary personal corpus or cross-platform performance.
