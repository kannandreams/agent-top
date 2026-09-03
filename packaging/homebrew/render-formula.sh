#!/usr/bin/env bash
# Render the Homebrew formula for a released version.
#
#   render-formula.sh <version> <dist-dir> > agent-top.rb
#
# <dist-dir> holds one `<asset>.tar.gz.sha256` per target, as produced by the
# release workflow. A missing checksum is a hard error: a formula with a
# placeholder left in it would fail for users on that platform only, which is
# the worst way to find out.
set -euo pipefail

version="${1:?usage: render-formula.sh <version> <dist-dir>}"
dist="${2:?usage: render-formula.sh <version> <dist-dir>}"
tmpl="$(dirname "$0")/agent-top.rb.tmpl"

out="$(sed "s/@VERSION@/${version}/g" "$tmpl")"

for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  file="${dist}/agent-top-v${version}-${target}.tar.gz.sha256"
  [ -f "$file" ] || { echo "missing checksum: $file" >&2; exit 1; }
  # `shasum`/`sha256sum` write "<hash>  <filename>"; keep the hash.
  sha="$(awk '{print $1}' "$file")"
  [ "${#sha}" -eq 64 ] || { echo "not a sha256 in $file: $sha" >&2; exit 1; }
  placeholder="@SHA_$(echo "$target" | tr 'a-z-' 'A-Z_')@"
  out="${out//${placeholder}/${sha}}"
done

case "$out" in
  *@*@*) echo "unsubstituted placeholder left in formula" >&2; exit 1 ;;
esac

printf '%s\n' "$out"
