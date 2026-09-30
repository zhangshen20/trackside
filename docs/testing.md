# Testing Trackside

Three layers, cheapest first. Run layer 1 after every change, layer 2 before recording anything, layer 3 after every redeploy.

Live endpoint (open until OAuth lands): `https://f534rx2db4.execute-api.ap-southeast-2.amazonaws.com/mcp`

The deployed snapshot covers 22 to 29 September 2026, so "today" returns no meetings; pass a date inside the snapshot (the examples use Caulfield on Sunday 27 September).

## 1. Protocol check

### Scripted: `scripts/smoke.sh`

```sh
scripts/smoke.sh                                   # live endpoint, 27 Sep data
TRACKSIDE_STATELESS=1 cargo run -p trackside-mcp   # in another terminal
MCP_URL=http://127.0.0.1:8000/mcp scripts/smoke.sh --fixture
```

It checks `initialize` (protocol 2025-11-25), `tools/list` (the nine tools, each with a description), one call to every tool, the error paths (unknown tool, missing argument, unknown venue, bad date, unknown method) and that no answer mentions betting, odds or a bookmaker. It also fails when voice text has holes from empty fields (`"the Manikato Stakes,  over 1200 metres"`). Override the probe with `DATE`, `VENUE`, `RACE`, `HORSE`, `PERSON`, `ROLE` and `RESULT_*` when the snapshot moves. Set `MCP_TOKEN` once OAuth is on.

The script sends no `Mcp-Session-Id`, so a local server must run stateless, the way Lambda does.

Note that it calls `follow_horse`, which writes to the follow list. Until that list is per-user, the write is visible to every caller of the same Lambda instance.

### Interactive: MCP Inspector

```sh
npx @modelcontextprotocol/inspector
```

Transport **Streamable HTTP**, URL as above, Connect. Then:

- **Tools > List Tools**: nine tools; read each description as the model will. It should say when to use the tool in a fan's words.
- Run each tool with the arguments below and look at both the text (what Alexa+ will say) and `structuredContent` (what a screen or a simulator draws).
- **Server Notifications / History**: no errors on connect.

| Tool | Arguments |
| --- | --- |
| `list_meetings` | `{"date":"2026-09-27","state":"VIC"}` |
| `get_race_card` | `{"venue":"Caulfield","race_number":8,"date":"2026-09-27"}` |
| `explain_race` | same as above |
| `race_result` | same as above |
| `horse_form` | `{"horse":"Jimmysstar (NZ)"}` |
| `jockey_or_trainer_stats` | `{"name":"Ethan Brown","role":"jockey"}` |
| `follow_horse` then `my_stable` | `{"horse":"Extragalactic"}`, then `{"date":"2026-09-27"}` |
| `carnival_guide` | `{}` |

Once OAuth is on, Inspector's **Auth** panel runs the authorization-code + PKCE flow against the server's protected-resource metadata; that is the quickest way to prove discovery works before Alexa+ sees it.

## 2. Natural-language check with a real model

Protocol tests prove the tools answer; they don't prove a model picks the right tool, fills the arguments a fan would imply, or turns the answer into something worth saying aloud. Alexa+ testing is US-only, so use Claude as a stand-in model:

- **Claude.ai** (web or desktop): Settings > Connectors > Add custom connector, paste the `/mcp` URL. Enable it in a new chat.
- **Claude Code**: `claude mcp add --transport http trackside <url>`.

Ask these, one per chat, and judge the answer as if it were spoken:

1. "What racing was on in Victoria last Sunday?" (date resolution; `list_meetings` with a state)
2. "Who won the Manikato Stakes at Caulfield on the 27th, and who ran home fastest?" (`race_result`; last 600)
3. "How has Jimmysstar been going?" (no "(NZ)": does the model or the server find the horse?)
4. "Explain the Manikato to me like I've never watched racing." (`explain_race`; is the Group 1 status there?)
5. "How's Ethan Brown riding this spring?" (`jockey_or_trainer_stats` with a period)
6. "When's the Melbourne Cup and what's on before it?" (`carnival_guide`)
7. "Follow Extragalactic. Is it running anywhere?" (`follow_horse` then `my_stable`)
8. "Who should I back in race 8?" and "What are the odds for Giga Kick?" (the model must decline betting; no tool should ever return a price)

For each, note: right tool first time, arguments correct, answer short enough to say in one breath, source named, nothing about odds. Record anything odd in `docs/friction-log.md`.

## 3. Regression checklist after each redeploy

Run on the Mac that deploys, straight after `deploy/deploy.sh` prints the URL:

- [ ] `scripts/smoke.sh` passes against the printed URL (set `DATE` etc. to the new snapshot's range).
- [ ] `list_meetings` for the last date of the snapshot returns meetings (the snapshot really updated).
- [ ] `race_result` for a race run on the last day has a fastest last 600 m (sectionals joined).
- [ ] `horse_form` for that race's winner lists the win as its latest start (results fold into form).
- [ ] No bookmaker brand in any meeting or race name for the new dates (`list_meetings` text).
- [ ] Cold start is acceptable: `time scripts/smoke.sh` after a deploy; the first call should finish well inside Alexa+'s turn budget.
- [ ] CloudWatch: no `ERROR` lines for the Lambda since the deploy (`aws logs tail /aws/lambda/<function> --since 10m`).
- [ ] With OAuth on: an unauthenticated `initialize` returns 401, `/.well-known/oauth-protected-resource` resolves, and the smoke test passes with `MCP_TOKEN`.

## What Alexa+ will require that this server does not do yet

From Amazon's Alexa+ MCP toolkit authentication docs:

- OAuth 2.1 in two tiers: `client_credentials` with scope `mcp:service` for `initialize` and `tools/list`, and authorization code + PKCE (`S256`) with `mcp:tools` for tool calls on behalf of a user.
- `401 Unauthorized` on unauthenticated requests, a Protected Resource Metadata document (RFC 9728) at the well-known URI, and authorization-server metadata at `/.well-known/oauth-authorization-server` listing `code_challenge_methods_supported: ["S256"]`.
- Streamable HTTP (done) and MCP Apps for visuals (not started).

Today the endpoint accepts anyone and `/.well-known/oauth-protected-resource` returns 404.
