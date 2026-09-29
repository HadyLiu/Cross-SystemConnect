#!/usr/bin/env bash
# Local CI/CD for Linux.
#   bash scripts/local-ci.sh
#   bash scripts/local-ci.sh --upload v0.1.0
set -euo pipefail

cd "$(dirname "$0")/.."

upload=0
version=""
if [[ "${1:-}" == "--upload" ]]; then
  upload=1
  version="${2:-}"
elif [[ $# -ne 0 ]]; then
  echo "Usage: bash scripts/local-ci.sh [--upload vX.Y.Z]" >&2
  exit 1
fi

failed=0
rows=()

add_result() {
  rows+=("$1|$2")
}

run_step() {
  local title="$1"
  shift
  if [[ "$failed" -ne 0 ]]; then
    add_result "$title" "skip"
    return
  fi
  echo ""
  echo "== $title"
  if "$@"; then
    add_result "$title" "pass"
  else
    failed=1
    add_result "$title" "fail"
    echo "$title failed"
  fi
}

printed=0
print_summary() {
  if [[ "$printed" -eq 1 ]]; then
    return
  fi
  printed=1
  echo ""
  printf "%-16s %s\n" "Check" "Result"
  printf "%-16s %s\n" "----------------" "------"
  local overall=pass row name result
  if [[ ${#rows[@]} -eq 0 ]]; then
    overall=fail
  fi
  for row in "${rows[@]}"; do
    name="${row%%|*}"
    result="${row#*|}"
    if [[ "$result" != "pass" ]]; then
      overall=fail
    fi
    printf "%-16s %s\n" "$name" "$result"
  done
  printf "%-16s %s\n" "overall" "$overall"
}
trap print_summary EXIT

if [[ "$upload" -eq 1 && ! "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Upload needs a version like v0.1.0" >&2
  add_result "version" "fail"
  print_summary
  exit 1
fi

run_step "fmt" cargo fmt --all -- --check
run_step "test" cargo test --workspace --all-targets
run_step "clippy" cargo clippy --workspace --all-targets -- -D clippy::correctness -D clippy::suspicious

if [[ "$failed" -ne 0 ]]; then
  add_result "audit" "skip"
elif ! command -v cargo-audit >/dev/null 2>&1; then
  echo ""
  echo "cargo-audit is missing. Install once: cargo install cargo-audit --locked" >&2
  failed=1
  add_result "audit" "fail"
else
  run_step "audit" cargo audit
fi

run_step "release build" cargo build --release -p csc-core -p csc-ui

if [[ "$failed" -ne 0 ]]; then
  add_result "package" "skip"
else
  echo ""
  echo "== package"
  mkdir -p dist
  ui="dist/Cross-SystemConnect-linux-x86_64"
  core="dist/Cross-SystemConnect-core-linux-x86_64"
  if cp -f target/release/Cross-SystemConnect "$ui" \
    && cp -f target/release/Cross-SystemConnect-core "$core" \
    && chmod +x "$ui" "$core"; then
    add_result "package" "pass"
    echo "Packaged:"
    echo "  $ui"
    echo "  $core"
  else
    failed=1
    add_result "package" "fail"
    echo "package failed"
  fi
fi

if [[ "$upload" -eq 1 ]]; then
  if [[ "$failed" -ne 0 ]]; then
    add_result "gh auth" "skip"
    add_result "upload" "skip"
  elif ! command -v gh >/dev/null 2>&1; then
    echo ""
    echo "GitHub CLI (gh) is missing. Install it and run: gh auth login" >&2
    failed=1
    add_result "gh auth" "fail"
    add_result "upload" "skip"
  else
    run_step "gh auth" gh auth status
    if [[ "$failed" -eq 0 ]]; then
      if [[ -n "$(git status --porcelain)" ]]; then
        echo ""
        echo "Working tree has uncommitted changes. The uploaded programs include this build; the GitHub tag points at the current commit."
      fi
      if gh release view "$version" >/dev/null 2>&1; then
        run_step "upload" gh release upload "$version" "$ui" "$core" --clobber
      else
        run_step "upload" gh release create "$version" --title "$version" --notes "Built locally." --target HEAD "$ui" "$core"
      fi
    else
      add_result "upload" "skip"
    fi
  fi
fi

print_summary
if [[ "$failed" -ne 0 ]]; then
  exit 1
fi
if [[ "$upload" -eq 0 ]]; then
  echo ""
  echo "Build only. To upload: bash scripts/local-ci.sh --upload v0.1.0"
fi
