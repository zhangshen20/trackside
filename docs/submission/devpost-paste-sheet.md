# Trackside: Devpost paste sheet (11 Oct 2026)

Field by field for the entry at devpost.com/submit-to/30992-.../manage/submissions/1230820-trackside.
Brought up to date from devpost-draft.md: 13 tools, 36 smoke checks, Open Source repo published, judge login live,
every [UPDATE] marker resolved. Save as draft only; Submit is Shen's step after review.

Two values are NOT in this file and go only into testing.txt (zipped as trackside-testing.zip under "Upload a File"; the form has no private testing field):
the judge password (judge-account.md) and the trackside-user client secret (command in devpost-draft.md).

=====================================================================
TAB 1. PROJECT OVERVIEW
=====================================================================

## Project name
Trackside

## Elevator pitch (max 200 characters; this is 181)
A voice-first form guide for Australian horse racing on Alexa+. Ask who's running, how a horse is going, or who ran home fastest. Real Racing Australia data, no betting and no odds.

## Thumbnail
Upload /mnt/project-files/hackathon/screens/mcp-app-race-card.png (crop to 3:2 if Devpost asks).

=====================================================================
TAB 2. PROJECT DETAILS
=====================================================================

## About the project (Markdown box; paste everything between the lines)
-----8<-----
### Inspiration

Racing is one of Australia's biggest sports, and spring is its peak: the Caulfield Cup, the Cox Plate and the Melbourne Cup fall within three weeks. But almost every racing app is built to sell a bet. Fans who want to know who is running, how a horse has been going or why a race matters have to read around odds, promotions and bookmaker banners. We wanted the knowledgeable friend at the track instead: someone who knows the form, can explain a race in a sentence, and never mentions a price. Voice suits it well, because racing fans mostly ask short questions while doing something else.

### What it does

Trackside is an Alexa+ add-on built as a self-hosted MCP server. A fan can ask:

- "What racing is on at Caulfield on Sunday?" (meetings, track condition, first race)
- "Who's in race 8?" (the full field with barriers, jockeys, trainers and weights)
- "How has Jimmysstar been going?" (career, track, distance and going records, recent starts, last-600 m times)
- "Tell me about this race." (what the race is, why it matters, the form contenders)
- "Who won, and who ran home fastest?" (placings, margins, time and the fastest last 600 m from official sectional timing)
- "How's Ethan Brown riding lately?" (jockey or trainer wins and places)
- "Follow Extragalactic" and later "How's my stable?" (a personal watch list: where your horses are running next and how they went)
- "What's on in the Spring Carnival?" (the 2026 feature races)
- On a screen, Trackside draws its own answers with an MCP App: the race card with a colour-coded form strip, a results podium with the fastest last 600 m, and a horse's form. Tap a horse and the App calls Trackside for its form; tap Follow and it's in your stable.
- "How's my stable?" a week later: "Since you last checked on Tuesday, Extragalactic won at Caulfield." Trackside remembers each listener across sessions (followed horses, home state, when they last asked) and opens with what's new. "I'm in Sydney" puts NSW meetings first; "forget me" deletes it all.
- "How was it run?" Results say where the placegetters were at the 800 and the 400 and their last 600 m ("led throughout", "came from last"), form says where a horse usually settles, and explanations say which runners usually lead and which come from the back, never how this race will be run.
- Names the way they're heard: "Cofield" finds Caulfield, "Demo Myla" finds Demo Miler ("Taking Demo Myla as Demo Miler"), and when two horses sound the same Trackside asks "Did you mean A or B?" instead of guessing.
- "What's the next race?" and "I'm in Brisbane": start times are spoken in the listener's own state's clock and at the track ("12:30 pm Queensland time, 1:30 pm at the track"), with "due to jump in about 20 minutes" when it's close.
- Group 1 status read out ("the Epsom, a Group 1 race over 1600 metres"), from a list of Australia's Group 1 races, since the source carries no grade. Wagering products are gone from race names too ("Edward Manifold Stakes", not "ONE POOL Edward Manifold Stakes").

