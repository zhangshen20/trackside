# Friction log

Kept from day one for the hackathon's friction-log score. One entry per snag with the tooling around the build: the Alexa+ MCP toolkit, AWS, and the developer tools we used. Every entry records the task, the steps, what we expected against what happened, a severity, the workaround and a suggestion. `scripts/lint-friction.py` checks that shape in CI.

## Severity scale

- **low**: cost under an hour or a few lines of code, and the fix or workaround was obvious once seen.
- **medium**: cost hours, changed a design choice, or leaves a question the docs cannot answer.
- **high**: blocked a required part of the build for a day or more, or left no path to the real target.

## Index

| Date | Area | Severity | One line | Status |
| --- | --- | --- | --- | --- |
| 2026-09-28 | Alexa+ toolkit | high | Add-on testing is documented for the US only | worked around |
| 2026-09-28 | Alexa+ docs | low | Requirements split across three pages | worked around |
| 2026-09-28 | Alexa+ docs | low | Two sidebar pages return 404 | open |
| 2026-09-28 | Alexa+ auth | low | No dynamic client registration; clients pre-registered by hand | worked around |
| 2026-09-28 | Lambda | low | Rust for arm64 Lambda needs a cross-compiler from outside the AWS toolchain | worked around |
| 2026-09-28 | API Gateway | low | HTTP API buffers responses, so no SSE streaming | worked around |
| 2026-09-30 | Cognito | medium | Cognito publishes no RFC 8414 metadata with S256 | worked around |
| 2026-09-30 | Cognito | low | Scope names carry Cognito's resource-server prefix | open |
| 2026-09-30 | Build environment | low | developer.amazon.com blocked from the cloud build box | worked around |
| 2026-09-30 | Alexa+ toolkit | high | No device or simulator path for Australian developers | worked around |
| 2026-09-30 | Bedrock SDK | low | JSON to Smithy Document conversion by hand | worked around |
| 2026-09-30 | Bedrock | low | Finding the right model ID in Sydney | worked around |
| 2026-09-30 | IAM | low | Inference profiles need foundation-model ARNs in every routed region | worked around |
| 2026-09-30 | Model behaviour | low | Model picked carnival_guide for a named race | worked around |
| 2026-09-30 | Bedrock | high | "Operation not allowed" with no next step | worked around |
| 2026-09-30 | Bedrock SDK | low | Tool schemas patched into object schemas before Converse | worked around |
| 2026-09-30 | API Gateway | medium | 30 s integration ceiling bounds the simulator's agent loop | open |
| 2026-10-03 | Bedrock | high | Three model-access gates, each hidden behind the last | worked around |
| 2026-10-03 | Service Quotas | medium | A zero quota surfaces as ValidationException | worked around |
| 2026-10-04 | Alexa+ docs | medium | Nothing says whether Alexa+ screens host MCP Apps | open |
| 2026-10-04 | EventBridge | low | Rule cron is UTC only; drifts an hour at daylight saving | worked around |
| 2026-10-04 | Lambda | low | No call to retire warm instances; the refresh touches the description | worked around |
| 2026-10-04 | Claude Code | medium | Remote Control refused the deploy script as a production deploy | worked around |

## 2026-09-28

### Alexa+ MCP Toolkit availability

- **Task:** Find how to test Trackside as an Alexa+ add-on from Australia.
- **Steps:** (1) Read the toolkit overview and the quickstart. (2) Checked the `addon.json` requirements. (3) Searched the docs for a developer-preview or non-US path.
- **Expected:** A developer-preview path for non-US developers, or a clear statement of what is possible from outside the US.
- **Actual:** The overview states the toolkit is available in the United States and the quickstart requires `"distributionCountries": ["US"]` in `addon.json`. We found no documented path to a real Alexa+ device or the web simulator from Australia.
- **Severity:** high, because the target surface could not be reached for the whole build.
- **Workaround:** MCP Inspector for protocol checks, plus our own web simulator for the demo (see the 2026-09-30 entry on demoing from outside the US).
- **Suggestion:** A worldwide developer-preview path, or a line on the overview saying what a non-US developer can and cannot do.

### Policy pages are split

