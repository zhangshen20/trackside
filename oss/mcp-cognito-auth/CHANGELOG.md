# Changelog

## 0.1.0 (October 2026)

First release, extracted from Trackside's MCP server.

- `CognitoAuth`: verifies Cognito access tokens (JWKS with refetch on unknown key ids, issuer,
  expiry, `token_use`, app client allow-list) on any axum router.
- Two-tier scopes: a service scope for discovery and a user scope for `tools/call` (configurable
  with `user_methods`), batches included.
- `401` / `403` challenges with `resource_metadata`, `scope` and `error`.
- RFC 9728 protected-resource metadata (bare and path-suffixed) and RFC 8414
  authorization-server metadata pointing at Cognito's endpoints, PKCE `S256`.
- `Caller` in request extensions for tools to read.
- `from_env` configuration; Lambda example and CloudFormation template.
