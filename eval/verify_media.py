#!/usr/bin/env python3
"""Exercise the installed native/optional-media dispatch without model downloads.

The media bridge uses an in-test SentenceTransformer stand-in. This proves CLI
integration, incremental upgrades, kind filters, and index compatibility; it
does not establish real-model semantic quality.
"""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/release/weft")
    binary = str(Path(parser.parse_args().binary).resolve())
    with tempfile.TemporaryDirectory(prefix="weft-media-") as tmp:
        # macOS /var is a symlink to /private/var; the CLI canonicalizes roots.
        root = Path(tmp).resolve()
        fake = root / "fake_python_modules"
        fake.mkdir()
        (fake / "sentence_transformers.py").write_text('''
import os
class Embeddings:
    def __init__(self, vectors):
        self.vectors = vectors
    def tolist(self):
        return self.vectors
class SentenceTransformer:
    def __init__(self, model, **kwargs):
        assert model == "google/embeddinggemma-2", model
        with open(os.environ["WEFT_FAKE_LOAD_LOG"], "a") as handle:
            handle.write("load\\n")
    def encode(self, inputs, **kwargs):
        if not isinstance(inputs, list):
            inputs = [inputs]
        vectors = []
        for item in inputs:
            kind = next(iter(item)) if isinstance(item, dict) else "text"
            column = {"text": 0, "image": 1, "audio": 2, "video": 3}[kind]
            vector = [0.] * 768
            vector[column] = 1.
            vectors.append(vector)
        return Embeddings(vectors)
''')
        docs = root / "docs"
        docs.mkdir()
        (docs / "note.txt").write_text("A red bird among green trees")
        (docs / "bird.png").write_bytes(b"fake-png")
        (docs / "speech.wav").write_bytes(b"fake-wav")
        (docs / "scene.mp4").write_bytes(b"fake-mp4")
        (docs / "ignored.bin").write_bytes(b"non-indexable")
        db = root / "vectors.sqlite"
        log = root / "loads.txt"
        env = dict(os.environ,
                   PYTHONPATH=str(fake), WEFT_PYTHON=sys.executable,
                   WEFT_FAKE_LOAD_LOG=str(log))

        def run(*args, target=db):
            result = subprocess.run(
                [binary, "--db", str(target), *map(str, args)],
                cwd=root, env=env, text=True, capture_output=True, timeout=90,
            )
            assert result.returncode == 0, (args, result.stdout, result.stderr)
            return result.stdout

        def kinds():
            with sqlite3.connect(db) as conn:
                return dict(conn.execute("SELECT path,kind FROM documents"))

        # Mixed native Q8 text and full-model media share one DB.
        run("index", docs, "--media")
        first = kinds()
        assert sorted(first.values()) == ["audio", "image", "text", "video"]
        with sqlite3.connect(db) as conn:
            assert dict(conn.execute("SELECT name,value FROM settings"))["backend"] == "native"
            assert all(length == 772 for (length,) in conn.execute("SELECT length(embedding) FROM chunks"))
        assert log.read_text().count("load") == 1, "media model should load once"
        for media_kind in ("image", "audio", "video"):
            hits = json.loads(run("retrieve", "bird", "--kind", media_kind, "--json"))["hits"]
            assert len(hits) == 1, (media_kind, hits)
            assert hits[0]["kind"] == media_kind
            assert hits[0]["source_state"] == "media_embedding_unverified"
            assert "context" not in hits[0] or hits[0]["context"] is None

        with sqlite3.connect(db) as conn:
            media_vectors_before = dict(conn.execute(
                "SELECT documents.path, chunks.embedding FROM documents "
                "JOIN chunks ON chunks.doc_id = documents.id "
                "WHERE documents.kind IN ('image', 'audio', 'video')"))

        # Ordinary global scans preserve image/audio/video content embeddings.
        run("index", docs, "--all")
        assert all(kinds()[p] == kind for p, kind in first.items())
        assert kinds()[str(docs / "ignored.bin")] == "path"
        with sqlite3.connect(db) as conn:
            media_vectors_after = dict(conn.execute(
                "SELECT documents.path, chunks.embedding FROM documents "
                "JOIN chunks ON chunks.doc_id = documents.id "
                "WHERE documents.kind IN ('image', 'audio', 'video')"))
        assert media_vectors_after == media_vectors_before

        run("index", docs, "--media", "--all")
        assert json.loads(run("stats", "--json"))["last_scan"]["indexed"] == 0
        assert log.read_text().count("load") == 1
        (docs / "bird.png").write_bytes(b"updated-fake-png")
        run("index", docs, "--media", "--all")
        assert json.loads(run("stats", "--json"))["last_scan"]["indexed"] == 1
        assert log.read_text().count("load") == 2
        (docs / "scene.mp4").unlink()
        run("index", docs, "--media", "--all", "--prune")
        assert str(docs / "scene.mp4") not in kinds()

        # Pure Python backend reuses one full model for both text and media.
        log.write_text("")
        python_db = root / "python.sqlite"
        run("--backend", "python", "--model", "google/embeddinggemma-2",
            "--full-modal", "index", docs, "--media", target=python_db)
        assert log.read_text().count("load") == 1
        with sqlite3.connect(python_db) as conn:
            assert dict(conn.execute("SELECT name,value FROM settings"))["backend"] == "python"
            assert len(list(conn.execute("SELECT id FROM documents"))) == 3
        print(json.dumps({"native_media_integration": True,
                          "python_backend_model_reused": True,
                          "filename_rescan_preserves_media": True,
                          "incremental_media_refresh": True,
                          "prune": True}))


if __name__ == "__main__":
    main()