- **Task:** Collect every requirement an MCP add-on must meet.
- **Steps:** (1) Read "Policy requirements", where the content rules for gambling-related add-ons sit. (2) Read the quickstart, which holds the transport and auth rules. (3) Read "Functional requirements", which holds the tool rules. (4) Merged the three into one list by hand.
- **Expected:** One page listing every requirement for an MCP add-on.
- **Actual:** The requirements sit on three pages under different headings, and nothing links them as one checklist.
- **Severity:** low, because it cost about an hour of reading and nothing was missed.
- **Workaround:** Read all three pages and merged them by hand.
- **Suggestion:** A single "requirements checklist" page for MCP add-ons, linked from the overview.

### Docs 404s

- **Task:** Read the MCP design guide and the authentication page linked from the overview sidebar.
- **Steps:** (1) Followed the sidebar link to `add-ons/mcp-design-guide.html`. (2) Followed the sidebar link to `add-ons/authentication.html`. Both on 2026-09-28.
- **Expected:** Both pages to load.
- **Actual:** Both returned 404.
- **Severity:** low, because the quickstart covered transport and auth, and the build did not depend on the design guide.
- **Workaround:** Took the auth requirements from the quickstart. Nothing replaced the design guide.
- **Suggestion:** Fix or remove the two sidebar links, and give each page a reachable URL before it is linked.

### Dynamic client registration not supported

- **Task:** Register Trackside's OAuth client so Alexa+ can sign a listener in.
- **Steps:** (1) Read the quickstart's auth section. (2) Declared the Cognito user pool, resource server and app clients in `deploy/trackside.yaml`. (3) Read the sign-in client's ID and secret back with `describe-user-pool-client` to hand to a host.
- **Expected:** A worked example of pre-registering a client with Cognito.
- **Actual:** The quickstart says dynamic client registration and OIDC are not supported, so the OAuth 2.1 server must be pre-registered, and leaves the how to the reader. Cognito needs an app client created ahead of time.
- **Severity:** low, because a pre-registered client is one template resource.
- **Workaround:** The user pool, resource server and three app clients are declared in `deploy/trackside.yaml`; the sign-in client's ID and secret are handed to Alexa+, Inspector or Claude by hand.
- **Suggestion:** A worked Cognito example in the Alexa+ MCP docs covering the pre-registered client.

### Rust on Lambda needs a cross-compiler

- **Task:** Build the MCP server for the `provided.al2023` arm64 runtime from an x86 build box.
- **Steps:** (1) Read the Lambda Rust docs, which assume `cargo lambda`. (2) Installed `cargo-zigbuild` and Zig. (3) Built for `aarch64-unknown-linux-gnu` in `deploy/deploy.sh`.
- **Expected:** An official build image or a `sam build` path for Rust.
- **Actual:** The runtime wants an aarch64 Linux binary. From an x86 box that means `cargo-lambda` or `cargo-zigbuild` plus Zig, and neither is in the AWS toolchain.
- **Severity:** low, because one extra tool solved it on day one.
- **Workaround:** `pip install cargo-zigbuild ziglang`; `deploy/deploy.sh` runs `cargo zigbuild --target aarch64-unknown-linux-gnu.2.34`.
- **Suggestion:** An official Rust build image, or Rust support in `sam build`.

### API Gateway HTTP APIs buffer responses

- **Task:** Serve MCP Streamable HTTP through an API Gateway HTTP API to Lambda.
- **Steps:** (1) Read the Streamable HTTP transport, which may answer with a server-sent-events stream. (2) Read how an HTTP API Lambda proxy integration returns. (3) Ran the server stateless with plain JSON responses.
- **Expected:** A note in the Alexa+ MCP quickstart on which AWS front doors fit Streamable HTTP.
- **Actual:** An HTTP API integration returns only when Lambda finishes, so a server-sent-events stream cannot pass through.
- **Severity:** low, because Alexa+ tool calls are request-response and stateless JSON serves them.
- **Workaround:** The server runs stateless with plain JSON responses: always on Lambda (`AWS_LAMBDA_RUNTIME_API` is set), and locally with `TRACKSIDE_STATELESS=1`.
- **Suggestion:** A note in the quickstart on which AWS front doors carry Streamable HTTP's SSE responses and which do not.

## 2026-09-30

### Cognito and the MCP auth metadata don't line up

