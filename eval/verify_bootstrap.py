#!/usr/bin/env python3
"""Isolated skill bootstrap lifecycle proof; tool stubs avoid network/build costs."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib

REPO = Path(__file__).resolve().parents[1]
SKILL = REPO / ".agents/skills/weft-semantic-search"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", help="Also exercise compatible reuse with a real built CLI")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="weft-bootstrap-") as tmp:
        root = Path(tmp)
        skill = root / "standalone-skill"
        shutil.copytree(SKILL, skill)
        tools = root / "tools"
        tools.mkdir()
        # Keep the test independent of whichever weft is on the host PATH.
        fallback = tools / "weft"
        fallback.write_text("#!/bin/sh\nprintf 'weft 0.0.0\\n'\n")
        fallback.chmod(0o755)
        log = root / "calls.jsonl"
        cli = root / "fake-weft"
        cli.write_text(f"#!{sys.executable}\n" + '''import json, os, sys
with open(os.environ['TEST_LOG'], 'a') as f: f.write(json.dumps(['cli', *sys.argv[1:]])+'\\n')
if sys.argv[1:] == ['--version']: print('weft 1.0.0')
else: print(json.dumps({'started':True,'pid':123}))
''')
        cli.chmod(0o755)
        for name, body in {
            "cargo": '''import shutil
out=Path((os.environ.get('WEFT_INSTALL_ROOT') or os.environ['MYVEC_INSTALL_ROOT']))/'bin/weft'
out.parent.mkdir(parents=True,exist_ok=True)
shutil.copy2(os.environ['TEST_BINARY'],out)
''',
            "curl": '''Path(sys.argv[sys.argv.index('--output')+1]).write_bytes(b'cached-test-model')
''',
            "git": '''import shutil
if os.environ.get('TEST_GIT_FAIL'): sys.exit(1)
source=Path(os.environ['TEST_SOURCE']); dest=Path(sys.argv[-1]); dest.mkdir()
shutil.copy2(source/'Cargo.toml',dest/'Cargo.toml')
shutil.copytree(source/'scripts',dest/'scripts')
shutil.copytree(source/'.agents',dest/'.agents')
''',
        }.items():
            target = tools / name
            target.write_text(f"#!{sys.executable}\n" + '''import json, os, sys
from pathlib import Path
with open(os.environ['TEST_LOG'],'a') as f: f.write(json.dumps([Path(sys.argv[0]).name,*sys.argv[1:]])+'\\n')
''' + body)
            target.chmod(0o755)
        env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                   WEFT_INSTALL_ROOT=str(root / "runtime"),
                   WEFT_SKILL_ROOT=str(root / "skills"),
                   WEFT_DATA_ROOT=str(root / "data"),
                   XDG_CACHE_HOME=str(root / "cache"), TMPDIR=str(root),
                   TEST_LOG=str(log), TEST_BINARY=str(cli), TEST_SOURCE=str(REPO))
        env.pop("WEFT_SOURCE_ROOT", None)

        def run(*args, fail=False, extra=None):
            p = subprocess.run(["sh", str(skill / "scripts/bootstrap.sh"), *map(str,args)],
                               env=env | (extra or {}), capture_output=True,text=True,timeout=30)
            assert (p.returncode != 0) if fail else (p.returncode == 0), p.stderr
            return p

        def calls():
            return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

        legacy_skill = root / "skills/myvec-semantic-search"
        legacy_skill.mkdir(parents=True)
        (legacy_skill / "SKILL.md").write_text("old skill")
        run("--source", REPO)
        assert (legacy_skill / "SKILL.previous.md").read_text() == "old skill"
        assert (legacy_skill / "SKILL.md").read_bytes() == (REPO / ".agents/skills/myvec-semantic-search/SKILL.md").read_bytes()
        first = calls()
        assert sum(c[0] == "cargo" for c in first) == 1
        assert sum(c[0] == "curl" for c in first) == 1
        assert not any("index" in c for c in first)
        installed = root / "skills/weft-semantic-search"
        for resource in ("SKILL.md","scripts/bootstrap.sh","scripts/setup-model.sh","references/advanced.md"):
            assert (installed / resource).read_bytes() == (SKILL / resource).read_bytes()
        assert not (root / "data/index.sqlite").exists()
        alias = root / "runtime/bin/myvec"
        assert alias.stat().st_size < 256
        assert subprocess.check_output([str(alias), "--version"], env=env, text=True).strip() == "weft 1.0.0"
        # Legacy configuration still selects the same install, model and DB.
        before_legacy = len(calls())
        legacy = {"WEFT_INSTALL_ROOT":"", "WEFT_SKILL_ROOT":"", "WEFT_DATA_ROOT":"",
                  "MYVEC_INSTALL_ROOT":env["WEFT_INSTALL_ROOT"],
                  "MYVEC_SKILL_ROOT":env["WEFT_SKILL_ROOT"],
                  "MYVEC_DATA_ROOT":env["WEFT_DATA_ROOT"]}
        run(extra=legacy)
        assert not any(c[0] in ("git", "cargo", "curl") or "index" in c for c in calls()[before_legacy:])
        assert (root / "cache/myvec/embeddinggemma-2-Q8_0.gguf").exists()
        assert not (root / "cache/weft").exists()
        run()
        second = calls()[len(first):]
        assert not any(c[0] in ("git","cargo","curl") or "index" in c for c in second)
        scope = root / "requested-folder"
        scope.mkdir()
        run("--scan-root", scope)
        indices = [c for c in calls() if "index" in c]
        assert len(indices) == 1 and str(scope) in indices[0] and "--background" in indices[0]
        before = len(calls())
        run("--scan-root", root / "missing-folder", fail=True)
        assert len(calls()) == before
        # A copied standalone skill can fetch the source, and temporary build
        # checkout cleanup works on both success and private-repository failure.
        (root / "runtime/bin/weft").unlink()
        before = len(calls())
        run()
        fetched = calls()[before:]
        assert sum(c[0] == "git" for c in fetched) == 1
        assert sum(c[0] == "cargo" for c in fetched) == 1
        assert not any(c[0] == "curl" or "index" in c for c in fetched)
        assert not list(root.glob("weft-source.*"))
        (root / "runtime/bin/weft").unlink()
        before = len(calls())
        error = run(fail=True, extra={"TEST_GIT_FAIL":"1"})
        assert "requires GitHub access" in error.stderr
        assert not any(c[0] == "cargo" for c in calls()[before:])
        assert not list(root.glob("weft-source.*"))
        if args.binary:
            actual = Path(args.binary).resolve()
            version = tomllib.loads((REPO / "Cargo.toml").read_text())["package"]["version"]
            assert subprocess.check_output([str(actual), "--version"], text=True).strip() == f"weft {version}"
            shutil.copy2(actual, root / "runtime/bin/weft")
            before = len(calls())
            ready = run()
            assert "File explorer ready:" in ready.stdout
            assert len(calls()) == before
            assert not (root / "data/index.sqlite").exists()
        print(json.dumps({"passed":True,"legacy_alias_and_settings":True,"no_duplicate_model":True,"standalone_skill_installs_resources":True,
                          "repeat_reuses_binary_and_model":True,"no_implicit_scan":True,
                          "explicit_scope_only":True,"private_access_failure_reported":True,
                          "temporary_source_cleaned":True,"real_cli_reuse":bool(args.binary)},indent=2))


if __name__ == "__main__":
    main()
