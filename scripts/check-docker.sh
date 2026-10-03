#!/usr/bin/env bash
# Unit tests and static checks with the toolchain image.
# An existing image is reused. The image is built only when it is missing,
# or when --image is passed.
#   bash scripts/check-docker.sh
#   bash scripts/check-docker.sh --image
set -euo pipefail
cd "$(dirname "$0")/.."

image=cross-systemconnect-packager:latest
rebuild=0
if [[ "${1:-}" == "--image" ]]; then
  rebuild=1
elif [[ $# -ne 0 ]]; then
  echo "Usage: bash scripts/check-docker.sh [--image]" >&2
  exit 1
elif ! docker image inspect "$image" >/dev/null 2>&1; then
  rebuild=1
fi

if [[ "$rebuild" -eq 1 ]]; then
  echo "Building toolchain image $image"
  docker build -f packaging/Dockerfile -t "$image" packaging
else
  echo "Using existing image $image"
fi

docker run --rm \
  -v "$PWD:/src" \
  -v cross-systemconnect-cargo-registry:/usr/local/cargo/registry \
  -v cross-systemconnect-cargo-git:/usr/local/cargo/git \
  -v cross-systemconnect-target:/src/target \
  -v cross-systemconnect-cargo-tools:/opt/cargo-tools \
  -w /src \
  "$image" \
  bash packaging/check.sh
