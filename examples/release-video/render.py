# SPDX-License-Identifier: AGPL-3.0-only
"""Conventional terminal video, measured chapters, captions and local narration."""
from pathlib import Path
from paths import ROOT, icon_path, mask_terminal_usernames
import functools
import hashlib
import json
import math
import subprocess
import sys
import wave

import numpy as np
from PIL import Image, ImageDraw, ImageFont


def render(run, plan):
    run = Path(run)
    root = ROOT
    import pyte, imageio_ffmpeg

    out = run / "deliverables"
    out.mkdir(exist_ok=True)
    output = out / f'tungsten-{plan["version"]}-comprehensive-terminal-review.mp4'
    if output.exists():
        raise FileExistsError(
            "Preserve this review video and use a new run for another render"
        )
    recording = json.loads(
        (run / "evidence/recording.json").read_text(encoding="utf-8")
    )
    sections = json.loads((run / "evidence/sections.json").read_text(encoding="utf-8"))
    events = [
        json.loads(s)
        for s in (run / "evidence/terminal-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    events = mask_terminal_usernames(events)
    speech = json.loads((run / "audio/segments.json").read_text(encoding="utf-8"))
    W, H, FPS, RATE = 1920, 1080, 20, 24000
    intro = plan["contents_seconds"]
    count = math.ceil((intro + recording["duration"]) * FPS)
    duration = count / FPS
    chapters = [{"title": "Contents", "start": 0}] + [
        {"title": s["title"], "start": intro + s["start"]} for s in sections
    ]
    for i, c in enumerate(chapters):
        c["end"] = chapters[i + 1]["start"] if i + 1 < len(chapters) else duration

    def clock(t):
        return f"{int(t)//60:02d}:{int(t)%60:02d}"

    (out / "chapters.txt").write_text(
        "\n".join(clock(c["start"]) + " " + c["title"] for c in chapters) + "\n",
        encoding="utf-8",
    )
    (out / "chapters.json").write_text(json.dumps(chapters, indent=2), encoding="utf-8")
    metadata = [
        ";FFMETADATA1",
        "title=Tungsten " + plan["version"] + " comprehensive terminal walkthrough",
        "comment=Review only. Real PTY recording; generic male Kokoro narration; no SMS or unattended publishing.",
    ]
    for c in chapters:
        metadata.extend(
            [
                "[CHAPTER]",
                "TIMEBASE=1/1000",
                "START=" + str(round(c["start"] * 1000)),
                "END=" + str(round(c["end"] * 1000)),
                "title=" + c["title"].replace("=", "\\="),
            ]
        )
    (out / "chapters.ffmetadata").write_text(
        "\n".join(metadata) + "\n", encoding="utf-8"
    )
    audio = np.zeros(round(duration * RATE), dtype=np.int16)
    cues = []
    for scene in [-1] + list(range(len(sections))):
        start = 0.3 if scene == -1 else intro + sections[scene]["start"] + 0.45
        limit = intro if scene == -1 else intro + sections[scene]["end"]
        for clip in [s for s in speech if s["scene"] == scene]:
            with wave.open(str(run / "audio" / clip["file"]), "rb") as wav:
                assert wav.getframerate() == RATE
                samples = np.frombuffer(wav.readframes(wav.getnframes()), dtype="<i2")
            end = start + len(samples) / RATE
            assert end < limit + 0.05, (
                "Narration exceeds captured chapter. Use a new run for a slower voice.",
                scene,
                end,
                limit,
            )
            offset = round(start * RATE)
            audio[offset : offset + len(samples)] = samples
            text = (
                clip["text"]
                .replace("zero point one point two", "0.1.2")
                .replace("Zero Text", "ZROtext")
                .replace("C sharp", "C#")
                .replace("JSON Schema twenty twenty twelve", "JSON Schema 2020-12")
                .replace("dot H T T P", ".http")
                .replace("dot rest", ".rest")
                .replace("L L M S dot text", "llms.txt")
            )
            cues.append(dict(scene=scene, start=start, end=end, text=text))
            start = end + 0.35
    with wave.open(str(out / "narration-master.wav"), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(RATE)
        wav.writeframes(audio.astype("<i2").tobytes())

    def srt(t):
        ms = round(t * 1000)
        return f"{ms//3600000:02d}:{ms//60000%60:02d}:{ms//1000%60:02d},{ms%1000:03d}"

    (out / "captions.srt").write_text(
        "\n\n".join(
            f"{i+1}\n{srt(c['start'])} --> {srt(c['end'])}\n{c['text']}"
            for i, c in enumerate(cues)
        )
        + "\n",
        encoding="utf-8",
    )
    (out / "timeline.json").write_text(
        json.dumps(
            dict(
                duration=duration,
                fps=FPS,
                intro=intro,
                captions=cues,
                chapters=chapters,
            ),
            indent=2,
        ),
        encoding="utf-8",
    )
    BG = "#0c0c0c"
    WHITE = "#dedede"
    MUTED = "#aaa"
    GREEN = "#8bca81"
    PURPLE = "#b0a5f4"
    import os

    fonts = Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts"

    @functools.lru_cache(None)
    def font(size, kind="regular"):
        mono = (
            "CascadiaMono.ttf"
            if (fonts / "CascadiaMono.ttf").is_file()
            else "consola.ttf"
        )
        return ImageFont.truetype(
            str(
                fonts
                / {"regular": "segoeui.ttf", "bold": "segoeuib.ttf", "mono": mono}[kind]
            ),
            size,
        )

    def wrap(text, size, width):
        d = ImageDraw.Draw(Image.new("RGB", (1, 1)))
        lines = []
        line = ""
        for word in text.split():
            test = (line + " " + word).strip()
            if line and d.textlength(test, font=font(size)) > width:
                lines.append(line)
                line = word
            else:
                line = test
        if line:
            lines.append(line)
        return "\n".join(lines)

    palette = {
        "default": WHITE,
        "black": "#0c0c0c",
        "red": "#e98585",
        "green": "#89ca79",
        "brown": "#dcc273",
        "blue": "#86a6ef",
        "magenta": "#bd9ae0",
        "cyan": "#90d3d6",
        "white": WHITE,
    }

    def fg(value):
        if value in palette:
            return palette[value]
        if len(value) == 6 and all(c in "0123456789abcdefABCDEF" for c in value):
            return "#" + value
        return WHITE

    MONO = font(29, "mono")
    CW = MONO.getlength("M")
    LH = 32

    @functools.lru_cache(maxsize=384)
    def row_image(cells):
        im = Image.new("RGB", (1810, LH), BG)
        d = ImageDraw.Draw(im)
        at = 0
        while at < len(cells):
            style = cells[at][1:]
            end = at + 1
            while end < len(cells) and cells[end][1:] == style:
                end += 1
            d.text(
                (at * CW, -1),
                "".join(x[0] for x in cells[at:end]),
                font=MONO,
                fill=fg(style[0]),
            )
            at = end
        return im

    base = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(base)
    d.rectangle((0, 0, W, 49), fill="#202020")
    logo = (
        Image.open(icon_path())
        .convert("RGBA")
        .resize((31, 31), Image.Resampling.LANCZOS)
    )
    base.paste(logo, (22, 9), logo)
    d.text(
        (68, 7),
        "Windows PowerShell  ·  Tungsten comprehensive walkthrough",
        font=font(25),
        fill=WHITE,
    )
    d.text(
        (1890, 10),
        plan["version"] + "  ·  REVIEW",
        font=font(24, "bold"),
        fill=PURPLE,
        anchor="ra",
    )
    d.line((22, 83, 1898, 83), fill="#3d3d3d")
    d.line((22, 864, 1898, 864), fill="#333")
    d.rectangle((0, 875, W, 1055), fill="#151515")
    d.text(
        (24, 1057),
        "Real PowerShell PTY · scripted inputs · original timing · local username masked",
        font=font(18),
        fill=MUTED,
    )
    d.text(
        (1895, 1057),
        "Local male Kokoro " + plan["voice"] + " · human review before publication",
        font=font(18),
        fill=MUTED,
        anchor="ra",
    )
    menu = base.copy()
    md = ImageDraw.Draw(menu)
    md.text(
        (35, 107),
        "Contents — jump using the embedded chapters or companion timestamps",
        font=font(33, "bold"),
        fill=WHITE,
    )
    md.text(
        (36, 157),
        "Video guide; timestamps are generated from the completed recording.",
        font=font(25),
        fill=MUTED,
    )
    for i, c in enumerate(chapters):
        y = 221 + i * 56
        md.text((55, y), clock(c["start"]), font=font(31, "mono"), fill=GREEN)
        md.text((190, y), c["title"], font=font(31), fill=WHITE)
    screen = pyte.Screen(100, 24)
    stream = pyte.Stream(screen)
    cursor = 0
    scene = 0
    last_input = -99
    frame_cache = {}

    def draw(t):
        nonlocal cursor, scene, last_input
        if t < intro:
            im = menu.copy()
            d = ImageDraw.Draw(im)
        else:
            elapsed = t - intro
            while cursor < len(events) and events[cursor]["t"] <= elapsed:
                event = events[cursor]
                if event["kind"] == "output":
                    stream.feed(event["text"])
                elif event["kind"] == "section":
                    scene = event["scene"]
                elif event["kind"] == "input":
                    last_input = event["t"]
                cursor += 1
            active_cue = next(
                (
                    i
                    for i, c in enumerate(cues)
                    if c["start"] - 0.03 <= t <= c["end"] + 0.07
                ),
                -1,
            )
            key = (cursor, scene, int(t * 2), elapsed - last_input < 1.1, active_cue)
            if frame_cache.get("key") == key:
                return frame_cache["image"]
            im = base.copy()
            d = ImageDraw.Draw(im)
            for r in range(24):
                cells = tuple(
                    (
                        screen.buffer[r][c].data,
                        screen.buffer[r][c].fg,
                        screen.buffer[r][c].bg,
                        screen.buffer[r][c].bold,
                    )
                    for c in range(100)
                )
                im.paste(row_image(cells), (29, 94 + r * LH))
            if not screen.cursor.hidden and int(t * 2) % 2 == 0:
                x = 29 + screen.cursor.x * CW
                y = 94 + screen.cursor.y * LH
                d.rectangle((x, y + 27, x + CW - 1, y + 29), fill=WHITE)
            if elapsed - last_input < 1.1:
                prompts = [r for r, s in enumerate(screen.display) if "PS demo>" in s]
                if prompts:
                    d.line(
                        (23, 94 + prompts[-1] * LH, 23, 94 + (prompts[-1] + 1) * LH),
                        fill=PURPLE,
                        width=3,
                    )
        title = "Contents" if t < intro else plan["chapters"][scene]["title"]
        d.text((25, 53), title, font=font(23), fill=MUTED)
        d.text(
            (1893, 54),
            clock(t) + "  ·  chaptered review copy",
            font=font(21, "mono"),
            fill=MUTED,
            anchor="ra",
        )
        cue = next((c for c in cues if c["start"] - 0.03 <= t <= c["end"] + 0.07), None)
        if cue:
            size = 34
            while size > 27 and len(wrap(cue["text"], size, 1800).splitlines()) > 4:
                size -= 1
            caption = wrap(cue["text"], size, 1800)
            assert len(caption.splitlines()) <= 4
            box = d.multiline_textbbox((0, 0), caption, font=font(size), spacing=5)
            y = 875 + (180 - (box[3] - box[1])) // 2 - 5
            d.multiline_text(
                (960, y),
                caption,
                font=font(size),
                fill="#f1f1f1",
                spacing=5,
                align="center",
                anchor="ma",
            )
        if t >= intro:
            frame_cache.update(key=key, image=im)
        return im

    ffmpeg = imageio_ffmpeg.get_ffmpeg_exe()
    args = [
        ffmpeg,
        "-n",
        "-f",
        "rawvideo",
        "-vcodec",
        "rawvideo",
        "-s",
        f"{W}x{H}",
        "-pix_fmt",
        "rgb24",
        "-r",
        str(FPS),
        "-i",
        "-",
        "-i",
        str(out / "narration-master.wav"),
        "-i",
        str(out / "captions.srt"),
        "-i",
        str(out / "chapters.ffmetadata"),
        "-map",
        "0:v:0",
        "-map",
        "1:a:0",
        "-map",
        "2:s:0",
        "-map_metadata",
        "3",
        "-map_chapters",
        "3",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        "-af",
        f"loudnorm=I=-16:TP=-1.5:LRA=11,apad=whole_dur={duration}",
        "-c:a",
        "aac",
        "-b:a",
        "160k",
        "-ar",
        "48000",
        "-c:s",
        "mov_text",
        "-metadata:s:s:0",
        "language=eng",
        "-movflags",
        "+faststart",
        "-t",
        str(duration),
        str(output),
    ]
    snapshots = {
        round((intro + s["start"] + 0.65 * (s["end"] - s["start"])) * FPS): i
        for i, s in enumerate(sections)
    }
    with (out / "render.log").open("w", encoding="utf-8") as log:
        p = subprocess.Popen(
            args,
            stdin=subprocess.PIPE,
            stdout=log,
            stderr=log,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            last_image = None
            frame_bytes = None
            for n in range(count):
                im = draw(n / FPS)
                if im is not last_image:
                    frame_bytes = im.tobytes()
                    last_image = im
                p.stdin.write(frame_bytes)
                if n == round(8 * FPS):
                    im.save(out / "contents.png")
                if n in snapshots:
                    im.save(out / f"chapter-{snapshots[n]+1:02d}.png")
                if n % (FPS * 15) == 0:
                    print(f"Rendered {n/FPS:.0f}/{duration:.1f}s", flush=True)
        finally:
            p.stdin.close()
        if p.wait(timeout=60):
            raise RuntimeError("FFmpeg render failed")
    (out / "render.json").write_text(
        json.dumps(
            dict(
                file=output.name,
                sha256=hashlib.sha256(output.read_bytes()).hexdigest(),
                duration=duration,
                frames=count,
                fps=FPS,
                chapters=len(chapters),
                voice=plan["voice"],
                publication="HUMAN_REVIEW_REQUIRED",
            ),
            indent=2,
        ),
        encoding="utf-8",
    )
    print("Rendered", output, flush=True)
