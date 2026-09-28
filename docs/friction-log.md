# Friction log

Kept from day one for the hackathon's friction-log bonus. One entry per snag with the Amazon tooling: what we tried, what happened, what we expected, how we got past it.

## 2026-09-28

- **Alexa+ MCP Toolkit availability.** The toolkit overview states it is available in the United States and the quickstart requires `"distributionCountries": ["US"]` in `addon.json`. Building from Australia, we could not find a documented path to test on a real Alexa+ device or the web simulator. Expected: a developer-preview path for non-US developers, or a clear statement of what is possible. Workaround: MCP Inspector plus our own web simulator for the demo.
- **Policy pages are split.** Gambling rules sit under "Policy requirements", transport and auth under the quickstart, tool rules under "Functional requirements". A single "requirements checklist" page for MCP add-ons would have saved an hour.
- **Docs 404s.** `add-ons/mcp-design-guide.html` and `add-ons/authentication.html` (linked from the overview sidebar) returned 404 on 2026-09-28.
- **Dynamic client registration not supported.** The quickstart says DCR and OIDC are not supported, so the OAuth 2.1 server must be pre-registered. Cognito needs a manually created app client; fine, but worth a worked example in the docs.
