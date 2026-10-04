# The submission video

`make.sh` records and cuts the hackathon demo video from the real thing: it builds the MCP
server and the simulator from this checkout, runs them locally on the live data snapshot (memory
in the real DynamoDB table, Claude on Bedrock choosing the tools), drives the simulator page with
Playwright, speaks the lines with Amazon Polly and renders one MP4 with ffmpeg. A re-cut after a
race day is one command.

```sh
demo/video/make.sh                      # -> demo/video/work/trackside-demo.mp4
VOICE_ENGINE=polly demo/video/make.sh   # insist on Polly (needs polly:SynthesizeSpeech)
SCENES=06-racecard demo/video/make.sh   # re-record one scene, keep the rest
```

Needs cargo, node with `playwright` and a Chromium it can launch, python3 with boto3, ffmpeg,
espeak-ng (the placeholder voice) and AWS credentials (`TRACKSIDE_ROLE_ARN` is assumed when set)
that can read Trackside's bucket, read and write the memory table, call Bedrock and Polly.

## How it fits together

| File | Does |
| --- | --- |
| `scenes.json` | The storyboard: each scene is a title card or a question to the simulator, with the narration around it and what to tap. |
| `record.cjs` | Drives the simulator with Playwright and saves a screenshot per step (idle, typing, thinking, answer, tap), plus the answer text. Keyframes rather than a screen recording keep the cut deterministic and hide Bedrock's latency. An `expect` pattern makes it ask again when the model answers about the wrong thing. |
| `awsx.py` | Runs a command with the deploy role's credentials, downloads the snapshot, and edits the demo listener's memory (`forget`, `last-checked`) between scenes. |
| `voice.py` | Speaks narration and questions in the narrator's voice and Trackside's answers in Polly's "Olivia", the simulator's own voice. Falls back to espeak-ng with a warning. |
| `render.py` | Holds each frame for as long as the voices need, mixes the audio, concatenates the scenes, burns in captions and normalises loudness. |
| `cards/` | The title, "three days later", architecture and closing cards (HTML, rendered by Playwright). |

The listener's memory for the demo is the key the server uses without auth (`local`) in the real
`trackside-listeners` table; `awsx.py memory last-checked 2026-10-01` is what turns "How's my
stable?" into a catch-up since Thursday. Nothing is faked on screen: every answer is what the
server said to that question when the frames were taken.

Outputs land in `work/` (ignored by git): `frames/`, `audio/`, `segments/`, `timeline.json`,
`voices.json`, `captions.srt` and the MP4.
