#!/usr/bin/env bash
# Response times of a running Trackside MCP server, as a client sees them.
#
#   MCP_URL=http://127.0.0.1:8000/mcp scripts/perf.sh --fixture      # local server on fixtures/demo.json
#   MCP_TOKEN=$(scripts/token.sh) scripts/perf.sh                     # the deployed endpoint
#   MCP_TOKEN=... scripts/perf.sh -n 50                               # 50 warm calls per tool
#
# Times the first request (initialize; on a Lambda that has just been deployed or has gone
# cold, this is the cold start), then N warm calls (default 20) to each read-only tool, and
# prints p50 and p95 per tool in milliseconds. Times are curl's time_total: network, API
# Gateway and the server together. The tool arguments are the smoke test's (override with
# DATE, VENUE, RACE, HORSE, PERSON, ROLE and RESULT_*). Needs bash, curl and python3.
#
# To measure a cold start on demand, recycle the function first (a configuration change
# retires its warm instances):
#   aws lambda update-function-configuration --function-name trackside-mcp --description "perf $(date +%s)"
#   aws lambda wait function-updated --function-name trackside-mcp
set -uo pipefail +B

MCP_URL="${MCP_URL:-https://mcp.racingaidataset.com.au/mcp}"
FIXTURE="" N=20
while [[ $# -gt 0 ]]; do
  case "$1" in
    --fixture) FIXTURE=1 ;;
    -n) N="${2:-}"; shift ;;
    *) echo "usage: scripts/perf.sh [--fixture] [-n CALLS_PER_TOOL]" >&2; exit 2 ;;
  esac
  shift
done
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "-n needs a positive number" >&2; exit 2; }
if [[ -n "$FIXTURE" ]]; then
  : "${DATE:=2026-10-17}" "${VENUE:=Caulfield}" "${RACE:=8}" "${HORSE:=sample stayer}"
  : "${RESULT_DATE:=2026-09-26}" "${RESULT_VENUE:=Flemington}" "${RESULT_RACE:=7}"
  : "${PERSON:=J. Example}" "${ROLE:=jockey}"
else
  : "${DATE:=2026-09-27}" "${VENUE:=Caulfield}" "${RACE:=8}" "${HORSE:=Jimmysstar}"
  : "${RESULT_DATE:=$DATE}" "${RESULT_VENUE:=$VENUE}" "${RESULT_RACE:=$RACE}"
  : "${PERSON:=Ethan Brown}" "${ROLE:=jockey}"
fi

AUTH=()
[[ -n "${MCP_TOKEN:-}" ]] && AUTH=(-H "Authorization: Bearer $MCP_TOKEN")

timed() { # timed <json body> -> prints "<http status> <seconds>"
  curl -sS -o /dev/null -w '%{http_code} %{time_total}' --max-time 60 "$MCP_URL" ${AUTH[@]+"${AUTH[@]}"} \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2025-11-25' \
    -d "$1"
}
tool_body() { # tool_body <tool> <arguments json>
  printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"%s","arguments":%s}}' "$1" "$2"
}

echo "Trackside response times against $MCP_URL ($N warm calls per tool)"
read -r code first < <(timed '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"perf","version":"1"}}}')
if [[ "$code" != 200 ]]; then
  echo "first request answered HTTP $code; is the server up, and does it need MCP_TOKEN?" >&2
  exit 1
fi

TOOLS=(
  "list_meetings|{\"date\":\"$DATE\"}"
  "get_race_card|{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}"
  "explain_race|{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}"
  "race_result|{\"venue\":\"$RESULT_VENUE\",\"race_number\":$RESULT_RACE,\"date\":\"$RESULT_DATE\"}"
  "horse_form|{\"horse\":\"$HORSE\"}"
  "jockey_or_trainer_stats|{\"name\":\"$PERSON\",\"role\":\"$ROLE\"}"
  "carnival_guide|{}"
  "my_stable|{\"date\":\"$DATE\"}"
)

samples=""
for entry in "${TOOLS[@]}"; do
  tool="${entry%%|*}" args="${entry#*|}"
  body=$(tool_body "$tool" "$args")
  timed "$body" >/dev/null  # one call to warm this tool's path
  for ((i = 0; i < N; i++)); do
    read -r code secs < <(timed "$body")
    [[ "$code" == 200 ]] || { echo "$tool answered HTTP $code" >&2; exit 1; }
    samples+="$tool $secs"$'\n'
  done
done

python3 - "$first" "$samples" <<'PY'
import sys
first, raw = float(sys.argv[1]), sys.argv[2]
by_tool = {}
for line in raw.splitlines():
    tool, secs = line.split()
    by_tool.setdefault(tool, []).append(float(secs) * 1000)

def pct(xs, p):  # nearest rank
    xs = sorted(xs)
    return xs[max(0, min(len(xs) - 1, round(p / 100 * len(xs) + 0.5) - 1))]

print(f"\nfirst request (initialize): {first * 1000:.0f} ms\n")
print(f"{'tool':<26}{'p50 ms':>9}{'p95 ms':>9}{'calls':>7}")
for tool, xs in by_tool.items():
    print(f"{tool:<26}{pct(xs, 50):>9.1f}{pct(xs, 95):>9.1f}{len(xs):>7}")
PY
