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

import { createDriver, startFailure, type SandboxEngine, type SandboxOutcome, type SandboxRunBase } from "./protocol.js";
import { runWasm } from "./wasm.js";

export { type SandboxCall, type SandboxEngine, type SandboxLateResult, type SandboxOutcome } from "./protocol.js";

export const SANDBOX_DEFAULTS = { timeoutMs: 30000, memoryMb: 128, maxCalls: 50 } as const;

const MAX_STDERR_CHARS = 4000;
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

/** The engine a configuration selects. `auto` (and a missing or unknown
 * value) is deno when its executable exists, else wasm; `deno` without the
 * executable is `missing`. */
export function selectEngine(configured: unknown, denoFound: boolean): SandboxEngine | "off" | "missing" {
  if (configured === "off") return "off";
  if (configured === "wasm") return "wasm";
  if (configured === "deno") return denoFound ? "deno" : "missing";
  return denoFound ? "deno" : "wasm";
}

export interface SandboxRun extends SandboxRunBase {
  engine?: SandboxEngine;
  deno: string;
  host: string;
}

/** Run one script on the engine `run.engine` (default deno); never throws. */
export async function runSandboxed(run: SandboxRun): Promise<SandboxOutcome> {
  if (run.engine === "wasm") return runWasm(run);
  let dir: string | null = null;
  try {
    dir = await mkdtemp(join(tmpdir(), "tungsten-run-"));
    await writeFile(join(dir, "main.ts"), mainModule(run.code));
    return await execute(run, dir);
  } catch (error) {
    return startFailure(`The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`);
  } finally {
    if (dir !== null) await rm(dir, { recursive: true, force: true }).catch(() => undefined);
  }
}

function execute(run: SandboxRun, dir: string): Promise<SandboxOutcome> {
  const { command, args } = sandboxCommand({ deno: run.deno, host: run.host, memoryMb: run.memoryMb, entry: join(dir, "main.ts") });
  const child = spawn(command, args, {
    cwd: dir,
    env: { NO_COLOR: "1", DENO_NO_UPDATE_CHECK: "1", DENO_NO_PROMPT: "1", DENO_DIR: join(dir, ".deno"), HOME: dir },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const driver = createDriver(run, {
    reply: (line) => void child.stdin.write(line),
    stop: () => void child.kill("SIGKILL"),
  });
  let stderr = "";
  child.on("error", (error) => driver.fail("spawn", `deno could not be started (${error.message}).`));
  child.stdin.on("error", () => undefined);
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk: string) => {
    if (stderr.length < MAX_STDERR_CHARS) stderr += chunk;
  });
  child.stdout.on("data", (chunk: Buffer) => driver.data(chunk));
  child.on("close", (code, signal) => {
    if (driver.ended) return;
    const detail = stderr.split(dir).join(".").replace(/data:application\/typescript;charset=utf-8,[^\s:]*/g, "script.ts").trim().slice(0, MAX_STDERR_CHARS);
    driver.fail("exit", `The script exited (${signal ?? `code ${code}`}) without returning${detail === "" ? "" : `: ${detail}`}`);
  });
  return driver.outcome;
}
