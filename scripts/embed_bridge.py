"""Optional full-multimodal EmbeddingGemma 2 backend, one JSON request per line."""
import argparse
import json
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", default="google/embeddinggemma-2")
    parser.add_argument("--text-only", action="store_true")
    args = parser.parse_args()
    try:
        from sentence_transformers import SentenceTransformer
        config_kwargs = {"vision_config": None, "audio_config": None} if args.text_only else {}
        model = SentenceTransformer(args.model, config_kwargs=config_kwargs)
    except Exception as exc:
        print(f"Cannot load {args.model}: {exc}", file=sys.stderr)
        raise SystemExit(1)

    for line in sys.stdin:
        try:
            request = json.loads(line)
            inputs = request["inputs"]
            role = request.get("role", "document")
            if not inputs:
                vectors = []
            elif all(i["kind"] == "text" for i in inputs):
                texts = [i["value"] for i in inputs]
                if role == "query":
                    vectors = model.encode(texts, prompt_name="SearchQuery").tolist()
                else:
                    vectors = model.encode(texts, prompt_name="Document").tolist()
            else:
                items = [{i["kind"]: i["value"]} for i in inputs]
                vectors = model.encode(items).tolist()
            print(json.dumps({"embeddings": vectors}), flush=True)
        except Exception as exc:
            print(json.dumps({"error": str(exc)}), flush=True)


if __name__ == "__main__":
    main()
