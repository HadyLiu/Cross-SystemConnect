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
# Refresh the advisory database, then let cargo deny read that same checkout.
# A second shallow clone often dies when the TLS handshake is dropped, and the
# refresh itself fails the same way inside the image. Retry the refresh, and
# if a checkout is already present, audit it without fetching again.
advisory_db="${CARGO_HOME:-$HOME/.cargo}/advisory-db"
if [[ -d "$advisory_db/.git" ]]; then
  # Local CI mounts a checkout fetched on the host. Refreshing it again from
  # inside the image drops the TLS handshake or cannot see the git remote.
  cargo audit --no-fetch --stale
else
  audit_log="$(mktemp)"
  audit_status=1
  for attempt in 1 2 3; do
    set +e
    timeout 30 cargo audit >"$audit_log" 2>&1
    audit_status=$?
    set -e
    cat "$audit_log"
    if [[ "$audit_status" -eq 0 ]]; then
      break
    fi
    if [[ "$audit_status" -ne 124 ]] && ! grep -q "couldn't fetch advisory database" "$audit_log"; then
      rm -f "$audit_log"
      exit "$audit_status"
    fi
    echo "cargo audit could not refresh the advisory database (attempt ${attempt})" >&2
    sleep $((attempt * 2))
  done
  rm -f "$audit_log"
  if [[ "$audit_status" -ne 0 ]]; then
    exit "$audit_status"
  fi
fi
# cargo deny looks up a URL-named subdirectory. Point it at the checkout
# cargo audit just used instead of cloning the repository again.
deny_db="$(dirname "$advisory_db")/advisory-dbs/advisory-db-3157b0e258782691"
mkdir -p "$(dirname "$deny_db")"
if [[ ! -e "$deny_db" ]]; then
  ln -s "$advisory_db" "$deny_db"
fi
# --offline needs every crate in Cargo.lock, including ones for other targets
# that the Linux build above never downloaded.
cargo fetch --locked
cargo deny --locked --offline check
