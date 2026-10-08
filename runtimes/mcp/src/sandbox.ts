// SPDX-License-Identifier: Apache-2.0
/**
 * `run_script` (planning/02 D7): agent-written TypeScript runs in a Deno
 * subprocess with network access to the API host only, no file, env, run
 * or FFI permission, a wall-clock limit, a V8 heap limit and a process data
 * limit. The script's `client` is a proxy whose calls come back to this
 * server over the child's stdin/stdout and go through the session's tool
 * calls, so credentials never enter the sandbox and every safety rule
 * (preview tokens, redaction) applies unchanged.
 *
 * Deno does not check read permission for the modules of the static graph
 * it builds at startup (every literal `import`, including a literal
 * `import()`), so the script is never part of that graph: the entry module
 * holds it as a string and imports it from a `data:` URL with a computed
 * specifier. Every module the script reaches is then loaded through the
 * permission check, and the process has no read permission at all.
 */
import { spawn } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, isAbsolute, join } from "node:path";

export const SANDBOX_DEFAULTS = { timeoutMs: 30000, memoryMb: 128, maxCalls: 50 } as const;

/** Lines a script may log, and characters per line. */
const MAX_LOGS = 100;
const MAX_LOG_CHARS = 2000;
/** Largest message line from the sandbox, in bytes. */
const MAX_LINE_BYTES = 4 * 1024 * 1024;
/** Everything a script may write to stdout, in bytes. */
const MAX_STDOUT_BYTES = 64 * 1024 * 1024;
const MAX_STDERR_CHARS = 4000;
/** Longest wait for calls still in flight when a script ends. */
const MAX_SETTLE_MS = 10000;
/** Data (writable private memory) that deno needs besides the V8 heap: it
 * reserves its code range up front, so it does not start below ~560 MB. */
export const DENO_RESERVED_MB = 768;

/** The executable for `path`: itself when it contains a separator, else
 * the first executable match on `PATH`; null when none exists. */
export function resolveDeno(path: string, searchPath: string = process.env.PATH ?? ""): string | null {
  const executable = (file: string): boolean => {
    try {
      accessSync(file, constants.X_OK);
      return true;
    } catch {
      return false;
    }
  };
  if (path === "") return null;
  if (isAbsolute(path) || path.includes("/") || path.includes("\\")) return executable(path) ? path : null;
  for (const dir of searchPath.split(delimiter)) {
    if (dir === "") continue;
    for (const candidate of process.platform === "win32" ? [join(dir, `${path}.exe`), join(dir, path)] : [join(dir, path)]) {
      if (executable(candidate)) return candidate;
    }
  }
  return null;
}

/** The API host (`host[:port]`) of a base URL, or null when it is not a
 * plain absolute http(s) URL. */
