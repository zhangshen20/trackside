# Trackside

A voice-first form guide for Australian thoroughbred racing, built as an Alexa+ add-on (a self-hosted MCP server) for the **Build, Ship, Shape: Amazon Developer Hackathon 2026**.

Ask Alexa+ what racing is on, who is in the Caulfield Cup, how a horse has been going, who ran the fastest last 600 metres, or to follow a horse through the Spring Carnival. Trackside is a fan companion: **it has no betting, no odds and no tips**, and every answer names its source.

- Track: Alexa+ (MCP server, Streamable HTTP, MCP spec 2025-11-25)
- Live endpoint: `https://mcp.racingaidataset.com.au/mcp` (OAuth 2.1, see Auth)
- Simulator: `https://mcp.racingaidataset.com.au/sim`, a simulated Alexa+ experience (see Simulator)
- Mini-challenges: AWS Builder (Lambda, API Gateway, Cognito, DynamoDB, Bedrock, Polly), Open Source (MIT, new repo)

## Layout

```
crates/trackside-core   domain model (odds-free) and the Store trait; JSON fixture store
crates/trackside-ingest parsers for the archived fields, form, results and sectionals, and the snapshot builder (prices and bookmaker names dropped)
crates/trackside-snapshot  CLI and scheduled Lambda: build a snapshot from the S3 archive (read-only) and publish it to Trackside's bucket
crates/trackside-mcp    the MCP server: 13 tools over axum + rmcp; runs locally or on AWS Lambda
crates/trackside-sim    the simulator: a web page plus a server where Claude on Amazon Bedrock drives the MCP server
fixtures/demo.json      a small demo fixture (synthetic names) for tests and local runs
deploy/                 CloudFormation stack (Lambda arm64 + API Gateway HTTP API) and deploy script
docs/                   testing guide, friction log, product feedback (draft), prior-work statement
scripts/smoke.sh        protocol smoke test against a running server (see docs/testing.md)
scripts/token.sh        a Cognito access token for the deployed stack, for the smoke test
oss/mcp-cognito-auth    open-source spin-off: Cognito OAuth for any Rust MCP server (own workspace, see below)
```

## Run locally

```sh
cargo run -p trackside-mcp            # serves http://127.0.0.1:8000/mcp from fixtures/demo.json
npx @modelcontextprotocol/inspector   # connect with transport "Streamable HTTP"
```

With real data (needs read access to the archive, see below):

```sh
cargo run --release -p trackside-snapshot -- --from 2026-09-22 --to 2026-09-29 --out snapshot.json.gz
TRACKSIDE_SNAPSHOT=snapshot.json.gz cargo run --release -p trackside-mcp
```

