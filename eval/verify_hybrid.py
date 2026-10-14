#!/usr/bin/env python3
"""Isolated hybrid CLI proof, using only stdlib and a local mock HTTP model.

Run after building: python3 eval/verify_hybrid.py --binary target/release/weft
Requires Python SQLite >= 3.43 with FTS5 for contentless-delete inspection.
All documents, databases, caches and model paths live in a temporary directory.
The deterministic embeddings exercise plumbing, not real-model search quality.
"""

import argparse
from contextlib import contextmanager
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import threading
import tempfile


REPO = Path(__file__).resolve().parents[1]
DIM = 256


def embedding(text):
    """Stable bag-of-token vectors; strip the transport's task/title prefix."""
    for prefix in ("task: search result | query: ", "title: none | text: "):
        if text.startswith(prefix):
            text = text[len(prefix):]
            break
    vector = [0.0] * DIM
    for token in re.findall(r"\w+", text.lower()) or ["empty"]:
        digest = hashlib.sha256(token.encode("utf-8")).digest()
        vector[int.from_bytes(digest[:2], "big") % DIM] += 1.0
    norm = math.sqrt(sum(value * value for value in vector))
    return [value / norm for value in vector]


@contextmanager
def mock_model():
    calls, errors = [], []

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            try:
                if self.path != "/v1/embeddings":
                    raise ValueError(f"unexpected endpoint {self.path}")
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if request.get("model") != "mock":
                    raise ValueError("unexpected embedding model")
                inputs = request.get("input")
                if not isinstance(inputs, list) or not inputs or not all(
                    isinstance(text, str) for text in inputs
                ):
                    raise ValueError("expected a nonempty array of text inputs")
                calls.append(inputs)
                body = json.dumps({"object": "list", "model": "mock", "data": [
                    {"object": "embedding", "index": index, "embedding": embedding(text)}
                    for index, text in reversed(list(enumerate(inputs)))
                ]}).encode("utf-8")
                self.send_response(200)
            except Exception as error:
                errors.append(str(error))
                body = json.dumps({"error": str(error)}).encode("utf-8")
                self.send_response(400)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}/v1/embeddings", calls, errors
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def vector_snapshot(db):
    with sqlite3.connect(db) as conn:
        return {
            "chunks": conn.execute(
                "SELECT d.path,c.ordinal,c.start_line,c.end_line,c.embedding,c.input_hash "
                "FROM chunks c JOIN documents d ON d.id=c.doc_id ORDER BY d.path,c.ordinal"
            ).fetchall(),
            "cache": conn.execute(
                "SELECT input_hash,embedding FROM embedding_cache ORDER BY input_hash"
            ).fetchall(),
        }