export function apiHost(baseUrl: unknown): string | null {
  if (typeof baseUrl !== "string") return null;
  let url: URL;
  try {
    url = new URL(baseUrl);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (url.username !== "" || url.password !== "") return null;
  return /^[A-Za-z0-9.\-[\]:]+$/.test(url.host) ? url.host : null;
}

/** The `deno run` arguments of one script: network to `host` only, no
 * prompts, no remote or npm modules, no config or lock file, and the
 * heap limit. Every other permission stays denied (no `--allow-read`). */
export function denoArguments(input: { host: string; memoryMb: number; entry: string }): string[] {
  return [
    "run",
    "--quiet",
    "--no-prompt",
    "--no-remote",
    "--no-npm",
    "--no-config",
    "--no-lock",
    `--allow-net=${input.host}`,
    `--v8-flags=--max-old-space-size=${Math.max(16, Math.floor(input.memoryMb))}`,
    input.entry,
  ];
}

/** The data limit of a script's process in MB: deno's own reservation
 * plus twice the heap limit, so memory outside the V8 heap (array buffer
 * backing stores) is bounded too. */
export function processMemoryMb(memoryMb: number): number {
  return DENO_RESERVED_MB + 2 * Math.max(16, Math.floor(memoryMb));
}

/** Lowers the data limit (RLIMIT_DATA, in KB) unless the hard limit is
 * already lower, then runs deno. */
const LIMIT_SCRIPT = 'h=$(ulimit -H -d); if [ "$h" = unlimited ] || [ "$h" -gt "$1" ]; then ulimit -d "$1" || exit 125; fi; shift; exec "$@"';

/** The command that runs one script: on POSIX systems deno under
 * `/bin/sh` with the process data limit of `processMemoryMb` (enforced by
 * Linux; macOS does not enforce RLIMIT_DATA, so there the heap limit is the
 * only memory bound), elsewhere deno itself. */
export function sandboxCommand(input: { deno: string; host: string; memoryMb: number; entry: string }, platform: string = process.platform): { command: string; args: string[] } {
  const args = denoArguments(input);
  if (platform === "win32") return { command: input.deno, args };
  return { command: "/bin/sh", args: ["-c", LIMIT_SCRIPT, "tungsten-sandbox", String(processMemoryMb(input.memoryMb) * 1024), input.deno, ...args] };
}

/** The script as a module: the body of an async function of `client`. */
export function scriptModule(code: string): string {
  return `export default async function (client: any): Promise<unknown> {\n${code}\n}\n`;
}

/** The entry module: the RPC client, console capture and the run. It has
 * no import of its own; the script (embedded as a JSON string) is imported
 * from a computed `data:` URL, so it is loaded with permission checks. */
export function mainModule(code: string): string {
  return `const SCRIPT = ${JSON.stringify(scriptModule(code))};\n${MAIN}`;
}

const MAIN = `const SPECIFIER = "data:application/typescript;charset=utf-8," + encodeURIComponent(SCRIPT);
const encoder = new TextEncoder();
let queue: Promise<void> = Promise.resolve();
function send(message: unknown): Promise<void> {
  let line: string;
  try {
    line = JSON.stringify(message) + "\\n";
  } catch {
    line = JSON.stringify({ type: "error", message: "The script's value is not JSON-serializable." }) + "\\n";
  }
  queue = queue.then(async () => {
    let data = encoder.encode(line);
    while (data.length > 0) data = data.subarray(await Deno.stdout.write(data));
  });
  return queue;
}
const pending = new Map<number, (value: unknown) => void>();
let next = 1;
(async () => {
  const decoder = new TextDecoder();
  let buffer = "";
  for await (const chunk of Deno.stdin.readable) {
    buffer += decoder.decode(chunk, { stream: true });
    let at = buffer.indexOf("\\n");
    while (at >= 0) {
      const message = JSON.parse(buffer.slice(0, at));
      buffer = buffer.slice(at + 1);
      pending.get(message.id)?.(message.result);
      pending.delete(message.id);
      at = buffer.indexOf("\\n");
    }
  }
})();
function request(method: string, name: unknown, args: unknown): Promise<unknown> {
  const id = next++;
  return new Promise((resolve) => {
    pending.set(id, resolve);
    void send({ type: "call", id, method, name, arguments: args === undefined ? {} : args });
  });
}
const format = (value: unknown): string => {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value) ?? String(value);
  } catch {
    return String(value);
  }
};
for (const level of ["log", "info", "warn", "error", "debug"] as const) {
  console[level] = (...values: unknown[]) => void send({ type: "log", text: values.map(format).join(" ") });
}
const base: Record<string, unknown> = {
  invoke: (name: unknown, args?: unknown) => request("invoke", name, args),
  preview: (name: unknown, args?: unknown) => request("preview", name, args),
};
const client = new Proxy(base, {
  get(target, prop) {
    if (typeof prop === "string" && prop in target) return target[prop];
    if (typeof prop !== "string" || prop === "then") return undefined;
    return (args?: unknown) => request("invoke", prop, args);
  },
});
try {
  const run = (await import(SPECIFIER)).default;
  const value = await run(client);
  await send({ type: "done", value: value === undefined ? null : value });
} catch (error) {
  const message = error instanceof Error ? error.name + ": " + error.message : format(error);
  await send({ type: "error", message: message.split(SPECIFIER).join("script.ts") });
}
Deno.exit(0);
`;

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

type FailReason = "error" | "timeout" | "exit" | "spawn" | "protocol";

export type SandboxOutcome =
  | { status: "done"; value: unknown; logs: string[]; calls: SandboxCall[]; unreceived: SandboxLateResult[] }
  | { status: "failed"; reason: FailReason; message: string; logs: string[]; calls: SandboxCall[]; unreceived: SandboxLateResult[] };

export interface SandboxRun {
  deno: string;
  host: string;
  code: string;
  timeoutMs: number;
  memoryMb: number;
  maxCalls: number;
  /** Executes one client call; returns `{ok, value}` or `{ok: false, error}`. */
  call(method: "invoke" | "preview", name: unknown, args: unknown): Promise<{ ok: boolean; [key: string]: unknown }>;
  /** Called with the limit exceeded instead of `call` once `maxCalls` is reached. */
  refuse(name: unknown): { ok: false; [key: string]: unknown };
}

/** Run one script; never throws. The temporary directory is removed. */
export async function runSandboxed(run: SandboxRun): Promise<SandboxOutcome> {
  let dir: string | null = null;
  try {
    dir = await mkdtemp(join(tmpdir(), "tungsten-run-"));
    await writeFile(join(dir, "main.ts"), mainModule(run.code));
    return await execute(run, dir);
  } catch (error) {
    return { status: "failed", reason: "spawn", message: `The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`, logs: [], calls: [], unreceived: [] };
  } finally {
    if (dir !== null) await rm(dir, { recursive: true, force: true }).catch(() => undefined);
  }
}

type Ending = { status: "done"; value: unknown } | { status: "failed"; reason: FailReason; message: string };

function execute(run: SandboxRun, dir: string): Promise<SandboxOutcome> {
  return new Promise((resolve) => {
    const logs: string[] = [];
    const calls: SandboxCall[] = [];
    const unreceived: SandboxLateResult[] = [];
    const inflight = new Set<Promise<void>>();
    const { command, args } = sandboxCommand({ deno: run.deno, host: run.host, memoryMb: run.memoryMb, entry: join(dir, "main.ts") });
    const child = spawn(command, args, {
      cwd: dir,
      env: { NO_COLOR: "1", DENO_NO_UPDATE_CHECK: "1", DENO_NO_PROMPT: "1", DENO_DIR: join(dir, ".deno"), HOME: dir },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let ended = false;
    let stderr = "";
    // When the script ends (any way), wait a bounded time for the calls it
    // started: they run in this server and their effects happen anyway.
    const finish = (ending: Ending): void => {
      if (ended) return;
      ended = true;
      clearTimeout(timer);
      child.kill("SIGKILL");
      const settle = inflight.size === 0 ? Promise.resolve() : new Promise<void>((done) => {
        const wait = setTimeout(done, Math.min(run.timeoutMs, MAX_SETTLE_MS));
        void Promise.allSettled([...inflight]).then(() => {
          clearTimeout(wait);
          done();
        });
      });
      void settle.then(() => resolve({ ...ending, logs: [...logs], calls: calls.map((c) => ({ ...c })), unreceived: [...unreceived] }));
    };
    const fail = (reason: FailReason, message: string): void => finish({ status: "failed", reason, message });
    const timer = setTimeout(() => fail("timeout", `The script ran longer than ${run.timeoutMs} ms and was stopped.`), run.timeoutMs);
    child.on("error", (error) => fail("spawn", `deno could not be started (${error.message}).`));
    child.stdin.on("error", () => undefined);
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => {
      if (stderr.length < MAX_STDERR_CHARS) stderr += chunk;
    });
    let used = 0;
    const message = (line: string): void => {
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
            else child.stdin.write(`${JSON.stringify({ id: m.id, result })}\n`);
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
    child.stdout.on("data", (chunk: Buffer) => {
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
    });
    child.on("close", (code, signal) => {
      if (ended) return;
      const detail = stderr.split(dir).join(".").replace(/data:application\/typescript;charset=utf-8,[^\s:]*/g, "script.ts").trim().slice(0, MAX_STDERR_CHARS);
      fail("exit", `The script exited (${signal ?? `code ${code}`}) without returning${detail === "" ? "" : `: ${detail}`}`);
    });
  });
}
