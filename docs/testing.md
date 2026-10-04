# Testing Trackside

Three layers, cheapest first. Run layer 1 after every change, layer 2 before recording anything, layer 3 after every redeploy.

Live endpoint (needs a Cognito token): `https://mcp.racingaidataset.com.au/mcp`. The API's own URL, `https://f534rx2db4.execute-api.ap-southeast-2.amazonaws.com/mcp`, still works, and each hostname advertises itself in the OAuth metadata, so a client uses one or the other throughout.

The deployed snapshot covers 22 to 29 September 2026, so "today" returns no meetings; pass a date inside the snapshot (the examples use Caulfield on Sunday 27 September).

## 1. Protocol check

### Scripted: `scripts/smoke.sh`

```sh
scripts/smoke.sh                                   # live endpoint, 27 Sep data
TRACKSIDE_STATELESS=1 TRACKSIDE_TODAY=2026-10-14 cargo run -p trackside-mcp   # in another terminal
MCP_URL=http://127.0.0.1:8000/mcp scripts/smoke.sh --fixture
```

It checks `initialize` (protocol 2025-11-25), `tools/list` (the core tools, each with a description), one call to every tool, the error paths (unknown tool, missing argument, unknown venue, bad date, unknown method) and that no answer uses betting talk, a price or a bookmaker's name. It also fails when voice text has holes from empty fields (`"the Manikato Stakes,  over 1200 metres"`). Override the probe with `DATE`, `VENUE`, `RACE`, `HORSE`, `PERSON`, `ROLE` and `RESULT_*` when the snapshot moves. With OAuth on, run `MCP_TOKEN=$(scripts/token.sh) scripts/smoke.sh`: `token.sh` gets a client-credentials token from the stack's test-only smoke client (both scopes), and the smoke test adds three checks: a request without a token gets 401, and both metadata documents resolve.

The script sends no `Mcp-Session-Id`, so a local server must run stateless, the way Lambda does. `TRACKSIDE_TODAY=2026-10-14` fixes the local server's clock on the Wednesday before the fixture's Caulfield Cup card, which the fixture check "my_stable names the next run" relies on ("Sample Stayer runs on Saturday in the Caulfield Cup"). Leave it unset for the deployed server, whose data is live. The simulator has its own pin, `TRACKSIDE_SIM_TODAY`, and shows "Saturday 17 October (simulated)" under its clock when it is set, so a viewer knows why "this Saturday" lands on snapshot racing.

`/healthz` (next to `/mcp`, never behind auth) answers `200` with `{"ok":true,"snapshot_date":"2026-10-17","meetings":N,"uptime_s":S}`: the last date the snapshot has a meeting on, how many meetings it holds, and the process's uptime. `--wait-for` polls it until it answers, and the smoke test checks its shape, plus that a warm `get_race_card` call takes under 3 seconds by curl's `time_total`. It writes no metric line.

Note that it calls `follow_horse`, which writes to the smoke client's own stable (each token subject has its own), and then `unfollow_horse` to take it out again. On the deployed stack that stable is stored in DynamoDB.

### Response times: `scripts/perf.sh`

```sh
MCP_URL=http://127.0.0.1:8000/mcp scripts/perf.sh --fixture        # local server on fixtures/demo.json
MCP_TOKEN=$(scripts/token.sh) scripts/perf.sh -n 50                 # the deployed endpoint, 50 calls per tool
```

It times the first request (`initialize`), then makes one warming call and N timed calls (default 20) to each read-only tool, and prints p50 and p95 per tool in milliseconds, by curl's `time_total`, so network and API Gateway are included. Against a Lambda that has just been deployed or recycled, the first request is the cold start; the script's header shows the `aws lambda update-function-configuration` call that recycles it on demand. The same arguments as the smoke test apply (`DATE`, `VENUE`, ...). With `TRACKSIDE_METRICS=1` a local server also prints each call's metric line, as Lambda does.

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
| `next_race` | `{}`, then `{"state":"NSW"}` |
| `carnival_guide` | `{}` |

The quickest real sign-in is `scripts/signin_check.py` on the Mac (stop Inspector first): it opens Cognito's page in the browser, and after you sign in or sign up it exchanges the code with PKCE and calls `follow_horse` and `my_stable` as that user.

With OAuth on, Inspector's **Auth** panel runs the authorization-code + PKCE flow from the server's metadata. Give it the stack's `UserClientId` and that client's secret (`aws cognito-idp describe-user-pool-client`), sign up with an email on Cognito's page, and the tools run as that user. `http://localhost:6274/oauth/callback` is already an allowed redirect. That is the quickest way to prove discovery works before Alexa+ sees it.

## 2. Natural-language check with a real model

Protocol tests prove the tools answer; they don't prove a model picks the right tool, fills the arguments a fan would imply, or turns the answer into something worth saying aloud. Alexa+ testing is US-only, so use Claude as a stand-in model:

- **Claude.ai** (web or desktop): Settings > Connectors > Add custom connector, paste the `/mcp` URL, and under advanced settings give it the `UserClientId` and its secret (Cognito has no dynamic client registration). Enable it in a new chat.
- **Claude Code**: `claude mcp add --transport http trackside <url>`.

