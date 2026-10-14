<div align="center">
  <img src="macos/assets/weft-logo.png" width="112" alt="Weft woven W logo" />
  <h1>Weft</h1>
  <p><strong>Remember the file. Forget where you saved it.</strong></p>
  <p>Mac search at a keyboard shortcut and a local file explorer that gives agents verifiable evidence.</p>
  <p>
    <a href="README.md">한국어</a> ·
    <a href="https://github.com/CREE1116/weft/releases/latest">Latest release</a> ·
    <a href="docs/TECHNICAL_REFERENCE.md">Technical reference</a>
  </p>
  <p>
    <a href="https://github.com/CREE1116/weft/releases/download/v1.0.2/Weft-1.0.2-arm64.dmg">Apple Silicon</a> ·
    <a href="https://github.com/CREE1116/weft/releases/download/v1.0.2/Weft-1.0.2-x86_64.dmg">Intel</a>
  </p>
  <p>
    <img alt="Latest release" src="https://img.shields.io/github/v/release/CREE1116/weft?display_name=tag&color=7655c9" />
    <img alt="Release downloads" src="https://img.shields.io/github/downloads/CREE1116/weft/total?color=7655c9" />
    <img alt="macOS 14+" src="https://img.shields.io/badge/macOS-14%2B-7655c9" />
    <img alt="Runs locally" src="https://img.shields.io/badge/search-local-7655c9" />
  </p>
</div>

---

Weft is a **lightweight desktop search app and local retrieval engine for agents**. Find files by name or meaning, inspect source locations, and retrieve verified context. Search and embeddings run on your device.

## Start in your language

Choose **한국어, English, 简体中文, or 日本語** on the first setup screen. The choice applies to onboarding, search, menus, and settings. Change it instantly under Settings → General → Language; switching UI language does not require reindexing.

![Weft setup begins with a language choice](docs/assets/weft-welcome-preview.png)

## See the search flow

![Weft filename search, content search, and identifier lookup](docs/assets/weft-search-demo.gif)

Find a filename, narrow by remembered content, then look up an identifier such as `INV-2041`. This edited demo uses the actual app UI and real results from temporary documents; playback is not a latency measurement. [Static preview](docs/assets/weft-demo-poster.png)

## What you can do

- **Find files on Mac** — Press `⌥ Space`, switch between filename and semantic search, browse with arrow keys, then inspect in Quick Look or Finder.
- **Keep folders current** — Index selected folders in the background with a default 25% compute budget. Indexing yields to interactive searches. The menu bar shows progress, estimated remaining work, watching state, and failures.
- **Search inside documents** — Index PDF, Word, PowerPoint, Excel, OpenDocument, EPUB, HWPX, and common text formats. Ordinary content search combines meaning, content keywords, and paths.
- **Keep matches relevant** — `md` and `pdf` use exact extension lookup. Complete identifier hits skip inference. General searches hide insufficient semantic evidence and results scoring less than half of the best result.
- **Preview centered pages** — Compact Word/PDF/Office previews fit the page in the center. Press `⌘Y` for a larger preview. [Preview example](docs/assets/weft-document-preview.png)
- **Use keyboard commands** — Run `@설정`, `@메모`, `@녹음`, `@web`, and file/folder scopes. Create aliases for apps and URL templates.
- **Give agents grounded file retrieval** — Install the Weft skill for Codex, Claude Code, and compatible agents to explore folders, select files, and inspect source passages.
- **Capture notes** — Transcribe multilingual voice notes on-device and save searchable Markdown notes.

| What you remember | Example | Behavior |
| --- | --- | --- |
| Filename or format | `report` in filename mode; `md` in content mode | Filename/path or exact extension lookup |
| Document content | `invoice approval`, `database retry policy` | Meaning + content keywords, with weak matches removed |
| Identifier or code symbol | `INV-2041`, `retryHandler` | Complete content-keyword matches bypass inference |
| Multiple criteria | `database + retries - travel` | Explicit semantic and negative criteria retain their meaning |

Basic semantic retrieval was checked in English, French, Spanish, German, Russian, Arabic, Japanese, and Chinese; this is not a per-language accuracy benchmark. Korean partial content-keyword search is supported. Mid-sentence Japanese and Chinese keyword substrings need further work. [Verification record](eval/MULTILINGUAL_SEMANTIC_2026-10-15.json)

## Download and get started

### macOS app

1. Download the DMG for your Mac and move `Weft.app` to Applications.
2. Choose your UI language first, then prepare the model, search folder, and global shortcut.
3. Start indexing or enable change monitoring from the Weft menu-bar menu.
4. Press `⌥ Space` to search.

Weft supports macOS 14 and later, with separate Apple Silicon and Intel builds. Public builds may be ad hoc signed without Developer ID notarization. If Gatekeeper blocks the app, use **System Settings → Privacy & Security → Open Anyway**.

### CLI and agent skill

Clone the repository to install the CLI, model, and agent skill without starting an index scan:

