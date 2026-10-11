#!/bin/sh
set -eu
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec "$REPO_ROOT/.agents/skills/myvec-semantic-search/scripts/setup-model.sh" "$@"
