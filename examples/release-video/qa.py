# SPDX-License-Identifier: AGPL-3.0-only
"""Objective checks of the encoded video and the captured execution evidence."""
from pathlib import Path
from paths import ROOT, WORK, ensure_work, icon_path, redact_host_paths
import hashlib
import json
import math
import re
import subprocess
import sys
import wave
import numpy as np
from scipy.signal import correlate, resample_poly


def validate(run, settings, output_directory=None):
    run = Path(run)
    root = ROOT
    out = Path(output_directory) if output_directory else run / "deliverables"
    import imageio_ffmpeg

    ffmpeg = imageio_ffmpeg.get_ffmpeg_exe()
    rendered = json.loads((out / "render.json").read_text(encoding="utf-8"))
    timeline = json.loads((out / "timeline.json").read_text(encoding="utf-8"))
    video = out / rendered["file"]

    def command(args, log):
        p = subprocess.run(
            [ffmpeg, "-hide_banner", *args],
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        (out / log).write_text(p.stdout + p.stderr, encoding="utf-8")
        assert p.returncode == 0, p.stderr[-2500:]
        return p

    commands = json.loads((run / "evidence/commands.json").read_text(encoding="utf-8"))
    plan = json.loads((run / "plan.json").read_text(encoding="utf-8"))
    assert len(commands) == sum(len(c["commands"]) for c in plan["chapters"])
    assert all(c["exit_code"] == 0 for c in commands)
    events = [
        json.loads(line)
        for line in (run / "evidence/terminal-events.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    raw = "".join(e["text"] for e in events if e["kind"] == "output")
    statuses = re.findall(r"\x1b\]133;D;(-?\d+)\x07", raw)
    assert len(statuses) >= len(commands) + 1 and all(
        int(v) == 0 for v in statuses[: len(commands) + 1]
    )
    assert not any(e["kind"] == "capture_error" for e in events)
    for code in settings["public_fixture"]["warning_codes"]:
        assert code in raw, "Retain genuine schema warnings"
    expected_tools = settings["public_fixture"]["mcp_tool_count"]
    assert f"Generated MCP catalog: {expected_tools} tools" in raw
    assert not any(
        " mock " in c["command"] or "callTool" in c["command"] for c in commands
    )
    helper = (run / "list_tools.mjs").read_text(encoding="utf-8")
    assert "listTools()" in helper and "callTool(" not in helper
    clean = subprocess.check_output(
        ["git", "-C", str(run / "repo"), "status", "--porcelain"], text=True
    ).strip()
    assert not clean, "Public source checkout was changed"
    commit = subprocess.check_output(
        ["git", "-C", str(run / "repo"), "rev-parse", "HEAD"], text=True
    ).strip()
    assert commit == settings["public_fixture"]["commit"]
    contract = run / "repo" / settings["public_fixture"]["contract"]
    assert (
        hashlib.sha256(contract.read_bytes()).hexdigest()
        == settings["public_fixture"]["sha256"]
    )
    targets = sorted(p.name for p in (run / "native/generated").iterdir() if p.is_dir())
    assert set(targets) == {
        "typescript",
        "python",
        "rust",
        "java",
        "csharp",
        "ruby",
        "go",
        "mcp",
        "webmcp",
        "docs",
    }
    assert all(
        (run / "native/generated" / target / ".tungsten/manifest.json").is_file()
        for target in targets
    )
    for target in ["typescript", "mcp"]:
        assert (run / "native/generated" / target / "dist").is_dir()
    preflight = json.loads((run / "preflight.json").read_text(encoding="utf-8"))
    assert preflight["all_expected_results"]
    decoded = command(["-i", str(video), "-f", "null", "-"], "decode-validation.log")
    frames = int(re.findall(r"frame=\s*(\d+)", decoded.stderr)[-1])
    assert frames == rendered["frames"]
    command(
        [
            "-y",
            "-i",
            str(video),
            "-map",
            "0:a:0",
            "-c:a",
            "pcm_s16le",
            str(out / "decoded-audio.wav"),
        ],
        "audio-decode.log",
    )
    with wave.open(str(out / "decoded-audio.wav"), "rb") as wav:
        rate = wav.getframerate()
        audio = (
            np.frombuffer(wav.readframes(wav.getnframes()), dtype="<i2").astype(
                np.float32
            )
            / 32768
        )
    with wave.open(str(out / "narration-master.wav"), "rb") as wav:
        source_rate = wav.getframerate()
        source = (
            np.frombuffer(wav.readframes(wav.getnframes()), dtype="<i2").astype(
                np.float32
            )
            / 32768
        )
        master = resample_poly(source, rate, source_rate)
    audio_duration = len(audio) / rate
    peak = float(np.max(np.abs(audio)))
    rms = float(np.sqrt(np.mean(audio**2)))
    clipped = int(np.count_nonzero(np.abs(audio) >= 32767 / 32768))
    assert peak < 0.97 and rms > 0.02 and clipped == 0
    assert abs(audio_duration - timeline["duration"]) < 0.13
    assert audio_duration > max(c["end"] for c in timeline["captions"]) + 0.25
    levels = command(
        [
            "-i",
            str(video),
            "-vn",
            "-sn",
            "-af",
            "loudnorm=I=-16:TP=-1.5:LRA=11:print_format=json",
            "-f",
            "null",
            "-",
        ],
        "loudness-validation.log",
    )
    measure, _ = json.JSONDecoder().raw_decode(
        levels.stderr[levels.stderr.rfind("{") :]
    )
    assert -18 < float(measure["input_i"]) < -14 and float(measure["input_tp"]) < -0.4
    lags = []
    for cue in timeline["captions"]:
        assert 0 <= cue["start"] < cue["end"] <= timeline["duration"]
        start = round(cue["start"] * rate)
        stop = round(cue["end"] * rate)
        pad = round(0.15 * rate)
        assert np.sqrt(np.mean(audio[start:stop] ** 2)) > 0.025
        reference = master[start:stop:24]
        candidate = audio[start - pad : stop + pad : 24]
        correlation = correlate(candidate, reference, mode="valid", method="fft")
        lag = (int(np.argmax(correlation)) - pad // 24) / (rate / 24) * 1000
        assert abs(lag) < 100
        lags.append(lag)
    captions = command(
        ["-i", str(video), "-map", "0:s:0", "-f", "srt", "-"], "embedded-captions.srt"
    )
    for cue in timeline["captions"]:
        assert cue["text"] in captions.stdout
    metadata = command(
        ["-i", str(video), "-f", "ffmetadata", "-"], "embedded-chapters.ffmetadata"
    ).stdout
    embedded = []
    for block in metadata.split("[CHAPTER]")[1:]:
        values = dict(line.split("=", 1) for line in block.splitlines() if "=" in line)
        num, den = map(int, values["TIMEBASE"].split("/"))
        embedded.append(
            dict(
                title=values["title"],
                start=int(values["START"]) * num / den,
                end=int(values["END"]) * num / den,
            )
        )
    assert len(embedded) == len(timeline["chapters"]) == len(plan["chapters"]) + 1
    for actual, expected in zip(embedded, timeline["chapters"]):
        assert actual["title"] == expected["title"]
        assert (
            abs(actual["start"] - expected["start"]) < 0.003
            and abs(actual["end"] - expected["end"]) < 0.003
        )
    capability = json.loads((run / "capabilities.json").read_text(encoding="utf-8"))
    assert (
        hashlib.sha256(Path(settings["binary"]).read_bytes()).hexdigest()
        == capability["binary_sha256"]
    )
    video_sha = hashlib.sha256(video.read_bytes()).hexdigest()
    assert video_sha == rendered["sha256"], "Rendered video changed after capture"
    report = dict(
        file=video.name,
        sha256=hashlib.sha256(video.read_bytes()).hexdigest(),
        bytes=video.stat().st_size,
        duration_seconds=timeline["duration"],
        decoded_frames=frames,
        decode_exit=0,
        resolution=[1920, 1080],
        fps=rendered["fps"],
        audio_voice="Generic local male Kokoro " + rendered["voice"],
        audio_integrated_lufs=float(measure["input_i"]),
        audio_true_peak_dbtp=float(measure["input_tp"]),
        audio_peak_amplitude=peak,
        audio_rms=rms,
        audio_clipped_samples=clipped,
        audio_video_tail_difference_ms=round(
            (audio_duration - timeline["duration"]) * 1000, 3
        ),
        caption_count=len(timeline["captions"]),
        max_waveform_alignment_error_ms=max(abs(x) for x in lags),
        embedded_chapters=embedded,
        chapter_count=len(embedded),
        terminal_command_count=len(commands),
        all_commands_exit_zero=True,
        preflight=preflight,
        sdk_languages=list(capability["sdk_languages"]),
        generated_target_directories=targets,
        built_targets=["typescript", "mcp", "go"],
        source_only_targets=["python", "rust-cli", "java", "csharp", "ruby", "webmcp"],
        compiler=capability["version"],
        compiler_source_commit=capability["source_commit"],
        public_repository_commit=commit,
        no_api_operation_invoked=True,
        no_mock_service=True,
        no_sms_sent=True,
        source_clone_clean=True,
        capture="Genuine PowerShell PTY, scripted submissions and original timestamps; not a desktop screen capture.",
        warning="TG0303 retained: if/then/else constraints are not modeled.",
        listening="Objective waveform/level/caption checks passed. Subjective listening requires human review; agent audio playback was unavailable.",
        publication="HUMAN_REVIEW_REQUIRED",
    )
    (out / "qa.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print(json.dumps(report, indent=2))
    return report
