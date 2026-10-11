# Open Source mini-challenge: what to submit

Written 4 Oct 2026 by the "Open Source bonus" thread. For the thread that owns devpost-draft.md.

## The contribution

**mcp-cognito-auth**: a new, standalone MIT Rust crate that puts an Amazon Cognito user pool in
front of any Rust MCP server the way MCP clients expect (OAuth 2.1 + PKCE, RFC 9728
protected-resource metadata, RFC 8414 authorization-server metadata Cognito doesn't publish,
access-token checks, the two-tier service/user scope model Alexa+ uses, a `Caller` for tools),
with a Lambda example, a CloudFormation template and 12 tests including an rmcp Streamable HTTP
server end to end. Generalised from Trackside's live auth layer.

Why this satisfies "new, additional": it is a separate library with its own repository, licence,
CI, docs and release, useful to anyone hosting an MCP server on AWS, not the Trackside entry.

- Staged in the Trackside repo (PR #23, `oss/mcp-cognito-auth`, own workspace, own CI job).
- Copy ready to become its own repo: /mnt/project-files/hackathon/open-source/mcp-cognito-auth/

## Published

Public repository created by Shen on 11 Oct 2026: https://github.com/zhangshen20/mcp-cognito-auth
(main = commit 4bec433, identical to oss/mcp-cognito-auth in the Trackside repo). Optional later:
`cargo publish` from Shen's machine (needs his crates.io token).

## Devpost fields

**Open Source contribution URL:**

https://github.com/zhangshen20/mcp-cognito-auth


**GitHub username:** zhangshen20

**Description (paste):**

mcp-cognito-auth is a new MIT-licensed Rust crate, built during the hackathon, that lets any Rust
MCP server use an Amazon Cognito user pool as its OAuth 2.1 authorization server. MCP clients
discover sign-in from the server (RFC 9728 and RFC 8414 metadata), but Cognito only publishes
OpenID discovery without PKCE methods, puts client_id where aud would be and prefixes custom
scopes, so a pool can't be used directly. The crate verifies Cognito access tokens, serves both
metadata documents with Cognito's endpoints, answers 401/403 with the challenges MCP clients
expect, enforces the service-versus-user scope tiers Alexa+ uses (discovery with
client_credentials, tool calls only for a signed-in user via PKCE) and hands each tool the
caller's identity. It works with axum and rmcp's Streamable HTTP server, including stateless on
AWS Lambda, and ships with a Lambda example, a CloudFormation template and 12 tests. It was
extracted from Trackside's live endpoint so other Alexa+ builders on AWS can skip that work.

## If only one mini-challenge can be claimed

Claim **AWS Builder**: Trackside's AWS story (Lambda, API Gateway, Cognito, DynamoDB, Bedrock,
Polly, EventBridge, S3, CloudFormation) is deep and is what the demo shows, while the
Open Source piece is a supporting library. If the form allows both, claim both.
