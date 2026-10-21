#!/usr/bin/env bash
# Packages a release build for running over SSH with --tui.
# The server needs Mesa (llvmpipe), a font, and the X keyboard libraries.
# See https://github.com/sergionoodles/gpui-tui/blob/main/GPUI-TUI-OVER-SSH.md.
#
# Usage: scripts/package-linux-over-ssh.sh <path to qwerio binary> <output directory>
set -euo pipefail

readonly MAX_GLIBC="2.35"

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <qwerio binary> <output directory>" >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
binary="$1"
output="$2"
arch="$(uname -m)"

if [[ "$arch" != "x86_64" ]]; then
  echo "SSH packages are built on x86_64 only (this machine is $arch)" >&2
  exit 1
fi

required_glibc="$(objdump -T "$binary" \
  | sed -nE 's/.*GLIBC_([0-9]+(\.[0-9]+)+).*/\1/p' \
  | sort -V | tail -n 1)"

if [[ -z "$required_glibc" ]]; then
  echo "Could not determine the binary's required glibc version" >&2
  exit 1
fi

highest_glibc="$(printf '%s\n' "$required_glibc" "$MAX_GLIBC" | sort -V | tail -n 1)"
if [[ "$highest_glibc" != "$MAX_GLIBC" ]]; then
  echo "Binary requires glibc $required_glibc, which exceeds the maximum $MAX_GLIBC" >&2
  exit 1
fi

version="$(cargo metadata --manifest-path "$root/Cargo.toml" --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "qwerio-desktop") | .version')"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

package="qwerio-linux-over-ssh-$version-$arch"
install -Dm755 "$binary" "$work/$package/qwerio"
install -Dm644 "$root/LICENSE" "$work/$package/LICENSE"

mkdir -p "$output"
tar -czf "$output/$package.tar.gz" -C "$work" "$package"
(
  cd "$output"
  sha256sum "$package.tar.gz" > "$package.tar.gz.sha256"
)
