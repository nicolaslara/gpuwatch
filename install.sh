#!/bin/sh
# Installs gpuwatch into an image: the binary, the Python start hook (a .pth
# file, so every Python interpreter in a GPU container starts the daemon) and
# gpuwatch-entrypoint for images whose command is not Python.
#   RUN curl -fsSL https://github.com/modal-projects/gpuwatch/releases/latest/download/install.sh | sh
set -eu
BASE="${GPUWATCH_BASE_URL:-https://github.com/modal-projects/gpuwatch/releases/latest/download}"
BIN=/usr/local/bin
PY="${GPUWATCH_PYTHON:-$(command -v python3 || command -v python || true)}"
# curl, else wget, else the image's Python: modal.Image.debian_slim ships none of the first two.
fetch() {
  case "$1" in file://*) cat "${1#file://}"; return;; esac
  if command -v curl >/dev/null; then curl -fsSL "$1"
  elif command -v wget >/dev/null; then wget -qO- "$1"
  else "$PY" -c 'import sys,urllib.request;sys.stdout.buffer.write(urllib.request.urlopen(sys.argv[1]).read())' "$1"
  fi
}
mkdir -p "$BIN"
fetch "$BASE/gpuwatch" > "$BIN/gpuwatch"
fetch "$BASE/entrypoint.sh" > "$BIN/gpuwatch-entrypoint"
chmod 755 "$BIN/gpuwatch" "$BIN/gpuwatch-entrypoint"
"$BIN/gpuwatch" --version >/dev/null 2>&1 || { echo "gpuwatch: the binary does not run here (needs x86_64 Linux with glibc 2.17+; Alpine/musl is not supported)" >&2; exit 1; }
if [ -n "$PY" ]; then
  SITE="$("$PY" -c 'import site; print(site.getsitepackages()[0])')"
  mkdir -p "$SITE/gpuwatch"
  fetch "$BASE/gpuwatch-init.py" > "$SITE/gpuwatch/__init__.py"
  fetch "$BASE/gpuwatch-boot.py" > "$SITE/gpuwatch/_boot.py"
  echo "import gpuwatch._boot" > "$SITE/gpuwatch_boot.pth"
  echo "gpuwatch: installed $BIN/gpuwatch and the start hook for $PY"
else
  echo "gpuwatch: installed $BIN/gpuwatch; no python found, set ENTRYPOINT [\"gpuwatch-entrypoint\"]"
fi
