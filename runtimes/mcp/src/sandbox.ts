// SPDX-License-Identifier: Apache-2.0
/**
 * `run_script` (planning/02 D7): agent-written TypeScript runs in a Deno
 * subprocess with network access to the API host only, no file, env, run
 * or FFI permission, a wall-clock limit and a V8 heap limit. The script's
 * `client` is a proxy whose calls come back to this server over the
 * child's stdin/stdout and go through the session's tool calls, so
 * credentials never enter the sandbox and every safety rule (preview
 * tokens, redaction) applies unchanged.
 */
import { spawn } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, isAbsolute, join } from "node:path";
import { createInterface } from "node:readline";

export const SANDBOX_DEFAULTS = { timeoutMs: 30000, memoryMb: 128, maxCalls: 50 } as const;

/** Lines a script may log, and characters per line. */
const MAX_LOGS = 100;
const MAX_LOG_CHARS = 2000;
/** Largest message line from the sandbox. */
const MAX_LINE_CHARS = 4 * 1024 * 1024;
const MAX_STDERR_CHARS = 4000;

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
 * heap limit. Every other permission stays denied. */
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

/** The wrapper module: the RPC client, console capture and the run. */
const MAIN = `import run from "./script.ts";
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
  const value = await run(client);
  await send({ type: "done", value: value === undefined ? null : value });
} catch (error) {
  await send({ type: "error", message: error instanceof Error ? error.name + ": " + error.message : format(error) });
}
Deno.exit(0);
`;

export interface SandboxCall {
  method: "invoke" | "preview";
  name: string;
  ok: boolean;
}

export type SandboxOutcome =
  | { status: "done"; value: unknown; logs: string[]; calls: SandboxCall[] }
  | { status: "failed"; reason: "error" | "timeout" | "exit" | "spawn" | "protocol"; message: string; logs: string[]; calls: SandboxCall[] };

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
  const logs: string[] = [];
  const calls: SandboxCall[] = [];
  try {
    dir = await mkdtemp(join(tmpdir(), "tungsten-run-"));
    await writeFile(join(dir, "script.ts"), `export default async function (client: any): Promise<unknown> {\n${run.code}\n}\n`);
    await writeFile(join(dir, "main.ts"), MAIN);
    return await execute(run, dir, logs, calls);
  } catch (error) {
    return { status: "failed", reason: "spawn", message: `The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`, logs, calls };
  } finally {
    if (dir !== null) await rm(dir, { recursive: true, force: true }).catch(() => undefined);
  }
}

function execute(run: SandboxRun, dir: string, logs: string[], calls: SandboxCall[]): Promise<SandboxOutcome> {
  return new Promise((resolve) => {
    const child = spawn(run.deno, denoArguments({ host: run.host, memoryMb: run.memoryMb, entry: join(dir, "main.ts") }), {
      cwd: dir,
      env: { NO_COLOR: "1", DENO_NO_UPDATE_CHECK: "1", DENO_NO_PROMPT: "1", DENO_DIR: join(dir, ".deno"), HOME: dir },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let settled: SandboxOutcome | null = null;
    let stderr = "";
    const finish = (outcome: SandboxOutcome): void => {
      if (settled) return;
      settled = outcome;
      clearTimeout(timer);
      child.kill("SIGKILL");
      resolve(outcome);
    };
    const fail = (reason: "error" | "timeout" | "exit" | "spawn" | "protocol", message: string): void =>
      finish({ status: "failed", reason, message, logs, calls });
    const timer = setTimeout(() => fail("timeout", `The script ran longer than ${run.timeoutMs} ms and was stopped.`), run.timeoutMs);
    child.on("error", (error) => fail("spawn", `deno could not be started (${error.message}).`));
    child.stdin.on("error", () => undefined);
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => {
      if (stderr.length < MAX_STDERR_CHARS) stderr += chunk;
    });
    let used = 0;
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    lines.on("line", (line) => {
      if (settled) return;
      if (line.length > MAX_LINE_CHARS) return fail("protocol", "The script sent a message over the size limit.");
      let message: unknown;
      try {
        message = JSON.parse(line);
      } catch {
        return fail("protocol", "The script wrote to stdout outside the client; use console.log.");
      }
      if (typeof message !== "object" || message === null) return fail("protocol", "The script sent an invalid message.");
      const m = message as Record<string, unknown>;
      if (m.type === "log") {
        if (logs.length < MAX_LOGS) logs.push(String(m.text).slice(0, MAX_LOG_CHARS));
      } else if (m.type === "call" && typeof m.id === "number" && (m.method === "invoke" || m.method === "preview")) {
        const method = m.method;
        const name = typeof m.name === "string" ? m.name : String(m.name);
        used += 1;
        const answer = used > run.maxCalls ? Promise.resolve(run.refuse(m.name)) : run.call(method, m.name, m.arguments);
        void answer
          .catch(() => run.refuse(m.name))
          .then((result) => {
            calls.push({ method, name, ok: result.ok });
            if (!settled) child.stdin.write(`${JSON.stringify({ id: m.id, result })}\n`);
          });
      } else if (m.type === "done") {
        finish({ status: "done", value: m.value ?? null, logs, calls });
      } else if (m.type === "error") {
        fail("error", `The script threw: ${String(m.message).slice(0, MAX_LOG_CHARS)}`);
      } else {
        fail("protocol", "The script sent an invalid message.");
      }
    });
    child.on("close", (code, signal) => {
      if (settled) return;
      const detail = stderr.split(dir).join(".").trim().slice(0, MAX_STDERR_CHARS);
      fail("exit", `The script exited (${signal ?? `code ${code}`}) without returning${detail === "" ? "" : `: ${detail}`}`);
    });
  });
}
