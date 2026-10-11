#!/bin/sh
# Prepare the file explorer on first skill use; indexing is explicitly scoped.
set -eu
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
INSTALL_ROOT="${MYVEC_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
SOURCE="${MYVEC_SOURCE_ROOT:-}"
FEATURE=""
REBUILD=0
SCAN_ROOT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --source) shift; SOURCE=${1:?--source requires a repository directory} ;;
    --cpu|--cuda) FEATURE=$1; REBUILD=1 ;;
    --scan-root) shift; SCAN_ROOT=${1:?--scan-root requires a directory} ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done
if [ -n "$SCAN_ROOT" ] && [ ! -d "$SCAN_ROOT" ]; then
  printf 'Scan root is not a directory: %s\n' "$SCAN_ROOT" >&2; exit 2
fi
CLI="$INSTALL_ROOT/bin/myvec"
if [ ! -x "$CLI" ]; then CLI=$(command -v myvec || true); fi
compatible() {
  [ -n "$CLI" ] && [ -x "$CLI" ] || return 1
  "$CLI" --version 2>/dev/null | awk '
    $1 == "myvec" && split($2, v, ".") >= 3 && (v[1] > 0 || v[2] >= 6) { ok=1 }
    END { exit !ok }'
}
if [ "$REBUILD" -eq 1 ] || ! compatible; then
  command -v cargo >/dev/null 2>&1 || {
    printf 'Rust Cargo is required to prepare myvec. Install Rust, then rerun this script.\n' >&2; exit 1;
  }
  if [ -z "$SOURCE" ] && [ -f "$SCRIPT_DIR/../../../../Cargo.toml" ] && [ -f "$SCRIPT_DIR/../../../../scripts/install.sh" ]; then
    SOURCE=$(CDPATH= cd -- "$SCRIPT_DIR/../../../.." && pwd)
  fi
  TMP_SOURCE=""
  trap '[ -z "$TMP_SOURCE" ] || rm -rf "$TMP_SOURCE"' EXIT
  trap 'exit 1' HUP INT TERM
  if [ -z "$SOURCE" ]; then
    command -v git >/dev/null 2>&1 || { printf 'Git is required to fetch myvec.\n' >&2; exit 1; }
    TMP_SOURCE=$(mktemp -d "${TMPDIR:-/tmp}/myvec-source.XXXXXX")
    if ! git clone --depth 1 https://github.com/CREE1116/DBforMyPC.git "$TMP_SOURCE/repo"; then
      printf 'Cannot fetch myvec. The private repository requires GitHub access; alternatively pass --source /path/to/DBforMyPC.\n' >&2
      exit 1
    fi
    SOURCE="$TMP_SOURCE/repo"
  fi
  [ -f "$SOURCE/Cargo.toml" ] && [ -f "$SOURCE/scripts/install.sh" ] || {
    printf 'Invalid myvec source directory: %s\n' "$SOURCE" >&2; exit 2;
  }
  # Explicitly suppress the desktop installer's whole-computer default scan.
  if [ -n "$FEATURE" ]; then
    "$SOURCE/scripts/install.sh" --no-index "$FEATURE"
  else
    "$SOURCE/scripts/install.sh" --no-index
  fi
  CLI="$INSTALL_ROOT/bin/myvec"
  compatible || { printf 'Installed myvec is incompatible; version 0.6.0 or newer is required.\n' >&2; exit 1; }
else
  "$SCRIPT_DIR/setup-model.sh"
fi
printf 'File explorer ready: %s\n' "$CLI"
if [ -n "$SCAN_ROOT" ]; then
  DATA_ROOT="${MYVEC_DATA_ROOT:-$HOME/.local/share/myvec}"
  "$CLI" --db "$DATA_ROOT/index.sqlite" index --all "$SCAN_ROOT" --background
fi