- **Task:** Publish the OAuth metadata Alexa+ reads to discover the authorization server.
- **Steps:** (1) Read the Alexa+ requirement: `/.well-known/oauth-authorization-server` listing `code_challenge_methods_supported: ["S256"]`. (2) Fetched Cognito's `/.well-known/openid-configuration`. (3) Compared the two.
- **Expected:** Cognito to publish RFC 8414 metadata the Alexa+ docs accept, or a Cognito recipe in the Alexa+ MCP docs.
- **Actual:** Cognito publishes only OpenID discovery, without `code_challenge_methods_supported`.
- **Severity:** medium, because the MCP server had to take on serving authorization-server metadata itself.
- **Workaround:** The MCP server publishes its own RFC 8414 document pointing at Cognito's endpoints; that code is now the `oss/mcp-cognito-auth` crate.
- **Suggestion:** A Cognito recipe in the Alexa+ MCP docs, since both are AWS, or RFC 8414 metadata from Cognito itself.

### Scope names

- **Task:** Name the scopes Alexa+ asks for.
- **Steps:** (1) Read the docs' `mcp:service` and `mcp:tools`. (2) Created them as Cognito resource-server scopes. (3) Read the scope strings Cognito put in the tokens.
- **Expected:** The docs to say whether the exact strings are required.
- **Actual:** Cognito always prefixes custom scopes with the resource server (`trackside/mcp:tools`), and the docs do not say whether Alexa+ expects the bare strings.
- **Severity:** low, because the server works with Cognito's names and the question is only what Alexa+ will send.
- **Workaround:** The server is configured with Cognito's prefixed names (`TRACKSIDE_SCOPE_*` in `deploy/trackside.yaml`) and advertises them in its metadata; its scope check (`has_scope` in `oss/mcp-cognito-auth`) also lets a prefixed grant satisfy a bare configured name.
- **Suggestion:** The Alexa+ docs to state whether scope strings are matched exactly or by suffix, with a Cognito example.

### Docs unreachable from our cloud build box

- **Task:** Check requirements against the Alexa+ docs while building in a cloud session.
- **Steps:** (1) Fetched `developer.amazon.com` pages from the build environment. (2) Fell back to web search.
- **Expected:** The docs to be reachable, or the environment to list the hosts it blocks.
- **Actual:** `developer.amazon.com` was blocked by the build environment's network policy.
- **Severity:** low, because search-result excerpts carried the rules we needed.
- **Workaround:** Checked requirements from search-result excerpts of the docs.
- **Suggestion:** Allow `developer.amazon.com` in the build environment's network policy; a plain-text or offline copy of the Alexa+ MCP docs would also help.

### No way to demo an Alexa+ add-on from outside the US

- **Task:** Show Trackside working on an Alexa+ surface for the demo.
- **Steps:** (1) Searched for a device or simulator path for Australian developers. (2) Found none. (3) Built `crates/trackside-sim`.
- **Expected:** A web-based Alexa+ test console for add-ons, usable worldwide.
- **Actual:** No device or simulator path for Australian developers.
- **Severity:** high, because the demo cannot show Alexa+'s own model or voice.
- **Workaround:** Our own simulator: Bedrock Converse with Trackside's MCP tools passed through as Bedrock tools, the browser's speech recognition for voice in, and at first the browser's speech synthesis for voice out (Amazon Polly's Olivia voice since 2026-10-04). It is our stand-in, not Alexa+'s model or voice.
- **Suggestion:** A web-based Alexa+ test console for add-ons, usable worldwide.

### MCP tool schemas into Bedrock tool specs, by hand

- **Task:** Pass the MCP server's tool list to Bedrock Converse as tool specs.
- **Steps:** (1) Read `ToolInputSchema::Json`, which takes a Smithy `Document`. (2) Wrote `to_document` and `to_json` in `crates/trackside-sim/src/agent.rs`. (3) Converted every tool definition, tool input and tool result through them.
- **Expected:** `From` impls between `serde_json::Value` and `aws_smithy_types::Document` in the SDK, or an MCP-to-Converse helper.
- **Actual:** The SDK offers neither; the conversion is by hand in both directions.
- **Severity:** low, because it is two small functions with a round-trip test.
- **Workaround:** The two conversion functions, pinned by a round-trip unit test.
- **Suggestion:** `From` impls in the SDK, or an MCP-to-Converse helper, for the MCP servers Alexa+ developers build.

