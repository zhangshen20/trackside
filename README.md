# Trackside

A voice-first form guide for Australian thoroughbred racing, built as an Alexa+ add-on (a self-hosted MCP server) for the **Build, Ship, Shape: Amazon Developer Hackathon 2026**.

Ask Alexa+ what racing is on, who is in the Caulfield Cup, how a horse has been going, who ran the fastest last 600 metres, or to follow a horse through the Spring Carnival. Trackside is a fan companion: **it has no betting, no odds and no tips**, and every answer names its source.

- Track: Alexa+ (MCP server, Streamable HTTP, MCP spec 2025-11-25)
- Mini-challenges: AWS Builder (Lambda, Bedrock, Cognito), Open Source (MIT, new repo)

## Layout

```
crates/trackside-core   domain model (odds-free) and the Store trait; JSON fixture store
crates/trackside-ingest parsers for the archived fields, form, results and sectionals, and the snapshot builder (prices and bookmaker names dropped)
crates/trackside-snapshot  CLI: build a snapshot from the S3 archive (read-only) and publish it to Trackside's bucket
crates/trackside-mcp    the MCP server: 9 tools over axum + rmcp; runs locally or on AWS Lambda
fixtures/demo.json      a small demo fixture (synthetic names) for tests and local runs
deploy/                 CloudFormation stack (Lambda arm64 + API Gateway HTTP API) and deploy script
docs/                   testing guide, friction log, product feedback, prior-work statement
scripts/smoke.sh        protocol smoke test against a running server (see docs/testing.md)
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

## Deploy to AWS

```sh
pip install cargo-zigbuild ziglang awscli && rustup target add aarch64-unknown-linux-gnu
SNAPSHOT_FROM=2026-09-22 SNAPSHOT_TO=2026-09-29 deploy/deploy.sh   # prints the /mcp URL
```

The script cross-compiles the server for arm64, creates a private `trackside-<account>-<region>` bucket for the Lambda zip and the snapshot, and deploys the `trackside-mcp` CloudFormation stack: one Lambda (`provided.al2023`, arm64) behind an API Gateway HTTP API with throttling. Everything is named `trackside-*` and tagged `Project=trackside`. Set `TRACKSIDE_ROLE_ARN` to run every AWS call as a deploy role. On Lambda the server is stateless, because consecutive requests can reach different instances.

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

## Data

Production data is read, read-only, from an existing daily archive of Racing Australia fields and form, official results and state sectional timing (see `docs/prior-work.md`). `trackside-snapshot` turns a date range into one gzipped snapshot:

- meetings and fields from Racing Australia, with track and weather from the official meeting list;
- form for every horse in those fields (latest day wins);
- results: finishing order and riders from the official results, margins, times and the fastest last 600 m from state sectional timing.

Price fluctuations, dividends and pools are never read (the wire types do not declare them), and wagering brands are removed from race and venue names ("Sportsbet Longreach" is Longreach). The snapshot is not committed: the data is licensed for this use, not for redistribution, so the repository only carries the synthetic demo fixture.

## Licence

MIT. See `LICENSE`.
