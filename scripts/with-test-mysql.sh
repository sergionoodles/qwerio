#!/usr/bin/env bash
# Run checks against a disposable, TLS-enabled MySQL or MariaDB container.
# Usage: scripts/with-test-mysql.sh mysql|mariadb command [arguments...]
set -euo pipefail
engine="${1:?Choose mysql or mariadb}"
shift
case "$engine" in
    mysql) image="mysql:8.4" ;;
    mariadb) image="mariadb:11.4" ;;
    *) echo 'Choose mysql or mariadb' >&2; exit 1 ;;
esac
mysql_test_directory="$(mktemp -d -t qwerio-mysql.XXXXXX)"
mysql_test_container="qwerio-test-${engine}-${RANDOM}-$$"
cleanup() {
    docker rm -f "$mysql_test_container" >/dev/null 2>&1 || true
    rm -rf "$mysql_test_directory"
}
trap cleanup EXIT
chmod 755 "$mysql_test_directory"
openssl req -x509 -newkey rsa:2048 -nodes -days 1 -subj '/CN=Qwerio test CA' \
    -keyout "$mysql_test_directory/ca.key" -out "$mysql_test_directory/ca.pem" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=localhost' \
    -keyout "$mysql_test_directory/server.key" -out "$mysql_test_directory/server.csr" >/dev/null 2>&1
cat > "$mysql_test_directory/extensions" <<'CERT'
subjectAltName=DNS:localhost,IP:127.0.0.1
extendedKeyUsage=serverAuth
CERT
openssl x509 -req -in "$mysql_test_directory/server.csr" -CA "$mysql_test_directory/ca.pem" \
    -CAkey "$mysql_test_directory/ca.key" -CAcreateserial -days 1 \
    -extfile "$mysql_test_directory/extensions" -out "$mysql_test_directory/server.pem" >/dev/null 2>&1
chmod 644 "$mysql_test_directory/server.key"
export QWERIO_TEST_MYSQL_PASSWORD="$(openssl rand -hex 24)"
printf 'MYSQL_ROOT_PASSWORD=%s\nMYSQL_ROOT_HOST=%%\nMARIADB_ROOT_PASSWORD=%s\nMARIADB_ROOT_HOST=%%\n' \
    "$QWERIO_TEST_MYSQL_PASSWORD" "$QWERIO_TEST_MYSQL_PASSWORD" > "$mysql_test_directory/server.env"
chmod 600 "$mysql_test_directory/server.env"
docker run --rm -d --name "$mysql_test_container" \
    -p 127.0.0.1::3306 --env-file "$mysql_test_directory/server.env" \
    -v "$mysql_test_directory:/qwerio-test:ro" "$image" \
    --ssl-ca=/qwerio-test/ca.pem --ssl-cert=/qwerio-test/server.pem \
    --ssl-key=/qwerio-test/server.key >/dev/null
is_ready=false
for ((attempt=0; attempt<120; attempt++)); do
    if docker exec "$mysql_test_container" sh -c 'MYSQL_PWD="$MYSQL_ROOT_PASSWORD"; export MYSQL_PWD; if command -v mariadb >/dev/null; then mariadb --protocol=tcp -h127.0.0.1 -uroot -e "SELECT 1"; else mysql --protocol=tcp -h127.0.0.1 -uroot -e "SELECT 1"; fi' >/dev/null 2>&1; then
        is_ready=true
        break
    fi
    sleep 1
done
if [[ "$is_ready" != true ]]; then
    echo 'Disposable test server did not become ready' >&2
    exit 1
fi
export QWERIO_TEST_MYSQL_ENGINE="$engine"
export QWERIO_TEST_MYSQL_HOST=127.0.0.1
export QWERIO_TEST_MYSQL_PORT="$(docker port "$mysql_test_container" 3306/tcp | cut -d: -f2)"
export QWERIO_TEST_MYSQL_CA="$mysql_test_directory/ca.pem"
"$@"
