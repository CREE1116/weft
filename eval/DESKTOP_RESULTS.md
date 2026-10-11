# Desktop installation and search verification

Verified on 2026-10-11 on the development Apple Silicon Mac.

The installer installs `myvec` into the Cargo binary directory, installs the global `myvec-semantic-search` agent skill, and invokes `index --all --background`. The background CLI starts a worker in an independent process group/session; it does not need the installer or terminal to remain running. No watcher or recurring scheduler is installed.

## Functional checks

A synthetic fixture contained overlapping text chunks, hidden text notes, a fake JPEG, a text file over the 2 MiB content limit, `.env`, a dependency folder and a custom excluded folder.

- Dry-run discovered four eligible files, with two filename-only candidates, without writing vectors or loading the model.
- Actual indexing stored four files in seven chunks: two text-content records and two filename-only records.
- JSON search found the retry document with positive line numbers. Filename-only hits were explicitly `kind=path` with zero line numbers.
- A second scan indexed zero files and skipped all four unchanged records.
- Stats remained readable while indexing; a second indexer on the same database was rejected by the lock.
- A separate detached background worker outlived the launching command and completed a 693-chunk file with no errors in 92.3 seconds. This is a lifecycle check, not a general performance benchmark.
- Installed CLI search was exercised from `/tmp`, outside the repository, and returned valid structured results against the shared index while the initial computer scan was active.
- Six unit tests pass, including exclusion policies and lossless Unicode chunking. Two model-reference tests remain explicitly opt-in. CPU-only compilation and shell syntax/format checks pass.
- A live scan exposed a renamed `.venv-*` dependency directory. Exclusions now also cover `.venv-*`, `.venv_*`, `venv-*`, `venv_*` directories and `site-packages`; regression cases preserve ordinary filenames such as `venv-notes.md`. Generated environment records were removed from the development index before resuming.

The initial computer scan is a background task; its eventual coverage and failures are shown by `myvec stats --json` and the database's `.log` sibling. Do not interpret the installation or these fixture tests as proof that every real computer file has finished indexing.

## Coverage and limits

A whole-computer scan excludes OS/app/runtime/cache directories, dependency trees, private-key/credential stores and its own files; honors ignore files; and does not follow symlinks. Default per-user application directories include agent/editor/browser profiles. Explicit `index --all FOLDER` can scan a chosen folder without the computer-level OS/app exclusions, while credential/dependency exclusions still apply. App-cache records produced during the early installation test were removed from the newly generated index when this policy was refined.

Supported UTF-8 text up to 2 MiB gets content embeddings and chunk line numbers. Unsupported formats, binary files and larger files get filename/folder/type embeddings. PDF/Office/media contents are not inferred from these filename hits. Native multimodal encoders and audio/video segment search remain unimplemented.

SQLite uses WAL with a busy timeout for concurrent search, and stats include the database plus WAL/shared-memory file sizes. Progress for long text files includes the active path and completed/total chunks. If a stored last scan says `running` but `indexer_running=false`, that run was interrupted; rerunning preserves committed files and skips unchanged data.
