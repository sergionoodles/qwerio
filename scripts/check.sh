#!/usr/bin/env bash
# The repository's checks: formatting, lints, and every test, including the
# PostgreSQL suite against a disposable local server.
#
# Usage: scripts/check.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

cargo fmt --all --check
cargo clippy --workspace --all-targets --features qwerio-core/contract-tests -- --deny warnings
scripts/with-test-postgres.sh cargo test --workspace --features qwerio-core/contract-tests