Every answer is short enough to be spoken, names its source ("According to Racing Australia…"), and also returns structured content for screen devices. There is no betting anywhere: no odds, no tips, no bookmaker names. Price fluctuations are dropped when the data is read, and wagering brands are stripped from race and venue names.

### How we built it

The architecture diagram is in docs/architecture.mmd and renders on GitHub.

- **MCP server in Rust** (axum + rmcp), MCP spec 2025-11-25 over Streamable HTTP, thirteen tools with fan-language descriptions so a model picks the right one, each with a title and MCP behaviour hints (read-only, destructive) and a plain input schema that passes MCP Inspector's portability check.
- **On AWS**: an arm64 Lambda (`provided.al2023`) behind an API Gateway HTTP API with throttling, a private S3 bucket for the data snapshot, a custom domain with an ACM certificate, all in one CloudFormation stack deployed by a single script.
- **OAuth 2.1 the way Alexa+ expects it**: Amazon Cognito issues a client-credentials token (`mcp:service`) for discovery and an authorization-code + PKCE (S256) token (`mcp:tools`) for tool calls. Unauthenticated requests get a 401 pointing at RFC 9728 protected-resource metadata, and the server publishes RFC 8414 authorization-server metadata. Each signed-in user has their own follow list. That memory lives in a DynamoDB table keyed by the Cognito subject, so it survives across sessions and Lambda instances.
- **Real data**: a Rust ingester reads Racing Australia fields and form, official results and state sectional timing from an existing read-only archive, and builds one compact snapshot. It never reads prices.
- **Kept fresh and watched**: an EventBridge schedule rebuilds the snapshot twice a day (after the evening's results and with the morning's final fields), and an hourly probe checks the public endpoint, the Cognito sign-in metadata, the simulator and the snapshot's age, with CloudWatch alarms on any failure.
- **Testing**: a protocol smoke test (36 checks, including "no answer mentions betting, odds or a bookmaker" and "no holes in spoken text"), MCP Inspector runs with a real Cognito sign-in, and natural-language checks through a Claude custom connector as a stand-in for Alexa+.
- **Amazon Bedrock** rewords `explain_race`'s facts (Claude Haiku 4.5 through the `au.` inference profile, so requests stay in Australia) into a few spoken sentences a newcomer can follow. The model sees only the retrieved facts, never prices, and an answer with a betting word in it falls back to the tool's own template.
- **A web simulator** (`crates/trackside-sim`) plays the Alexa+ conversation for the demo, because the Alexa+ toolkit is US-only: Bedrock chooses Trackside's MCP tools, Amazon Polly's Australian voice speaks the answers, and the page hosts the MCP App like an Echo Show would.

### Challenges we ran into

