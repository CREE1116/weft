# Hierarchical vectors and change-direction experiment

Executed with the installed myvec CLI on the development Apple Silicon Mac on 2026-10-11. Synthetic files and the existing 24-document evaluation corpus were indexed into an isolated temporary database; no personal source files or database are included in this report. Native text Q8 GGUF, 768-dimensional Q8 vectors, 400-character chunks and zero overlap were used.

## Direct usage observation

A two-criterion retrieval restricted to this source repository in the shared computer index returned `evaluated_chunks=0` and no results. The initial computer scan had not supplied eligible text chunks for this scope. The command still loaded the Metal model before discovering that the scope was empty. This identifies two remaining usability/cost issues: explicitly distinguish missing coverage from poor relevance, and preflight scope coverage before inference. The isolated experiment below ensures coverage; its successes do not establish whole-computer readiness.

## File representatives

The file representative is the normalized sum of reconstructed chunk vectors, matching the installed file-centroid behavior. These are cosine distances between representatives, not percentages of edited text or calibrated severity.

| Variant | Chunks | Centroid cosine distance | Largest new-chunk nearest-old distance | Missing / added chunk identities |
| --- | ---: | ---: | ---: | ---: |
| Unchanged | 8 | 0.000000 | 0.000000 | 0 / 0 |
| Paragraph reorder | 8 | 0.000000 | 0.000000 | 0 / 0 |
| Retry section paraphrased | 8 | 0.011056 | 0.177330 | 3 / 3 |
| Retry attempts 8 -> 100 | 8 | 0.003569 | 0.041458 | 3 / 3 |
| Monitoring replaced with Kyoto travel | 8 | 0.032753 | 0.382961 | 2 / 2 |
| Kyoto travel appended | 10 | 0.014124 | 0.382961 | 0 / 2 |
| Whole document repeated three times | 24 | 0.000000 | 0.000000 | 0 / 16 |

Reordering changes the file contents and chunk sequence but preserves the representative. Repeating the entire document triples its chunk count while preserving the representative. The multiset identity-change statistic in the JSON is `1 - intersection_count / union_count`; it is not an edited-character fraction. Missing/added identities are also affected by changed chunk boundaries.

The retry-limit modification is a material behavior change, yet its centroid movement is small. Paraphrasing moves the centroid more in this example. Replacing a topic produces a stronger signal. New-chunk nearest-old distances reveal local differences that the average can dilute, but nearest matching is many-to-one, loses order and does not determine factual importance.

## Folder weighting

For a folder containing retry and travel material, normalizing each file representative and then averaging gives cosine similarity approximately 0.8861 to either topic. Treating the retry material as ten repeated chunks before aggregating gives similarity approximately 0.9970 to retry and 0.6323 to travel. File-weighted and chunk-weighted folder representatives answer different questions. File weighting is appropriate when each file is equally important; it does not preserve the influence of every chunk. A single representative also hides minority topics.

## Existing retrieval as change-direction search

The installed command was exercised as:

```sh
myvec retrieve --file MODIFIED_FILE --avoid-file ORIGINAL_FILE --under CORPUS --kind text --top 5 --json
```

A deliberately absent model path was supplied to verify that file-only retrieval requires no inference. For one positive and one negative normalized file representative with weight 1, `d(C,new) - d(C,old) = dot(C,old - new)`. Sorting ascending therefore ranks candidates along the new-minus-old direction. The example replaces monitoring content with Kyoto travel; the five returned files were Kyoto budget, Kyoto weather, Tokyo budget, Osaka plan and Osaka hotel. This discovers material aligned with the added emphasis; it is not a verified causal dependency or a precise textual diff.

A separate two-text-criterion retrieval (`safe database schema migration` plus `expand contract rollout`) ranked the migration document first and returned verified excerpts within the requested total budget. This is a guided synthetic example, not a held-out accuracy improvement claim.

## Applications and limits

Useful next steps are revision-aware context refresh, retrieving material associated with newly introduced topics, and folder/project drift triage. Keep exact content hashes, chunk identity/count/sequence changes and semantic displacement together. Use representative displacement for prioritization; do not discard low-drift edits or infer contradictions, code dependency, correctness, edit volume or impact from embedding distance.

No automatic revision history or production folder-vector API is implemented by this experiment. Both before/after reference files must currently exist and be indexed. Compact opt-in baselines could retain a representative and hashes without storing all historical text; retention limits would be necessary to keep disk usage bounded.

Reproduce with `python3 eval/experiment_hierarchy.py`. Raw results are in [HIERARCHY_RESULTS.json](HIERARCHY_RESULTS.json).
