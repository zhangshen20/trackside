#!/usr/bin/env python3
"""Cuts the demo video: frames from record.cjs, voices from voice.py, one MP4 out.

Each scene becomes a segment whose frames are held for as long as its voices need (the question
is typed while the narrator asks it, the answer frame stays up while Trackside speaks, the
narration after an answer plays over the screen, a tap lands as the narrator says it). Segments
are concatenated, narration and questions are burnt in as captions below the simulator, and the
result is loudness-normalised.

    render.py [OUT.mp4]      default work/trackside-demo.mp4
"""
import json
import os
import re
import subprocess
import sys

here = os.path.dirname(os.path.abspath(__file__))
work = os.environ.get("VIDEO_WORK", os.path.join(here, "work"))
seg_dir = os.path.join(work, "segments")
os.makedirs(seg_dir, exist_ok=True)
cfg = json.load(open(os.path.join(here, "scenes.json")))
timeline = json.load(open(os.path.join(work, "timeline.json")))
voices = json.load(open(os.path.join(work, "voices.json")))
out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(work, "trackside-demo.mp4")

# Narration, tap lines and caption flags come from scenes.json as it stands, so rewording a
# line or re-timing a tap needs no re-record; the recorded ask and answer stay with the frames.
_scenes = {s["id"]: s for s in cfg["scenes"]}
for _t in timeline:
    for _k in ("narration", "narration_before", "narration_after", "then", "caption"):
        _t.pop(_k, None)
        if _k in _scenes.get(_t["id"], {}):
            _t[_k] = _scenes[_t["id"]][_k]
timeline = [t for t in timeline if t["id"] in _scenes]
timeline.sort(key=lambda t: list(_scenes).index(t["id"]))

FPS = 30
# Pacing, in seconds.
IDLE = 0.5  # the simulator before the question starts typing
THINKING = 0.9  # "thinking" state; the real wait is Bedrock's and not worth watching
ANSWER_LEAD = 0.3  # the answer frame appears, then Trackside starts speaking
ANSWER_TAIL = 0.45  # the answer stays up after the voice stops
AFTER_TAIL = 0.5  # the screen stays up after the narration that follows an answer
HOVER = 0.9  # the tapped row's hover state while the narrator says "Tap a horse"
TAP_TAIL = 1.5  # the form view and its tool-call line stay up in silence
CARD_LEAD = 0.5
CARD_TAIL = 0.8
CARD_MIN = 2.6
MIN_CUE = 18  # characters; a shorter sentence joins the next caption cue


def voice(key):
    v = voices.get(key)
    return (v["file"], v["seconds"]) if v else (None, 0.0)


def cues(text):
    """One caption cue per sentence, short fragments joined to the next."""
    parts = [p for p in re.split(r"(?<=[.!?:])\s+", text.strip()) if p]
    out = []
    for p in parts:
        if out and len(out[-1]) < MIN_CUE:
            out[-1] = out[-1] + " " + p
        else:
            out.append(p)
    return out


class Segment:
    def __init__(self, scene):
        self.scene = scene
        self.frames = []  # (file, seconds)
        self.audio = []  # (file, start)
        self.captions = []  # (start, end, text)
        self.t = 0.0

    def hold(self, file, seconds):
        self.frames.append((file, max(seconds, 1 / FPS)))
        self.t += seconds

    def say(self, key, text, caption=True, at=None):
        """Plays a voice line; returns its length. Lines whose text is gone from scenes.json
        never play, even if voices.json still has them."""
        if not text:
            return 0.0
        file, secs = voice(key)
        if not file:
            return 0.0
        start = self.t if at is None else at
        self.audio.append((file, start))
        if caption:
            parts = [text] if text.startswith("You: ") else cues(text)
            total = sum(len(p) for p in parts) or 1
            t = start
            for p in parts:
                d = secs * len(p) / total
                self.captions.append((t, t + d, p))
                t += d
        return secs


def build(scene):
    seg = Segment(scene)
    sid = scene["id"]
    frames = scene["frames"]
    if scene["type"] == "card":
        secs = seg.say(f"{sid}-narration", scene.get("narration"), caption=scene.get("caption", True) is not False, at=CARD_LEAD)
        seg.hold(frames[0]["file"], max(CARD_MIN, CARD_LEAD + secs + CARD_TAIL))
        return seg
    by_kind = {}
    for f in frames:
        by_kind.setdefault(f["kind"], []).append(f["file"])
    idle = by_kind["idle"][0]
    before = seg.say(f"{sid}-before", scene.get("narration_before"))
    seg.hold(idle, IDLE + before + (0.4 if before else 0))
    # The question is typed over the narrator asking it.
    q_secs = seg.say(f"{sid}-ask", "You: " + scene["ask"])
    typing = by_kind["typing"]
    each = max(0.25, (q_secs + 0.2) / len(typing))
    for f in typing:
        seg.hold(f, each)
    seg.hold(by_kind["thinking"][0], THINKING)
    answer = by_kind["answer"][0]
    a_secs = seg.say(f"{sid}-answer", scene.get("answer"), caption=False, at=seg.t + ANSWER_LEAD)
    seg.hold(answer, ANSWER_LEAD + a_secs + ANSWER_TAIL)
    after = seg.say(f"{sid}-after", scene.get("narration_after"))
    then = (scene.get("then") or [{}])[0]
    taps = by_kind.get("tap", [])
    hover = by_kind.get("hover", [])
    if taps and then.get("say"):
        # The narration after the answer plays over the card, then the tap line: the hover
        # state shows as the narrator says "Tap a horse", the form view lands as the line ends.
        seg.hold(answer, after + (0.3 if after else 0))
        tap_secs = seg.say(f"{sid}-tap", then["say"])
        if hover:
            seg.hold(hover[0], HOVER)
            seg.hold(taps[-1], max(tap_secs - HOVER, 0.5) + TAP_TAIL)
        else:
            seg.hold(taps[-1], tap_secs + TAP_TAIL)
    elif taps:
        at = then.get("at", 0.5)
        total_after = after + AFTER_TAIL
        seg.hold(answer, total_after * at)
        for f in taps:
            seg.hold(f, total_after * (1 - at) / len(taps))
    elif after:
        seg.hold(answer, after + AFTER_TAIL)
    return seg


