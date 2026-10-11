# Devpost: the video fields

Devpost takes the demo video as a YouTube or Vimeo link (the "Video demo link" field) and shows it at the top of the entry. Nothing is uploaded to Devpost itself. Shen's steps when the final cut is ready:

1. Upload the MP4 to YouTube as **Unlisted** or Public (Unlisted is fine: the rules only need the link to be viewable). Title and description below.
2. Paste the link into the Devpost project's video field.

## YouTube title

Trackside: an Alexa+ form guide for Australian racing (Build, Ship, Shape 2026 demo)

## YouTube description

Trackside is an Alexa+ add-on, built as a self-hosted MCP server, that answers a racing fan's questions about Australian thoroughbred racing: who's running, how a horse has been going, who ran home fastest, and what's new with the horses you follow. It remembers each listener across sessions, draws race cards on screen with an MCP App, and never mentions a price: no betting, no odds, no tips.

Shown in this video: a simulated Alexa+ experience (the web simulator in the repo) driving the real Trackside MCP server on real Racing Australia data. Rust on AWS Lambda behind API Gateway, OAuth 2.1 with PKCE through Amazon Cognito, DynamoDB for memory, Amazon Bedrock and Amazon Polly in the simulator.

Code (MIT): https://github.com/zhangshen20/trackside
Built for the Build, Ship, Shape: Amazon Developer Hackathon 2026, Alexa+ track.

## Line for the Devpost "What it does" or testing section (optional)

The demo video shows a simulated Alexa+ experience (allowed by the Alexa+ track rules) talking to the live Trackside MCP server; every answer in it is the server's real answer on real data.

## Uploaded

Cut five is on YouTube (Unlisted) at https://youtu.be/RCE1BqnIQek, uploaded 11 Oct 2026, and that link is in the Devpost video field. A later cut needs a new upload and a new link.

## Cuts

- `trackside-demo-cut1-placeholder-voice.mp4` (4 Oct, 2:37): first full cut. All scenes recorded from the real server and simulator on data through 5 Oct; **the voices are a placeholder (espeak)** until Polly can be called from the build box. Not for upload.
- `trackside-demo-cut2-placeholder-voice.mp4` (4 Oct, 2:14): cut two after a judge-panel review of cut one. New order (hook, title, MCP App race card with a tap, follow, days later, stable catch-up, architecture, close), captions in a band under the simulator, Inter font. Still the placeholder voice. Not for upload.
- `trackside-demo-cut3-placeholder-voice.mp4` (5 Oct, 2:11): cut two with the stable scene re-recorded a day later, after Hellcibell (the horse followed in the race card scene) ran second at Muswellbrook: "One day later" is now literally true. Still the placeholder voice. Not for upload.
- `trackside-demo-cut4.mp4` (11 Oct, 2:52): Shen's 11 Oct asks: an intro before the example (what was built, how it runs on AWS, how a listener uses it) and natural voices (Kokoro neural voices, offline, since Polly is still refused). Uploadable, superseded by cut five.
- `trackside-demo-cut5-polly.mp4` (11 Oct, 2:33): cut four re-spoken with Amazon Polly after Shen granted polly:SynthesizeSpeech: narrator "Matthew", Trackside in the Australian "Olivia" (the simulator's own voice). **This is the file to upload** until the Caulfield Cup re-cut.
- Final cut: after the Caulfield Cup (17 Oct), with Polly voices, by running `demo/video/make.sh` in the repo.
