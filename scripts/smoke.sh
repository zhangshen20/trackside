#!/usr/bin/env bash
# Protocol-level smoke test for a running Trackside MCP server.
#
#   scripts/smoke.sh                                   # the live AWS endpoint, 27 Sep data
#   MCP_URL=http://127.0.0.1:8000/mcp scripts/smoke.sh --fixture   # local server on fixtures/demo.json
#   MCP_URL=http://127.0.0.1:8000/mcp scripts/smoke.sh --fixture --wait-for 60
#                                                      # same, but first wait up to 60 s for the server
#
# The script sends no session id, so a local server must run stateless, the way Lambda does, and
# the fixture checks expect its clock fixed before the fixture's Caulfield Cup card:
#   TRACKSIDE_STATELESS=1 TRACKSIDE_TODAY=2026-10-14 cargo run -p trackside-mcp
#
# Checks initialize, tools/list (the core tools; the server has twelve), a call to ten of the
# twelve tools (set_home_state and forget_me are left alone), the error paths, and that no
# answer mentions betting, odds or a bookmaker. Needs bash, curl and python3. Exits non-zero on
# any failure. With --wait-for N it first polls /healthz next to MCP_URL for up to N seconds, so
# it can be started in the same breath as the server; CI runs it that way.
#
# Against a server with OAuth on, pass a token with both scopes and the script also checks the
# 401 challenge and the metadata documents:
#   MCP_TOKEN=$(scripts/token.sh) scripts/smoke.sh
set -uo pipefail +B  # macOS bash 3.2 brace-expands {"a":1,"b":2} inside "$(...)"

MCP_URL="${MCP_URL:-https://mcp.racingaidataset.com.au/mcp}"
FIXTURE="" WAIT_FOR=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --fixture) FIXTURE=1 ;;
    --wait-for) WAIT_FOR="${2:-}"; shift ;;
    *) echo "usage: scripts/smoke.sh [--fixture] [--wait-for SECONDS]" >&2; exit 2 ;;
  esac
  shift
done
[[ "$WAIT_FOR" =~ ^[0-9]+$ ]] || { echo "--wait-for needs a number of seconds" >&2; exit 2; }
if [[ -n "$FIXTURE" ]]; then
  : "${DATE:=2026-10-17}" "${VENUE:=Caulfield}" "${RACE:=8}" "${HORSE:=sample stayer}"
  : "${RESULT_DATE:=2026-09-26}" "${RESULT_VENUE:=Flemington}" "${RESULT_RACE:=7}"
  : "${PERSON:=J. Example}" "${ROLE:=jockey}"
else
  : "${DATE:=2026-09-27}" "${VENUE:=Caulfield}" "${RACE:=8}" "${HORSE:=Jimmysstar}"
  : "${RESULT_DATE:=$DATE}" "${RESULT_VENUE:=$VENUE}" "${RESULT_RACE:=$RACE}"
  : "${PERSON:=Ethan Brown}" "${ROLE:=jockey}"
fi
# Perth is always behind the eastern states, so a Western Australian listener hears the race
# card's start in their own clock on any date and the label can be checked.
: "${HOME_STATE:=WA}"

PASS=0 FAIL=0
AUTH=()
[[ -n "${MCP_TOKEN:-}" ]] && AUTH=(-H "Authorization: Bearer $MCP_TOKEN")

rpc() { # rpc <json body>  -> prints the response body
  curl -sS --max-time 30 "$MCP_URL" ${AUTH[@]+"${AUTH[@]}"} \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H 'MCP-Protocol-Version: 2025-11-25' \
    -d "$1"
}

# check <label> <response> <python expression over r (the parsed response)>
check() {
  if python3 - "$2" "$3" <<'PY'
import json, re, sys
body, expr = sys.argv[1], sys.argv[2]
if body.startswith("event:") or body.startswith("data:"):  # SSE framing from a stateful server
    body = [l[5:] for l in body.splitlines() if l.startswith("data:") and l[5:].strip()][-1]
r = json.loads(body)
text = " ".join(c.get("text", "") for c in r.get("result", {}).get("content", []))
banned = re.compile(r"\b(odds|bet|bets|betting|wager|bookmaker|sportsbet|ladbrokes|tab|bet365|neds|pointsbet)\b", re.I)
sys.exit(0 if eval(expr, {"r": r, "text": text, "banned": banned, "re": re}) else 1)
PY
  then PASS=$((PASS + 1)); echo "ok    $1"
  else FAIL=$((FAIL + 1)); echo "FAIL  $1"; echo "      ${2:0:400}"
  fi
}

