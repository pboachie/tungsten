// SPDX-License-Identifier: Apache-2.0
/**
 * What both `run_script` engines share (planning/02 D7): the line protocol a
 * script speaks to the server (log, call, done, error), the limits on it,
 * the bookkeeping of the calls a script made, and the result of a run. An
 * engine only starts the script and moves lines; this driver decides what a
 * line means, so the same script gets the same outcome from either engine.
 */

/** Lines a script may log, and characters per line. */
const MAX_LOGS = 100;
const MAX_LOG_CHARS = 2000;
/** Largest message line from the sandbox, in bytes. */
export const MAX_LINE_BYTES = 4 * 1024 * 1024;
/** Everything a script may write to stdout, in bytes. */
const MAX_STDOUT_BYTES = 64 * 1024 * 1024;
/** Longest wait for calls still in flight when a script ends. */
const MAX_SETTLE_MS = 10000;

/** One client call of a script, listed when it starts; `ok` is null while
 * it is still in flight (its outcome unknown). */
export interface SandboxCall {
  method: "invoke" | "preview";
  name: string;
  ok: boolean | null;
}

/** The result of a call that completed after the script ended, so the
 * script never received it (it may hold a one-time secret). */
export interface SandboxLateResult {
  method: "invoke" | "preview";
  name: string;
  result: { ok: boolean; [key: string]: unknown };
}

export type FailReason = "error" | "timeout" | "memory" | "exit" | "spawn" | "protocol";

export type SandboxOutcome =
  | { status: "done"; value: unknown; logs: string[]; calls: SandboxCall[]; unreceived: SandboxLateResult[] }
  | { status: "failed"; reason: FailReason; message: string; logs: string[]; calls: SandboxCall[]; unreceived: SandboxLateResult[] };

/** The sandbox engines: a Deno subprocess, or QuickJS compiled to WASM
 * running inside this process. */
export type SandboxEngine = "deno" | "wasm";

/** What an engine needs to know about one script; engine-specific settings
 * (the deno binary, the API host) are added by the engine's own run type. */
export interface SandboxRunBase {
  code: string;
  timeoutMs: number;
  memoryMb: number;
  maxCalls: number;
  /** Executes one client call; returns `{ok, value}` or `{ok: false, error}`. */
  call(method: "invoke" | "preview", name: unknown, args: unknown): Promise<{ ok: boolean; [key: string]: unknown }>;
  /** Called with the limit exceeded instead of `call` once `maxCalls` is reached. */
  refuse(name: unknown): { ok: false; [key: string]: unknown };
}

/** The engine's side of a run. */
export interface Transport {
  /** Deliver the answer of a call to the script. */
  reply(line: string): void;
  /** Stop the script now; called once, when the run ends. */
  stop(): void;
}

export type Ending = { status: "done"; value: unknown } | { status: "failed"; reason: FailReason; message: string };

export interface Driver {
  /** Settles when the run ended and the calls it started were awaited. */
  readonly outcome: Promise<SandboxOutcome>;
  readonly ended: boolean;
  /** Wall-clock instant (ms since the epoch) after which the run times out. */
  readonly deadline: number;
  /** Whether a call the script started is still being executed. */
  busy(): boolean;
  /** One protocol line from the script. */
  message(line: string): void;
  /** A chunk of the script's output stream (engines that read bytes). */
  data(chunk: Buffer): void;
  /** One complete output line (engines that do not read bytes). */
  line(text: string): void;
  fail(reason: FailReason, message: string): void;
}

