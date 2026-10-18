#!/usr/bin/env bash
# Runs a command against a disposable local PostgreSQL server.
#
# The cluster lives under target/test-postgres, listens on localhost only,
# requires a generated password over TCP, and serves TLS with a throwaway CA.
# Integration tests create and drop their own databases inside it. Never point
# the QWERIO_TEST_POSTGRES_* variables at a shared or production server.
#
# Usage: scripts/with-test-postgres.sh cargo test -p qwerio-postgres
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
data="${QWERIO_TEST_PG_DIR:-$root/target/test-postgres}"
port="${QWERIO_TEST_PG_PORT:-55432}"
user="qwerio_test"

for tool in initdb pg_ctl openssl; do
  command -v "$tool" >/dev/null || { echo "missing required tool: $tool" >&2; exit 1; }
done

create_certificates() {
  local dir="$1"
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Qwerio Test CA" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -keyout "$dir/ca.key" -out "$dir/ca.crt" 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" \
    -keyout "$dir/server.key" -out "$dir/server.csr" 2>/dev/null
  printf 'subjectAltName=DNS:localhost\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' \
    > "$dir/server.ext"
  openssl x509 -req -in "$dir/server.csr" -CA "$dir/ca.crt" -CAkey "$dir/ca.key" -CAcreateserial \
    -days 3650 -extfile "$dir/server.ext" -out "$dir/server.crt" 2>/dev/null
  chmod 600 "$dir/server.key"
}

if [ ! -f "$data/PG_VERSION" ]; then
  mkdir -p "$data"
  password="$(openssl rand -hex 16)"
  printf '%s' "$password" > "$data.password"
  chmod 600 "$data.password"
  initdb --pgdata="$data" --username="$user" --pwfile="$data.password" --auth-local=trust \
    --auth-host=scram-sha-256 --encoding=UTF8 --no-locale >/dev/null
  create_certificates "$data"
  {
    echo "ssl = on"
    echo "ssl_cert_file = 'server.crt'"
    echo "ssl_key_file = 'server.key'"
    echo "listen_addresses = 'localhost'"
    echo "unix_socket_directories = '$data'"
    echo "max_connections = 200"
    echo "fsync = off"
  } >> "$data/postgresql.conf"
fi

pg_ctl --pgdata="$data" --log="$data/server.log" --options="-p $port" --wait start >/dev/null
trap 'pg_ctl --pgdata="$data" --mode=fast --wait stop >/dev/null' EXIT

export QWERIO_TEST_POSTGRES_HOST="localhost"
export QWERIO_TEST_POSTGRES_PORT="$port"
export QWERIO_TEST_POSTGRES_USER="$user"
export QWERIO_TEST_POSTGRES_PASSWORD="$(cat "$data.password")"
export QWERIO_TEST_POSTGRES_CA="$data/ca.crt"

"$@"
