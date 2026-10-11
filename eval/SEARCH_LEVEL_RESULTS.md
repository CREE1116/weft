# Search-unit verification (myvec 0.4.0)

Verified locally on macOS with CPU and Metal builds. This is correctness and native CLI integration evidence, not a held-out relevance benchmark or a competitive speed claim.

## Deterministic ranking experiment

`retrieve::tests::levels_rank_different_units_and_equal_weight_files` uses both F32 and Q8 storage, five files and sixteen chunks:

- A mixed folder has ten `[1,0]` chunks in one file and one `[0,1]` chunk in another. Its representative is the normalized equal-file sum: cosine to `[1,0]` is approximately 0.7071, rather than approximately 0.995 from weighting all eleven chunks equally.
- A focused folder with representative `[0.8,0.6]` therefore ranks above the mixed folder for criterion `[1,0]`.
- A nested folder is a separate immediate-parent candidate. Five files become four folder candidates.
- A document with one `[1,0]` and two `[0,1]` chunks has zero best-passage distance but whole-file distance greater than 0.5. This demonstrates why the result unit affects relevance; aggregation is not uniformly more accurate.
- Excluding the ten-chunk file changes its folder to one file/one chunk, distance 1, and a different indexed membership fingerprint. Only six eligible chunks remain.
- Pool size one returns the same top folder as exhaustive candidate retention, while still evaluating every folder.
- File/folder hits omit chunk identity and excerpts and report `aggregate_not_checked` even when a source context budget is requested. They do not claim fresh source verification.

## Native CLI integration

`python3 eval/verify_features.py` creates a temporary synthetic nine-chunk index, updates one document, and exercises:

- Text-to-folder search with all three member files and all indexed chunks counted.
- All three explicit levels with positive and negative indexed-file criteria, a deliberately missing model path, and folder/type filters: results succeed without starting inference.
- Chunk excerpts retain verified source hashes; aggregate results consume no excerpt budget.
- Existing multi-criterion search, stale-source detection, incremental chunk reuse with byte-identical vectors, and pause/resume/limit/stop behavior continue to pass.

CPU and Metal unit suites: 18 passed, 2 optional model/reference tests ignored. The agent skill passes the skill creator validator. A strict repository-wide Clippy run also surfaced existing style/structure warnings in the inference/indexing modules; it is not claimed as a passing gate.

## Costs and boundaries

No persisted aggregate vectors, additional source copies, model changes or schema changes. Each request scans eligible stored chunks. File mode uses a current file sum and bounded candidate pool. Folder mode additionally uses O(parent directories × dimensions) working memory. Folder membership consists of eligible immediate children after hard filters and reference-file exclusions; it is not a recursive ancestor summary. The membership fingerprint identifies the indexed snapshot and is not a live filesystem hash. The existing default remains one best passage per file; `--level file` explicitly selects a whole-document representative. Zero/cancelling representative sums fail normalization rather than fabricating a vector.