`TRACKSIDE_STATELESS=1` runs the server the way Lambda does (no sessions, JSON responses). `TRACKSIDE_TODAY=2026-10-14` fixes the server's clock on the Wednesday before the fixture's Caulfield Cup card, so a stable report says "runs on Saturday" whatever day you try it (local runs only; the deployed server's data is live).

The simulator, against that local server (needs AWS credentials that can call Bedrock):

```sh
TRACKSIDE_SIM_TODAY=2026-09-26 cargo run -p trackside-sim   # open http://127.0.0.1:8001/sim
```

## Auth

The deployed server is an OAuth 2.1 resource server in the shape Alexa+ expects: a Cognito user pool issues a client-credentials token for discovery (`trackside/mcp:service`) and an authorization-code + PKCE token for tool calls (`trackside/mcp:tools`). Requests without a token get `401` pointing at `/.well-known/oauth-protected-resource`, and the server publishes `/.well-known/oauth-authorization-server` for Cognito. Each signed-in user gets their own memory (see Memory). Locally, auth is off unless `TRACKSIDE_AUTH_ISSUER` is set; see `crates/trackside-mcp/src/auth.rs` and `docs/testing.md`.

The same approach, generalised for any axum or rmcp server, is the standalone crate [`mcp-cognito-auth`](oss/mcp-cognito-auth) (MIT, with its own tests, a Lambda example and a CloudFormation template). It lives here until it moves to its own repository.

## Deploy to AWS

```sh
pip install cargo-zigbuild ziglang awscli && rustup target add aarch64-unknown-linux-gnu
HR_ENV=staging SNAPSHOT_FROM=2026-09-22 SNAPSHOT_TO=2026-09-30 deploy/deploy.sh   # prints the /mcp URL
```

The script cross-compiles the server for arm64, creates a private `trackside-<account>-<region>` bucket for the Lambda zip and the snapshot, and deploys the `trackside-mcp` CloudFormation stack: one Lambda (`provided.al2023`, arm64) behind an API Gateway HTTP API with throttling, and a Cognito user pool with its OAuth domain and app clients. Everything is named `trackside-*` and tagged `Project=trackside`. Set `TRACKSIDE_ROLE_ARN` to run every AWS call as a deploy role. On Lambda the server is stateless, because consecutive requests can reach different instances.

To serve it on your own hostname, run the script once with `TRACKSIDE_DOMAIN=mcp.example.com` (and `TRACKSIDE_HOSTED_ZONE_ID` when the domain's DNS is in Route 53 in the same account). It requests a DNS-validated ACM certificate and adds an API Gateway custom domain; with Route 53 it writes both DNS records itself, otherwise it prints the certificate's validation CNAME and, once the certificate is issued and you rerun it, the CNAME for the hostname. Later deploys keep the domain, and the execute-api URL keeps working. In an account that has never had an API Gateway custom domain, create API Gateway's service-linked role once as an admin first (`aws iam create-service-linked-role --aws-service-name ops.apigateway.amazonaws.com`); the deploy role can't.

## Tools

| Tool | Answers |
| --- | --- |
| `list_meetings` | meetings on a date, track condition, first race (in your own clock when your state keeps a different time) |
| `get_race_card` | one race, asked for by name ("the Caulfield Cup", or "Cofield Cup" as heard) or by venue and number: its conditions and full field, when it jumps in your clock and the venue's ("4 pm Queensland time, 5 pm at the track, due to jump in about 25 minutes"), with the MCP App on screens |
| `horse_form` | career and condition records, recent starts with where it was at the 800, last-600 m times, and where it usually settles |
| `explain_race` | a race by name or number: what it is, why it matters, the strongest recent form and where the field usually settles; a carnival race whose field is not out yet is answered from the guide ("The Cox Plate is on Saturday 24 October at Moonee Valley; fields come out on the Wednesday before") |
| `race_result` | a race by name or number: placings, margins, time, and how it was run: where the placegetters were at the 800 and their last 600 m |
| `jockey_or_trainer_stats` | wins and places over a period |
| `follow_horse` / `unfollow_horse` / `my_stable` | a stable of followed horses, remembered across sessions: what they've done since you last asked, today's engagements and results, and where each horse runs next ("runs on Saturday in the Caulfield Cup, race 8 at Caulfield at 5 pm, barrier 4") |
| `next_race` | the next race to jump, anywhere or in one state or at one venue, how far off it is and its start in your clock ("race 8 at Caulfield, due to jump in about 25 minutes, at 4 pm Queensland time, 5 pm at the track"), with racing in your home state first; after the last race, tomorrow's first |
| `set_home_state` | read meetings in your state first and in full, and hear every start time in your state's clock |
| `forget_me` | delete everything Trackside remembers about you |
| `carnival_guide` | the 2026 Spring Racing Carnival as it stands on the day: what is next (with its field and jump time once out), the latest feature result, and what follows; or one feature by name ("when's the Cox Plate") |

Every tool carries a title and the MCP behaviour hints (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`), so a host knows that `forget_me` and `unfollow_horse` delete, that `follow_horse`, `my_stable` and `set_home_state` write the listener's profile, and that the other eight only read. Input schemas use only what every model provider reads (plain types, no nullable type arrays, formats or bounds), which is what MCP Inspector's schema portability check looks for.

## MCP App: the race card on screen

On a device with a screen, Trackside draws its own answers. The server ships an MCP App (the MCP Apps extension, `io.modelcontextprotocol/ui`): `get_race_card`, `race_result`, `horse_form`, `explain_race` and `my_stable` name the resource `ui://trackside/race-card.html` in their `_meta`, and a host that supports MCP Apps shows it in a sandboxed iframe next to the spoken answer.

- **Race card**: the field with saddlecloth numbers, jockeys, trainers, barriers, weights and a colour-coded form strip (gold, silver, bronze for 1st to 3rd, gaps for spells), plus the grade, distance, purse and jump time.
- **Tap a horse** and the App calls `horse_form` through the host and opens its records and recent starts with last-600 m times; **Follow** calls `follow_horse`. **Explain this race** and **Result** call those tools in place.
- **Result**: a podium, margins and the fastest last 600 m. **Stable**: what's new since you last checked, and where your horses run next.

The App is one self-contained HTML file (`crates/trackside-mcp/static/race-card.html`): no network access of its own, so it declares no CSP domains, and it only draws what the tools return. The simulator is an MCP Apps host too: it answers `ui/initialize`, passes the tool result in, relays the App's `tools/call` requests to the MCP server as the linked user, and labels each relayed call in its tool-call panel.

## How the race was run

Trackside reads races the way a race caller does, from sectional data:

- **Results** say how the race unfolded: "Sample Stayer came from 5th at the 800 and ran its last 600 in 34.9 seconds. Demo Miler ran the fastest last 600, 34.6 seconds, from 9th at the 800 to finish 2nd." Each runner's last 600 m comes from state sectional timing; its position at the 800 comes from its Racing Australia form line once that is published.
- **Form** names where a horse was at the 800 in each recent start and its habit over its last six runs: it usually leads, races on the pace, settles midfield, or settles back and runs on.
- **Race cards and explanations** group the field by those habits ("On past runs, Placeholder Prince usually leads, and Demo Miler usually settles back in the field"). The MCP App draws it as a map of the field. It describes past runs only and never says how a race will be run.

## Names the way they're heard

Speech recognition respells racing names: "Jimmy's Star" for Jimmysstar, "Cofield" for Caulfield, "Demo Myla" for Demo Miler. When a horse, jockey, trainer or venue isn't found as spelt, Trackside compares names by sound (spaces and punctuation squashed, common spellings of one sound folded together, a dropped Australian final "r", then a small edit distance). One clear match is used and said aloud ("Taking Jimmy Star as Jimmysstar"); several close ones get "Did you mean A or B?"; a surname alone finds a jockey or trainer. See `crates/trackside-core/src/names.rs`.

## Memory

Trackside remembers each signed-in listener between sessions, keyed by their Cognito subject in a DynamoDB table (`trackside-listeners`, on-demand, encrypted): the horses they follow, their home state, and the day they last heard their stable report. So a conversation can pick up where the last one left off:

> "How's my stable?" "Since you last checked on Sunday 20 September: Demo Miler ran 2nd of 12 at Flemington on Saturday 26 September; Sample Stayer won at Flemington..."

The catch-up comes from each followed horse's form between the last check and today, and is said once. `forget_me` deletes the item. The table holds no names or emails, only the subject, horse names, a state and a date. Locally (no `TRACKSIDE_MEMORY_TABLE`) memory lives in the process. See `crates/trackside-mcp/src/memory.rs`.

## Simulator

The Alexa+ MCP toolkit only runs in the United States, so Trackside ships its own simulated Alexa+ experience for demos and for anyone testing from elsewhere. It is labelled as a simulation on the page.

- **Voice in, voice out**: tap the mic and ask (browser speech recognition, `en-AU`), or type. The answer is spoken with the browser's Australian voice.
- **A screen like an Echo Show**: when a tool has an MCP App, the screen hosts Trackside's own App (see above), and taps in it call the MCP server through the simulator; other answers draw a card from the tool's structured content (the meetings, the carnival guide).
- **The same path Alexa+ takes**: "Link account" signs in on Cognito's page (authorization code + PKCE, exchanged server-side with the client secret, as Alexa+ account linking does). Each turn, a Claude model on Amazon Bedrock (Converse API, `au.` inference profile) reads the MCP server's own tool list, picks tools, and the simulator calls them on `/mcp` over Streamable HTTP with the user's token. The page lists every MCP call it made.

`crates/trackside-sim/src/main.rs` documents its settings. The deploy script builds it as a second Lambda behind the same API, on `/sim`.

## Bedrock race explanations

`explain_race` gathers its facts (the race, why it matters, conditions, the field's recent form) and, when `TRACKSIDE_BEDROCK_MODEL` is set, has a Bedrock model reword them into a few sentences a newcomer can follow by ear. The model sees only those facts, never prices; an answer that uses betting words, or a call that fails or takes more than six seconds, falls back to the tool's template sentence. The structured content says which one was used (`written_by`). The stack sets the model to Claude Haiku 4.5 through the `au.` inference profile, so requests stay in Australian regions (`BedrockModel` parameter; empty turns it off).

## Data

Production data is read, read-only, from an existing daily archive of Racing Australia fields and form, official results and state sectional timing (see `docs/prior-work.md`). `trackside-snapshot` turns a date range into one gzipped snapshot:

- meetings and fields from Racing Australia, with track and weather from the official meeting list;
- form for every horse in those fields (latest day wins);
- results: finishing order and riders from the official results, margins, times and the fastest last 600 m from state sectional timing.

Price fluctuations, dividends and pools are never read (the wire types do not declare them), and wagering brands and products are removed from race and venue names ("Sportsbet Longreach" is Longreach; "TAB ONE POOL Edward Manifold Stakes" is the Edward Manifold Stakes; a race that was nothing but a sponsor is "race 2"). Racing Australia's fields carry no grade, so Group 1 races are recognised from a list of their names (`GROUP_ONE_RACES` in `crates/trackside-ingest`), with lead-ups, trials and sponsor messages about a race left ungraded. The snapshot is not committed: the data is licensed for this use, not for redistribution, so the repository only carries the synthetic demo fixture.

On AWS the same binary also runs as a scheduled Lambda (`trackside-refresh`, deployed when `HR_ENV` is set): twice a day, after the evening's results and again early with the day's final fields, it rebuilds the snapshot from a fixed start date to four days ahead, uploads it, and recycles the MCP function so new requests see it. Its role can only read the two archive buckets, write `snapshots/` in Trackside's bucket, and update the MCP function's configuration (it touches the function description so warm instances reload the snapshot).

Every hour the same function runs a health probe (`trackside-snapshot --probe` runs it locally). It checks four things:

- the public `/mcp` endpoint refuses a request with no token and points at its sign-in metadata;
- that metadata, Cognito's discovery document and Cognito's keys are all served;
- `/sim` loads;
- the snapshot is under 26 hours old.

CloudWatch alarms fire if the refresh or the probe fails, the probe stops running, the MCP function errors, or the API returns 5xx. Set `ALERT_EMAIL` when you run `deploy/deploy.sh` to have the alarms emailed, through an SNS topic.

## Performance and operations

Every tool call writes one CloudWatch Embedded Metric Format line to the function's log, which CloudWatch turns into metrics in the `Trackside` namespace with no agent and no extra API call (`crates/trackside-mcp/src/telemetry.rs`). Each line has the tool and its outcome (found, not found, did you mean, error) as dimensions, and the call's latency. When `explain_race` asks Bedrock it adds Bedrock's answer time, and, when the answer falls back to the template, `BedrockFallback` with the reason (timeout, error or a rejected answer). Each process adds one line at start-up with how long the snapshot took to load. A line carries the Lambda request id so it can be found in the log, and never the listener's account, a token, a horse's name or anything else a listener said. Locally, `TRACKSIDE_METRICS=1` prints the same lines. The hourly probe adds the snapshot's age in hours.

Internal errors never reach the listener as error text: a store, S3 or DynamoDB failure is heard as "Trackside couldn't read its data just now; try again in a moment.", and the detail goes to the log with the request id. `/healthz` answers `{"ok":true,"snapshot_date":...,"meetings":N,"uptime_s":...}`.

The stack's `trackside` CloudWatch dashboard (the `DashboardUrl` output) shows Lambda duration p50 and p99, cold starts with their Init Duration, the snapshot load time, calls by tool and by outcome, latency by tool, Lambda errors and throttles, API Gateway requests, 4xx and 5xx, Bedrock fallbacks and answer time, the snapshot's age and the alarms. Beside the four alarms above, a fifth, `trackside-bedrock-fallback-rate`, fires when more than half of `explain_race`'s Bedrock answers in 15 minutes fall back to the template.

`scripts/perf.sh` measures what a client sees: the first request, then warm calls to each read-only tool, with p50 and p95 per tool. These are local figures, a debug build serving `fixtures/demo.json` on a 4-core x86_64 container, measured with `scripts/perf.sh --fixture -n 50`:

| Measure | Figure |
| --- | --- |
| Local binary, process start to first answer (`initialize`) | 25 ms |
| Local, warm p50: `list_meetings` | 2.7 ms |
| Local, warm p50: `get_race_card` | 2.9 ms |
| Local, warm p50: `explain_race` (template, no Bedrock) | 2.7 ms |
| Local, warm p50: `race_result` | 3.1 ms |
| Local, warm p50: `horse_form` | 3.0 ms |
| Local, warm p50: `jockey_or_trainer_stats` | 3.0 ms |
| Local, warm p50: `carnival_guide` | 2.7 ms |
| Local, warm p50: `my_stable` | 2.6 ms |
| Lambda cold start (ap-southeast-2, arm64) | run `scripts/perf.sh` against the deployed URL |

## Licence

MIT. See `LICENSE`.
