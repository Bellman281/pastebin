#!/usr/bin/env bash
# Load-test the server. Measures throughput/latency and concurrent connections.
# The performance TARGETS (RPS, p95, concurrent conns) are yours to measure on
# real hardware — this script just drives the load; it does not assert numbers.
#
# Usage:
#   ./scripts/loadtest.sh [URL] [DURATION] [CONNECTIONS] [THREADS]
# Defaults:
#   URL=http://127.0.0.1:8080/  DURATION=30s  CONNECTIONS=10000  THREADS=8
#
# Requires `wrk` (https://github.com/wg/wrk). `ab` alternative shown below.
set -euo pipefail

URL="${1:-http://127.0.0.1:8080/}"
DURATION="${2:-30s}"
CONNECTIONS="${3:-10000}"
THREADS="${4:-8}"

echo "Start the server first, e.g.:"
echo "  MAX_CONNECTIONS=20000 APP_BIND_ADDR=127.0.0.1:8080 cargo run --release"
echo
echo "You may need a higher open-file limit for 10k+ connections:"
echo "  ulimit -n 65535"
echo

if command -v wrk >/dev/null 2>&1; then
  echo "== wrk: $CONNECTIONS conns / $THREADS threads / $DURATION =="
  exec wrk -t"$THREADS" -c"$CONNECTIONS" -d"$DURATION" --latency "$URL"
elif command -v ab >/dev/null 2>&1; then
  echo "== ab fallback: 100000 requests, concurrency $CONNECTIONS =="
  exec ab -n 100000 -c "$CONNECTIONS" -k "$URL"
else
  echo "Neither 'wrk' nor 'ab' found. Install one:"
  echo "  macOS:  brew install wrk"
  echo "  Debian: apt-get install wrk   (or apache2-utils for ab)"
  exit 1
fi
