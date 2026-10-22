#!/bin/sh
# Run the CalDAV end-to-end tests (tests/caldav_e2e.rs) against a throwaway
# Radicale server on 127.0.0.1. Radicale is installed into
# $CARGO_TARGET_DIR/radicale (default target/radicale).
set -eu

port="${CAPYCAL_TEST_PORT:-5299}"
root="$(cd "$(dirname "$0")/.." && pwd)"
venv="${CARGO_TARGET_DIR:-$root/target}/radicale/venv"
work="$(mktemp -d "${TMPDIR:-/tmp}/capycal-test-radicale.XXXXXX")"

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

"$venv/bin/radicale" --config "$work/config" > "$work/radicale.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true; rm -rf "$work"' EXIT INT TERM

for _ in 1 2 3 4 5 6 7 8 9 10; do
    if curl -s -o /dev/null "http://127.0.0.1:$port/"; then
        break
    fi
    sleep 0.5
done

CAPYCAL_TEST_CALDAV_URL="http://127.0.0.1:$port/test/" \
    cargo test --test caldav_e2e -- --ignored "$@"
