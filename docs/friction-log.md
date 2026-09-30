# Friction log

Kept from day one for the hackathon's friction-log bonus. One entry per snag with the Amazon tooling: what we tried, what happened, what we expected, how we got past it.

## 2026-09-28

- **Alexa+ MCP Toolkit availability.** The toolkit overview states it is available in the United States and the quickstart requires `"distributionCountries": ["US"]` in `addon.json`. Building from Australia, we could not find a documented path to test on a real Alexa+ device or the web simulator. Expected: a developer-preview path for non-US developers, or a clear statement of what is possible. Workaround: MCP Inspector plus our own web simulator for the demo.
- **Policy pages are split.** Gambling rules sit under "Policy requirements", transport and auth under the quickstart, tool rules under "Functional requirements". A single "requirements checklist" page for MCP add-ons would have saved an hour.
- **Docs 404s.** `add-ons/mcp-design-guide.html` and `add-ons/authentication.html` (linked from the overview sidebar) returned 404 on 2026-09-28.
- **Dynamic client registration not supported.** The quickstart says DCR and OIDC are not supported, so the OAuth 2.1 server must be pre-registered. Cognito needs a manually created app client; fine, but worth a worked example in the docs.
- **Rust on Lambda needs a cross-compiler.** `provided.al2023` on arm64 wants an aarch64 Linux binary; from an x86 build box that means `cargo-lambda` or `cargo-zigbuild` plus Zig. Neither is in the AWS toolchain, and the Lambda Rust docs assume `cargo lambda`. Worked, but one more tool to learn on day one. Expected: an official build image or a `sam build` path for Rust. Workaround: `pip install cargo-zigbuild ziglang`.
- **API Gateway HTTP APIs buffer responses.** MCP Streamable HTTP can answer with a server-sent-events stream; an HTTP API integration returns only when Lambda finishes. We run the MCP server stateless with plain JSON responses on Lambda, which suits Alexa+'s request-response tool calls anyway. Expected: a note in the Alexa+ MCP quickstart on which AWS front doors fit Streamable HTTP.

## 2026-09-30

- **Cognito and the MCP auth metadata don't line up.** Alexa+ wants authorization-server metadata at `/.well-known/oauth-authorization-server` listing `code_challenge_methods_supported: ["S256"]`. Cognito publishes only `/.well-known/openid-configuration`, without that field. Workaround: the MCP server publishes its own RFC 8414 document pointing at Cognito's endpoints. Expected: a Cognito recipe in the Alexa+ MCP docs, since both are AWS.
- **Scope names.** The docs describe `mcp:service` and `mcp:tools` scopes; Cognito always prefixes custom scopes with the resource server (`trackside/mcp:tools`). We accept either form. Unclear whether Alexa+ expects the exact strings.
- **Docs unreachable from our cloud build box.** `developer.amazon.com` was blocked by the build environment's network policy, so requirements were checked from search-result excerpts of the docs.

