# The submission video

`make.sh` records and cuts the hackathon demo video from the real thing: it builds the MCP
server and the simulator from this checkout, runs them locally on the live data snapshot (memory
in the real DynamoDB table, Claude on Bedrock choosing the tools), drives the simulator page with
Playwright, speaks the lines with Amazon Polly and renders one MP4 with ffmpeg. A re-cut after a
race day is one command.

```sh
demo/video/make.sh                      # -> demo/video/work/trackside-demo.mp4
VOICE_ENGINE=polly demo/video/make.sh   # insist on Polly (needs polly:SynthesizeSpeech)
VOICE_ENGINE=kokoro demo/video/make.sh  # offline neural voices (see Voices below)
SCENES=06-stable demo/video/make.sh     # re-record one scene, keep the rest
```

Needs cargo, node with `playwright` and a Chromium it can launch, python3 with boto3, ffmpeg,
espeak-ng (the placeholder voice) and AWS credentials (`TRACKSIDE_ROLE_ARN` is assumed when set)
that can read Trackside's bucket, read and write the memory table, call Bedrock and Polly.

## How it fits together

| File | Does |
| --- | --- |
| `scenes.json` | The storyboard: each scene is a title card or a question to the simulator, with the narration around it and what to tap. |
| `record.cjs` | Drives the simulator with Playwright and saves a screenshot per step (idle, typing, thinking, answer, hover, tap), plus the answer text. Keyframes rather than a screen recording keep the cut deterministic and hide Bedrock's latency. An `expect` pattern makes it ask again when the model answers about the wrong thing. |
| `awsx.py` | Runs a command with the deploy role's credentials, downloads the snapshot, and edits the demo listener's memory (`forget`, `last-checked`) between scenes. |
| `voice.py` | Speaks narration and questions in the narrator's voice and Trackside's answers in a second voice: Polly ("Matthew" and the Australian "Olivia", the simulator's own voice) when the role may call it, else Kokoro offline, else espeak-ng as a robotic placeholder, with a warning. |
| `render.py` | Holds each frame for as long as the voices need, mixes the audio, concatenates the scenes, burns in captions (one sentence at a time, in a band under the simulator) and normalises loudness. Narration and tap lines are read from `scenes.json` at render time, so rewording a line needs no re-record. |
| `cards/` | The title, "what I built", architecture, "how you use it", "one day later" and closing cards (HTML, rendered by Playwright). |
| `fonts.conf` | A fontconfig alias so Chromium draws the simulator's `system-ui` as Inter, the font the design uses, whatever the build box has installed. |

The listener's memory for the demo is the key the server uses without auth (`local`) in the real
`trackside-listeners` table; the `memory` field on a scene (`forget`, or `last-checked:2026-09-20`)
is applied before it is recorded, which is what turns "How's my stable?" into a catch-up since
that day. Nothing is faked on screen: every answer is what the server said to that question when
the frames were taken. The questions name a day ("today", "on Saturday") relative to Sydney time,
so check them in `scenes.json` before a re-cut.

## Voices

`auto` tries Amazon Polly first. When the AWS role is refused Polly, `voice.py` uses
[Kokoro](https://github.com/thewh1teagle/kokoro-onnx), an 82M-parameter neural voice that runs on
the CPU (about twice real time on four cores) with no network: `pip install kokoro-onnx soundfile`
and put `kokoro-v1.0.onnx` and `voices-v1.0.bin` from that project's "model-files-v1.0" release in
`$KOKORO_DIR` (default `work/kokoro`). The voices are picked in `scenes.json` (`kokoro`, `lang`,
`speed` per speaker; cut four uses `bm_george` for the narrator and `bf_emma` for Trackside).
espeak-ng is the last resort and sounds like a robot; a cut made with it is not for upload.

Outputs land in `work/` (ignored by git): `frames/`, `audio/`, `segments/`, `timeline.json`,
`voices.json`, `captions.srt` and the MP4.
