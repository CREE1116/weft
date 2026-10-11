# Weft rename verification (0.7.0)

Verified locally on macOS Apple Silicon, 2026-10-11.

- Package, executable, current examples and primary skill use `weft` / `weft-semantic-search`.
- The installer creates a small `myvec` wrapper that forwards all arguments to the same executable. Existing legacy skills retain a compatibility entry and their previous instructions are backed up.
- `WEFT_*` setup and Python/reference settings accept the old `MYVEC_*` names as fallbacks.
- Database and model cache paths retain their historical `myvec` directory names. No database migration, model copy or re-embedding is part of this rename.
- CPU and Metal unit suites each passed 26 tests; two optional reference tests remain ignored.
- Real native feature and document verification passed using `target/release/weft`, including cache reuse, chunk/file/folder retrieval, source verification and document images.
- Isolated bootstrap tests passed: first install, repeat reuse, legacy settings and command forwarding, legacy skill backup, explicit scan scope, private repository failure and temporary checkout cleanup. Real CLI bootstrap reuse also passed.
- The existing installation had 25,950 stored vectors before installation. A SHA-256 over ordered vector identifiers and bytes was identical afterward. The cached model's inode, size and modification time were unchanged. Both installed commands reported `weft 0.7.0`, and installed primary skill resources matched the repository.

Reproduce the isolated checks:

```sh
cargo test --locked
cargo test --locked --features metal
cargo build --release --locked --features metal
python3 eval/verify_bootstrap.py --binary target/release/weft
python3 eval/verify_features.py --binary target/release/weft
python3 eval/verify_documents.py --binary target/release/weft
```

Rollback requires no data conversion: stop an active indexer, reinstall the prior source revision with `scripts/install.sh --no-index`, and use `myvec` against the same database and model. Existing search semantics and the storage schema were not changed. The renamed GitHub repository remains private.
