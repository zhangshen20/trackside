# Trackside

A voice-first form guide for Australian thoroughbred racing, built as an Alexa+ add-on (a self-hosted MCP server) for the **Build, Ship, Shape: Amazon Developer Hackathon 2026**.

Ask Alexa+ what racing is on, who is in the Caulfield Cup, how a horse has been going, who ran the fastest last 600 metres, or to follow a horse through the Spring Carnival. Trackside is a fan companion: **it has no betting, no odds and no tips**, and every answer names its source.

- Track: Alexa+ (MCP server, Streamable HTTP, MCP spec 2025-11-25)
- Live endpoint: `https://mcp.racingaidataset.com.au/mcp` (OAuth 2.1, see Auth)
- Simulator: `https://mcp.racingaidataset.com.au/sim`, a simulated Alexa+ experience (see Simulator)
- Mini-challenges: AWS Builder (Lambda, API Gateway, Cognito, Bedrock), Open Source (MIT, new repo)

## Layout

```
crates/trackside-core   domain model (odds-free) and the Store trait; JSON fixture store
crates/trackside-ingest parsers for the archived fields, form, results and sectionals, and the snapshot builder (prices and bookmaker names dropped)
crates/trackside-snapshot  CLI: build a snapshot from the S3 archive (read-only) and publish it to Trackside's bucket
crates/trackside-mcp    the MCP server: 9 tools over axum + rmcp; runs locally or on AWS Lambda
crates/trackside-sim    the simulator: a web page plus a server where Claude on Amazon Bedrock drives the MCP server
fixtures/demo.json      a small demo fixture (synthetic names) for tests and local runs
deploy/                 CloudFormation stack (Lambda arm64 + API Gateway HTTP API) and deploy script
docs/                   testing guide, friction log, prior-work statement
scripts/smoke.sh        protocol smoke test against a running server (see docs/testing.md)
scripts/token.sh        a Cognito access token for the deployed stack, for the smoke test
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

`TRACKSIDE_STATELESS=1` runs the server the way Lambda does (no sessions, JSON responses).

The simulator, against that local server (needs AWS credentials that can call Bedrock):

```sh
TRACKSIDE_SIM_TODAY=2026-09-26 cargo run -p trackside-sim   # open http://127.0.0.1:8001/sim
```

## Auth

The deployed server is an OAuth 2.1 resource server in the shape Alexa+ expects: a Cognito user pool issues a client-credentials token for discovery (`trackside/mcp:service`) and an authorization-code + PKCE token for tool calls (`trackside/mcp:tools`). Requests without a token get `401` pointing at `/.well-known/oauth-protected-resource`, and the server publishes `/.well-known/oauth-authorization-server` for Cognito. Each signed-in user gets their own follow list. Locally, auth is off unless `TRACKSIDE_AUTH_ISSUER` is set; see `crates/trackside-mcp/src/auth.rs` and `docs/testing.md`.

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
| `list_meetings` | meetings on a date, track condition, first race |
| `get_race_card` | one race's conditions and full field |
| `horse_form` | career and condition records, recent starts, last-600 m times |
| `explain_race` | what a race is, why it matters, form contenders |
| `race_result` | placings, margins, time, fastest last 600 m |
| `jockey_or_trainer_stats` | wins and places over a period |
| `follow_horse` / `my_stable` | a watch list with engagements and latest results |
| `carnival_guide` | the 2026 Spring Racing Carnival feature races |

## Simulator

The Alexa+ MCP toolkit only runs in the United States, so Trackside ships its own simulated Alexa+ experience for demos and for anyone testing from elsewhere. It is labelled as a simulation on the page.

- **Voice in, voice out**: tap the mic and ask (browser speech recognition, `en-AU`), or type. The answer is spoken with the browser's Australian voice.
- **A screen like an Echo Show**: each answer draws a card from the tool's structured content: the race card, a result with the fastest last 600 m, a horse's form with its records by going, the meetings, your stable.
- **The same path Alexa+ takes**: "Link account" signs in on Cognito's page (authorization code + PKCE, exchanged server-side with the client secret, as Alexa+ account linking does). Each turn, a Claude model on Amazon Bedrock (Converse API, `au.` inference profile) reads the MCP server's own tool list, picks tools, and the simulator calls them on `/mcp` over Streamable HTTP with the user's token. The page lists every MCP call it made.

`crates/trackside-sim/src/main.rs` documents its settings. The deploy script builds it as a second Lambda behind the same API, on `/sim`.

## Data

Production data is read, read-only, from an existing daily archive of Racing Australia fields and form, official results and state sectional timing (see `docs/prior-work.md`). `trackside-snapshot` turns a date range into one gzipped snapshot:

- meetings and fields from Racing Australia, with track and weather from the official meeting list;
- form for every horse in those fields (latest day wins);
- results: finishing order and riders from the official results, margins, times and the fastest last 600 m from state sectional timing.

Price fluctuations, dividends and pools are never read (the wire types do not declare them), and wagering brands are removed from race and venue names ("Sportsbet Longreach" is Longreach). The snapshot is not committed: the data is licensed for this use, not for redistribution, so the repository only carries the synthetic demo fixture.

## Licence

MIT. See `LICENSE`.