call() { # call <tool> <arguments json>
  rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}
answered='"result" in r and not r["result"].get("isError") and text and not banned.search(text)'

echo "Trackside smoke test against $MCP_URL"

if (( WAIT_FOR > 0 )); then  # a server started a moment ago may not be listening yet
  HEALTH="${MCP_URL%/mcp}/healthz" DEADLINE=$((SECONDS + WAIT_FOR))
  until curl -sf --max-time 2 "$HEALTH" >/dev/null 2>&1; do
    if (( SECONDS >= DEADLINE )); then echo "FAIL  $HEALTH did not answer within ${WAIT_FOR}s"; exit 1; fi
    sleep 1
  done
  echo "server answered on $HEALTH after ${SECONDS}s"
fi

res=$(rpc '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}')
check "initialize speaks 2025-11-25 with tools" "$res" \
  'r["result"]["protocolVersion"] == "2025-11-25" and "tools" in r["result"]["capabilities"]'

res=$(rpc '{"jsonrpc":"2.0","id":2,"method":"tools/list"}')
check "tools/list has the core tools" "$res" \
  'set(["list_meetings","get_race_card","horse_form","explain_race","race_result","jockey_or_trainer_stats","follow_horse","my_stable","carnival_guide"]) <= set(t["name"] for t in r["result"]["tools"])'
TOOLS="$res"
check "every tool has a description" "$res" 'all(len(t.get("description","")) > 40 for t in r["result"]["tools"])'
# The MCP App (servers since the app change): tools point at it and it reads as HTML.
if grep -q 'ui://trackside/race-card.html' <<<"$TOOLS"; then
  check "resources/read returns the MCP App" \
    "$(rpc '{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"ui://trackside/race-card.html"}}')" \
    'r["result"]["contents"][0]["mimeType"] == "text/html;profile=mcp-app" and "ui/initialize" in r["result"]["contents"][0]["text"]'
fi

check "list_meetings"   "$(call list_meetings "{\"date\":\"$DATE\"}")" "$answered and r['result']['structuredContent']['meetings']"
check "get_race_card"   "$(call get_race_card "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" "$answered and r['result']['structuredContent']['card']['runners']"
check "explain_race"    "$(call explain_race "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" "$answered and r['result']['structuredContent']['found']"
check "race_result"     "$(call race_result "{\"venue\":\"$RESULT_VENUE\",\"race_number\":$RESULT_RACE,\"date\":\"$RESULT_DATE\"}")" "$answered and r['result']['structuredContent']['found']"
check "horse_form"      "$(call horse_form "{\"horse\":\"$HORSE\"}")" "$answered and r['result']['structuredContent'].get('form')"
check "jockey_or_trainer_stats" "$(call jockey_or_trainer_stats "{\"name\":\"$PERSON\",\"role\":\"$ROLE\"}")" "$answered and r['result']['structuredContent']['found']"
check "carnival_guide"  "$(call carnival_guide '{}')" "$answered and len(r['result']['structuredContent']['races']) >= 5"
check "follow_horse"    "$(call follow_horse "{\"horse\":\"$HORSE\"}")" "$answered"
check "my_stable"       "$(call my_stable "{\"date\":\"$DATE\"}")" "$answered"
# The stable looks ahead (fixture only; the server must run with TRACKSIDE_TODAY=2026-10-14, the
# Wednesday before the fixture's Caulfield Cup, or any real day before it).
if [[ -n "$FIXTURE" ]]; then
  check "my_stable names the next run" "$(call my_stable '{"date":"2026-10-14"}')" \
    "$answered and 'runs on Saturday in the Caulfield Cup' in text and r['result']['structuredContent']['upcoming'][0]['race_number'] == 8"
fi
# Listener memory (servers since the memory change): unfollowing what was just followed.
if grep -q '"unfollow_horse"' <<<"$TOOLS"; then
  check "unfollow_horse" "$(call unfollow_horse "{\"horse\":\"$HORSE\"}")" \
    "$answered and r['result']['structuredContent']['found']"
fi

