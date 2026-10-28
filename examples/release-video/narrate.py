# SPDX-License-Identifier: AGPL-3.0-only
"""Local generic male Kokoro narration; no hosted TTS, keys or voice cloning."""
from pathlib import Path
import json
import hashlib
import soundfile as sf
from kokoro_onnx import Kokoro


def synthesize(run, plan, model_dir):
    run = Path(run)
    model_dir = Path(model_dir)
    out = run / "audio"
    out.mkdir(exist_ok=True)
    if (out / "segments.json").exists():
        raise FileExistsError(
            "Narration exists; use a new run to preserve earlier audio"
        )
    from bootstrap import ASSETS

    provenance = []
    for name, url, digest in ASSETS:
        if name == "go.zip":
            continue
        actual = hashlib.sha256((model_dir / name).read_bytes()).hexdigest()
        if actual != digest:
            raise ValueError("Unreviewed local narration model: " + name)
        provenance.append(dict(file=name, sha256=actual, url=url))
    (out / "voice-provenance.json").write_text(
        json.dumps(
            dict(
                voice=plan["voice"],
                speed=plan["speed"],
                engine="kokoro-onnx 0.6.1",
                assets=provenance,
            ),
            indent=2,
        ),
        encoding="utf-8",
    )
    engine = Kokoro(
        str(model_dir / "kokoro-v1.0.onnx"), str(model_dir / "voices-v1.0.bin")
    )
    entries = []
    texts = [(-1, plan["intro"])] + [
        (i, t) for i, c in enumerate(plan["chapters"]) for t in c["narration"]
    ]
    for index, (scene, text) in enumerate(texts):
        data, rate = engine.create(
            text, voice=plan["voice"], speed=plan["speed"], lang="en-us"
        )
        file = out / f"speech-{index:02d}.wav"
        sf.write(str(file), data, rate, subtype="PCM_16")
        entries.append(
            dict(
                scene=scene,
                file=file.name,
                text=text,
                duration=len(data) / rate,
                rate=rate,
            )
        )
        print(
            f'Voice {plan["voice"]}: clip {index+1}/{len(texts)} · {len(data)/rate:.2f}s',
            flush=True,
        )
    (out / "segments.json").write_text(json.dumps(entries, indent=2), encoding="utf-8")


if __name__ == "__main__":
    import sys

    synthesize(
        sys.argv[1],
        json.loads(Path(sys.argv[2]).read_text(encoding="utf-8")),
        sys.argv[3],
    )