```sh
git clone https://github.com/CREE1116/weft.git
cd weft
./scripts/install.sh --no-index
```

```sh
weft search "database retry policy" --json
weft index ~/Documents --watch
weft stats --json
```

`@settings`, `@note`, and `@record` also work alongside the Korean command names. CLI search accepts `--min-similarity 0.3 --weak-ratio 0.5` to tune relevance; exact format and identifier matches retain their keyword evidence.

The text model is downloaded during initial setup. The skill is in `.agents/skills/weft-semantic-search`. See the [technical reference](docs/TECHNICAL_REFERENCE.md) for detailed setup and CLI usage.

## Privacy and indexing

- Search and embeddings run locally. Queries and source contents are not sent to a search service.
- The index stores paths, hashes, locations, and vectors; it does not keep full copies of source documents. Content keyword tokens remain on disk in an inverted index, so treat the index as private data.
- Onboarding does not scan the whole disk. You choose folders and control indexing, monitoring, exclusions, and index deletion in Settings.
- Initial model downloads, GitHub update checks, and web searches you request may use the network. Media embeddings and speech models are optional downloads.

## Supported files

| Type | Indexed content |
| --- | --- |
| Source, Markdown, text, config, notebooks, and more | Filename, document text, and contextual chunks |
| PDF, DOCX/DOCM, PPTX/PPTM, XLSX/XLSM | Extractable text and source locations |
| ODT/ODP/ODS, EPUB, HWPX | Extractable text |
| Other or over-limit files | Filename and path search |
| Images, audio, and video | Content embeddings with the optional media model |

See the [technical reference](docs/TECHNICAL_REFERENCE.md) for parser limits, supported details, and media requirements. A filename match does not mean file contents were indexed; Weft keeps those results distinguishable.

## Benchmarks

### Relevance and result selection — 2026-10-15

Apple M4 Pro, Metal, 768-dimensional Q8 index, **26 files and 21 labeled queries**. Search latency uses seven warm samples per query and **excludes model loading, query embedding, and process startup**. This small development fixture does not establish general relevance or large-index performance.

| Search variant | Recall@10 | MRR@10 | NDCG@10 | Search p50 | Search p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Semantic | 1.000 | 0.913 | 0.935 | 0.034 ms | 0.044 ms |
| Legacy semantic + path RRF | 1.000 | 0.913 | 0.935 | 0.047 ms | 0.059 ms |
| Content keywords + meaning + cutoff | 1.000 | 0.976 | 0.982 | 0.431 ms | 0.669 ms |

Higher MRR and NDCG indicate relevant files closer to the top. Fusion improves ordering on this fixture, while legacy RRF remains cheaper for result selection alone. Content lookup adds work. [Full comparison, limits, and reproduction](eval/HYBRID_SEARCH_2026-10-15.md) · [Raw results](eval/HYBRID_SEARCH_2026-10-15.json) With relevance cutoffs, fusion returned 1.57 results per query on average without losing a labeled answer on this fixture. [Cutoff results](eval/HYBRID_SEARCH_CUTOFF_2026-10-15.json)

### Model loading and larger vector search — 2026-10-12

Seven alternating runs of previous and optimized builds on the same Apple M4 Pro and the same isolated snapshot: **32,636 files and 89,894 chunks**. Values are medians. This experiment uses a different dataset, build, and measured stage from the small relevance fixture above; the latencies are not directly comparable.

| Metric | Previous build | Optimized build | Change |
| --- | ---: | ---: | ---: |
| Model load | 324.096 ms | 176.155 ms | **45.6% lower** |
| mmap SIMD scan + Top-K | 8.217 ms | 3.701 ms | **55.0% lower** |
| Single-text WQL retrieval | 22.002 ms | 16.813 ms | **23.6% lower** |
| Peak process memory | 953.609 MiB | 873.109 MiB | **80.500 MiB less** |
| Short-query inference p50 | 11.458 ms | 11.594 ms | 1.2% higher |

Scan timing excludes inference and SQL. Memory is peak RSS for the full benchmark process. Indexing throughput, power, and GPU utilization were not measured. [Full results](eval/M4PRO_PERFORMANCE_2026-10-12.md)

The current runtime normalizes on the inference device and transfers one vector to CPU for Q8 search. Snapshot writes stream vectors rather than copying the entire payload into RAM. Long-document masks retain only the active sequence size and are released for short queries. The 25% background setting is a compute-time budget, not a guaranteed GPU-utilization percentage.

## Project links

- [Releases and changelog](https://github.com/CREE1116/weft/releases)
- [Technical reference and CLI guide](docs/TECHNICAL_REFERENCE.md)
- [Relevance comparison](eval/HYBRID_SEARCH_2026-10-15.md) · [Larger-index performance](eval/M4PRO_PERFORMANCE_2026-10-12.md)
- [Document parser verification](eval/DOCUMENT_RESULTS.md)
- [Report an issue](https://github.com/CREE1116/weft/issues)

Weft ranks likely matches; semantic similarity does not establish correctness or completeness. Verify important evidence in the source file.
