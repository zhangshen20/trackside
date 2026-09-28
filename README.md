# Trackside

A voice-first form guide for Australian thoroughbred racing, built as an Alexa+ add-on (a self-hosted MCP server) for the **Build, Ship, Shape: Amazon Developer Hackathon 2026**.

Ask Alexa+ what racing is on, who is in the Caulfield Cup, how a horse has been going, who ran the fastest last 600 metres, or to follow a horse through the Spring Carnival. Trackside is a fan companion: **it has no betting, no odds and no tips**, and every answer names its source.

- Track: Alexa+ (MCP server, Streamable HTTP, MCP spec 2025-11-25)
- Mini-challenges: AWS Builder (Lambda, Bedrock, Cognito), Open Source (MIT, new repo)

## Layout

```
crates/trackside-core   domain model (odds-free) and the Store trait; JSON fixture store
crates/trackside-mcp    the MCP server: 9 tools over axum + rmcp
fixtures/demo.json      a small demo fixture (synthetic names)
docs/                   friction log, product feedback, prior-work statement
```

## Run locally

```sh
cargo run -p trackside-mcp            # serves http://127.0.0.1:8000/mcp
npx @modelcontextprotocol/inspector   # connect with transport "Streamable HTTP"
```

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

Production data is read from an existing daily archive of Racing Australia fields and form, official results and state sectional timing (see `docs/prior-work.md`). Price fluctuations present in the raw form pages are dropped at ingest and never enter the model.

## Licence

MIT. See `LICENSE`.
