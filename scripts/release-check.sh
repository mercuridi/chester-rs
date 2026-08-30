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

echo "Updating changelog..."
git-cliff -o CHANGELOG.md --tag "$1"
