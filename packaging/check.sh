#!/usr/bin/env bash
# Unit tests and static checks. Run inside the toolchain image.
set -euo pipefail
cd /src

export PATH="/opt/cargo-tools/bin:${PATH}"

if [[ ! -x /opt/cargo-tools/bin/cargo-audit ]]; then
  cargo install cargo-audit --locked --root /opt/cargo-tools
fi
if [[ ! -x /opt/cargo-tools/bin/cargo-deny ]]; then
  cargo install cargo-deny --locked --root /opt/cargo-tools
fi

cargo test --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo fmt --all -- --check
cargo audit
cargo deny check
