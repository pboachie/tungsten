// SPDX-License-Identifier: Apache-2.0
/**
 * The WASM engine of `run_script` (planning/02 D7): the script runs in
 * QuickJS compiled to WebAssembly, inside this process, with no host
 * binary. The isolate has no host object at all: no network, file, environment,
 * process or module loader. Its only way out is the two functions the server
 * gives it (one protocol line out, one timer), and the lines it sends are
 * the same the Deno engine sends over stdout, so the shared driver
 * (protocol.ts) treats both alike: calls go through the session, credentials
 * never enter the isolate, the limits and the result are the same.
 *
 * The isolate runs in a worker thread (wasm-worker.ts), so a script that
 * computes never occupies this process's event loop; the worker is
 * terminated when the run ends or at the wall-clock deadline.
 *
 * Limits: the isolate's WebAssembly memory cannot grow past `memoryMb`
 * (QuickJS's own allocator limit does not count in this build, so the hard
 * bound is the memory object itself; a script that reaches it gets an
 * out-of-memory error); an interrupt
 * handler and the termination of the worker stop the script at the
 * wall-clock deadline; the stack is bounded. TypeScript is stripped (types only, no transformation)
 * with Node's own `module.stripTypeScriptTypes` so a script reads the same
 * under both engines.
 */
import { stripTypeScriptTypes } from "node:module";
import { Worker } from "node:worker_threads";

import { createDriver, type FailReason, type SandboxOutcome, type SandboxRunBase } from "./protocol.js";

/** What the worker is started with. */
export interface WorkerInit {
  source: string;
  memoryMb: number;
  timeoutMs: number;
  /** Wall-clock instant (ms since the epoch) after which the run times out. */
  deadline: number;
}

/** Messages from the worker. `idle` carries the number of replies the
 * worker had received when it found nothing left to wait for. */
export type WorkerToParent =
  | { type: "line"; text: string }
  | { type: "fail"; reason: FailReason; message: string }
  | { type: "idle"; replies: number };

let warningsQuieted = false;

/** The script as plain JavaScript: the body of an async function of
 * `client`, with TypeScript types blanked out. Throws on a syntax error. */
export function wasmScript(code: string): string {
  const source = `(async function (client: any): Promise<unknown> {\n${code}\n})`;
  if (typeof stripTypeScriptTypes !== "function") return source;
  // Node reports stripTypeScriptTypes as experimental on stderr, which an
  // MCP server's operator did not ask for: drop that one warning.
  const emit = process.emitWarning;
  if (!warningsQuieted) {
    process.emitWarning = ((warning: unknown, ...rest: unknown[]) => {
      if (typeof warning === "string" && warning.includes("stripTypeScriptTypes")) return;
      return (emit as (...args: unknown[]) => void).call(process, warning, ...rest);
    }) as typeof process.emitWarning;
    warningsQuieted = true;
  }
  return stripTypeScriptTypes(source, { mode: "strip" });
}

/** Run one script in a fresh QuickJS instance in a worker thread; never throws. */
export async function runWasm(run: SandboxRunBase): Promise<SandboxOutcome> {
  let source: string;
  try {
    source = wasmScript(run.code);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return syntaxFailure(run, message);
  }
  let worker: Worker;
  let repliesSent = 0;
  const driver = createDriver(run, {
    reply: (line) => {
      repliesSent += 1;
      worker.postMessage({ type: "reply", line });
    },
    // Ends the script wherever it is, including inside a computation.
    stop: () => void worker.terminate().catch(() => undefined),
  });
  const init: WorkerInit = { source, memoryMb: run.memoryMb, timeoutMs: run.timeoutMs, deadline: driver.deadline };
  try {
    worker = new Worker(new URL("./wasm-worker.js", import.meta.url), { workerData: init, env: {}, stdout: true, stderr: true });
  } catch (error) {
    driver.fail("spawn", `The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`);
    return driver.outcome;
  }
  worker.on("message", (message: WorkerToParent) => {
    if (driver.ended) return;
    if (message.type === "line") driver.line(message.text);
    else if (message.type === "fail") driver.fail(message.reason, message.message);
    else if (message.type === "idle" && message.replies === repliesSent && !driver.busy()) driver.fail("exit", "The script exited (code 0) without returning");
  });
  worker.on("error", (error) => driver.fail(/memory/i.test(error.message) ? "memory" : "exit", /memory/i.test(error.message) ? `The script used more than ${Math.max(16, Math.floor(run.memoryMb))} MB of memory and was stopped.` : `The sandbox stopped unexpectedly (${error.message}).`));
  worker.on("exit", () => driver.fail("exit", "The script exited (code 0) without returning"));
  return driver.outcome;
}

/** A script that could not be parsed: the same envelope as a thrown error. */
async function syntaxFailure(run: SandboxRunBase, message: string): Promise<SandboxOutcome> {
  const driver = createDriver(run, { reply: () => undefined, stop: () => undefined });
  driver.message(JSON.stringify({ type: "error", message: `SyntaxError: ${message.split("\n")[0]}` }));
  return driver.outcome;
}