### Picking a Bedrock model ID in Sydney

- **Task:** Choose the model ID for Claude Haiku 4.5 in ap-southeast-2.
- **Steps:** (1) Tried the bare foundation-model ID. (2) Compared the `apac.`, `au.` and `global.` inference profiles. (3) Ran `aws bedrock list-inference-profiles`.
- **Expected:** The console's model page to show the profile to use per region.
- **Actual:** The same Claude model appears as a foundation model and as three inference profiles, and on-demand calls to the bare model ID fail for newer models.
- **Severity:** low, because one CLI call found `au.anthropic.claude-haiku-4-5-20251001-v1:0`.
- **Workaround:** The `au.` profile, which keeps requests in Australian regions; it is the default for `BedrockModel` and `SimModel` in `deploy/trackside.yaml`.
- **Suggestion:** The console's model page naming the profile to use per region, and an error on the bare ID that names the profile.

### IAM for inference profiles

- **Task:** Give the Lambda roles the least permission that lets them call through the `au.` profile.
- **Steps:** (1) Allowed `bedrock:InvokeModel` on the profile ARN. (2) Calls failed. (3) Added the foundation-model ARNs.
- **Expected:** A documented minimal policy per profile.
- **Actual:** A Lambda calling through an inference profile needs `bedrock:InvokeModel` on both the profile ARN and the foundation-model ARNs in every region the profile routes to.
- **Severity:** low, because the policy is a few lines once known.
- **Workaround:** Allow `arn:aws:bedrock:*::foundation-model/anthropic.*` in all regions, in `deploy/trackside.yaml`.
- **Suggestion:** A documented minimal policy per inference profile, or the profile ARN alone being enough.

### Tool choice for named races

- **Task:** Have the model answer "tell me about the Caulfield Cup" with `explain_race`.
- **Steps:** (1) Asked the question in the simulator. (2) Read the tool-call panel.
- **Expected:** `explain_race`, after a lookup of the race's venue, date and number.
- **Actual:** The model called `carnival_guide` instead of `explain_race`, because it did not know the race number.
- **Severity:** low, because one sentence in the tool description fixed it.
- **Workaround:** The `explain_race` description in `crates/trackside-mcp/src/tools.rs` ends: "Given only a race's name, find its venue, date and race number with list_meetings first."
- **Suggestion:** The Alexa+ design guide showing how to chain lookup and detail tools.

### Bedrock says "Operation not allowed" with no next step

- **Task:** Make the simulator's first Converse call from our account.
- **Steps:** (1) Called Converse and InvokeModel for Claude and for Amazon Nova from an IAM role allowed `bedrock:InvokeModel`. (2) Checked `ListFoundationModels`, which listed the models. (3) Checked the exception type.
- **Expected:** An error naming the cause (model access not enabled, account under review, quota of zero) and the console page that fixes it.
- **Actual:** Every call failed with `ValidationException: Operation not allowed`. It is not an IAM denial (that would be `AccessDeniedException`), and the message does not say what to do.
- **Severity:** high, because every Bedrock call was blocked until 2026-10-03.
- **Workaround:** Built and tested the conversation loop against a scripted model and the real MCP server, and raised it with the account owner. Resolved on 2026-10-03 (see the three gates entry).
- **Suggestion:** An error that names the missing step and links the console page that fixes it.

### Tool schemas patched for Bedrock Converse

- **Task:** Pass the MCP tools' input schemas to Bedrock Converse unchanged.
- **Steps:** (1) Read `ToolInputSchema::Json(Document)` in the SDK. (2) Compared the `tools/list` schemas with what Converse accepts. (3) Added `tool_schema` in `crates/trackside-sim/src/agent.rs`.
- **Expected:** Any JSON Schema an MCP server publishes to be accepted as a tool spec, or the SDK type to say what shape it wants.
- **Actual:** The SDK takes an untyped `Document`, so nothing checks the shape before the call. The simulator's own comment records the rule: "Bedrock wants an object schema; MCP servers may leave out `type` or `properties`." Schemars also adds a `$schema` line to every schema.
- **Severity:** low, because a ten-line function covers it.
- **Workaround:** `tool_schema` adds `type: object` and an empty `properties` when they are missing and drops `$schema` before every Converse call; the unit test `schemas_become_objects` pins it.
- **Suggestion:** A typed tool-schema builder in the SDK, or a Converse error that names the offending tool and key.

