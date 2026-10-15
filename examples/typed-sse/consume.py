# SPDX-License-Identifier: AGPL-3.0-only
"""Consume typed named events, with explicit terminal-event and cleanup rules."""

import asyncio
import os

from run_events_sdk import AsyncRunEventsClient, ClientOptions, Err, RunEventsClient
from run_events_sdk.models import DoneEvent, StepEvent


def options() -> ClientOptions:
    return ClientOptions(
        auth={"ApiKey": os.environ["SSE_API_KEY"]},
        validate_responses="strict",
        max_reconnects=0,
    )


def sync() -> None:
    with RunEventsClient(options()) as client:
        stream = client.runs.events.list_stream(run_id="demo")
        completed = False
        try:
            for item in stream:
                if isinstance(item, Err):
                    # Do not log the envelope's potentially sensitive received_value.
                    raise RuntimeError(item.error["category"])
                payload = item.value
                if item.event == "step" and isinstance(payload, StepEvent):
                    print("sync step", payload.index, payload.step)
                elif item.event == "done" and isinstance(payload, DoneEvent):
                    print("sync done", payload.run_id, payload.steps)
                    completed = True
                    break
                else:
                    raise RuntimeError("Unexpected event name/payload combination")
        finally:
            # Also runs on early break, consumer failure or KeyboardInterrupt.
            stream.close()
        if not completed:
            raise RuntimeError("Stream ended before the application received done")


async def asynchronous() -> None:
    async with AsyncRunEventsClient(options()) as client:
        # An async stream is iterated, not awaited.
        stream = client.runs.events.list_stream(run_id="demo")
        completed = False
        try:
            async for item in stream:
                if isinstance(item, Err):
                    raise RuntimeError(item.error["category"])
                payload = item.value
                if item.event == "step" and isinstance(payload, StepEvent):
                    print("async step", payload.index, payload.step)
                elif item.event == "done" and isinstance(payload, DoneEvent):
                    print("async done", payload.run_id, payload.steps)
                    completed = True
                    break
                else:
                    raise RuntimeError("Unexpected event name/payload combination")
        finally:
            # For another task's pending read, call stream.cancel() first,
            # await that reader, then aclose() once no task is reading.
            await stream.aclose()
        if not completed:
            raise RuntimeError("Stream ended before the application received done")


if __name__ == "__main__":
    sync()
    asyncio.run(asynchronous())
