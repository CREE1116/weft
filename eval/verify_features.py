#!/usr/bin/env python3
"""Optional native end-to-end proof. Requires the cached text GGUF, no server."""
import argparse
import json
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/release/myvec")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="myvec-features-") as tmp:
        root = Path(tmp).resolve()
        docs = root / "docs"
        docs.mkdir()
        db = root / "index.sqlite"

        def run(*argv, stdin=None):
            p = subprocess.run([binary, "--db", str(db), *map(str, argv)],
                               input=stdin, capture_output=True, text=True, timeout=60)
            if p.returncode:
                raise AssertionError(p.stderr)
            return p.stdout

        def stats():
            return json.loads(run("stats", "--json"))

        def wait(predicate, seconds=20):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                s = stats()
                if predicate(s):
                    return s
                time.sleep(0.1)
            raise AssertionError("worker state transition timed out")

        retry = docs / "retry.md"
        retry.write_text("\n".join(
            f"Retry policy section {i}: retry failed requests with exponential backoff and jitter."
            for i in range(70)))
        timeout = docs / "timeout.md"
        timeout.write_text("Network timeout and retry policies must coordinate their deadlines.")
        (docs / "trip.txt").write_text("Kyoto travel budget and hotel reservations.")
        run("index", docs)
        initial = stats()
        assert initial["files"] == 3
        retry.write_text(retry.read_text() + "\nNew retry exception: never retry invalid credentials.")
        run("index", docs)
        refreshed = stats()
        assert refreshed["last_scan"]["reused_chunks"] > 0
        assert refreshed["last_scan"]["indexed"] == 1

        # Metadata and file-to-file search must succeed without any model file.
        paths = json.loads(run("--model-file", root / "missing.gguf", "search", "retry.md", "--mode", "path", "--json"))
        assert len(paths) == 1 and paths[0]["path"] == str(retry)
        related = json.loads(run("--model-file", root / "missing.gguf", "file2file", retry, "--json"))
        assert related and all(h["path"] != str(retry) for h in related)
        spec = {"op": "intersection", "args": [
            {"op": "file", "path": str(retry), "top": 3},
            {"op": "path", "query": "timeout.md", "under": str(docs)}]}
        hits = json.loads(run("query", "--spec", "-", "--json", stdin=json.dumps(spec)))
        assert len(hits) == 1 and hits[0]["path"] == str(timeout)
        semantic = json.loads(run("search", "retry failed network requests", "--json"))
        hybrid = json.loads(run("search", "retry", "--mode", "hybrid", "--json"))
        assert semantic and hybrid
        assert len({h["path"] for h in hybrid}) == len(hybrid)

        multi = json.loads(run("retrieve", "network retry", "--also", "request deadlines",
                               "--avoid", "travel", "--under", docs, "--kind", "text",
                               "--chunks", "--context-chars", "120", "--json"))
        assert multi["evaluated_chunks"] == refreshed["chunks"]
        assert len(multi["criteria"]) == 3
        assert 0 < multi["context_chars_used"] <= 120
        assert sum(len(h.get("context", "")) for h in multi["hits"]) == multi["context_chars_used"]
        assert all(len(h["criterion_distances"]) == 3 for h in multi["hits"])
        file_only = json.loads(run("--model-file", root / "missing.gguf", "retrieve",
                                   "--file", retry, "--avoid-file", docs / "trip.txt",
                                   "--context-chars", "200", "--json"))
        assert len(file_only["hits"]) == 1
        assert file_only["hits"][0]["path"] == str(timeout)
        assert file_only["hits"][0]["source_state"] == "verified"
        for level in ("chunk", "file", "folder"):
            result = json.loads(run("--model-file", root / "missing.gguf", "retrieve",
                                    "--file", retry, "--avoid-file", docs / "trip.txt",
                                    "--under", docs, "--kind", "text", "--level", level,
                                    "--context-chars", "200", "--json"))
            assert result["level"] == level
            assert result["evaluated_candidates"] == 1
            assert result["evaluated_chunks"] == 1
            assert len(result["hits"]) == 1
            hit = result["hits"][0]
            assert hit["member_files"] == hit["member_chunks"] == 1
            if level == "chunk":
                assert hit["source_state"] == "verified"
            else:
                assert hit["source_state"] == "aggregate_not_checked"
                assert "context" not in hit and "chunk_index" not in hit
                assert result["context_chars_used"] == 0
                assert hit["path"] == str(docs if level == "folder" else timeout)
        folder_text = json.loads(run("retrieve", "network retry", "--level", "folder", "--json"))
        assert folder_text["hits"][0]["member_files"] == 3
        assert folder_text["hits"][0]["member_chunks"] == refreshed["chunks"]
        saved = timeout.read_text()
        timeout.write_text(saved + " changed after indexing")
        stale = json.loads(run("--model-file", root / "missing.gguf", "retrieve",
                               "--file", retry, "--avoid-file", docs / "trip.txt",
                               "--context-chars", "200", "--json"))
        assert stale["hits"][0]["source_state"] == "changed_since_index"
        assert "context" not in stale["hits"][0]
        timeout.write_text(saved)

        # Recompute reused embeddings to prove cache reuse preserves vectors.
        with sqlite3.connect(db) as conn:
            cached = conn.execute("SELECT c.ordinal,c.embedding FROM chunks c JOIN documents d ON d.id=c.doc_id WHERE d.path=? ORDER BY ordinal", (str(retry),)).fetchall()
            plan = [row[-1] for row in conn.execute("EXPLAIN QUERY PLAN SELECT d.id,d.path,d.kind,c.start_line,c.end_line,c.embedding FROM chunks c JOIN documents d ON d.id=c.doc_id ORDER BY c.doc_id,c.ordinal")]
            assert not any("TEMP B-TREE" in line for line in plan)
        fresh_db = root / "fresh.sqlite"
        p = subprocess.run([binary, "--db", str(fresh_db), "index", str(retry)],capture_output=True,text=True,timeout=60)
        assert p.returncode == 0, p.stderr
        with sqlite3.connect(fresh_db) as conn:
            fresh = conn.execute("SELECT ordinal,embedding FROM chunks ORDER BY ordinal").fetchall()
        assert cached == fresh

        # Cooperative stop must not publish half of a file or prune other files.
        slow = docs / "slow.txt"
        slow.write_text("\n".join(f"Unique topic {i}: background indexing document number {i}. " * 12 for i in range(150)))
        pid = None
        try:
            started = json.loads(run("index", slow, "--background", "--duty-cycle", "5", "--prune"))
            pid = started["pid"]
            wait(lambda s: s["indexer_running"])
            run("control", "pause")
            paused = wait(lambda s: s["last_scan"]["status"] == "paused")
            time.sleep(0.5)
            assert stats()["chunks"] == paused["chunks"]
            run("control", "limit", "--duty-cycle", "20")
            assert stats()["index_duty_cycle"] == "20"
            run("control", "resume")
            wait(lambda s: s["last_scan"]["status"] == "running")
            run("control", "stop")
            stopped = wait(lambda s: not s["indexer_running"])
            assert stopped["last_scan"]["status"] == "stopped"
            assert stopped["files"] == refreshed["files"]
            assert stopped["chunks"] == refreshed["chunks"]
            pid = None
            started = json.loads(run("index", retry, "--background"))
            pid = started["pid"]
            completed = wait(lambda s: s["last_scan"]["pid"] == pid and not s["indexer_running"])
            assert completed["last_scan"]["status"] == "complete"
            assert completed["index_duty_cycle"] == "25"
            assert completed["last_scan"]["indexed"] == 0
            pid = None
        finally:
            if pid is not None:
                import os
                import signal
                try:
                    os.kill(pid, signal.SIGINT)
                except ProcessLookupError:
                    pass
        print(json.dumps({"passed": True, "initial_chunks": initial["chunks"],
                          "reused_chunks": refreshed["last_scan"]["reused_chunks"],
                          "cache_vectors_equal_fresh": True,
                          "pause_resume_limit_stop": True,
                          "background_default_budget": 25,
                          "multi_basis_full_scan_and_context_budget": True,
                          "file_only_retrieval_without_model": True,
                          "stale_evidence_withheld": True,
                          "chunk_file_folder_levels_without_model": True,
                          "atomic_stop_preserved_index": True,
                          "ordered_scan_uses_no_temp_sort": True}, indent=2))


if __name__ == "__main__":
    main()
