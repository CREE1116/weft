#!/bin/sh
# Install the native CLI + agent skill, and start the first desktop scan.
set -eu
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
INSTALL_ROOT="${WEFT_INSTALL_ROOT:-${MYVEC_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}}"
SKILL_ROOT="${WEFT_SKILL_ROOT:-${MYVEC_SKILL_ROOT:-$HOME/.agents/skills}}"
DATA_ROOT="${WEFT_DATA_ROOT:-${MYVEC_DATA_ROOT:-$HOME/.local/share/myvec}}"
FEATURES=""
if [ "$(uname -s)" = Darwin ]; then FEATURES=metal; fi
INDEX=1
MEDIA=0
SCAN_ROOT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --cpu) FEATURES="" ;;
    --cuda) FEATURES=cuda ;;
    --no-index) INDEX=0 ;;
    --media) MEDIA=1 ;;
    --scan-root) shift; SCAN_ROOT=${1:?--scan-root requires a path} ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done
"$REPO_ROOT/scripts/setup_native.sh"
if [ "$MEDIA" -eq 1 ]; then
  "$REPO_ROOT/scripts/setup_media.sh"
fi
if [ -n "$FEATURES" ]; then
  cargo install --path "$REPO_ROOT" --locked --force --root "$INSTALL_ROOT" --features "$FEATURES"
else
  cargo install --path "$REPO_ROOT" --locked --force --root "$INSTALL_ROOT"
fi
CLI="$INSTALL_ROOT/bin/weft"
# A tiny alias shares the same executable and database; no second engine.
cat > "$INSTALL_ROOT/bin/myvec.new.$$" <<'SHIM'
#!/bin/sh
exec "$(dirname -- "$0")/weft" "$@"
SHIM
chmod +x "$INSTALL_ROOT/bin/myvec.new.$$"
mv "$INSTALL_ROOT/bin/myvec.new.$$" "$INSTALL_ROOT/bin/myvec"
mkdir -p "$SKILL_ROOT/weft-semantic-search" "$DATA_ROOT"
SKILL="$SKILL_ROOT/weft-semantic-search/SKILL.md"
if [ -f "$SKILL" ] && ! cmp -s "$SKILL" "$REPO_ROOT/.agents/skills/weft-semantic-search/SKILL.md"; then
  cp "$SKILL" "$SKILL.previous.md"
fi
# Replace files atomically, including a bootstrap script that may be running.
install_resource() {
  if [ -f "$2" ] && cmp -s "$1" "$2"; then return; fi
  cp "$1" "$2.new.$$"
  mv "$2.new.$$" "$2"
}
install_resource "$REPO_ROOT/.agents/skills/weft-semantic-search/SKILL.md" "$SKILL"
mkdir -p "$SKILL_ROOT/weft-semantic-search/scripts" "$SKILL_ROOT/weft-semantic-search/references"
for RESOURCE in scripts/bootstrap.sh scripts/setup-model.sh references/advanced.md; do
  install_resource "$REPO_ROOT/.agents/skills/weft-semantic-search/$RESOURCE" "$SKILL_ROOT/weft-semantic-search/$RESOURCE"
done
# Upgrade an existing legacy skill without leaving its bootstrap stale.
if [ -d "$SKILL_ROOT/myvec-semantic-search" ]; then
  LEGACY="$SKILL_ROOT/myvec-semantic-search"
  if [ -f "$LEGACY/SKILL.md" ] && ! cmp -s "$LEGACY/SKILL.md" "$REPO_ROOT/.agents/skills/myvec-semantic-search/SKILL.md"; then
    cp "$LEGACY/SKILL.md" "$LEGACY/SKILL.previous.md"
  fi
  mkdir -p "$LEGACY/scripts"
  for RESOURCE in SKILL.md scripts/bootstrap.sh; do
    install_resource "$REPO_ROOT/.agents/skills/myvec-semantic-search/$RESOURCE" "$LEGACY/$RESOURCE"
  done
fi
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
  *) printf 'Add %s/bin to PATH to call weft from any folder.\n' "$INSTALL_ROOT" ;;
esac
