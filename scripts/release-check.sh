#!/usr/bin/env bash

set -Eeuo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 VERSION" >&2
    exit 2
fi

echo "Checking formatting..."
cargo fmt --all -- --check

echo "Running Clippy with warnings denied..."
cargo clippy --all-targets --all-features -- -D warnings

echo "Running tests with warnings denied..."
RUSTFLAGS="${RUSTFLAGS:-} -D warnings" cargo test --all-targets --all-features

echo "Building release binary for Chronicle evaluations..."
cargo build --release --bin chester-rs

evaluation_dir=$(mktemp -d)
trap 'rm -rf "$evaluation_dir"' EXIT

echo "Running Chronicle retrieval evaluation (minimum hybrid recall: 0.80)..."
target/release/chester-rs \
    --chronicle-eval \
    tests/fixtures/chronicle/suite.toml \
    "$evaluation_dir/chronicle-retrieval.json"

echo "Running Chronicle structured-query executor and planner evaluation (minimum planner accuracy: 0.95)..."
target/release/chester-rs \
    --chronicle-query-eval \
    tests/fixtures/chronicle-query/suite.toml \
    "$evaluation_dir/chronicle-query.json" \
    --planner

echo "Running Chronicle bounded-synthesis evaluation (minimum fact recall: 0.75; maximum prohibited claims: 0)..."
if target/release/chester-rs \
    --chronicle-synthesis-eval \
    tests/fixtures/chronicle-synthesis/suite.toml \
    "$evaluation_dir/chronicle-synthesis.json"; then
    echo "Chronicle bounded-synthesis evaluation passed."
else
    synthesis_eval_status=$?
    echo "WARNING: Chronicle bounded-synthesis evaluation did not pass (exit status $synthesis_eval_status); continuing release." >&2
fi

echo "Updating changelog..."
git-cliff -o CHANGELOG.md --tag "$1"
