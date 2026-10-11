# Trackside submission video: script and storyboard

Cut five: 2:33 (limit 3:00). 1920x1080, English, no music (nothing to license), every answer recorded from the real Trackside server on real Racing Australia data. The simulator screen is labelled "Simulated Alexa+ experience" throughout, as the rules allow. Captions sit in a band under the simulator, one sentence at a time, so no caption ever covers the screen.

Structure, as Shen asked on 11 Oct: first what was built, how it is built on AWS and how a listener uses it; then the worked example.

What the judges score, and where each criterion is answered on screen:

| Criterion | Where in the video |
| --- | --- |
| Pass/fail: uses the track's tools | The "what I built" card names MCP 2025-11-25, Streamable HTTP and OAuth 2.1; every example scene shows a real MCP tool call in the side panel. |
| Technical | The architecture card (every AWS service and how they connect), the MCP App that fetches form through the host, memory across sessions (DynamoDB), sectional timing. |
| Design | A voice answer first, then the screen draws itself; one question, one short answer, a source on every card. |
| Impact | The opening: the form guide, not the bookmaker; who's running, how a horse is going, who ran home fastest, never a price. |
| Idea | The MCP App and memory carry the example, because the judges' guidance calls exactly those "creative" and a one-question bot "obvious". |

Voices: two voices only, both Amazon Polly neural since cut five (Shen granted polly:SynthesizeSpeech on 11 Oct): the narrator "Matthew" also reads the questions, and Trackside answers in the Australian "Olivia", the same voice the simulator uses. Cut four used Kokoro voices rendered offline; `VOICE_ENGINE=kokoro` still produces that if Polly is ever unavailable.

Hard boundary checked in every line: no betting, no odds, no bookmaker names, past form only.

## Scenes

Times are cut five's (Polly is a little quicker than Kokoro). The narration is final text; the answers are what Trackside actually said when recorded and change a little on each re-cut.

### 1. Title (0:00 - 0:16): what it is
Card: **Trackside**. "The form, not the odds. Ask who's running, how a horse is going, who ran home fastest, and it remembers the horses you follow."

Narrator: "This is Trackside: an Alexa Plus add-on for Australian horse racing, built as an MCP server. It's the form guide, not the bookmaker: who's running, how a horse is going, who ran home fastest, and never a price."

### 2. What I built (0:16 - 0:37)
Card: three panels. The server (an MCP server in Rust: 13 tools over Streamable HTTP, MCP 2025-11-25, OAuth 2.1 with PKCE). The data (real Racing Australia fields, results and sectional times from a read-only archive, rebuilt twice a day; prices never read). The difference (it remembers you: your stable, home state, last check; draws its own race card as an MCP App).

Narrator: "Twelve MCP tools that Alexa Plus calls over Streamable HTTP, behind OAuth 2.1. Real Racing Australia fields, results and sectional times, refreshed twice a day. And memory: the horses you follow, your home state, when you last checked, plus a race card it draws itself as an MCP App."

### 3. How it is built on AWS (0:37 - 0:58)
Card: the architecture diagram. Alexa+ → MCP over Streamable HTTP + OAuth 2.1 → API Gateway (custom domain) → Lambda (arm64, Rust, axum + rmcp) → S3 snapshot (Racing Australia archive via a snapshot Lambda on an EventBridge rule, twice a day) + DynamoDB memory; Cognito; the simulator beside it (Claude Haiku 4.5 on Bedrock, Polly). Service strip: Lambda, API Gateway, Cognito, S3, DynamoDB, EventBridge, Bedrock, Polly, ACM, Route 53.

Narrator: "It all runs on AWS in one CloudFormation stack: Rust on Lambda behind API Gateway, Cognito for sign-in, the data snapshot on S3 built by a second Lambda on an EventBridge schedule, memory in DynamoDB, and Claude on Bedrock playing Alexa Plus in the simulator."

