# Demo script: 60 seconds

This is the script, not a transcript of a recording. The presenter's lines are fixed. Trackside's lines are what the server answered when the scenes were first recorded on the live snapshot (early October 2026, the same questions as `demo/video/scenes.json`) and will differ in names, times and wording on a later snapshot; re-check each against the running server before recording. The last answer, the decline, comes from the host model (Alexa+ or the simulator's model), not from a tool, so it is written as the expected shape rather than a recorded line. The three-minute version of this demo and its storyboard live in `demo/video/`.

Setting: the web simulator at `/sim`, labelled "Simulated Alexa+ experience" on screen, signed in with a test account. The tool-call panel stays visible so every answer shows its MCP call.

| Time | Who | Line | On screen |
| --- | --- | --- | --- |
| 0:00 | You | "Who ran home fastest in race 6 at Flemington on Saturday?" | The question types itself; `race_result` appears in the panel |
| 0:04 | Trackside | "The Next Episode ran home fastest, with the quickest last 600 metres in 33.58 seconds on the heavy track." | Podium card: placings, margins, fastest last 600 |
| 0:09 | Presenter | "Trackside is a form guide for Australian racing on Alexa+, built as an MCP server. No betting, no odds, no tips: the form, and a source on every answer." | Title card |
| 0:16 | You | "Follow The Next Episode." | `follow_horse` in the panel |
| 0:18 | Trackside | "Done. I'm following The Next Episode for you now." | Stable card |
| 0:20 | Card | Three days later. | Interstitial card |
| 0:22 | You | "How's my stable?" | `my_stable` in the panel |
| 0:24 | Trackside | "Since you last checked on Thursday, The Next Episode won at Flemington on Saturday ..." | Catch-up card: what each followed horse did since the last check |
| 0:29 | Presenter | "It remembers you between sessions: your horses, your state, when you last asked. That lives in DynamoDB, keyed by your account. Say 'forget me' and it's gone." | Same card |
| 0:35 | You | "Show me race 4 at Warwick Farm tomorrow." | `get_race_card` in the panel |
| 0:37 | Trackside | "Race 4 at Warwick Farm tomorrow is the Drinkwise Mile over 1600 metres with eight runners ..." | The server's own MCP App: the field, a colour-coded form strip, and a map of where each horse usually settles |
| 0:41 | Presenter | "On a screen Trackside draws its own answers: this race card is an MCP App served by the server. Tap a horse ..." | Tap Storm Season |
| 0:44 | Presenter | "... and the App calls the server for its form. That's the call in the panel." | The form view slides in; the panel shows `MCP App -> horse_form` |
| 0:47 | You | "Who won the Turnbull Stakes, race 8 at Flemington on Saturday?" | `race_result` in the panel |
| 0:49 | Trackside | "Cosmic Crusader won the Turnbull Stakes ... and ran the fastest last 600 metres in 36.3 seconds on the Heavy 8 track." | Podium card for a Group 1 |
| 0:53 | You | "Who should I back in race 8?" | No tool call: the model declines |
| 0:55 | Trackside | "That's not something Trackside does. I can give you the form instead: Cosmic Crusader won last start and ran home fastest; Storm Season has settled midfield in each of its last three runs." | Form cards for the named horses |
| 0:59 | Presenter | "The form, nothing else. Trackside." | Closing card: repository and licence |

## Word budget

The presenter speaks about 90 words in 60 seconds, leaving room for Trackside's answers (about 110 words) and two seconds of silence around each card. If the server's answers run long on the day, trim the presenter's two middle lines first; the hook, the memory scene, the tap on the race card, the Group 1 result and the decline are the five things the sixty seconds must show.

## Lines that must never change

No line, from either voice, mentions a price, a bookmaker or who will win. `scripts/check-docs.sh` checks this file for those words, and the server's own `explain_race` guard drops any model answer that uses them.
