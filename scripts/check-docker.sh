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

# cargo audit and cargo deny both clone this repository from inside the image.
# That clone often drops the TLS handshake. Fetch it on the host instead, and
# mount the checkout where cargo audit already looks. check.sh links cargo
# deny at the same checkout.
advisory_db="${XDG_CACHE_HOME:-$HOME/.cache}/cross-systemconnect/advisory-db"
if [[ ! -d "$advisory_db/.git" ]]; then
  echo "Fetching advisory database"
  mkdir -p "$(dirname "$advisory_db")"
  git clone --depth 1 https://github.com/RustSec/advisory-db.git "$advisory_db"
else
  git -C "$advisory_db" pull --ff-only || echo "advisory database refresh failed; using the copy on disk"
fi

docker run --rm \
  -v "$PWD:/src" \
  -v cross-systemconnect-cargo-registry:/usr/local/cargo/registry \
  -v cross-systemconnect-cargo-git:/usr/local/cargo/git \
  -v cross-systemconnect-target:/src/target \
  -v cross-systemconnect-cargo-tools:/opt/cargo-tools \
  -v "$advisory_db:/usr/local/cargo/advisory-db" \
  -w /src \
  "$image" \
  bash packaging/check.sh
