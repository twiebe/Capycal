#!/bin/sh
# Run end-to-end tests against a throwaway server on 127.0.0.1.
# CAPYCAL_TEST_SERVER picks it: radicale (default, installed into
# $CARGO_TARGET_DIR/radicale), stalwart or nextcloud (containers, need Docker).
# CAPYCAL_TEST_SUITE picks the tests: caldav_e2e (default) or jmap_e2e (only on
# stalwart).
set -eu

server="${CAPYCAL_TEST_SERVER:-radicale}"
suite="${CAPYCAL_TEST_SUITE:-caldav_e2e}"
port="${CAPYCAL_TEST_PORT:-5299}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/capycal-test-$server.XXXXXX")"
container="capycal-test-$server-$$"
password="capycal-test-password"

STALWART_IMAGE="stalwartlabs/stalwart:v1.0.2"
NEXTCLOUD_IMAGE="nextcloud:34.0.5-apache"

cleanup() {
    [ -z "${pid:-}" ] || kill "$pid" 2>/dev/null || true
    [ "$server" = radicale ] || docker rm -f "$container" >/dev/null 2>&1 || true
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# Waits until the server answers `url`, shows the log if it never does
wait_for() {
    tries=0
    until curl -s -f -o /dev/null "$1"; do
        tries=$((tries + 1))
        if [ "$tries" -ge "$2" ]; then
            echo "$server did not start" >&2
            [ "$server" = radicale ] && cat "$work/server.log" >&2 || docker logs "$container" >&2
            exit 1
        fi
        sleep 1
    done
}

start_radicale() {
    venv="${CARGO_TARGET_DIR:-$root/target}/radicale/venv"
    if [ ! -x "$venv/bin/radicale" ]; then
        python3 -m venv "$venv"
        "$venv/bin/pip" install --quiet radicale
    fi
    echo "test:test" > "$work/users"
    cat > "$work/config" <<CONFIG
[server]
hosts = 127.0.0.1:$port
[auth]
type = htpasswd
htpasswd_filename = $work/users
htpasswd_encryption = plain
[rights]
type = authenticated
[storage]
filesystem_folder = $work/data
[logging]
level = warning
CONFIG
    "$venv/bin/radicale" --config "$work/config" > "$work/server.log" 2>&1 &
    pid=$!
    wait_for "http://127.0.0.1:$port/.web/" 20
    url="http://127.0.0.1:$port/test/"
    user=test
    password=test
}

start_stalwart() {
    echo '{"@type":"RocksDb","path":"/var/lib/stalwart/"}' > "$work/config.json"
    chmod 755 "$work"
    chmod 644 "$work/config.json"
    docker run -d --rm --name "$container" \
        -e STALWART_RECOVERY_ADMIN="admin:$password" \
        -v "$work/config.json:/etc/stalwart/config.json:ro" \
        -p "127.0.0.1:$port:8080" "$STALWART_IMAGE" >/dev/null
    wait_for "http://127.0.0.1:$port/healthz/live" 60
    curl -s -f -u "admin:$password" -H 'Content-Type: application/json' \
        "http://127.0.0.1:$port/jmap/" -d '{
            "using": ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"],
            "methodCalls": [
                ["x:Domain/set", {"create": {"d": {"name": "capycal.test"}}}, "0"],
                ["x:Account/set", {"create": {"u": {
                    "@type": "User", "name": "test", "domainId": "#d",
                    "credentials": {"0": {"@type": "Password", "secret": "'"$password"'"}}
                }}}, "1"]
            ]
        }' > "$work/setup.json"
    grep -q '"created":{"u"' "$work/setup.json" || { cat "$work/setup.json" >&2; exit 1; }
    url="http://127.0.0.1:$port/dav/cal/test%40capycal.test/"
    export CAPYCAL_TEST_JMAP_URL="http://127.0.0.1:$port"
    user="test@capycal.test"
    # At most 4 requests of an account at once by default, so one test at a time
    test_threads=1
}

start_nextcloud() {
    docker run -d --rm --name "$container" \
        -e SQLITE_DATABASE=capycal \
        -e NEXTCLOUD_ADMIN_USER=test \
        -e NEXTCLOUD_ADMIN_PASSWORD="$password" \
        -e NEXTCLOUD_TRUSTED_DOMAINS=127.0.0.1 \
        -p "127.0.0.1:$port:80" "$NEXTCLOUD_IMAGE" >/dev/null
    wait_for "http://127.0.0.1:$port/status.php" 180
    # The tests create more calendars than a user may by default
    docker exec -u www-data "$container" php occ config:app:set dav rateLimitCalendarCreation --value=100000 >/dev/null
    docker exec -u www-data "$container" php occ config:app:set dav maximumCalendarsSubscriptions --value=-1 >/dev/null
    # An app password: a real password costs each request 200 ms of hashing
    password="$(docker exec -u www-data -e NC_PASS="$password" "$container" \
        php occ user:auth-tokens:add --password-from-env --name e2e test | grep -E '^[A-Za-z0-9]{20,}$')"
    url="http://127.0.0.1:$port/remote.php/dav/calendars/test/"
    user=test
    # SQLite takes one writer at a time
    test_threads=1
}

test_threads=""
case "$server" in
    radicale) start_radicale ;;
    stalwart) start_stalwart ;;
    nextcloud) start_nextcloud ;;
    *) echo "unknown CAPYCAL_TEST_SERVER: $server" >&2; exit 2 ;;
esac

export CAPYCAL_TEST_CALDAV_URL="$url"
export CAPYCAL_TEST_CALDAV_USER="$user"
export CAPYCAL_TEST_CALDAV_PASSWORD="$password"
cargo test --test "$suite" -- --ignored ${test_threads:+--test-threads="$test_threads"} "$@"
