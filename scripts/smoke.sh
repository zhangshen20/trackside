#!/usr/bin/env bash
# Protocol-level smoke test for a running Trackside MCP server.
#
#   scripts/smoke.sh                                   # the live AWS endpoint, 27 Sep data
#   MCP_URL=http://127.0.0.1:8000/mcp scripts/smoke.sh --fixture   # local server on fixtures/demo.json
#
# The script sends no session id, so a local server must run stateless, the way Lambda does:
#   TRACKSIDE_STATELESS=1 cargo run -p trackside-mcp
#
# Checks initialize, tools/list (nine tools), a call to every tool, the error paths, and that no
# answer mentions betting, odds or a bookmaker. Needs bash, curl and python3. Exits non-zero on
# any failure. Set MCP_TOKEN to send "Authorization: Bearer $MCP_TOKEN" once OAuth is on.
set -uo pipefail

MCP_URL="${MCP_URL:-https://f534rx2db4.execute-api.ap-southeast-2.amazonaws.com/mcp}"
if [[ "${1:-}" == "--fixture" ]]; then
  : "${DATE:=2026-10-17}" "${VENUE:=Caulfield}" "${RACE:=8}" "${HORSE:=sample stayer}"
  : "${RESULT_DATE:=2026-09-26}" "${RESULT_VENUE:=Flemington}" "${RESULT_RACE:=7}"
  : "${PERSON:=J. Example}" "${ROLE:=jockey}"
else
  : "${DATE:=2026-09-27}" "${VENUE:=Caulfield}" "${RACE:=8}" "${HORSE:=Jimmysstar (NZ)}"
  : "${RESULT_DATE:=$DATE}" "${RESULT_VENUE:=$VENUE}" "${RESULT_RACE:=$RACE}"
  : "${PERSON:=Ethan Brown}" "${ROLE:=jockey}"
fi

PASS=0 FAIL=0
AUTH=()
[[ -n "${MCP_TOKEN:-}" ]] && AUTH=(-H "Authorization: Bearer $MCP_TOKEN")

rpc() { # rpc <json body>  -> prints the response body
  curl -sS --max-time 30 "$MCP_URL" "${AUTH[@]}" \
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
sys.exit(0 if eval(expr, {"r": r, "text": text, "banned": banned}) else 1)
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

res=$(rpc '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}')
check "initialize speaks 2025-11-25 with tools" "$res" \
  'r["result"]["protocolVersion"] == "2025-11-25" and "tools" in r["result"]["capabilities"]'

res=$(rpc '{"jsonrpc":"2.0","id":2,"method":"tools/list"}')
check "tools/list has the nine tools" "$res" \
  'sorted(t["name"] for t in r["result"]["tools"]) == sorted(["list_meetings","get_race_card","horse_form","explain_race","race_result","jockey_or_trainer_stats","follow_horse","my_stable","carnival_guide"])'
check "every tool has a description" "$res" 'all(len(t.get("description","")) > 40 for t in r["result"]["tools"])'

check "list_meetings"   "$(call list_meetings "{\"date\":\"$DATE\"}")" "$answered and r['result']['structuredContent']['meetings']"
check "get_race_card"   "$(call get_race_card "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" "$answered and r['result']['structuredContent']['card']['runners']"
check "explain_race"    "$(call explain_race "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" "$answered and r['result']['structuredContent']['found']"
check "race_result"     "$(call race_result "{\"venue\":\"$RESULT_VENUE\",\"race_number\":$RESULT_RACE,\"date\":\"$RESULT_DATE\"}")" "$answered and r['result']['structuredContent']['found']"
check "horse_form"      "$(call horse_form "{\"horse\":\"$HORSE\"}")" "$answered and r['result']['structuredContent'].get('form')"
check "jockey_or_trainer_stats" "$(call jockey_or_trainer_stats "{\"name\":\"$PERSON\",\"role\":\"$ROLE\"}")" "$answered and r['result']['structuredContent']['found']"
check "carnival_guide"  "$(call carnival_guide '{}')" "$answered and len(r['result']['structuredContent']['races']) >= 5"
check "follow_horse"    "$(call follow_horse "{\"horse\":\"$HORSE\"}")" "$answered"
check "my_stable"       "$(call my_stable "{\"date\":\"$DATE\"}")" "$answered"

# Voice text must read cleanly: no doubled spaces from empty fields, no "A  over".
check "race card text has no blank fields" "$(call get_race_card "{\"venue\":\"$VENUE\",\"race_number\":$RACE,\"date\":\"$DATE\"}")" \
  '"  " not in text and ", ," not in text'

# Error paths.
check "unknown tool is an error" "$(call place_bet '{}')" '"error" in r or r["result"].get("isError")'
check "missing argument is a tool error" "$(call get_race_card "{\"venue\":\"$VENUE\"}")" '"error" in r or r["result"].get("isError")'
check "unknown venue says not found" "$(call get_race_card "{\"venue\":\"Atlantis\",\"race_number\":1,\"date\":\"$DATE\"}")" \
  '"result" in r and r["result"]["structuredContent"]["found"] is False'
check "bad date is rejected" "$(call list_meetings '{"date":"27/09/2026"}')" '"error" in r or r["result"].get("isError")'
check "unknown method is -32601" "$(rpc '{"jsonrpc":"2.0","id":9,"method":"nope"}')" 'r["error"]["code"] == -32601'

echo
echo "$PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