### API Gateway caps a simulator turn at 30 seconds

- **Task:** Run a whole simulator turn (model call, tool calls, model call again) behind the HTTP API.
- **Steps:** (1) Set the simulator Lambda's timeout and its integration timeout in `deploy/trackside.yaml`. (2) Read the agent loop in `crates/trackside-sim/src/agent.rs`. (3) Read what the page does when the request fails.
- **Expected:** A longer integration timeout, or response streaming, for a Lambda that drives a model loop.
- **Actual:** HTTP API integrations time out at 30 s at most. The simulator's Lambda runs 29 s under a 30 000 ms integration (the MCP server: 20 s under 25 000 ms). A turn may make up to five tool rounds (`MAX_ROUNDS`), each a Converse call plus tool calls; the Converse call has no timeout of its own in `agent.rs`, and the simulator's HTTP client gives each MCP or Cognito call 20 s. The MCP server's own Bedrock call in `explain_race` is capped at 6 s and falls back to the template sentence. A turn that outruns 29 s ends as a Lambda timeout and the page shows "Sorry, something went wrong", not the simulator's own "took too many steps" answer.
- **Severity:** medium, because a slow model turn fails at the gateway with no spoken answer.
- **Workaround:** A 29 s function timeout under the 30 s ceiling, a fast model (Haiku 4.5) by default, a 20 s HTTP client timeout, five rounds at most, and the 6 s cap on the server's Bedrock call.
- **Suggestion:** A per-turn deadline in `respond` that returns a short spoken answer before the function times out; from AWS, a documented pattern for model loops behind HTTP API (a longer integration timeout or streaming).

## 2026-10-03

### Three gates before the first Bedrock call, each hidden behind the last

- **Task:** Clear `Operation not allowed` and make the first Converse call succeed.
- **Steps:** (1) Checked Service Quotas for Claude in ap-southeast-2. (2) Filled in the Anthropic use-case form from the Bedrock console. (3) Found the AWS Marketplace subscription step. (4) The account owner sent one message to Haiku 4.5 from the Bedrock Playground as admin. (5) Retried from Lambda.
- **Expected:** One "model access" page that lists the three steps with their state, and an error that names the one that is missing.
- **Actual:** Three separate things, each showing itself only once the previous one was cleared. (1) On a new account every Claude quota in ap-southeast-2 was 0 requests per minute, with no quota error in the response. (2) The Anthropic use-case form, filled in once per account. (3) An AWS Marketplace subscription per Anthropic model, created on the first successful call, which needs a principal allowed `aws-marketplace:Subscribe`; our least-privilege Lambda roles cannot do that. Any other model (Opus, Sonnet) needs its own subscription first.
- **Severity:** high, because it cost a day and the error named none of the three causes.
- **Workaround:** The account owner sent one message to Haiku 4.5 from the Bedrock Playground as admin, and the same model worked from Lambda a minute later.
- **Suggestion:** One model-access page with the three steps and their state, and an error that names the missing one.

### The quota page doesn't say the quota is the problem

- **Task:** Find out why Converse failed while the Claude quota was 0.
- **Steps:** (1) Read the exception type. (2) Compared it with the Service Quotas console values for Claude in ap-southeast-2.
- **Expected:** A throttling or quota error, and a non-zero default for a new account.
- **Actual:** With a quota of 0, Converse fails as `ValidationException` rather than `ThrottlingException`, so nothing points at Service Quotas.
- **Severity:** medium, because the error type sends the reader to the wrong place.
- **Workaround:** Cleared the quota as the first of the three gates above.
- **Suggestion:** A `ThrottlingException` or quota error for a zero quota, and a non-zero default on new accounts.

## 2026-10-04

### MCP Apps on an Echo Show are undocumented