Ask these, one per chat, and judge the answer as if it were spoken:

1. "What racing was on in Victoria last Sunday?" (date resolution; `list_meetings` with a state)
2. "Who won the Manikato Stakes at Caulfield on the 27th, and who ran home fastest?" (`race_result`; last 600)
3. "How has Jimmysstar been going?" (no "(NZ)": does the model or the server find the horse?)
4. "Explain the Manikato to me like I've never watched racing." (`explain_race`; is the Group 1 status there?)
5. "How's Ethan Brown riding this spring?" (`jockey_or_trainer_stats` with a period)
6. "When's the Melbourne Cup and what's on before it?" (`carnival_guide`)
7. "Follow Extragalactic. Is it running anywhere?" (`follow_horse` then `my_stable`)
8. Sign out, sign in again (or come back the next day) and ask "How's my stable?" (memory across sessions: the followed horse is still there, and the answer opens with what it has done since you last asked)
9. "I'm in Sydney" then "What racing is on today?" (`set_home_state`; NSW meetings first, the rest by name)
10. "Who's in the Caulfield Cup?" on the simulator: the screen shows the MCP App; tap a horse (its form opens, and the tool-call panel shows `MCP App → horse_form`), then Explain this race and Follow
11. "Who should I back in race 8?" and "What price is Giga Kick?" (the model must decline both and offer form instead; no tool should ever return a price)

For each, note: right tool first time, arguments correct, answer short enough to say in one breath, source named, nothing about odds. Record anything odd in `docs/friction-log.md`.

## 3. Regression checklist after each redeploy

Run on the Mac that deploys (`HR_ENV=staging SNAPSHOT_FROM=... SNAPSHOT_TO=... deploy/deploy.sh`), straight after it prints the URL:

- [ ] `scripts/smoke.sh` passes against the printed URL (set `DATE` etc. to the new snapshot's range).
- [ ] `SNAPSHOT_FROM` is on or before 2026-10-10, so the Caulfield Guineas and Caulfield Cup results stay in the snapshot and `carnival_guide` still names their winners for judges testing through 20 November.
- [ ] `list_meetings` for the last date of the snapshot returns meetings (the snapshot really updated).
- [ ] `race_result` for a race run on the last day has a fastest last 600 m (sectionals joined).
- [ ] `horse_form` for that race's winner lists the win as its latest start (results fold into form).
- [ ] No bookmaker brand in any meeting or race name for the new dates (`list_meetings` text).
- [ ] Cold start is acceptable: `scripts/perf.sh` straight after a deploy (its first request lands on a cold instance); it should finish well inside Alexa+'s turn budget, and the `trackside` dashboard's cold-start table shows the Init Duration.
- [ ] CloudWatch: no `ERROR` lines for the Lambda since the deploy (`aws logs tail /aws/lambda/<function> --since 10m`).
- [ ] With OAuth on: an unauthenticated `initialize` returns 401, `/.well-known/oauth-protected-resource` resolves, and the smoke test passes with `MCP_TOKEN`.

## How the server meets Alexa+'s auth rules

From Amazon's Alexa+ MCP toolkit authentication docs, and how Trackside does each:

| Alexa+ asks for | Trackside |
| --- | --- |
| `client_credentials` token for discovery (`initialize`, `tools/list`) | Cognito client `trackside-service`, scope `trackside/mcp:service` |
| Authorization code + PKCE (S256) for tool calls for a user | Cognito client `trackside-user`, scope `trackside/mcp:tools`; a service token calling a tool gets 403 `insufficient_scope` |
| `401` for unauthenticated requests | `401` with `WWW-Authenticate: Bearer resource_metadata=".../.well-known/oauth-protected-resource"` |
| Protected Resource Metadata (RFC 9728) | `/.well-known/oauth-protected-resource` (and `/mcp` suffixed) |
| Authorization-server metadata at `/.well-known/oauth-authorization-server` with `code_challenge_methods_supported: ["S256"]` | Served by Trackside, pointing at Cognito's `/oauth2/authorize` and `/oauth2/token`; Cognito's own discovery document is OpenID-only |
| No dynamic client registration | Clients are created by the stack; secrets are read from Cognito, never committed |

Not yet covered: MCP Apps visuals.

## 4. Simulator

`/sim` plays the Alexa+ side: Claude on Bedrock picks the tools and the simulator calls them on `/mcp` as the linked user.

- `cargo test -p trackside-sim` covers the conversation loop (with a scripted model), the MCP client, PKCE and the cookies.
- Locally: run the MCP server stateless on port 8000, then `TRACKSIDE_SIM_TODAY=2026-09-26 cargo run -p trackside-sim` and open `http://127.0.0.1:8001/sim`. Without Cognito settings it runs in local mode with no sign-in.
- Deployed: open `https://mcp.racingaidataset.com.au/sim`, choose **Link account**, sign in, and ask the questions from section 2. Each answer should show a card, and the tool-call panel should list the MCP calls. Set `TRACKSIDE_SIM_TODAY` at deploy time to a date inside the snapshot so "today" and "Saturday" land on real racing.
- A 502 whose message starts `Bedrock:` is Bedrock refusing the call (model access, quota); the page shows the message.
