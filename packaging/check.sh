#!/usr/bin/env bash
# Unit tests and static checks. Run inside the toolchain image.
# clippy, rustfmt, cargo-audit, and cargo-deny belong in the image.
# The downloads below run only when an older image is missing them.
set -euo pipefail
cd /src

# Current images already contain these tools. Download only for an older image,
# and do not let that copy hide the binaries shipped in the image.
if [[ ! -x /usr/local/cargo/bin/cargo-audit || ! -x /usr/local/cargo/bin/cargo-deny ]]; then
  export PATH="/opt/cargo-tools/bin:${PATH}"
fi

if ! cargo clippy --version >/dev/null 2>&1 || ! cargo fmt --version >/dev/null 2>&1; then
  rustup component add clippy rustfmt
fi

AUDIT_VERSION=0.22.2
DENY_VERSION=0.20.2

install_release() {
  local bin="$1"
  local url="$2"
  local dest="/opt/cargo-tools/bin/${bin}"
  if [[ -x "$dest" ]]; then
    return
  fi
  mkdir -p /opt/cargo-tools/bin
  local tmp
  tmp="$(mktemp -d)"
  curl -fsSL "$url" | tar -xz -C "$tmp"
  local found
  found="$(find "$tmp" -type f -name "$bin" -print -quit)"
  if [[ -z "$found" ]]; then
    echo "release archive for ${bin} has no ${bin} binary" >&2
    exit 1
  fi
  install -m 0755 "$found" "$dest"
  rm -rf "$tmp"
}

if ! command -v cargo-audit >/dev/null 2>&1; then
  install_release cargo-audit \
    "https://github.com/rustsec/rustsec/releases/download/cargo-audit/v${AUDIT_VERSION}/cargo-audit-x86_64-unknown-linux-musl-v${AUDIT_VERSION}.tgz"
fi
if ! command -v cargo-deny >/dev/null 2>&1; then
  install_release cargo-deny \
    "https://github.com/EmbarkStudios/cargo-deny/releases/download/${DENY_VERSION}/cargo-deny-${DENY_VERSION}-x86_64-unknown-linux-musl.tar.gz"
fi

cargo test --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo fmt --all -- --check
cargo audit
cargo deny check
