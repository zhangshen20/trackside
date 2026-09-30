# Product feedback

The hackathon asks every entry for feedback on each tool, API or SDK used. This is Trackside's, drawn from `docs/friction-log.md`. It is a draft: the author's own verdicts go in before submission.

## Which developer tools, APIs and SDKs did we use, and for what?

| Tool | What for |
| --- | --- |
| Alexa+ MCP toolkit (docs, quickstart, auth and policy requirements) | The target: Trackside is an Alexa+ add-on, a self-hosted MCP server on spec 2025-11-25 over Streamable HTTP |
| Model Context Protocol and `rmcp` (Rust SDK) | The server's nine tools, and the simulator's MCP client |
| AWS Lambda (`provided.al2023`, arm64) | Runs the MCP server, stateless |
| Amazon API Gateway HTTP API + ACM + custom domain | The public `/mcp` endpoint at `mcp.racingaidataset.com.au`, with throttling |
| Amazon Cognito | The OAuth 2.1 authorization server Alexa+ needs: client-credentials for discovery, authorization code + PKCE for tool calls |
| Amazon S3 | The Lambda package and the odds-free racing snapshot |
| Amazon Bedrock (Converse API, Claude Haiku 4.5 through the `au.` inference profile) | `explain_race` rewords its facts for the ear; the simulator uses Bedrock with the MCP tools to stand in for Alexa+'s model |
| AWS CloudFormation, CloudWatch Logs, IAM | The whole stack as one template; logs; least-privilege roles |
| AWS SDK for Rust | S3 reads and Bedrock calls |

## What worked well?

- **MCP as the add-on contract.** Building to an open spec meant we could test the whole server with MCP Inspector, Claude and our own client long before any Alexa+ access, and the same server works for all of them.
- **Streamable HTTP in stateless JSON mode** fits Lambda and API Gateway well: one request, one response, no sticky sessions.
- **Cognito** covered every OAuth flow Alexa+ asks for (client credentials, authorization code with PKCE, pre-registered clients) with no custom auth server.
- **Bedrock Converse** gave one request shape for tool use across models, and the `au.` inference profile kept data in Australia with no extra work. Haiku 4.5 wrote `explain_race` answers in about two to four seconds in our local tests.
- **CloudFormation** let the whole stack (Lambda, API, Cognito, domain, IAM) deploy and tear down in one command.

## What needs work?

- **Alexa+ add-on testing is US-only.** From Australia there was no documented way to try the add-on on a device or a web simulator, so we built our own simulator for the demo.
- **Alexa+ docs are split and partly broken.** Policy, transport, auth and tool rules live on separate pages; two pages linked from the overview returned 404 on 2026-09-28.
- **Cognito and MCP auth metadata don't line up.** Alexa+ expects RFC 8414 metadata at `/.well-known/oauth-authorization-server` with `S256`; Cognito publishes only OpenID discovery. We serve the document ourselves. Cognito also prefixes custom scopes (`trackside/mcp:tools`), and the Alexa+ docs don't say whether that's acceptable.
- **API Gateway buffers responses**, so Streamable HTTP's server-sent events can't stream through a Lambda integration. A note in the Alexa+ quickstart on which AWS front doors suit Streamable HTTP would help.
- **Rust on Lambda** needs `cargo-lambda` or `cargo-zigbuild` plus Zig for arm64; there is no official build image or `sam build` path for Rust.
- **Bedrock from Rust:** converting tool schemas and inputs between `serde_json::Value` and the SDK's `Document` type is manual, and choosing between the foundation model, `apac.`, `au.` and `global.` IDs, with the IAM each one needs, took trial and error.

## How was the onboarding (zero to hello world)?

_Author to add times._ Draft: MCP itself was quick: `rmcp`'s macros turn annotated Rust functions into tools, and MCP Inspector showed a working Streamable HTTP server straight away. AWS hosting took longer, mostly cross-compiling Rust for arm64 and fitting Cognito to the auth metadata Alexa+ expects (the repository's first commits, on 2026-09-28, cover both). Alexa+ itself: we never reached hello world on a real Alexa+ surface, because add-on testing is US-only. That is the biggest gap for developers outside the US.

## Would we build with these devices and services again?

_Author to confirm._ Draft: Yes. MCP on AWS was a productive, well-documented path, and Bedrock made the voice answers better with little code. What would make it an easy yes for Alexa+ specifically is a worldwide test console and a single requirements checklist for MCP add-ons.

## AWS Builder mini-challenge: services and how they're used

Lambda (the MCP server), API Gateway HTTP API with an ACM certificate and custom domain (the public endpoint), Cognito (OAuth 2.1 for Alexa+), S3 (code and data), Bedrock Converse with Claude Haiku 4.5 (`explain_race` summaries in the server, and the simulator's model), CloudFormation, IAM and CloudWatch. See `deploy/trackside.yaml`, `crates/trackside-mcp/src/summary.rs` and `crates/trackside-sim/src/bedrock.rs`.
