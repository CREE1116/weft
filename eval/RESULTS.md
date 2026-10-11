# myvec retrieval and storage smoke evaluation

Ran on 2026-10-11 with the Q8_0 GGUF of Google EmbeddingGemma 2 through the locally built llama.cpp 0.6.0-dev embedding server on an Apple Silicon Mac. The corpus contains 24 short text files across eight topics; each file produced one chunk. The hand-labeled set contains 16 English paraphrase queries with one relevant file each. Metric: Recall@1, with exact cosine ranking over chunk embeddings. This small synthetic set is useful for checking the implementation and representation compression; it does not estimate quality on a user's own mixed-language files.

| Representation | Labeled Recall@1 | Top-1 agreement with 768d FP32 | Vector bytes, 24 chunks | Mean exact scan |
| --- | ---: | ---: | ---: | ---: |
| 768d FP32 | 15/16 (93.75%) | 100% | 73,728 | 0.019 ms |
| 768d Q8 | 15/16 (93.75%) | 100% | 18,528 | 0.019 ms |
| 512d FP32 | 15/16 (93.75%) | 100% | 49,152 | 0.013 ms |
| 512d Q8 | 15/16 (93.75%) | 100% | 12,384 | 0.014 ms |
| 256d FP32 | 15/16 (93.75%) | 100% | 24,576 | 0.008 ms |
| 256d Q8 | 15/16 (93.75%) | 100% | 6,240 | 0.008 ms |

The Rust CLI indexed the 24-file corpus in 1.32 seconds with the local model server already warm. All variants made the same one error: “What is the procedure for safely changing a production database schema?” retrieved the nearby schema-backup note instead of the migration rollout note. The unchanged results across dimensions reflect this corpus and are not evidence that dimension reduction is generally lossless.

768d Q8 preserves all model coordinates and reduced vector storage by 74.9% (4.0x) versus FP32; it remains the default to avoid dimensionality loss. 256d Q8 used 91.5% less vector storage than 768d FP32, with identical ranking on this small set. Search timings over 24 vectors are too small to support a scalability claim; latency is expected to be dominated by linear scan work at larger corpus sizes.

Reproduce with `eval/corpus`, `eval/queries.json`, and the `bench` command described in the README. The test model and venv are not included in the repository.