def check_storage(db, expected_files, raw_bodies):
    with sqlite3.connect(db) as conn:
        schema = conn.execute(
            "SELECT sql FROM sqlite_schema WHERE name='chunk_fts'"
        ).fetchone()[0]
        compact = re.sub(r"\s+", "", schema.lower())
        assert "content=''" in compact and "contentless_delete=1" in compact, schema
        assert conn.execute(
            "SELECT count(*) FROM sqlite_schema WHERE name='chunk_fts_content'"
        ).fetchone()[0] == 0, "FTS retained a content shadow table"
        assert conn.execute("SELECT count(*) FROM documents").fetchone()[0] == expected_files
        assert conn.execute("SELECT count(*) FROM lexical_documents").fetchone()[0] == expected_files
        postings = conn.execute("SELECT rowid,tokens FROM chunk_fts ORDER BY rowid").fetchall()
        assert postings and all(tokens is None for _, tokens in postings), "raw FTS content readable"
        mappings = conn.execute("SELECT rowid FROM lexical_chunks ORDER BY rowid").fetchall()
        assert [(rowid,) for rowid, _ in postings] == mappings, "orphan/missing FTS postings"
        assert conn.execute("PRAGMA foreign_key_check").fetchall() == []
        for table in ("documents", "chunks", "lexical_documents", "lexical_chunks"):
            columns = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
            assert not columns & {"body", "text", "content", "tokens"}, (table, columns)
        # Detect plaintext in all persistent tables, including FTS shadow tables.
        dump = "\n".join(conn.iterdump())
        assert all(body not in dump for body in raw_bodies), "raw body persisted in SQLite"
        count = len(postings)
    for path in (db, Path(str(db) + "-wal"), Path(str(db) + ".vectors.bin")):
        if path.exists():
            data = path.read_bytes()
            assert all(body.encode("utf-8") not in data for body in raw_bodies), path
    return count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default=str(REPO / "target/release/weft"))
    args = parser.parse_args()
    binary = Path(args.binary).resolve()
    if not binary.is_file():
        parser.error(f"build the CLI first: missing {binary}")
    if sqlite3.sqlite_version_info < (3, 43, 0):
        parser.error("Python SQLite >= 3.43 is required to inspect contentless-delete FTS5")

    with tempfile.TemporaryDirectory(prefix="weft-hybrid-") as tmp, mock_model() as mock:
        root = Path(tmp).resolve()
        endpoint, calls, errors = mock
        docs = root / "docs"
        docs.mkdir()
        db = root / "index.sqlite"
        env = dict(os.environ, HOME=str(root / "home"), XDG_CACHE_HOME=str(root / "cache"),
                   XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(root / "config"),
                   TMPDIR=str(root))
        (root / "home").mkdir()
        bodies = {
            "a.txt": "Private source sentence must never be stored as a raw body.\n"
                     "Invoice INV-2041 identifier K7Q2LM account 4815162342.\n"
                     "Call load_model then loadModel through Store::open.\n",
            "b.txt": "Invoice INV-2042 identifier K7Q2LN account 4815162343.\n"
                     "Retry requests with exponential backoff and jitter.\n",
            "c.txt": "운영서비스의메모리사용량을확인하고장애대응절차를기록합니다.\n",
            "d.txt": "Original Z9OLDTOKEN configuration for timeout and retries.\n",
            "e.txt": "Obsolete Z8PRUNEME vacation hotel and train expenses.\n",
            "f.txt": "\n".join(
                f"Section {i}: {'split_first' if i == 0 else 'split_last' if i == 11 else 'ordinary'} "
                + "separate chunk evidence with unrelated prose and stable indexing. " * 2
                for i in range(12)
            ) + "\n",
        }
        for name, body in bodies.items():
            (docs / name).write_text(body, encoding="utf-8")

        def run(*argv, offline=False, database=db, stdin=None, fail=False):
            # No command can select the user's real database or native model.
            assert database.is_relative_to(root), database
            options = ["--db", str(database), "--model-file", str(root / "missing.gguf")]
            if offline:
                options += ["--backend", "native", "--model", "nonexistent-model",
                            "--embedding-socket", str(root / "missing.sock")]
            else:
                options += ["--backend", "http", "--endpoint", endpoint, "--model", "mock"]
            result = subprocess.run([str(binary), *options, *map(str, argv)], cwd=root,
                                    env=env, input=stdin, capture_output=True, text=True, timeout=60)
            if (result.returncode == 0) == fail:
                raise AssertionError(f"{' '.join(map(str, argv))}\n{result.stderr}\n{result.stdout}")
            return result

        def index(path=docs, database=db, prune=False):
            run("index", path, "--dim", DIM, "--chunk-chars", "180", "--overlap-lines", "0",
                *( ["--prune"] if prune else []), database=database)
            status = json.loads(run("stats", "--json", database=database).stdout)
            assert status["last_scan"]["errors"] == 0, status
            return status

        def search(query, mode=None, offline=False, database=db, top=10):
            hits = json.loads(run("search", query, *( ["--mode", mode] if mode else []),
                                  "--top", top, "--json", offline=offline, database=database).stdout)
            assert len({hit["path"] for hit in hits}) == len(hits), "duplicate files"
            assert len(hits) <= top and all(math.isfinite(hit["score"]) for hit in hits)
            return hits

        def only(hits, name):
            assert [hit["path"] for hit in hits] == [str(docs / name)], hits

        (docs / ".DS_Store").write_text("ignored Finder metadata")
        initial = index()
        assert initial["files"] == len(bodies) and initial["chunks"] > len(bodies), initial
        assert calls, "indexing did not use the mock embedding server"
        format_hits = search("txt", offline=True)
        assert len(format_hits) == len(bodies) and all(Path(hit["path"]).suffix == ".txt" and hit.get("chunk_index") is None for hit in format_hits)
        assert "--min-similarity" in run("search", "query", "--min-similarity", "NaN", fail=True).stderr
        initial_postings = check_storage(db, len(bodies), list(bodies.values()))
        with sqlite3.connect(db) as conn:
            assert dict(conn.execute("SELECT name,value FROM settings"))["dim"] == str(DIM)

        # Both explicit lexical mode and default hybrid exact lookup bypass
        # backend compatibility, model loading and a deliberately invalid socket.
        before = len(calls)
        for query in ("INV-2041", "K7Q2LM", "4815162342", "load_model", "loadModel", "Store::open"):
            for mode in (None, "hybrid", "lexical"):
                only(search(query, mode, offline=True), "a.txt")
        only(search("split_first split_last", offline=True), "f.txt")
        korean = search("메모리사용량", "lexical", offline=True)
        only(korean, "c.txt")
        assert korean[0]["start_line"] == korean[0]["end_line"] == 1
        only(search("a.txt", "path", offline=True), "a.txt")
        assert search("absent_lookup_999", "lexical", offline=True) == []
        assert search("INV-2041", top=0, offline=True) == []
        assert len(calls) == before, "lookup/lexical/path unexpectedly performed inference"

        # Explicit semantic mode still embeds even an exact identifier. General
        # default hybrid must agree with explicit hybrid and use HTTP inference.
        assert search("INV-2041", "semantic") == [], "weak semantic hits should not fill the result list"
        assert len(calls) == before + 1, "semantic mode skipped embedding"
        natural = "retry requests with exponential backoff and jitter"
        before = len(calls)
        assert search(natural) == search(natural, "hybrid")
        assert len(calls) == before + 2, "general hybrid query did not embed"

        # A file unchanged on disk but missing its lexical coverage marker must
        # regenerate postings without re-embedding or rewriting vector rows.
        preserved = vector_snapshot(db)
        with sqlite3.connect(db) as conn:
            conn.execute("PRAGMA foreign_keys=ON")
            doc_id = conn.execute("SELECT id FROM documents WHERE path=?", (str(docs / "a.txt"),)).fetchone()[0]
            conn.execute("DELETE FROM lexical_chunks WHERE doc_id=?", (doc_id,))
            conn.execute("DELETE FROM lexical_documents WHERE doc_id=?", (doc_id,))
        assert search("INV-2041", "lexical", offline=True) == []
        before = len(calls)
        repaired = index()
        assert repaired["last_scan"]["indexed"] == 0, repaired
        assert len(calls) == before and vector_snapshot(db) == preserved, "migration changed vectors/inferred"
        only(search("INV-2041", offline=True), "a.txt")
        check_storage(db, len(bodies), list(bodies.values()))
        index()
        assert len(calls) == before and vector_snapshot(db) == preserved, "repeat scan not idempotent"

        # Also emulate a legacy database with no lexical schema at all.
        legacy_db = root / "legacy.sqlite"
        with sqlite3.connect(db) as source, sqlite3.connect(legacy_db) as legacy:
            source.backup(legacy)
            legacy.executescript("DROP TRIGGER lexical_chunks_delete; DROP TABLE lexical_documents; "
                                 "DROP TABLE lexical_chunks; DROP TABLE chunk_fts;")
        assert search("INV-2041", "lexical", offline=True, database=legacy_db) == []
        with sqlite3.connect(legacy_db) as conn:
            assert conn.execute("SELECT count(*) FROM sqlite_schema WHERE name='chunk_fts'").fetchone()[0] == 0
        before = len(calls)
        index(database=legacy_db)
        assert len(calls) == before and vector_snapshot(legacy_db) == preserved
        only(search("INV-2041", offline=True, database=legacy_db), "a.txt")
        check_storage(legacy_db, len(bodies), list(bodies.values()))

        # Update must remove stale postings; prune must cascade to both mappings
        # and FTS without removing files outside the explicitly scanned scope.
        new_body = "Replacement Z9NEWTOKEN configuration for deadlines and retries.\n"
        (docs / "d.txt").write_text(new_body, encoding="utf-8")
        before = len(calls)
        updated = index()
        assert updated["last_scan"]["indexed"] == 1 and len(calls) > before, updated
        assert search("Z9OLDTOKEN", "lexical", offline=True) == []
        only(search("Z9NEWTOKEN", offline=True), "d.txt")
        outside = root / "outside.txt"
        outside.write_text("Outside scope Z7OUTSIDE remains indexed.\n", encoding="utf-8")
        index(outside)
        (docs / "e.txt").unlink()
        before = len(calls)
        pruned = index(prune=True)
        assert pruned["files"] == len(bodies), pruned
        assert len(calls) == before, "unchanged prune performed inference"
        assert search("Z8PRUNEME", "lexical", offline=True) == []
        assert search(str(docs / "e.txt"), "path", offline=True) == []
        assert search("Z7OUTSIDE", offline=True)[0]["path"] == str(outside)
        with sqlite3.connect(db) as conn:
            assert conn.execute("SELECT count(*) FROM documents WHERE path=?", (str(docs / "e.txt"),)).fetchone()[0] == 0
        check_storage(db, len(bodies), [*bodies.values(), new_body])

        # Composed query difference remains a file-set operation without a model.
        before = len(calls)
        spec = {"op": "difference", "args": [
            {"op": "path", "under": str(docs)}, {"op": "path", "query": "a.txt"}]}
        composed = json.loads(run("query", "--spec", "-", "--top", "100", "--json",
                                  stdin=json.dumps(spec), offline=True).stdout)
        assert {hit["path"] for hit in composed} == {str(docs / name) for name in bodies if name not in ("a.txt", "e.txt")}

        # File-only WQL avoids daemon startup and proves the negative-affine
        # distance formula on every indexed chunk, including a multichunk file.
        def wql(expression, fail=False):
            return run("q", expression + " level chunk top 100", "--json", fail=fail)

        positive = f"file({json.dumps(str(docs / 'a.txt'))})"
        negative = f"file({json.dumps(str(docs / 'b.txt'))})"
        pos = json.loads(wql(positive).stdout)
        neg = json.loads(wql(negative).stdout)
        avoided = json.loads(wql(f"avoid({positive},{negative})").stdout)
        distances = lambda result: {(hit["path"], hit["chunk_index"]): hit["distance"] for hit in result["hits"]}
        p_dist, n_dist, a_dist = map(distances, (pos, neg, avoided))
        assert p_dist.keys() == n_dist.keys() == a_dist.keys() and a_dist
        assert all(abs(a_dist[key] - (p_dist[key] + 1 - n_dist[key]) / 2) < 2e-5 for key in a_dist), "WQL avoid changed"
        invalid = wql(f"avoid({positive},any({positive},{negative}))", fail=True)
        assert "InvalidNegativeExpression" in invalid.stderr, invalid.stderr
        assert len(calls) == before, "file-only composition embedded"

        # Benchmark comparisons share query vectors and return bounded metrics.
        labels = [
            {"query": "INV-2041", "relevant": ["a.txt"]},
            {"query": "메모리사용량", "relevant": ["c.txt"]},
            {"query": "split_first split_last", "relevant": ["f.txt"]},
        ]
        queries = root / "queries.json"
        queries.write_text(json.dumps(labels, ensure_ascii=False), encoding="utf-8")
        before = len(calls)
        bench = json.loads(run("bench-search", "--queries", queries, "--top", "10", "--json").stdout)
        assert bench["top"] == 10 and bench["query_count"] == len(labels)
        assert len(calls) == before + len(labels), "benchmark re-embedded per variant"
        variants = {item["variant"]: item for item in bench["variants"]}
        assert set(variants) == {"semantic", "rrf", "lexical", "fts_cosine", "intent_cosine", "intent_zscore"}
        for item in variants.values():
            for metric in ("recall_at_k", "mrr_at_k", "ndcg_at_k"):
                assert math.isfinite(item[metric]) and 0 <= item[metric] <= 1, item
            assert 0 <= item["p50_search_ms"] <= item["p95_search_ms"]
            assert len(item["queries"]) == len(labels)
            assert all(len(q["hits"]) <= 10 and len(set(q["hits"])) == len(q["hits"]) for q in item["queries"])
        assert variants["lexical"]["mrr_at_k"] == variants["lexical"]["recall_at_k"] == 1.0
        assert not errors, errors

        print(json.dumps({
            "passed": True, "initial_chunks": initial["chunks"], "lexical_postings": initial_postings,
            "embedding_dimensions": DIM, "http_requests": len(calls),
            "default_hybrid_and_explicit_modes": True,
            "identifiers_without_model_or_valid_socket": True, "korean_substring": True,
            "cross_chunk_coverage": True, "updates_and_scoped_prune": True,
            "legacy_backfill_without_inference": True, "embedding_bytes_preserved": True,
            "contentless_no_raw_body": True, "negative_composition_and_wql_avoid": True,
            "bench_search_variants": True,
        }, indent=2))


if __name__ == "__main__":
    main()
