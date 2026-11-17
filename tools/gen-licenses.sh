#!/usr/bin/env bash
# Write the third-party license files with the full license texts that release archives,
# packages, container images, and `ferraus licenses` ship:
#
#   crates/ferraus-cli/THIRD-PARTY-LICENSES.md            ferraus and ferraus-plughost
#   crates/ferraus-plughost-vst3/THIRD-PARTY-LICENSES.md  the VST3 host
#   crates/ferraus-engrave/THIRD-PARTY-LICENSES.md        the LGPL engraving helper
#
# Rust crates come from cargo-about (about.toml, about.hbs) for every release target; the C and
# C++ libraries cargo cannot see are appended from licenses/ (ONNX Runtime, musl).
#
#   tools/gen-licenses.sh          rewrite the files
#   tools/gen-licenses.sh --check  fail if they are out of date (CI)
#
# Needs cargo-about 0.9.3: cargo install cargo-about --locked --features cli
set -euo pipefail
cd "$(dirname "$0")/.."
check=false
[ "${1:-}" = "--check" ] && check=true
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

about() { cargo about generate about.hbs -m "crates/$1/Cargo.toml" --locked -o "$2"; }

# append FILE TITLE TEXT: one more license section, fenced like cargo-about's
append() {
  { printf '\n## %s\n\n````\n' "$2"; cat "$3"; printf '\n````\n'; } >> "$1"
}

about ferraus-cli "$tmp/ferraus-cli.md"
append "$tmp/ferraus-cli.md" "ONNX Runtime 1.31.0 (MIT), statically linked into ferraus" licenses/onnxruntime/LICENSE
append "$tmp/ferraus-cli.md" "ONNX Runtime 1.31.0: notices for the libraries it includes" licenses/onnxruntime/ThirdPartyNotices.txt
append "$tmp/ferraus-cli.md" "musl libc (MIT), statically linked into the *-unknown-linux-musl builds" licenses/musl/COPYRIGHT

about ferraus-plughost-vst3 "$tmp/ferraus-plughost-vst3.md"

about ferraus-engrave "$tmp/ferraus-engrave.md"
append "$tmp/ferraus-engrave.md" "musl libc (MIT), statically linked into the *-unknown-linux-musl builds" licenses/musl/COPYRIGHT

stale=()
for app in ferraus-cli ferraus-plughost-vst3 ferraus-engrave; do
  # some upstream texts use CRLF; git stores LF, so normalize or --check never matches
  tr -d '\r' < "$tmp/$app.md" > "$tmp/$app.lf" && mv "$tmp/$app.lf" "$tmp/$app.md"
  out="crates/$app/THIRD-PARTY-LICENSES.md"
  if $check; then
    cmp -s "$tmp/$app.md" "$out" || stale+=("$out")
  else
    cp "$tmp/$app.md" "$out"
    echo "wrote $out ($(wc -c < "$out" | tr -d ' ') bytes)"
  fi
done
if [ ${#stale[@]} -gt 0 ]; then
  echo "out of date: ${stale[*]}" >&2
  echo "run tools/gen-licenses.sh and commit the result (Cargo.lock changed?)" >&2
  exit 1
fi
