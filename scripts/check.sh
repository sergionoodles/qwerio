#!/usr/bin/env bash
# The repository's checks: formatting, lints, and every test, including the
# PostgreSQL, MySQL, and MariaDB suites against disposable local servers.
# Requires PostgreSQL tools, OpenSSL, and Docker.
#
# Usage: scripts/check.sh
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

cargo fmt --all --check
cargo clippy --workspace --all-targets --features qwerio-core/contract-tests -- --deny warnings
scripts/with-test-postgres.sh scripts/with-test-mysql.sh mysql cargo test --workspace --features qwerio-core/contract-tests
scripts/with-test-mysql.sh mariadb cargo test -p qwerio-mysql --features qwerio-core/contract-tests