- **Alexa+ testing is US-only.** The MCP toolkit requires `distributionCountries: ["US"]`, and we're in Australia, so we couldn't test on a real Alexa+ device. We tested the exact protocol and auth Alexa+ uses with MCP Inspector and a real model through a custom connector, and built a simulator for the demo.
- **Cognito and the MCP auth metadata don't line up.** Alexa+ wants `/.well-known/oauth-authorization-server` with `code_challenge_methods_supported: ["S256"]`; Cognito only publishes OpenID configuration without that field. The server publishes its own metadata document pointing at Cognito. Cognito also prefixes custom scopes (`trackside/mcp:tools`), so the server accepts both forms.
- **Streamable HTTP on Lambda.** API Gateway HTTP APIs buffer responses, so server-sent-event streams don't work. We run the server stateless with plain JSON responses, which suits Alexa+'s request-response tool calls.
- **Making data speakable.** Real racing data has empty fields, legalese prize strings, barrier trials mixed with race starts and venue codes like "CAUL". Each one produced a sentence that sounded wrong aloud; the smoke test now fails on holes in spoken text.
- **Keeping gambling out.** Racing data is soaked in prices. Keeping them out took design at the boundary (the wire types don't declare price fields at all) rather than filtering at the end.

### Accomplishments that we're proud of

- A real, public, OAuth-protected MCP server that follows the Alexa+ auth model end to end, verified with a real user sign-in.
- Live Australian racing data including sectional timing, which lets Trackside answer "who ran home fastest", a question fans ask and most apps don't answer.
- A racing product with no betting in it at all.

### What we learned

- The tool description is the interface. Which tool a model picks, and with what arguments, depends far more on a description written in a fan's words than on the schema.
- Voice answers have to be tested by ear. "0.8 L", "VIC" and "2:02.41" pass every schema check and sound wrong, so the smoke test now fails on unspeakable text.
- Memory changes the product. Once Trackside remembers your horses and your state, every answer can open with what matters to you, and a single-question bot becomes a companion.
- Standard OAuth pays off. Because Alexa+ uses plain OAuth 2.1 metadata, MCP Inspector and a Claude connector could test the exact auth path, and the Cognito gap became a reusable open-source crate.

### What's next for Trackside

- Push alerts when a followed horse is entered or has run (memory across sessions is built; Alexa+ proactive notifications would deliver it).
- More jurisdictions (New Zealand, Hong Kong, the UK) as new ingesters, and Alexa+ certification once the toolkit opens outside the US.
-----8<-----

## Built with
rust, axum, rmcp, model-context-protocol, mcp-apps, alexa-plus, aws-lambda, amazon-api-gateway, amazon-cognito, amazon-s3, amazon-dynamodb, amazon-bedrock, amazon-polly, amazon-eventbridge, amazon-cloudwatch, amazon-sns, aws-cloudformation, aws-certificate-manager, amazon-route-53, oauth2, javascript

## "Try it out" links
https://github.com/zhangshen20/trackside
https://mcp.racingaidataset.com.au/sim
https://mcp.racingaidataset.com.au/mcp

## Image gallery (upload in this order, captions in brackets)
1. trackside repo docs/images/race-card.png  [The race card as an MCP App, on a simulated Echo Show]
2. docs/images/tap-a-horse.png  [Tap a horse: the App calls horse_form through the host]
3. docs/images/how-it-was-run.png  [How the race was run, from official sectional timing]
4. docs/images/stable.png  [How's my stable? Memory across sessions]
5. docs/images/explain.png  [Explain this race, grounded on Bedrock]
(Same files under /mnt/project-files/hackathon/screens/ for the first three.)

## Video demo link
LEAVE FOR SHEN. Upload /mnt/project-files/hackathon/video/trackside-demo-cut5-polly.mp4 to YouTube (Unlisted is fine)
with the title and description in hackathon/video/devpost-video-text.md, then paste the YouTube URL here.
Swap it for the Caulfield Cup re-cut after 17 Oct if you keep that plan.

=====================================================================
TAB 3. ADDITIONAL INFO (hackathon questions; match by meaning, the form's wording may differ)
=====================================================================

## Which track?
Alexa+

## Which mini-challenge(s)?
Tick AWS Builder and Open Source. If the form allows only one: AWS Builder.

## AWS Builder: which AWS services did you use, and how?
AWS Lambda (arm64, Rust custom runtime), Amazon API Gateway (HTTP API, custom domain), Amazon Cognito (user pool, OAuth 2.1 with PKCE, resource server scopes), Amazon S3, AWS CloudFormation, AWS Certificate Manager, Amazon Route 53, AWS IAM (least-privilege deploy role), Amazon Bedrock (Claude Haiku 4.5 via the `au.` inference profile), Amazon DynamoDB (listener memory), Amazon Polly (the simulator's voice), Amazon EventBridge (twice-daily data refresh and an hourly health probe), Amazon CloudWatch (logs, and alarms on the refresh, the probe, server errors and API 5xx), Amazon SNS (optional alarm email). The full account, with each service's template resource, job, IAM boundary and estimated cost, plus data residency in ap-southeast-2, is at https://github.com/zhangshen20/trackside/blob/main/docs/aws.md.

## Open Source: repository or contribution URL
https://github.com/zhangshen20/mcp-cognito-auth

## Open Source: GitHub username
zhangshen20

## Open Source: description
mcp-cognito-auth is a new MIT-licensed Rust crate, built during the hackathon, that lets any Rust MCP server use an Amazon Cognito user pool as its OAuth 2.1 authorization server. MCP clients discover sign-in from the server (RFC 9728 and RFC 8414 metadata), but Cognito only publishes OpenID discovery without PKCE methods, puts client_id where aud would be and prefixes custom scopes, so a pool can't be used directly. The crate verifies Cognito access tokens, serves both metadata documents with Cognito's endpoints, answers 401/403 with the challenges MCP clients expect, enforces the service-versus-user scope tiers Alexa+ uses (discovery with client_credentials, tool calls only for a signed-in user via PKCE) and hands each tool the caller's identity. It works with axum and rmcp's Streamable HTTP server, including stateless on AWS Lambda, and ships with a Lambda example, a CloudFormation template and 12 tests. It was extracted from Trackside's live endpoint so other Alexa+ builders on AWS can skip that work.

## Code repository URL
https://github.com/zhangshen20/trackside   (public, MIT; setup in README "Run locally" and docs/testing.md)

## Pre-existing work / was this started before 31 Aug?
Trackside was created from scratch on 28 Sep 2026, inside the submission window. It reads, read-only, from a daily archive of Australian racing data collected by the author's separate, pre-existing private data pipeline. That pipeline is not part of this submission and no code from it is used. Everything Alexa+-facing (MCP server, tools, OAuth layer, AWS stack, tests, simulator) is new. See docs/prior-work.md.

## Product feedback
The rules put the AWS Builder answer here too: say which AWS services were used and how.

**1. Which developer tools, APIs and SDKs did you use, and for what?**
- Alexa+ MCP Toolkit docs: the add-on contract (MCP 2025-11-25, Streamable HTTP, OAuth 2.1 + PKCE, `mcp:service` / `mcp:tools` scopes, policy rules).
- Model Context Protocol (rmcp Rust SDK) and MCP Inspector: the server and interactive testing.
- AWS Lambda (arm64, `provided.al2023`, Rust): runs the MCP server.
- Amazon API Gateway HTTP API: public front door, throttling and the custom domain.
- Amazon Cognito: the OAuth 2.1 authorization server (user pool, hosted sign-in, resource server scopes, client-credentials and PKCE clients).
- Amazon S3: the private data snapshot and Lambda artifacts.
- AWS CloudFormation: the whole stack in one template, deployed by one script.
- AWS Certificate Manager and Amazon Route 53: the TLS certificate and DNS for mcp.racingaidataset.com.au.
- AWS IAM: a least-privilege deploy role, separate from the read-only data archive.
- Amazon Bedrock: grounded race explanations, and the simulator's stand-in for Alexa+'s model.
- Amazon DynamoDB: each listener's stable, home state and last check, remembered across sessions.
- Amazon Polly: the simulator's Australian voice.
- Amazon EventBridge: the twice-daily snapshot refresh and the hourly health probe.
- Amazon CloudWatch: logs, and alarms if the refresh or probe fails, the probe goes quiet, or the server or API errors.

**2. What worked well?**
- Lambda on arm64 with a Rust binary: fast cold starts, and a stateless server suits Alexa+'s request-response tool calls.
- Cognito covered both OAuth flows Alexa+ needs (client credentials for discovery, authorization code + PKCE for tool calls) with no server of our own.
- CloudFormation plus one script gave a repeatable deploy we could run many times a day.
- The Alexa+ auth model (401 plus protected-resource metadata) is standard OAuth, so off-the-shelf tools like MCP Inspector could test it.

**3. What needs work?**
- Alexa+ MCP Toolkit is US-only (`distributionCountries: ["US"]`), with no documented path for non-US developers to test on a device or simulator.
- Cognito publishes no `/.well-known/oauth-authorization-server` and no `code_challenge_methods_supported`, so we had to publish our own metadata document. Cognito also prefixes scopes (`trackside/mcp:tools`), and it's unclear whether Alexa+ accepts that.
- API Gateway HTTP APIs buffer responses, so Streamable HTTP's SSE mode doesn't work through them, and the Alexa+ docs don't say which AWS front doors fit.
- Docs: MCP add-on requirements are spread over three pages, and two sidebar links returned 404 on 28 Sep.
- Rust on Lambda needs a third-party cross-compiler (cargo-lambda or cargo-zigbuild plus Zig).

**4. How was onboarding (zero to hello world)?**
MCP server running locally with Inspector: about an hour. Deployed on Lambda: most of a day, mostly spent on cross-compiling. OAuth that matches the Alexa+ model: most of a second day, mostly spent on the Cognito metadata gap.

**5. Would you build with these again?**
Yes. Lambda, API Gateway and Cognito were a fast, cheap home for an MCP server. We'd build for Alexa+ again, and we'd ship to real users as soon as the toolkit opens outside the US. A Cognito recipe in the Alexa+ docs would have saved a day.

## Feature requests (optional)
1. **Alexa+ MCP testing outside the US** (a developer preview or simulator). Without it, non-US builders can't check their add-on against real Alexa+ behaviour. *Critical.*
2. **A Cognito recipe for Alexa+ MCP auth**, or Cognito publishing RFC 8414 metadata with PKCE methods. Both are AWS services; it would save a day and a hand-written metadata document. *Important.*
3. **One requirements checklist page for MCP add-ons** (transport, auth, policy, tools). It would cut onboarding time and prevent missed policy rules. *Nice-to-have.*

## Friction log (optional bonus)
Paste the link, then the entries if the field is a text box:
https://github.com/zhangshen20/trackside/blob/main/docs/friction-log.md
23 entries (4 high, 5 medium, 14 low), each with task, steps, expected, actual, severity, workaround and suggestion.

## Testing instructions (the form has no private field)
The credentials go in testing.txt: fill its <<PASSWORD>> and <<CLIENT SECRET>> blanks, zip it as trackside-testing.zip and upload it under "Upload a File". The text below is the public version for the story; it names the judge email but never the password or secret.
-----8<-----
**Sign-in for judges:** email `judge@racingaidataset.com.au`; the password is in the uploaded file trackside-testing.zip.

**Quickest: the simulator.** Open https://mcp.racingaidataset.com.au/sim (it plays the Alexa+ side, labelled as a simulation), choose **Link account** and sign in with the details above. Then try: "Who won race 6 at Flemington on 3 October?" (The Next Episode, ridden by Mark Zahra, last 600 m in 33.58 s); "I'm in Brisbane", then "Show me the Caulfield Cup card" (the card opens as an MCP App and the start time is given in Queensland time and at the track); "Follow" on any horse in the card, then "How's my stable?" (the followed horse is remembered across sessions); and "What's the next race?". The tool-call panel shows each MCP call the model made. The data runs from 22 Sep 2026 to four days ahead and refreshes twice a day, so today's racing is in.

**As an MCP client (MCP Inspector, `npx @modelcontextprotocol/inspector`):** transport Streamable HTTP, URL https://mcp.racingaidataset.com.au/mcp. In the Auth panel use client ID `4j7nr8c6eum58ln4rf7v1i2u8f` and the client secret from trackside-testing.zip, then sign in with the judge account on Cognito's page. Try `get_race_card` with `{"venue":"Caulfield","race_number":8,"date":"2026-09-27"}`. The same client ID and secret work for a Claude.ai custom connector.

**Without signing in:** follow "Run locally" in the README, which uses a bundled demo fixture; start the server with `TRACKSIDE_TODAY=2026-10-14` so the fixture's "this Saturday" is the Caulfield Cup on any day.

Privacy: sign-in uses Amazon Cognito in the entrant's AWS account (Sydney). Only the email address is stored (the password is held by Cognito as a salted hash and never reaches Trackside); test accounts are deleted after judging or on request.
-----8<-----

=====================================================================
TAB 4. SUBMIT
=====================================================================
Shen only: tick the terms and press Submit after review. Not done by Claude.
