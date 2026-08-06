#!/bin/sh
# MemMesh installer — downloads a prebuilt binary (no Rust toolchain needed).
#
#   curl -fsSL https://memmesh.ai/install.sh | sh
#
# Env overrides:
#   MEMMESH_VERSION=v0.1.2   install a specific tag (default: latest)
#   MEMMESH_BIN_DIR=~/.local/bin   install location (default: ~/.local/bin)
set -eu

REPO="ThinkfleetAI/memmesh"
BIN="memmesh"
VERSION="${MEMMESH_VERSION:-latest}"
BIN_DIR="${MEMMESH_BIN_DIR:-$HOME/.local/bin}"

err() { printf '\033[31merror:\033[0m %s\n' "$1" >&2; exit 1; }
info() { printf '\033[36m::\033[0m %s\n' "$1"; }

# --- detect platform -> Rust target triple ---
os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Darwin) os_triple="apple-darwin" ;;
  Linux)  os_triple="unknown-linux-gnu" ;;
  *) err "unsupported OS '$os' (need macOS or Linux). Build from source: https://github.com/$REPO" ;;
esac
case "$arch" in
  arm64|aarch64) arch_triple="aarch64" ;;
  x86_64|amd64)  arch_triple="x86_64" ;;
  *) err "unsupported architecture '$arch'" ;;
esac
target="${arch_triple}-${os_triple}"

# --- resolve download URL (GitHub Releases) ---
if [ "$VERSION" = "latest" ]; then
  url="https://github.com/$REPO/releases/latest/download/${BIN}-${target}.tar.gz"
else
  url="https://github.com/$REPO/releases/download/${VERSION}/${BIN}-${target}.tar.gz"
fi

command -v curl >/dev/null 2>&1 || err "curl is required"
command -v tar  >/dev/null 2>&1 || err "tar is required"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

info "downloading memmesh ($target, $VERSION)"
curl -fSL --proto '=https' --tlsv1.2 "$url" -o "$tmp/memmesh.tar.gz" \
  || err "download failed: $url  (has a release with binaries been published?)"

tar -xzf "$tmp/memmesh.tar.gz" -C "$tmp"
[ -f "$tmp/$BIN" ] || err "archive did not contain a '$BIN' binary"

mkdir -p "$BIN_DIR"
mv "$tmp/$BIN" "$BIN_DIR/$BIN"
chmod +x "$BIN_DIR/$BIN"
info "installed to $BIN_DIR/$BIN"

# --- PATH hint ---
case ":$PATH:" in
  *":$BIN_DIR:"*) : ;;
  *) printf '\n\033[33mAdd it to your PATH:\033[0m  export PATH="%s:$PATH"\n' "$BIN_DIR" ;;
esac

ver="$("$BIN_DIR/$BIN" --version 2>/dev/null || echo "$BIN")"
cat <<EOF

  ${ver} installed.

  Next — wire it into your AI tools (Claude Code, Cursor, Codex, Windsurf):

      $BIN_DIR/$BIN install

  That adds the MCP server + the auto-observe hook so your agent
  remembers across sessions. Then restart your tool.

EOF
