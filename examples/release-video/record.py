# SPDX-License-Identifier: AGPL-3.0-only
"""Generic genuine PowerShell PTY recorder driven by a reviewed chapter plan."""
from pathlib import Path
from paths import ROOT, WORK, ensure_work, icon_path, redact_host_paths
import base64
import json
import os
import re
import shutil
import sys
import threading
import time


def record(run, settings, plan):
    run = Path(run)
    root = ROOT
    from winpty import PtyProcess

    evidence = run / "evidence"
    evidence.mkdir(exist_ok=True)
    narration = json.loads((run / "audio/segments.json").read_text(encoding="utf-8"))
    durations = {
        i: sum(s["duration"] for s in narration if s["scene"] == i)
        + 0.35 * sum(s["scene"] == i for s in narration)
        + 0.8
        for i in range(len(plan["chapters"]))
    }
    clock = time.perf_counter()
    guard = threading.Condition()
    events = []
    commands = []
    sections = []
    statuses = []
    stopping = False
    event_file = (evidence / "terminal-events.jsonl").open("w", encoding="utf-8")
    cast = (evidence / "session.cast").open("w", encoding="utf-8")
    cast.write(
        json.dumps(
            dict(
                version=2,
                width=100,
                height=24,
                timestamp=int(time.time()),
                title="Tungsten release walkthrough " + settings["version"],
            )
        )
        + "\n"
    )

    def emit(kind, text="", **extra):
        with guard:
            t = time.perf_counter() - clock
            e = dict(t=t, kind=kind, text=text, **extra)
            events.append(e)
            event_file.write(json.dumps(e, ensure_ascii=False) + "\n")
            event_file.flush()
            if kind in ["input", "output"]:
                cast.write(
                    json.dumps(
                        [t, "i" if kind == "input" else "o", text], ensure_ascii=False
                    )
                    + "\n"
                )
                cast.flush()
            return t

    def quote(s):
        return str(s).replace("'", "''")

    startup = (
        "$global:LASTEXITCODE=0; $ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); "
        "Remove-Module PSReadLine -ErrorAction SilentlyContinue; function global:prompt { [Console]::Write([char]27 + ']133;D;' + [string]$global:LASTEXITCODE + [char]7); [char]27+'[38;5;114mPS demo> '+[char]27+'[0m' }; "
        "function global:tungsten { & '"
        + quote(settings["binary"])
        + "' @args }; function global:go { & '"
        + quote(settings["go"])
        + "' @args }; function global:python { & '"
        + quote(sys.executable)
        + "' @args }"
    )
    env = dict(
        os.environ,
        NO_COLOR="1",
        PYTHONIOENCODING="utf-8",
        PYTHONUNBUFFERED="1",
        GIT_TERMINAL_PROMPT="0",
        GCM_INTERACTIVE="never",
        npm_config_progress="false",
        GOCACHE=str(WORK / "cache/go-build"),
        GOPATH=str(WORK / "cache/go-work"),
    )
    encoded = base64.b64encode(startup.encode("utf-16-le")).decode()
    proc = PtyProcess.spawn(
        [
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-NoExit",
            "-EncodedCommand",
            encoded,
        ],
        cwd=str(run),
        env=env,
        dimensions=(24, 100),
    )

    def reader():
        pending = ""
        try:
            while proc.isalive():
                s = proc.read(8192)
                if not s:
                    continue
                t = emit("output", s)
                pending += s
                matches = list(re.finditer(r"\x1b\]133;D;(-?\d+)\x07", pending))
                if matches:
                    with guard:
                        statuses.extend((t, int(m.group(1))) for m in matches)
                        guard.notify_all()
                    pending = pending[matches[-1].end() :]
                elif len(pending) > 2048:
                    pending = pending[-128:]
        except EOFError:
            pass
        except Exception as exc:
            if not stopping:
                emit("capture_error", str(exc))

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()

    def wait_after(count, timeout=180):
        deadline = time.monotonic() + timeout
        with guard:
            while len(statuses) <= count:
                left = deadline - time.monotonic()
                if left <= 0:
                    raise TimeoutError(
                        "Actual PowerShell command did not return to prompt"
                    )
                guard.wait(min(left, 0.5))
            return statuses[count]

    try:
        wait_after(0, 20)
        for i, chapter in enumerate(plan["chapters"]):
            start = emit("section", "", scene=i)
            for j, command in enumerate(chapter["commands"]):
                # Lockfiles were preflighted, then copied after generation and before npm ci.
                if command.startswith("npm.cmd"):
                    target = "typescript" if "typescript" in command else "mcp"
                    lockfile = WORK / "cache/locks" / f"{target}.package-lock.json"
                    shutil.copy2(
                        lockfile,
                        run / "native/generated" / target / "package-lock.json",
                    )
                with guard:
                    count = len(statuses)
                began = emit("input", command + "\r", key=f"{i}-{j}")
                proc.write(command + "\r")
                ended, code = wait_after(count)
                commands.append(
                    dict(
                        chapter=i,
                        key=f"{i}-{j}",
                        command=command,
                        start=began,
                        end=ended,
                        exit_code=code,
                    )
                )
                (evidence / "commands.json").write_text(
                    json.dumps(commands, indent=2), encoding="utf-8"
                )
                print(f"Chapter {i+1}, command {j+1}: exit {code}", flush=True)
                if code != 0:
                    raise RuntimeError(
                        f"Actual command failed: {command} (exit {code})"
                    )
                time.sleep(0.7)
            end_target = max(chapter["minimum"], durations[i])
            remaining = end_target - (time.perf_counter() - clock - start)
            if remaining > 0:
                time.sleep(remaining)
            sections.append(
                dict(
                    scene=i,
                    title=chapter["title"],
                    start=start,
                    end=time.perf_counter() - clock,
                )
            )
            (evidence / "sections.json").write_text(
                json.dumps(sections, indent=2), encoding="utf-8"
            )
    finally:
        stopping = True
        proc.write("exit\r")
        thread.join(timeout=4)
        if proc.isalive():
            proc.terminate(force=True)
        event_file.close()
        cast.close()
    meta = dict(
        duration=time.perf_counter() - clock,
        command_count=len(commands),
        all_commands_exit_zero=all(c["exit_code"] == 0 for c in commands),
        capture="Genuine Windows PowerShell pseudoterminal; actual prompt, command echo, progress and cursor events.",
        timing="Original timestamps, scripted submissions, no fake typing or output retiming",
        version=settings["version"],
        publication="HUMAN_REVIEW_REQUIRED",
    )
    (evidence / "recording.json").write_text(
        json.dumps(meta, indent=2), encoding="utf-8"
    )
    return meta
