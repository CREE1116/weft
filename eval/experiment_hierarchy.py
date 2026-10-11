#!/usr/bin/env python3
"""Native semantic-change experiment on synthetic files; no personal data."""
import argparse
from collections import Counter
import json
import math
from pathlib import Path
import shutil
import sqlite3
import struct
import subprocess
import tempfile


def normalize(v):
    n = math.sqrt(sum(x*x for x in v))
    return [x/n for x in v]


def aggregate(vectors):
    return normalize([sum(x) for x in zip(*vectors)])


def cosine(a, b):
    return max(-1.0, min(1.0, sum(x*y for x, y in zip(a, b))))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default=str(Path.home()/".cargo/bin/weft"))
    parser.add_argument("--output", default="eval/HIERARCHY_RESULTS.json")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    corpus = Path(__file__).resolve().parent/"corpus"
    with tempfile.TemporaryDirectory(prefix="weft-hierarchy-") as tmp:
        root = Path(tmp).resolve()
        docs = root/"docs"
        docs.mkdir()
        shutil.copytree(corpus, docs/"corpus")
        # Each paragraph is one oversized line. Chunk boundaries stay within
        # that paragraph, so permuting paragraphs preserves chunk identities.
        blocks = [
            "Failed background jobs retry with exponential backoff. Start with a one second delay, double the wait after each failure, cap the delay at five minutes, and stop after eight attempts. A dead-letter queue holds jobs that exhaust their retries. "*4,
            "Production database schema changes use expand and contract. Add a nullable field, deploy compatible readers and writers, backfill records in batches, and enforce constraints after old versions are retired. "*4,
            "Monitor deployment health using request latency and error rates. Alert the operations team when a release raises failures. Track service availability and investigate incidents with logs and metrics. "*4,
        ]
        travel = "The Kyoto vacation budget includes hotels, train tickets, restaurant meals and visits to temples. Reserve accommodation near transit and set aside money for transport and food during the trip. "*4
        paraphrase = "Retry failed jobs using progressively longer intervals. Begin at one second, multiply the delay by two, never exceed five minutes, and allow eight tries. Move unsuccessful jobs into the dead-letter queue. "*4
        base = "\n".join(blocks)
        variants = {
            "baseline": base,
            "reordered": "\n".join(reversed(blocks)),
            "paraphrased": "\n".join([paraphrase, *blocks[1:]]),
            "critical_number": base.replace("eight attempts", "one hundred attempts"),
            "topic_shift": "\n".join([*blocks[:2], travel]),
            "topic_added": "\n".join([*blocks, travel]),
            "duplicated": "\n".join([base]*3),
        }
        paths = {}
        for name, text in variants.items():
            paths[name] = docs/f"{name}.txt"
            paths[name].write_text(text)
        db = root/"index.sqlite"

        def cli(*argv):
            p = subprocess.run([binary, "--db", str(db), *map(str, argv)],
                               text=True, capture_output=True, timeout=180)
            if p.returncode:
                raise RuntimeError(p.stderr)
            return p.stdout

        cli("index", docs, "--chunk-chars", "400", "--overlap-lines", "0")
        with sqlite3.connect(db) as conn:
            def records(path):
                result = []
                for blob, fingerprint in conn.execute("SELECT c.embedding,c.input_hash FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.path=? ORDER BY ordinal", (str(path),)):
                    scale = struct.unpack("<f", blob[:4])[0]
                    values = [x*scale for x in struct.unpack(f"{len(blob)-4}b", blob[4:])]
                    result.append((values, fingerprint))
                return result

            data = {name: records(path) for name, path in paths.items()}
            representatives = {name: aggregate([v for v, _ in rows]) for name, rows in data.items()}
            reference = representatives["baseline"]
            old_hashes = Counter(h for _, h in data["baseline"])
            measurements = []
            for name, text in variants.items():
                rows = data[name]
                new_hashes = Counter(h for _, h in rows)
                overlap = sum((old_hashes & new_hashes).values())
                union = sum((old_hashes | new_hashes).values())
                coherence = math.sqrt(sum(x*x for x in [sum(x)/len(rows) for x in zip(*(normalize(v) for v, _ in rows))]))
                old_vectors = [normalize(v) for v, _ in data["baseline"]]
                largest_new_distance = max(1-max(cosine(normalize(v), old) for old in old_vectors) for v, _ in rows)
                measurements.append({"variant": name, "characters": len(text), "chunks": len(rows),
                                     "content_changed": text != base,
                                     "chunk_sequence_changed": [h for _, h in rows] != [h for _, h in data["baseline"]],
                                     "old_chunks_missing": sum((old_hashes-new_hashes).values()),
                                     "new_chunks_added": sum((new_hashes-old_hashes).values()),
                                     "centroid_cosine_distance": 1-cosine(reference, representatives[name]),
                                     "chunk_identity_change": 1-overlap/union,
                                     "largest_new_chunk_nearest_old_distance": largest_new_distance,
                                     "chunk_coherence": coherence})

            # Compare folder aggregation rules using two equally important files,
            # one of which merely repeats its subject many more times.
            retry = records(docs/"corpus/engineering/retry-policy.md")
            holiday = records(docs/"corpus/travel/kyoto-budget.md")
            retry_v = aggregate([v for v, _ in retry])
            holiday_v = aggregate([v for v, _ in holiday])
            equal_file = aggregate([retry_v, holiday_v])
            chunk_weighted = aggregate([v for v, _ in retry]*10 + [v for v, _ in holiday])
            folder = {
                "equal_file_similarity_retry": cosine(equal_file, retry_v),
                "equal_file_similarity_travel": cosine(equal_file, holiday_v),
                "chunk_weighted_similarity_retry": cosine(chunk_weighted, retry_v),
                "chunk_weighted_similarity_travel": cosine(chunk_weighted, holiday_v),
            }

        def result_names(response):
            return [{"file": Path(h["path"]).relative_to(docs).as_posix(),
                     "distance": h["distance"], "source_state": h["source_state"],
                     "context": h.get("context", "")} for h in response["hits"]]

        task = json.loads(cli("retrieve", "safe database schema migration", "--also",
                              "expand contract rollout", "--under", docs/"corpus",
                              "--kind", "text", "--top", "3", "--context-chars", "800", "--json"))
        delta = json.loads(cli("--model-file", root/"missing.gguf", "retrieve", "--file",
                               paths["topic_shift"], "--avoid-file", paths["baseline"],
                               "--under", docs/"corpus", "--kind", "text", "--top", "5", "--json"))
        report = {"model": "EmbeddingGemma 2 text Q8 GGUF; Q8 index, 768 dimensions",
                  "chunk_chars": 400, "overlap_lines": 0,
                  "measurements": measurements, "folder_aggregation": folder,
                  "multi_criterion_task": result_names(task),
                  "change_direction_related_files": result_names(delta),
                  "limitations": "Synthetic examples; centroid displacement is not edit count, factual importance, or a calibrated change threshold. Delta ranking is exploratory, not a guaranteed topic extractor."}
        output = Path(args.output)
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2)+"\n")
        print(json.dumps({"report": str(output), "measurements": measurements,
                          "folder": folder, "delta_files": [h["file"] for h in report["change_direction_related_files"]],
                          "task_files": [h["file"] for h in report["multi_criterion_task"]]}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