# Start times in the listener's own clock (servers since the time-zone change).
if grep -q '"set_home_state"' <<<"$TOOLS"; then
  check "set_home_state" "$(call set_home_state "{\"state\":\"$HOME_STATE\"}")" "$answered"
  check "race card speaks the listener's time" "$(call get_race_card "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" \
    "$answered and ('Perth time' in text or r['result']['structuredContent']['jump']['venue_state'] == '$HOME_STATE' or not r['result']['structuredContent']['card']['start_local'])"
fi

# Voice text must read cleanly: no doubled spaces from empty fields, no "A  over".
check "race card text has no blank fields" "$(call get_race_card "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" \
  '"  " not in text and ", ," not in text'

check "explain_race has no gaps" "$(call explain_race "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" \
  '"  " not in text and "A  over" not in text'
check "horse_form reads cleanly" "$(call horse_form "{\"horse\":\"$HORSE\"}")" \
  '"  " not in text and "0 from 0" not in text and "Jump track" not in text and not re.search(r"\bat [A-Z]{4}\b", text)'
check "follow_horse refuses an unknown horse" "$(call follow_horse '{"horse":"Not A Real Horse Zzq"}')" \
  'r["result"]["structuredContent"]["found"] is False'

# Telemetry: /healthz reports what is served as JSON (servers since the telemetry change; the
# fixture server always does), and a warm race card answers well inside a voice turn.
health=$(curl -sS --max-time 10 "${MCP_URL%/mcp}/healthz")
if [[ -n "$FIXTURE" || "$health" == "{"* ]]; then
  check "healthz returns JSON with ok true" "$health" \
    'r.get("ok") is True and isinstance(r.get("meetings"), int) and "snapshot_date" in r and "uptime_s" in r'
fi
call get_race_card "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}" >/dev/null
warm=$(curl -sS -o /dev/null -w '%{time_total}' --max-time 30 "$MCP_URL" ${AUTH[@]+"${AUTH[@]}"} \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"get_race_card\",\"arguments\":{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}}}")
if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) < 3 else 1)' "${warm:-99}"; then
  PASS=$((PASS + 1)); echo "ok    warm get_race_card in ${warm}s"
else
  FAIL=$((FAIL + 1)); echo "FAIL  warm get_race_card took ${warm:-?}s (limit 3s)"
fi

# Error paths.
check "unknown tool is an error" "$(call place_bet '{}')" '"error" in r or r["result"].get("isError")'
check "missing argument is a tool error" "$(call get_race_card "{\"venue\":\"$VENUE\"}")" '"error" in r or r["result"].get("isError")'
check "unknown venue says not found" "$(call get_race_card "{\"venue\":\"Atlantis\",\"race_number\":1,\"date\":\"$DATE\"}")" \
  '"result" in r and r["result"]["structuredContent"]["found"] is False'
check "bad date is rejected" "$(call list_meetings '{"date":"27/09/2026"}')" '"error" in r or r["result"].get("isError")'
check "unknown method is -32601" "$(rpc '{"jsonrpc":"2.0","id":9,"method":"nope"}')" 'r["error"]["code"] == -32601'

# Leave the smoke subject's profile as it was found: no home state or stable in storage.
if grep -q '"forget_me"' <<<"$TOOLS"; then
  check "forget_me clears the smoke subject" "$(call forget_me '{}')" "$answered and r['result']['structuredContent']['forgotten'] is True"
fi

# OAuth, when the caller brought a token.
status() { curl -sS -o /dev/null -w '%{http_code}' --max-time 30 "$@"; }
if [[ -n "${MCP_TOKEN:-}" ]]; then
  BASE="${MCP_URL%/mcp}"
  got=$(status -X POST "$MCP_URL" -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}')
  if [[ "$got" == 401 ]]; then PASS=$((PASS + 1)); echo "ok    no token gets 401"; else FAIL=$((FAIL + 1)); echo "FAIL  no token gets 401 (got $got)"; fi
  check "protected-resource metadata" "$(curl -sS "$BASE/.well-known/oauth-protected-resource")" \
    'r["resource"] == "'"$MCP_URL"'" and r["authorization_servers"]'
  check "authorization-server metadata lists S256" "$(curl -sS "$BASE/.well-known/oauth-authorization-server")" \
    '"S256" in r["code_challenge_methods_supported"] and r["token_endpoint"].endswith("/oauth2/token")'
fi

echo
echo "$PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
