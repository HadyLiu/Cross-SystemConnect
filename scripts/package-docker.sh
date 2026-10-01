#!/usr/bin/env bash
# Compile Linux and Windows packages with the toolchain image.
# An existing image is reused. The image is built only when it is missing,
# or when --image is passed.
#   bash scripts/package-docker.sh
#   bash scripts/package-docker.sh --image
set -euo pipefail
cd "$(dirname "$0")/.."

image=cross-systemconnect-packager:latest
rebuild=0
if [[ "${1:-}" == "--image" ]]; then
  rebuild=1
elif [[ $# -ne 0 ]]; then
  echo "Usage: bash scripts/package-docker.sh [--image]" >&2
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

mkdir -p dist
docker run --rm \
  -v "$PWD:/src" \
  -v cross-systemconnect-cargo-registry:/usr/local/cargo/registry \
  -v cross-systemconnect-cargo-git:/usr/local/cargo/git \
  -v cross-systemconnect-target:/src/target \
  -w /src \
  -e "HOST_UID=$(id -u)" \
  -e "HOST_GID=$(id -g)" \
  "$image" \
  bash packaging/compile.sh

echo ""
echo "Packages are in dist/"
