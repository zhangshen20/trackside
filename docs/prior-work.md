# Prior work statement

Trackside (this repository) was created from scratch on 2026-09-28, inside the hackathon submission window (which opened 2026-08-31).

It reads, read-only, from a daily archive of Australian racing data that the author's separate, private, pre-existing data pipeline collects: Racing Australia fields and full form pages, official results, and state racing bodies' sectional timing files. That pipeline is prior work and is not part of this submission; no code from it is used here. Trackside's ingester parses those archived files with its own code, drops all price data at the boundary, and builds its own store.

Everything Alexa+-facing (the MCP server, its tools, the OAuth 2.1 layer, the Bedrock-driven simulator and the AWS stack) is new for the hackathon.
