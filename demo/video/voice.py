#!/usr/bin/env python3
"""Speaks the demo video's lines: narration and questions in the narrator's voice, Trackside's
answers in "Olivia", the Australian voice the simulator itself uses.

Reads work/timeline.json (from record.cjs) and scenes.json; writes work/audio/*.wav and
work/voices.json with each line's duration. Lines whose text hasn't changed are kept.

    voice.py [--engine auto|polly|kokoro|espeak]

"polly" needs polly:SynthesizeSpeech (run under `awsx.py run` for the role); "kokoro" is an
offline neural voice (pip install kokoro-onnx soundfile, model files in $KOKORO_DIR, see
README); "espeak" is the robotic placeholder; "auto" (default) tries Polly, then Kokoro, then
espeak with a warning.
"""
import hashlib
import json
import os
import subprocess
import sys

here = os.path.dirname(os.path.abspath(__file__))
work = os.environ.get("VIDEO_WORK", os.path.join(here, "work"))
audio_dir = os.path.join(work, "audio")
os.makedirs(audio_dir, exist_ok=True)
cfg = json.load(open(os.path.join(here, "scenes.json")))
timeline = json.load(open(os.path.join(work, "timeline.json")))
# Narration is taken from scenes.json as it stands, so rewording a line needs no re-record.
_scenes = {s["id"]: s for s in cfg["scenes"]}
for _t in timeline:
    for _k in ("narration", "narration_before", "narration_after", "then", "caption"):
        _t.pop(_k, None)
        if _k in _scenes.get(_t["id"], {}):
            _t[_k] = _scenes[_t["id"]][_k]
engine = "auto"
if "--engine" in sys.argv:
    engine = sys.argv[sys.argv.index("--engine") + 1]


def lines():
    for s in timeline:
        if s.get("narration"):
            yield f"{s['id']}-narration", "narrator", s["narration"]
        if s.get("narration_before"):
            yield f"{s['id']}-before", "narrator", s["narration_before"]
        if s.get("ask"):
            yield f"{s['id']}-ask", "narrator", s["ask"]
        if s.get("answer"):
            yield f"{s['id']}-answer", "alexa", s["answer"]
        if s.get("narration_after"):
            yield f"{s['id']}-after", "narrator", s["narration_after"]
        for act in s.get("then") or []:
            if act.get("say"):
                yield f"{s['id']}-tap", "narrator", act["say"]


def duration(path):
    out = subprocess.check_output(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path], text=True
    )
    return float(out.strip())


def to_wav(src, dst):
    subprocess.run(["ffmpeg", "-y", "-v", "error", "-i", src, "-ar", "48000", "-ac", "1", dst], check=True)


_polly = None


def polly(text, voice, out):
    global _polly
    if _polly is None:
        import boto3

        _polly = boto3.client("polly", region_name=os.environ.get("AWS_REGION", "ap-southeast-2"))
    from xml.sax.saxutils import escape

    ssml = f'<speak><prosody rate="{voice.get("rate", "100%")}">{escape(text)}</prosody></speak>'
    r = _polly.synthesize_speech(
        Text=ssml, TextType="ssml", VoiceId=voice["polly"], Engine=voice["engine"], OutputFormat="mp3"
    )
    mp3 = out + ".mp3"
    with open(mp3, "wb") as f:
        f.write(r["AudioStream"].read())
    to_wav(mp3, out)
    os.remove(mp3)


_kokoro = None


def kokoro_dir():
    return os.environ.get("KOKORO_DIR", os.path.join(work, "kokoro"))


def kokoro(text, voice, out):
    """Kokoro-82M through onnxruntime: natural voices, no network, about 2x real time on 4 cores."""
    global _kokoro
    if _kokoro is None:
        from kokoro_onnx import Kokoro

        d = kokoro_dir()
        _kokoro = Kokoro(os.path.join(d, "kokoro-v1.0.onnx"), os.path.join(d, "voices-v1.0.bin"))
    import soundfile as sf

    samples, rate = _kokoro.create(text, voice=voice["kokoro"], speed=voice.get("speed", 1.0), lang=voice.get("lang", "en-gb"))
    sf.write(out + ".raw.wav", samples, rate)
    to_wav(out + ".raw.wav", out)
    os.remove(out + ".raw.wav")


def kokoro_ready():
    d = kokoro_dir()
    return os.path.exists(os.path.join(d, "kokoro-v1.0.onnx")) and os.path.exists(os.path.join(d, "voices-v1.0.bin"))


def espeak(text, voice, out):
    subprocess.run(
        ["espeak-ng", "-v", voice["espeak"], "-s", "178", "-p", "45", "-a", "150", "-w", out + ".raw.wav", text],
        check=True,
    )
    to_wav(out + ".raw.wav", out)
    os.remove(out + ".raw.wav")


def main():
    global engine
    voices_file = os.path.join(work, "voices.json")
    voices = json.load(open(voices_file)) if os.path.exists(voices_file) else {}
    warned = False
    for key, who, text in lines():
        voice = cfg["voices"][who]
        out = os.path.join(audio_dir, key + ".wav")
        digest = hashlib.sha1(f"{engine}|{who}|{text}".encode()).hexdigest()
        if voices.get(key, {}).get("hash") == digest and os.path.exists(out):
            continue
        used = engine
        if engine in ("auto", "polly"):
            try:
                polly(text, voice, out)
                used = "polly"
            except Exception as e:  # noqa: BLE001 - any Polly failure means the next voice down
                if engine == "polly":
                    raise
                engine = "kokoro" if kokoro_ready() else "espeak"
                if not warned:
                    print(f"WARNING: Polly unavailable ({e}); using {engine} instead", file=sys.stderr)
                    warned = True
                used = engine
        if used == "kokoro":
            kokoro(text, voice, out)
        if used == "espeak":
            espeak(text, voice, out)
        voices[key] = {"file": out, "seconds": duration(out), "hash": digest, "who": who, "text": text, "engine": used}
        print(f"{key}: {voices[key]['seconds']:.1f}s ({used})")
    json.dump(voices, open(voices_file, "w"), indent=2)
    total = sum(v["seconds"] for v in voices.values())
    print(f"{len(voices)} lines, {total:.0f}s of speech -> {voices_file}")


if __name__ == "__main__":
    main()
