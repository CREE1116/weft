# Agent file explorer workflow (myvec 0.5.0)

The product entry point is the portable agent skill. The Rust CLI remains the local execution engine; search-unit and vector-distance semantics are unchanged.

## First-use lifecycle

`python3 eval/verify_bootstrap.py` uses a copied standalone skill and isolated install/cache/data directories. Cargo, Git and model download are stubbed so lifecycle checks use no real network, build or user index:

- A missing/incompatible CLI can be installed from an explicit local checkout, with the complete skill scripts and advanced reference installed alongside it.
- A subsequent call reuses both CLI and model, without Cargo, Git or download calls.
- Bootstrap starts no index operation by default and creates no index database.
- An explicitly supplied folder starts one background scan of that folder, with no whole-computer scan. A nonexistent folder fails before setup work.
- A standalone copy can fetch the source when needed. Private repository access failure produces an actionable setup error rather than a search result. Temporary source checkouts are cleaned on success and failure.

The optional `--binary target/release/myvec` check also exercises compatible reuse with the actual CLI without opening an index or loading weights. Version output is covered by a CLI contract test. The stub lifecycle test is included in CPU/Linux and Metal/macOS CI. It proves bootstrap behavior, not compiler installation, credentials or model-download availability on every machine.

## Coverage and evidence

CPU and Metal unit suites each pass 20 tests, with 2 optional model/reference tests ignored. A new indexed-coverage test checks nested folders, path-component boundaries, kind filtering, empty documents and empty scopes.

`python3 eval/verify_features.py` exercises the real native CLI and cached model on synthetic files. New checks run scoped stats with a deliberately missing model path: empty indexes/unindexed subfolders report `no_indexed_chunks`; indexed scopes return exact stored file/chunk/vector-byte counts without inference. Existing folder/file/chunk ranking, verified excerpts, changed-source withholding and background resource controls remain passing.

The skill checks scope coverage before semantic retrieval, uses folder representatives as navigation hints, and drills down to chunks for verifiable evidence. A zero-result or unindexed scope is not a factual claim that no relevant source exists. Coverage does not walk the filesystem or verify fresh source contents. No automatic top-folder candidate pruning was added.

## Installed-agent workflow exercise

The real skill bootstrap updated the development machine's CLI and complete installed skill, then a second invocation reused the CLI and cached model. The real first pass revealed that version output was missing; the CLI now implements `--version`, with both a parser contract test and a real-binary bootstrap check preventing that regression.

The project's shared index scope initially reported zero indexed text chunks. A scoped refresh indexed 61 text files / 326 chunks. An installed-CLI query about preparing the engine without starting a computer scan returned a folder overview and verified documentation passages within a 1,600-character total budget. Other selected passages reported `budget_exhausted` rather than fabricated context. Broad semantic results favored explanatory documentation over some implementation files; this is evidence of a working exploration/evidence flow, not proof that code navigation or task accuracy is solved. The skill preserves ordinary file-tool fallback and does not force a top-folder cutoff. The previous whole-computer worker was restarted with its default 25% inference duty cycle.

## Distribution boundaries

A skill file does not execute a platform installation hook merely when copied. Its first-use instructions invoke the bundled bootstrap helper. Current distribution builds from Rust source and downloads/reuses approximately 296 MiB of text weights; prebuilt engine binaries are not yet provided. The source repository is private and requires Git access or a supplied checkout. Build-tool/dependency storage is separate from the small runtime executable. Source-build temporary checkouts are deleted by the bootstrap helper after installation.

No persistent inference service, SIMD-search backend, approximate index or native media encoder was added in this slice. It establishes an agent-facing installation, coverage and evidence workflow before engine/search optimization.
