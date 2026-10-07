#!/usr/bin/env bash
# Which `agent-top serve --listen` address a Docker container can reach on
# Linux. Runs in CI on ubuntu-latest (.github/workflows/containers.yml), and
# the docs page "Receive telemetry" quotes what it asserts.
#
#   BIN=target/release/agent-top scripts/container-reachability.sh
#
# Each case starts `serve --listen` on one address, sends one span from a
# container with `curl`, and checks both the HTTP status and that the span
# reached the store.
set -euo pipefail

BIN=${BIN:-target/release/agent-top}
IMAGE=${IMAGE:-curlimages/curl:8.10.1}
WORK=$(mktemp -d)
export HOME="$WORK/home" XDG_DATA_HOME="$WORK/home" AGENT_TOP_NO_UPDATE_CHECK=1
mkdir -p "$HOME"
trap 'rm -rf "$WORK"' EXIT

BRIDGE=$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')
docker pull -q "$IMAGE" >/dev/null

span() {
  printf '{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"%s"}}]},"scopeSpans":[{"spans":[{"traceId":"%032d","spanId":"%016d","name":"chat","startTimeUnixNano":"1000000000","endTimeUnixNano":"2000000000","attributes":[{"key":"gen_ai.operation.name","value":{"stringValue":"chat"}},{"key":"gen_ai.usage.input_tokens","value":{"intValue":10}}]}]}]}]}' "$1" "$2" "$2"
}

failures=0
n=0
# name | --listen address | URL inside the container | expected | docker run flags
check() {
  local name=$1 listen=$2 url=$3 expect=$4
  shift 4
  n=$((n + 1))
  local db="$WORK/$n.db" port=${listen##*:}
  "$BIN" serve --interval 3600 --db "$db" --listen "$listen" >/dev/null 2>&1 &
  local pid=$!
  for _ in $(seq 1 100); do
    nc -z "${listen%:*}" "$port" 2>/dev/null && break
    sleep 0.1
  done
  local code
  code=$(docker run --rm "$@" "$IMAGE" -s -o /dev/null -w '%{http_code}' --max-time 5 \
    -H 'Content-Type: application/json' --data "$(span "case-$n" "$n")" "$url" || true)
  local stored
  # CSV rows end in CRLF.
  stored=$("$BIN" sql --csv --db "$db" "SELECT count(*) FROM sessions WHERE project = 'case-$n'" | tail -1 | tr -d '\r')
  kill "$pid" && wait "$pid" 2>/dev/null || true
  local got=unreachable
  [ "$code" = 200 ] && [ "$stored" = 1 ] && got=reachable
  printf '%-44s %-22s http %-4s stored %-2s %s\n' "$name" "$listen" "$code" "$stored" "$got (expected $expect)"
  [ "$got" = "$expect" ] || failures=$((failures + 1))
}

check "host network, loopback listener" 127.0.0.1:4318 http://127.0.0.1:4318/v1/traces reachable --network host
check "bridge, listener on the bridge gateway" "$BRIDGE:4319" http://host.docker.internal:4319/v1/traces reachable \
  --add-host=host.docker.internal:host-gateway
check "bridge, loopback listener" 127.0.0.1:4320 http://host.docker.internal:4320/v1/traces unreachable \
  --add-host=host.docker.internal:host-gateway

echo "bridge gateway: $BRIDGE"
exit "$failures"