- **Task:** Show Trackside's race card on the screen when Alexa+ runs on a device with one, such as an Echo Show.
- **Steps:** (1) Searched the Alexa+ MCP toolkit docs we could reach for MCP Apps, `io.modelcontextprotocol/ui` and `_meta` UI keys. (2) Built the App (`crates/trackside-mcp/static/race-card.html`) and pointed five tools at it. (3) Made the simulator an MCP Apps host.
- **Expected:** The docs to say whether Alexa+ hosts MCP Apps on devices with screens, and which `_meta` key it reads.
- **Actual:** Nothing found. The MCP Apps extension has had two spellings of the key, so `app_meta()` in `crates/trackside-mcp/src/tools.rs` sets both `_meta.ui.resourceUri` and `_meta["ui/resourceUri"]` (its comment: `ui/resourceUri` is the key earlier hosts read). Whether an Echo Show renders the App is unknown.
- **Severity:** medium, because the screen experience of five tools cannot be verified on the target.
- **Workaround:** Both keys in `_meta`; the simulator hosts the App; every tool's spoken text stands alone, so a host that ignores the App still answers.
- **Suggestion:** A line in the Alexa+ MCP docs on MCP Apps: supported or not, on which devices, and which `_meta` key.

### EventBridge rule cron has no time zone

- **Task:** Refresh the racing snapshot after the day's results and again before the first race, Melbourne time.
- **Steps:** (1) Wrote `RefreshSchedule` (`AWS::Events::Rule`) in `deploy/trackside.yaml`. (2) Set `ScheduleExpression: cron(0 12,20 * * ? *)`.
- **Expected:** A time zone on the rule, so 22:00 and 06:00 Melbourne stay put across daylight saving.
- **Actual:** A rule's cron is UTC only. 12:00 and 20:00 UTC are 22:00 and 06:00 in Melbourne on standard time and 23:00 and 07:00 once daylight saving starts (Sunday 2026-10-04), so both runs drift an hour.
- **Severity:** low, because the template's hour of slack covers the drift.
- **Workaround:** Times chosen with an hour of slack, and the drift written into the template's comment ("about 10 or 11 pm", "about 6 or 7 am").
- **Suggestion:** Move the rule to EventBridge Scheduler (`AWS::Scheduler::Schedule`), which takes a `ScheduleExpressionTimezone`; or the rules console and docs saying up front that `cron()` is UTC.

### Lambda has no call to retire warm instances

- **Task:** Make the running MCP function serve a snapshot just uploaded to S3, without redeploying code.
- **Steps:** (1) Looked for a Lambda API call that retires warm execution environments. (2) Found none. (3) Used `UpdateFunctionConfiguration` on the function description.
- **Expected:** An API call or CLI command that recycles a function's warm instances.
- **Actual:** No such call. A configuration change retires the warm instances, so `touch()` in `crates/trackside-snapshot/src/main.rs` and the end of `deploy/deploy.sh` both set the MCP function's description to a timestamped string through `UpdateFunctionConfiguration`. For this the refresh role holds `lambda:UpdateFunctionConfiguration` on the MCP function, which also lets it change environment variables, memory and timeout.
- **Severity:** low, because the touch works and runs twice a day.
- **Workaround:** The description touch, in both the refresh function and the deploy script.
- **Suggestion:** A narrow Lambda action that retires warm instances, or a documented idiom with its minimal IAM.

### Remote Control refused the deploy script as a production deploy

- **Task:** Run `deploy/deploy.sh` from a fresh Claude Code Remote Control session on the developer's Mac on 2026-10-04.
- **Steps:** (1) Asked the session to run the script. (2) The session's permission check refused it as a production deploy, before any AWS call or prompt. (3) Noted that an earlier Remote Control session on the same Mac, started on 2026-09-30, had been allowed to run the same script and still was.
- **Expected:** The same script from the same repository to be treated the same way across sessions, with the decision left to the developer.
- **Actual:** Refused outright as a production deploy: the script runs `aws cloudformation deploy`, which applies the stack with nothing to review first, and the new session had no record of the earlier approval.
- **Severity:** medium, because the day's deploys needed a second path.
- **Workaround:** Routed every deploy through the earlier Remote Control session, whose approval still held; the thread that wanted the deploy sends a note and that session runs `deploy/deploy.sh`, the refresh and the smoke test.
- **Suggestion:** A plan mode in `deploy.sh` (`aws cloudformation deploy --no-execute-changeset`, or `create-change-set` then `describe-change-set`) so a reviewer, human or tool, sees the change set before anything is applied; and Remote Control remembering an allowed command across sessions in the same repository.
