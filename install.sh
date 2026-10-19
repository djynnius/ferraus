#!/bin/sh
# ferraus installer shim.
#
#   curl -fsSL https://raw.githubusercontent.com/djynnius/ferraus/main/install.sh | sh
#
# Resolves the latest (or $FERRAUS_VERSION) GitHub release and runs the installer that the
# release workflow generated and attached to it. The real installer verifies the archive's
# SHA-256 (and its GitHub artifact attestation when `gh` is available), installs to
# ~/.ferraus/bin (override with FERRAUS_INSTALL_DIR), and never asks for sudo.
set -eu

repo="djynnius/ferraus"
if [ -n "${FERRAUS_VERSION:-}" ]; then
  url="https://github.com/${repo}/releases/download/v${FERRAUS_VERSION#v}/ferraus-installer.sh"
else
  url="https://github.com/${repo}/releases/latest/download/ferraus-installer.sh"
fi

if command -v curl >/dev/null 2>&1; then
  fetch() { curl --proto '=https' --tlsv1.2 -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget --https-only -qO- "$1"; }
else
  echo "ferraus: need curl or wget to download the installer" >&2
  exit 1
fi

tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT INT TERM
if ! fetch "$url" > "$tmp"; then
  echo "ferraus: could not download $url" >&2
  exit 1
fi
sh "$tmp" "$@"