### 4. How you use it (0:58 - 1:14)
Card: three steps. 1 Enable and link (add Trackside in the Alexa app, link your account: OAuth 2.1 through Cognito, which is what makes the memory yours). 2 Just ask ("Alexa, who won the Caulfield Cup, and who ran home fastest?"; on an Echo Show the answer comes with a race card). 3 Follow your horses ("Follow Hellcibell." … "Alexa, how's my stable?").

Narrator: "To use it: enable Trackside in the Alexa app and link your account, then just ask. Follow the horses you care about, and next time it tells you what you missed. Here a simulator stands in for Alexa Plus; every answer is the live server's."

### 5. The example begins (1:14 - 1:35)
Screen: the simulator, idle. The question types itself, the device thinks, the answer appears with the race_result call in the panel and the result card (placings, fastest last 600 m).

- Narrator: "Let's start with last Saturday at Flemington."
- You: "Who won race 6 at Flemington on Saturday, and who ran home fastest?"
- Trackside: "The Next Episode won race 6 at Flemington on Saturday, beating Natural Fling by three-tenths of a length, and The Next Episode also ran the fastest last 600 metres in 33.58 seconds."

### 6. The MCP App (1:35 - 2:10)
- You: "Show me race 4 at Muswellbrook today."
- Trackside: "Race 4 at Muswellbrook is the Coolmore Denman Cup ... There are eight runners: Imatruestar, Hellcibell, Navy Steel ..." The screen is the server's own MCP App: the race card with a colour-coded form strip and where the field usually settles.
- Narrator: "On screen, Trackside draws its own answers: this race card is an MCP App served by the Trackside server, with a form strip and where the field usually settles."
- The pointer hovers on Hellcibell, then taps. The panel shows "MCP App → horse_form" and the form view lands.
- Narrator: "Tap a horse, and the App fetches its form through the host."

### 7. Follow (2:10 - 2:17)
- You: "Follow Hellcibell."
- Trackside: "Done, I'm following Hellcibell for you."

### 8. "One day later" (2:17 - 2:21)
Card: "One day later… A new conversation, same listener, same horses." Narrator, uncaptioned: "One day later, in a new conversation."

### 9. Memory across sessions (2:21 - 2:43)
- You: "How's my stable?"
- Trackside: "Since you last checked on Sunday, Hellcibell ran a close second of eight at Muswellbrook on Monday. Do you want to know more about that horse or see what's running today?" The catch-up card shows that run (2nd, Mon 5 Oct, Muswellbrook), the very race the listener saw on the card the day before.
- Narrator: "That memory lives in DynamoDB against each listener's account, so Trackside opens with what you missed. Say 'forget me' and it's gone."

### 10. Close (2:43 - 2:52)
Card: **Trackside**. "The form, not the odds. Remembers the horses you follow. Draws its own race card as an MCP App." github.com/zhangshen20/trackside, MIT licence, "Alexa+ track · AWS Builder · Open Source". "Repo and a judge sign-in are in the submission."

Narrator: "Trackside: the form, not the odds. It remembers your horses, draws its own race card, and never mentions a price."

## Recording notes
- Example scenes recorded from the Trackside MCP server and simulator built from `main`, against the real-data snapshot, with Claude Haiku 4.5 on Bedrock choosing the tools. The race card and follow were recorded on Monday 5 Oct (Sydney) before Muswellbrook race 4, the catch-up that evening after the archive carried the result, with the listener's last check set to Sunday 4 Oct: "One day later" is literally true.
- The archive carried Muswellbrook's results only in the 20:00 UTC refresh (the 12:00 one had the metropolitan meetings), so a re-cut that depends on a day's result should plan for the evening refresh.
- Re-cut after the Caulfield Cup (17 Oct): change the questions in `demo/video/scenes.json` and run `demo/video/make.sh`; the intro cards need no change. `SCENES=06-stable demo/video/make.sh` re-records one scene and keeps the rest.
