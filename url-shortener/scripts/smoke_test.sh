#!/usr/bin/env bash
# Smoke test for a *running* url-shortener instance.
#
# Optional dev tooling (the service itself is Rust-only). Drives the live API
# end to end with curl: create a short link, follow the code back to the
# original URL (the "reverse" lookup), verify the redirect target, check
# metadata/hit count, then clean up. Exits non-zero on any failure.
#
# Usage:
#   ./scripts/smoke_test.sh                 # defaults to http://127.0.0.1:8080
#   BASE_URL=http://127.0.0.1:9000 ./scripts/smoke_test.sh
#
# Requires: bash, curl. (No jq — we parse with grep/sed so it runs anywhere.)

set -euo pipefail

BASE_URL="${BASE_URL:-http://127.0.0.1:8080}"
TARGET="${TARGET:-https://example.com/very/long/path?ref=smoke}"
pass=0; fail=0
ok()   { echo "  PASS: $1"; pass=$((pass+1)); }
bad()  { echo "  FAIL: $1"; fail=$((fail+1)); }

# Extract a top-level JSON string field value without jq.
json_field() { sed -n "s/.*\"$2\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" <<<"$1"; }

echo "== url-shortener smoke test against ${BASE_URL} =="

# 0. Liveness + readiness ----------------------------------------------------
echo "[0] health & readiness"
[ "$(curl -s -o /dev/null -w '%{http_code}' "${BASE_URL}/health")" = "200" ] \
  && ok "/health is 200" || bad "/health not 200"
[ "$(curl -s -o /dev/null -w '%{http_code}' "${BASE_URL}/health/ready")" = "200" ] \
  && ok "/health/ready is 200" || bad "/health/ready not 200"

# 1. Create (shorten) --------------------------------------------------------
echo "[1] POST /api/links (shorten)"
create_body="$(curl -s -X POST "${BASE_URL}/api/links" \
  -H 'Content-Type: application/json' \
  -d "{\"url\":\"${TARGET}\"}")"
echo "    response: ${create_body}"
code="$(json_field "${create_body}" code)"
[ -n "${code}" ] && ok "got code '${code}'" || { bad "no code returned"; echo "$pass passed, $fail failed"; exit 1; }
[ "${#code}" -eq 7 ] && ok "code is 7 chars" || bad "code length is ${#code}, expected 7"

# 2. Reverse lookup via the redirect ----------------------------------------
echo "[2] GET /${code} (reverse → original URL, expect 302)"
headers="$(curl -s -D - -o /dev/null "${BASE_URL}/${code}")"
status="$(printf '%s' "$headers" | sed -n 's/^HTTP\/[0-9.]* \([0-9]*\).*/\1/p' | head -1)"
location="$(printf '%s' "$headers" | tr -d '\r' | sed -n 's/^[Ll]ocation: //p' | head -1)"
[ "${status}" = "302" ] && ok "redirect status is 302" || bad "redirect status is ${status}, expected 302"
[ "${location}" = "${TARGET}" ] \
  && ok "Location matches original URL" \
  || bad "Location was '${location}', expected '${TARGET}'"

# 3. Metadata: hit was counted ----------------------------------------------
echo "[3] GET /api/links/${code} (metadata)"
meta="$(curl -s "${BASE_URL}/api/links/${code}")"
echo "    response: ${meta}"
grep -q '"hits":1' <<<"${meta}" && ok "hit count is 1 after one redirect" || bad "hit count not 1"

# 4. Delete + confirm gone ---------------------------------------------------
echo "[4] DELETE /api/links/${code}"
[ "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "${BASE_URL}/api/links/${code}")" = "204" ] \
  && ok "delete returns 204" || bad "delete not 204"
[ "$(curl -s -o /dev/null -w '%{http_code}' "${BASE_URL}/api/links/${code}")" = "404" ] \
  && ok "code is 404 after delete" || bad "code not 404 after delete"

echo "== done: ${pass} passed, ${fail} failed =="
[ "${fail}" -eq 0 ]
