#!/usr/bin/env bash
# Rehearse CI on this machine. Does not push, tag, or publish a Release.
#
#   bash scripts/ci-local.sh                 # the check job
#   bash scripts/ci-local.sh --build         # check, then compile and SHA256SUMS
#   bash scripts/ci-local.sh --image         # rebuild the toolchain image first
#   bash scripts/ci-local.sh --image --build
set -euo pipefail
cd "$(dirname "$0")/.."

rebuild=0
build=0
for arg in "$@"; do
  case "$arg" in
    --image) rebuild=1 ;;
    --build) build=1 ;;
    -h|--help)
      cat <<'EOF'
Usage: bash scripts/ci-local.sh [--image] [--build]

  (no flags)  test, clippy, fmt, cargo audit, cargo deny — the CI check job
  --build     after check, compile Linux and Windows and write dist/SHA256SUMS
  --image     rebuild the toolchain image from packaging/Dockerfile first

Nothing is pushed and no GitHub Release is created.
To run the real check workflow on GitHub: gh workflow run ci.yml
A Release is published only by pushing a v* tag.
EOF
      exit 0
      ;;
    *)
      echo "Usage: bash scripts/ci-local.sh [--image] [--build]" >&2
      exit 1
      ;;
  esac
done

image_args=()
if [[ "$rebuild" -eq 1 ]]; then
  image_args=(--image)
fi

echo "== CI check (packaging/check.sh) =="
bash scripts/check-docker.sh "${image_args[@]}"

if [[ "$build" -eq 1 ]]; then
  echo "== CI build (packaging/compile.sh, no Release) =="
  bash scripts/package-docker.sh
  (
    cd dist
    sha256sum Cross-SystemConnect-linux-x86_64.tar.gz \
      Cross-SystemConnect-windows-x86_64.zip > SHA256SUMS
  )
  echo "SHA256SUMS:"
  cat dist/SHA256SUMS
fi

echo "Local CI rehearsal finished. Nothing was pushed or published."
