#!/usr/bin/env bash
# Build the bundled userspace network helper beside the selected Pond executable.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="${1:-${repo_root}/target/release}"
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
cd "$repo_root/native/pondnet"
# go.mod pins the toolchain and go.sum pins every module. No system VPN installation.
CGO_ENABLED=0 go build -mod=readonly -trimpath -ldflags='-s -w' -o "$output_dir/pondnet" ./cmd/pondnet
printf 'Bundled network helper ready: %s/pondnet\n' "$output_dir"
