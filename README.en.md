<div align="center">
  <img src="macos/assets/weft-logo.svg" width="112" alt="Weft woven W logo" />
  <h1>Weft</h1>
  <p><strong>Remember the file. Forget where you saved it.</strong></p>
  <p>Spotlight-fast Mac search and a local file explorer that gives agents verifiable evidence.</p>
  <p>
    <a href="README.md">한국어</a> ·
    <a href="https://github.com/CREE1116/weft/releases/latest">Latest release</a> ·
    <a href="docs/TECHNICAL_REFERENCE.md">Technical reference</a>
  </p>
  <p>
    <a href="https://github.com/CREE1116/weft/releases/latest/download/Weft-1.0.0-arm64.dmg">Apple Silicon</a> ·
    <a href="https://github.com/CREE1116/weft/releases/latest/download/Weft-1.0.0-x86_64.dmg">Intel</a>
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

## What you can do

- **Find files on Mac** — Press `⌥ Space`, switch between filename and semantic search, browse with arrow keys, then inspect in Quick Look or Finder.
- **Keep folders current** — Start an initial scan and change monitoring for selected folders, or refresh manually. Indexing continues in the background after the window closes.
- **Search inside documents** — Index PDF, Word, PowerPoint, Excel, OpenDocument, EPUB, HWPX, and common text formats.
- **Use keyboard commands** — Run `@설정`, `@메모`, `@녹음`, `@web`, and file/folder scopes. Create aliases for apps and URL templates.
- **Give agents grounded file retrieval** — Install the Weft skill for Codex, Claude Code, and compatible agents to explore folders, select files, and inspect source passages.
- **Capture notes** — Transcribe multilingual voice notes on-device and save searchable Markdown notes.

## Download and get started

### macOS app

1. Download the DMG for your Mac and move `Weft.app` to Applications.
2. Open Weft and choose folders and a global shortcut in onboarding.
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

The text model is downloaded during initial setup. The skill is in `.agents/skills/weft-semantic-search`. See the [technical reference](docs/TECHNICAL_REFERENCE.md) for detailed setup and CLI usage.

## Privacy and indexing

- Search and embeddings run locally. Queries and source contents are not sent to a search service.
- The index stores paths, hashes, locations, and vectors; it does not keep copies of source document contents.
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

## Performance

In a seven-run comparison on an Apple M4 Pro, the optimized build reduced median model loading time by **45.6%** and mmap vector-search time by **55.0%** versus the previous local build. Median short-query time was about **11.6 ms**. These are development measurements on one Mac and a 32,636-file index, not guarantees across devices or datasets. [Full methodology and results](eval/M4PRO_PERFORMANCE_2026-10-12.md).

## Project links

- [Releases and changelog](https://github.com/CREE1116/weft/releases)
- [Technical reference and CLI guide](docs/TECHNICAL_REFERENCE.md)
- [Performance measurements](eval/M4PRO_PERFORMANCE_2026-10-12.md)
- [Document parser verification](eval/DOCUMENT_RESULTS.md)
- [Report an issue](https://github.com/CREE1116/weft/issues)

Weft ranks likely matches; semantic similarity does not establish correctness or completeness. Verify important evidence in the source file.