/** Drive one run. The wall-clock limit starts now. */
export function createDriver(run: SandboxRunBase, transport: Transport): Driver {
  const logs: string[] = [];
  const calls: SandboxCall[] = [];
  const unreceived: SandboxLateResult[] = [];
  const inflight = new Set<Promise<void>>();
  const deadline = Date.now() + run.timeoutMs;
  let ended = false;
  let resolveOutcome!: (outcome: SandboxOutcome) => void;
  const outcome = new Promise<SandboxOutcome>((resolve) => {
    resolveOutcome = resolve;
  });
  // When the script ends (any way), wait a bounded time for the calls it
  // started: they run in this server and their effects happen anyway.
  const finish = (ending: Ending): void => {
    if (ended) return;
    ended = true;
    clearTimeout(timer);
    transport.stop();
    const settle =
      inflight.size === 0
        ? Promise.resolve()
        : new Promise<void>((done) => {
            const wait = setTimeout(done, Math.min(run.timeoutMs, MAX_SETTLE_MS));
            void Promise.allSettled([...inflight]).then(() => {
              clearTimeout(wait);
              done();
            });
          });
    void settle.then(() => resolveOutcome({ ...ending, logs: [...logs], calls: calls.map((c) => ({ ...c })), unreceived: [...unreceived] }));
  };
  const fail = (reason: FailReason, message: string): void => finish({ status: "failed", reason, message });
  const timer = setTimeout(() => fail("timeout", timeoutMessage(run.timeoutMs)), run.timeoutMs);
  let used = 0;
  const message = (line: string): void => {
    if (ended) return;
    let parsed: unknown;
    try {
      parsed = JSON.parse(line);
    } catch {
      return fail("protocol", "The script wrote to stdout outside the client; use console.log.");
    }
    if (typeof parsed !== "object" || parsed === null) return fail("protocol", "The script sent an invalid message.");
    const m = parsed as Record<string, unknown>;
    if (m.type === "log") {
      if (logs.length < MAX_LOGS) logs.push(String(m.text).slice(0, MAX_LOG_CHARS));
    } else if (m.type === "call" && typeof m.id === "number" && (m.method === "invoke" || m.method === "preview")) {
      const method = m.method;
      const name = typeof m.name === "string" ? m.name : String(m.name);
      used += 1;
      const entry: SandboxCall = { method, name, ok: null };
      calls.push(entry);
      const answer = used > run.maxCalls ? Promise.resolve(run.refuse(m.name)) : run.call(method, m.name, m.arguments);
      const settled: Promise<void> = answer
        .catch(() => run.refuse(m.name))
        .then((result) => {
          entry.ok = result.ok;
          if (ended) unreceived.push({ method, name, result });
          else transport.reply(`${JSON.stringify({ id: m.id, result })}\n`);
        })
        .finally(() => inflight.delete(settled));
      inflight.add(settled);
    } else if (m.type === "done") {
      finish({ status: "done", value: m.value ?? null });
    } else if (m.type === "error") {
      fail("error", `The script threw: ${String(m.message).slice(0, MAX_LOG_CHARS)}`);
    } else {
      fail("protocol", "The script sent an invalid message.");
    }
  };
  // Lines are split on bytes and bounded before they are joined, so a
  // script cannot make this process buffer an unbounded line.
  let pending: Buffer[] = [];
  let pendingBytes = 0;
  let total = 0;
  const data = (chunk: Buffer): void => {
    if (ended) return;
    total += chunk.length;
    if (total > MAX_STDOUT_BYTES) return fail("protocol", `The script wrote more than ${MAX_STDOUT_BYTES / (1024 * 1024)} MB of output.`);
    let start = 0;
    for (let at = chunk.indexOf(10, start); at >= 0 && !ended; at = chunk.indexOf(10, start)) {
      const part = chunk.subarray(start, at);
      start = at + 1;
      if (pendingBytes + part.length > MAX_LINE_BYTES) return fail("protocol", "The script sent a message over the size limit.");
      const line = pendingBytes === 0 ? part : Buffer.concat([...pending, part]);
      pending = [];
      pendingBytes = 0;
      message(line.toString("utf8"));
    }
    if (ended || start >= chunk.length) return;
    const rest = chunk.subarray(start);
    pendingBytes += rest.length;
    if (pendingBytes > MAX_LINE_BYTES) return fail("protocol", "The script sent a message over the size limit.");
    pending.push(rest);
  };
  const line = (text: string): void => {
    if (ended) return;
    const size = Buffer.byteLength(text) + 1;
    total += size;
    if (total > MAX_STDOUT_BYTES) return fail("protocol", `The script wrote more than ${MAX_STDOUT_BYTES / (1024 * 1024)} MB of output.`);
    if (size - 1 > MAX_LINE_BYTES) return fail("protocol", "The script sent a message over the size limit.");
    message(text);
  };
  return {
    outcome,
    get ended() {
      return ended;
    },
    deadline,
    busy: () => inflight.size > 0,
    message,
    data,
    line,
    fail,
  };
}

/** The message of a script that ran past its wall-clock limit. */
export function timeoutMessage(timeoutMs: number): string {
  return `The script ran longer than ${timeoutMs} ms and was stopped.`;
}

/** The outcome of a run that could not start. */
export function startFailure(message: string): SandboxOutcome {
  return { status: "failed", reason: "spawn", message, logs: [], calls: [], unreceived: [] };
}