def encode(seg, path):
    # Each frame is a looped still of its own length, joined with the concat filter: exact timing,
    # unlike the concat demuxer's per-image durations. Simulator frames are scaled into a
    # 1760x990 bezel so the captions get a band of their own under the device; cards are not.
    cmd = ["ffmpeg", "-y", "-v", "error"]
    for file, secs in seg.frames:
        cmd += ["-loop", "1", "-framerate", str(FPS), "-t", f"{secs:.3f}", "-i", file]
    n = len(seg.frames)
    fit = ""
    if seg.scene["type"] == "sim":
        fit = "scale=1760:990:flags=lanczos,pad=1920:1080:80:0:color=0x0b1016,drawbox=x=79:y=-1:w=1762:h=992:color=0x263244:t=1,"
    filters = ["".join(f"[{i}:v]" for i in range(n)) + f"concat=n={n}:v=1:a=0,fps={FPS},{fit}format=yuv420p[vout]"]
    for j, (file, start) in enumerate(seg.audio):
        cmd += ["-i", file]
        filters.append(f"[{n + j}:a]adelay={int(start * 1000)}:all=1[a{j}]")
    if seg.audio:
        mix = "".join(f"[a{j}]" for j in range(len(seg.audio)))
        filters.append(f"{mix}amix=inputs={len(seg.audio)}:normalize=0:duration=longest,apad[aout]")
    else:
        filters.append("anullsrc=r=48000:cl=mono[aout]")
    cmd += [
        "-filter_complex", ";".join(filters),
        "-map", "[vout]", "-map", "[aout]",
        "-t", f"{seg.t:.3f}",
        "-c:v", "libx264", "-preset", "medium", "-crf", "18",
        "-c:a", "aac", "-b:a", "160k", "-ar", "48000",
        path,
    ]
    subprocess.run(cmd, check=True)


def probe(path):
    out = subprocess.check_output(
        ["ffprobe", "-v", "error", "-select_streams", "v", "-show_entries", "stream=duration", "-of", "csv=p=0", path],
        text=True,
    )
    return float(out.strip())


def srt_time(t):
    ms = int(round(t * 1000))
    return f"{ms // 3600000:02}:{ms // 60000 % 60:02}:{ms // 1000 % 60:02},{ms % 1000:03}"


def main():
    segments = []
    offset = 0.0
    captions = []
    for scene in timeline:
        seg = build(scene)
        path = os.path.join(seg_dir, scene["id"] + ".mp4")
        encode(seg, path)
        for start, end, text in seg.captions:
            captions.append((offset + start, offset + end, text))
        actual = probe(path)
        print(f"{scene['id']}: {actual:5.1f}s  (at {offset:5.1f}s)")
        offset += actual
        segments.append(path)
    print(f"total {offset:.1f}s")

    srt = os.path.join(work, "captions.srt")
    with open(srt, "w") as f:
        for n, (start, end, text) in enumerate(captions, start=1):
            f.write(f"{n}\n{srt_time(start)} --> {srt_time(max(start + 0.4, end - 0.15))}\n{text}\n\n")
    lst = os.path.join(work, "segments.txt")
    with open(lst, "w") as f:
        for p in segments:
            f.write(f"file '{p}'\n")
    # libass sizes in a 288-line reference frame: FontSize 8.5 is about 32 px at 1080p, and
    # MarginV 4 keeps a two-line cue inside the 90 px band under the simulator.
    style = (
        "FontName=Inter,FontSize=8.5,PrimaryColour=&H00F2F6FA,BackColour=&HB3000000,"
        "BorderStyle=3,Outline=0.7,Shadow=0,MarginV=4,MarginL=60,MarginR=60"
    )
    vf = f"subtitles={srt}:force_style='{style}',fade=t=in:st=0:d=0.5,fade=t=out:st={offset - 0.6:.2f}:d=0.6"
    subprocess.run(
        [
            "ffmpeg", "-y", "-v", "error", "-f", "concat", "-safe", "0", "-i", lst,
            "-vf", vf,
            "-af", "loudnorm=I=-16:TP=-1.5:LRA=11",
            "-r", str(FPS), "-pix_fmt", "yuv420p",
            "-c:v", "libx264", "-preset", "slow", "-crf", "19",
            "-c:a", "aac", "-b:a", "160k", "-ar", "48000",
            "-movflags", "+faststart",
            out,
        ],
        check=True,
    )
    print(f"wrote {out} ({offset:.1f}s)")


if __name__ == "__main__":
    main()
