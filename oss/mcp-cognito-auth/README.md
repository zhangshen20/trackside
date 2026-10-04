# mcp-cognito-auth

Put an [Amazon Cognito](https://docs.aws.amazon.com/cognito/latest/developerguide/cognito-user-pools.html)
user pool in front of a Rust [MCP](https://modelcontextprotocol.io) server, the way MCP clients
expect it: OAuth 2.1 with PKCE, a `401` that tells the client where to sign in, and the
discovery documents Cognito doesn't publish itself.

Works with any [axum](https://crates.io/crates/axum) router and with
[rmcp](https://crates.io/crates/rmcp)'s Streamable HTTP server, including stateless servers on
AWS Lambda. MIT licensed.

## Why

The [MCP authorization spec](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization)
has clients discover everything from the server:

1. The client calls `/mcp` without a token and gets `401` with
   `WWW-Authenticate: Bearer resource_metadata="…"`.
2. It reads the server's **protected-resource metadata** ([RFC 9728](https://www.rfc-editor.org/rfc/rfc9728))
   to find the authorization server.
3. It reads that server's **authorization-server metadata** ([RFC 8414](https://www.rfc-editor.org/rfc/rfc8414))
   to find the authorize and token endpoints and check PKCE `S256` support.

Cognito publishes only OpenID Connect discovery, which doesn't list PKCE methods, and its
access tokens carry `client_id` instead of `aud` and prefix custom scopes with the resource
server (`myapp/mcp:tools`). So pointing an MCP client straight at a user pool doesn't work.
This crate is the missing piece: a few hundred lines that verify Cognito access tokens and serve
both metadata documents, with Cognito's endpoints filled in.

It also enforces the **two-tier scope** model that voice assistants such as Alexa+ use:

| Token | Grant | Scope | May |
|---|---|---|---|
| Service | `client_credentials` | `…/mcp:service` | `initialize`, `tools/list`, `ping`, notifications |
| User | authorization code + PKCE | `…/mcp:tools` | everything, including `tools/call` for that user |

A service token that tries `tools/call` (alone or hidden in a JSON-RPC batch) gets `403
insufficient_scope` with the scope it needs, so the client knows to sign the user in.

## Use it

```toml
[dependencies]
mcp-cognito-auth = { git = "https://github.com/zhangshen20/mcp-cognito-auth" }
```

```rust
use mcp_cognito_auth::CognitoAuth;

let auth = CognitoAuth::builder(
    "https://cognito-idp.ap-southeast-2.amazonaws.com/ap-southeast-2_AbCdEf123", // issuer
    "https://my-pool.auth.ap-southeast-2.amazoncognito.com",                     // OAuth domain
)
.clients([service_client_id, user_client_id]) // only these app clients' tokens
.scopes("myapp/mcp:service", "myapp/mcp:tools") // Cognito's prefixed names
.resource_name("My MCP server")
.build();

// Only the routes you pass in need a token. `protect` also adds the public routes
// /.well-known/oauth-protected-resource[/mcp] and /.well-known/oauth-authorization-server.
let app = auth
    .protect(axum::Router::new().nest_service("/mcp", mcp_service))
    .route("/healthz", axum::routing::get(|| async { "ok" }));
```

Or configure it from the environment, so the same binary runs open on your laptop and
protected when deployed:

```rust
let app = match CognitoAuth::from_env()? {
    Some(auth) => auth.protect(mcp),
    None => mcp, // MCP_AUTH_ISSUER unset
};
```

| Variable | Meaning |
|---|---|
| `MCP_AUTH_ISSUER` | `https://cognito-idp.<region>.amazonaws.com/<pool id>`; auth is off without it |
| `MCP_AUTH_DOMAIN` | the pool's OAuth domain, `https://<prefix>.auth.<region>.amazoncognito.com` |
| `MCP_AUTH_CLIENTS` | comma-separated app client ids to accept (empty accepts any client of the pool) |
| `MCP_AUTH_SCOPE_SERVICE` / `MCP_AUTH_SCOPE_USER` | scope names, default `mcp:service` / `mcp:tools` |
| `MCP_AUTH_PUBLIC_URL` | public base URL, when `Host` isn't it (behind CloudFront, say) |
| `MCP_AUTH_RESOURCE_PATH` | MCP endpoint path, default `/mcp` |
| `MCP_AUTH_RESOURCE_NAME` | display name in the metadata |

### Knowing who is calling

Every request that passes gets a `Caller` in its extensions: the token's `sub` (stable per
user, a good key for per-user memory), `client_id`, `username`, scopes and whether it acts for
a signed-in user. In an rmcp tool:

```rust
#[tool(description = "Greet the signed-in user")]
async fn hello(&self, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, McpError> {
    let caller = ctx
        .extensions
        .get::<http::request::Parts>()
        .and_then(mcp_cognito_auth::Caller::from_parts);
    // …
}
```

## On AWS Lambda

[`examples/lambda.rs`](examples/lambda.rs) is a complete server that runs the same binary
locally and on Lambda, and [`examples/template.yaml`](examples/template.yaml) deploys it with
its user pool, a service client, a user client and an HTTP API. Two rmcp settings matter on
Lambda, because each request may land on a different instance and arrives with API Gateway's
host name:

```rust
StreamableHttpServerConfig::default()
    .with_legacy_session_mode(false) // stateless: no Mcp-Session-Id to lose between instances
    .with_json_response(true)        // a plain JSON answer instead of an SSE stream
    .with_sse_keep_alive(None)
    .disable_allowed_hosts();        // the localhost-only DNS-rebinding check would refuse the API's host
```

Deploy:

```sh
cargo lambda build --release --arm64 --example lambda
(cd target/lambda/lambda && zip -q ../../hello.zip bootstrap)
aws s3 cp target/hello.zip s3://$BUCKET/hello.zip
aws cloudformation deploy --stack-name mcp-hello --template-file examples/template.yaml \
  --capabilities CAPABILITY_IAM \
  --parameter-overrides CodeBucket=$BUCKET CodeKey=hello.zip DomainPrefix=mcp-hello-$RANDOM
```

Then open MCP Inspector (`npx @modelcontextprotocol/inspector`), point it at the stack's
`McpUrl`, and give it the user client's id and secret: it reads the metadata, sends you to
Cognito's hosted sign-in with PKCE, and calls `hello` as you. Check the pieces with curl:

```sh
curl -si "$MCP_URL" -d '{}'                                       # 401 + WWW-Authenticate
curl -s  "${MCP_URL%/mcp}/.well-known/oauth-protected-resource/mcp"
curl -s  "${MCP_URL%/mcp}/.well-known/oauth-authorization-server"
```

## What it checks

- RS256 signature against the pool's JWKS (fetched on first use and again, at most once a
  minute, when a token names an unknown key, so key rotation needs no restart);
- `iss` is the pool, `exp` hasn't passed, `token_use` is `access` (ID tokens are refused);
- `client_id` is one of the configured app clients;
- the scope the JSON-RPC method needs. A granted `myapp/mcp:tools` satisfies a configured
  `mcp:tools`, so either spelling works.

## Limits

- Cognito has no dynamic client registration, so clients use pre-registered app clients
  (MCP Inspector, Claude custom connectors and Alexa+ all accept a client id and secret).
- The metadata names this server as the authorization server's `issuer`, as RFC 9728 and
  RFC 8414 require for the documents to match; tokens are still Cognito's.
- Request bodies are read once (default limit 1 MiB) to decide the scope, then passed on
  unchanged.

## Where it comes from

Extracted from [Trackside](https://github.com/zhangshen20/trackside), an Australian horse
racing form companion for Alexa+, where this code has guarded the live MCP endpoint
(`https://mcp.racingaidataset.com.au/mcp`) on Lambda since September 2026. Built during the
Amazon Developer "Build, Ship, Shape" hackathon as a reusable piece for anyone hosting an MCP
server on AWS.

## Develop

```sh
cargo test             # 12 tests: challenges, metadata, scope tiers, JWKS fetch, rmcp end to end
cargo clippy --all-targets -- -D warnings
cargo run --example lambda   # http://127.0.0.1:8000/mcp, open
```

## License

MIT
