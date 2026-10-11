# Native document and image extraction (myvec 0.6.0)

## Implemented path

Rust parses PDF, DOCX/DOCM, PPTX/PPTM, XLSX/XLSM and ODT/ODP/ODS. Text follows the existing chunk/file/folder retrieval pipeline. `kind=text` and `line_kind=extracted_text` distinguish content vectors and extracted coordinates from filename-only records. Verified excerpts carry PDF page, presentation slide, spreadsheet sheet or XML-part locations. DOCX page layout is not inferred.

`myvec extract DOCUMENT --json` inventories text and embedded images without starting inference or opening a database. `--images-dir DIRECTORY` explicitly exports images. Office images keep original bytes; PDF JPEG/JPX streams keep original encoded data and supported 8-bit RGB/gray raw/Flate images become PNG. Every PDF image stream is inventoried, including nested resources and masks. Direct page references identify known pages; other objects keep object IDs. Unsupported PDF encodings remain marked, without invalid image output. No rendering, alpha-mask compositing, inline-image extraction, OCR or native visual embedding was added.

## Evidence

CPU and Metal unit suites each pass **26 tests**, with two unchanged optional model/reference tests ignored. New extraction tests cover:

- Word text/entity/Unicode/table extraction, original embedded media bytes and text-only indexing without reading media parts.
- Presentation relationship order (reordered slide files), rather than sorting part filenames.
- Shared-string/rich-string, inline-string and numeric spreadsheet values.
- OpenDocument text/images, oversized ZIP parts and declared ZIP expansion rejection.
- Two PDF pages and a shared embedded raster: output PNG decodes to the expected red pixel; exports reuse identical bytes and refuse conflicting output files.

`python3 eval/verify_documents.py` builds synthetic PDF/DOCX/PPTX/XLSX/ODT fixtures using Python's standard library and exercises the actual native release CLI:

- All five formats produce text, location metadata and exported images with a deliberately nonexistent model path, without creating the chosen index database.
- Office image exports match the original bytes exactly.
- Real native embeddings make document text searchable and return hash-verified excerpts with extracted coordinate labels and source locations.
- File-only criteria reconstruct binary-document evidence without loading a model.
- A simulated legacy filename-only record upgrades to text on reindex; a corrupt PDF falls back to `kind=path` with `--all`, while the scan correctly reports a parsing error and exits nonzero rather than claiming full content coverage.
- Modified PDF bytes suppress stale excerpts.

The existing native feature integration and standalone skill/bootstrap lifecycle checks also pass. These are synthetic correctness checks, not a broad Office/PDF compatibility corpus, visual search benchmark or proof of arbitrary-document retrieval accuracy.

## Cost and limits

The Metal release executable is approximately **13.5 MiB**, versus **11.6 MiB** before parser support. The approximately 296 MiB text model is unchanged. No extracted source text, image payloads or secondary document index are persisted during indexing; only existing vector/path/hash/line records are used. Explicit image exports consume their own chosen output space, with content-derived filenames deduplicating identical image data.

Document input limit: 32 MiB. Extracted text: 2 MiB. ZIP part: 8 MiB; at most 8,192 parts and 64 MiB declared total expansion. Per-request image output: 32 MiB; at most 4,096 PDF image streams and 2,048 PDF pages. PDF parser internal working memory is not bounded by the input limit. Fonts, complex encodings, layout and unusual producer behavior can still affect extraction quality. Encrypted/corrupt documents are not decrypted or repaired. Scanned PDFs without text need OCR. Legacy DOC/PPT/XLS and HWP remain filename-only. Spreadsheet cached values are read, without formula evaluation or formatting/date reconstruction. Linked external images are not fetched.
