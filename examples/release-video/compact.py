# SPDX-License-Identifier: AGPL-3.0-only
"""Manual 1080p delivery copy with retained chapters, subtitles and narration."""
from pathlib import Path
import hashlib
import json
import shutil
import subprocess
from qa import validate


def compact(run, settings, max_bytes=16_000_000):
    import imageio_ffmpeg

    run = Path(run)
    out = run / "deliverables"
    report = json.loads((out / "qa.json").read_text(encoding="utf-8"))
    source = out / report["file"]
    destination = out / (source.stem + "-compact.mp4")
    if destination.exists():
        raise FileExistsError("Preserve this compact copy and use a new run")
    if hashlib.sha256(source.read_bytes()).hexdigest() != report["sha256"]:
        raise ValueError("Source differs from the verified video")
    ffmpeg = imageio_ffmpeg.get_ffmpeg_exe()
    master = out / "narration-master.wav"
    audio_filter = "loudnorm=I=-16:TP=-3:LRA=11"
    analysis = subprocess.run(
        [
            ffmpeg,
            "-hide_banner",
            "-i",
            str(master),
            "-af",
            audio_filter + ":print_format=json",
            "-f",
            "null",
            "-",
        ],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=True,
    )
    measured, _ = json.JSONDecoder().raw_decode(
        analysis.stderr[analysis.stderr.rfind("{") :]
    )
    audio_filter += (
        "".join(
            ":" + key + "=" + measured[value]
            for key, value in [
                ("measured_I", "input_i"),
                ("measured_TP", "input_tp"),
                ("measured_LRA", "input_lra"),
                ("measured_thresh", "input_thresh"),
                ("offset", "target_offset"),
            ]
        )
        + ":linear=true,volume=1dB"
    )
    args = [
        ffmpeg,
        "-hide_banner",
        "-n",
        "-i",
        str(source),
        "-i",
        str(master),
        "-map",
        "0:v:0",
        "-map",
        "1:a:0",
        "-map",
        "0:s:0",
        "-map_metadata",
        "0",
        "-map_chapters",
        "0",
        "-c:v",
        "libx264",
        "-preset",
        "slow",
        "-tune",
        "stillimage",
        "-crf",
        "25",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "64k",
        "-ar",
        "24000",
        "-ac",
        "1",
        "-af",
        audio_filter,
        "-c:s",
        "copy",
        "-t",
        str(report["duration_seconds"]),
        "-movflags",
        "+faststart",
        str(destination),
    ]
    with (out / "compact-encode.log").open("w", encoding="utf-8") as log:
        subprocess.run(args, stdout=log, stderr=log, check=True)
    if destination.stat().st_size >= max_bytes:
        raise ValueError(
            "Compact copy exceeds the delivery budget; retain it for review and revise settings in a new run"
        )
    check = out / "compact-validation"
    check.mkdir(exist_ok=False)
    for name in ["timeline.json", "narration-master.wav"]:
        shutil.copy2(out / name, check / name)
    rendered = json.loads((out / "render.json").read_text(encoding="utf-8"))
    rendered.update(
        file="../" + destination.name,
        sha256=hashlib.sha256(destination.read_bytes()).hexdigest(),
    )
    (check / "render.json").write_text(json.dumps(rendered, indent=2), encoding="utf-8")
    result = validate(run, settings, output_directory=check)
    result["source_video"] = source.name
    result["encoding"] = (
        "H.264 CRF25 slow/stillimage, original resolution/frame timing; AAC mono 64kbps, local male narration"
    )
    (out / "compact-qa.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    return result
