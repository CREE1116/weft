#!/bin/sh
# Install the native CLI + agent skill, and start the first desktop scan.
set -eu
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
INSTALL_ROOT="${MYVEC_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
SKILL_ROOT="${MYVEC_SKILL_ROOT:-$HOME/.agents/skills}"
DATA_ROOT="${MYVEC_DATA_ROOT:-$HOME/.local/share/myvec}"
FEATURES=""
if [ "$(uname -s)" = Darwin ]; then FEATURES=metal; fi
INDEX=1
SCAN_ROOT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --cpu) FEATURES="" ;;
    --cuda) FEATURES=cuda ;;
    --no-index) INDEX=0 ;;
    --scan-root) shift; SCAN_ROOT=${1:?--scan-root requires a path} ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done
"$REPO_ROOT/scripts/setup_native.sh"
if [ -n "$FEATURES" ]; then
  cargo install --path "$REPO_ROOT" --locked --force --root "$INSTALL_ROOT" --features "$FEATURES"
else
  cargo install --path "$REPO_ROOT" --locked --force --root "$INSTALL_ROOT"
fi
CLI="$INSTALL_ROOT/bin/myvec"
mkdir -p "$SKILL_ROOT/myvec-semantic-search" "$DATA_ROOT"
SKILL="$SKILL_ROOT/myvec-semantic-search/SKILL.md"
if [ -f "$SKILL" ] && ! cmp -s "$SKILL" "$REPO_ROOT/.agents/skills/myvec-semantic-search/SKILL.md"; then
  cp "$SKILL" "$SKILL.previous.md"
fi
cp "$REPO_ROOT/.agents/skills/myvec-semantic-search/SKILL.md" "$SKILL"
printf 'Installed CLI: %s\nInstalled agent skill: %s\n' "$CLI" "$SKILL"
if [ "$INDEX" -eq 1 ]; then
  DB="$DATA_ROOT/index.sqlite"
  if [ -n "$SCAN_ROOT" ]; then
    "$CLI" --db "$DB" index --all "$SCAN_ROOT" --background
  else
    "$CLI" --db "$DB" index --all --background
  fi
  printf 'Progress: %s --db "%s" stats --json\n' "$CLI" "$DB"
  printf 'Index log: %s/index.log\n' "$DATA_ROOT"
fi
case ":$PATH:" in
  *":$INSTALL_ROOT/bin:"*) ;;
  *) printf 'Add %s/bin to PATH to call myvec from any folder.\n' "$INSTALL_ROOT" ;;
esac
