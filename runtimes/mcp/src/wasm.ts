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
 * Limits: the isolate's WebAssembly memory cannot grow past `memoryMb`
 * (QuickJS's own allocator limit does not count in this build, so the hard
 * bound is the memory object itself; a script that reaches it gets an
 * out-of-memory error); an interrupt
 * handler stops the script at the wall-clock deadline (the event loop of
 * this process is busy while a script computes, up to that deadline); the
 * stack is bounded. TypeScript is stripped (types only, no transformation)
 * with Node's own `module.stripTypeScriptTypes` so a script reads the same
 * under both engines.
 */
import { stripTypeScriptTypes } from "node:module";

import releaseSync from "@jitl/quickjs-wasmfile-release-sync";
import { newQuickJSWASMModuleFromVariant, newVariant, type QuickJSContext, type QuickJSHandle, type QuickJSRuntime } from "quickjs-emscripten-core";

import { createDriver, startFailure, timeoutMessage, type SandboxOutcome, type SandboxRunBase } from "./protocol.js";

/** Stack of the isolate, in bytes. */
const STACK_BYTES = 1024 * 1024;
/** Timers a script may have pending at once. */
const MAX_TIMERS = 1000;
/** Heap floor in MB, as for the Deno engine. */
const MIN_MEMORY_MB = 16;
/** WebAssembly pages are 64 KiB. The module needs 256 pages (16 MB) for its
 * own stack and data before the script gets any; the memory may grow by
 * `memoryMb` beyond that and no further. */
const BASE_PAGES = 256;
const PAGES_PER_MB = 16;
/** The most a 32-bit WebAssembly memory is allowed to hold here: 2 GB. */
const MAX_PAGES = 32768;

/** The guest side: the client proxy, console capture and timers. It gets the
 * two host functions through globals it deletes at once, and hands the host
 * three hooks (`deliver`, `fire`, `start`) through `__hooks`. */
const PRELUDE = `(() => {
  const send = globalThis.__send;
  const timer = globalThis.__timer;
  delete globalThis.__send;
  delete globalThis.__timer;
  const emit = (message) => {
    let line;
    try {
      line = JSON.stringify(message);
    } catch {
      line = JSON.stringify({ type: "error", message: "The script's value is not JSON-serializable." });
    }
    send(line);
  };
  const pending = new Map();
  let next = 1;
  const request = (method, name, args) => {
    const id = next++;
    return new Promise((resolve) => {
      pending.set(id, resolve);
      emit({ type: "call", id, method, name, arguments: args === undefined ? {} : args });
    });
  };
  const format = (value) => {
    if (typeof value === "string") return value;
    try {
      return JSON.stringify(value) ?? String(value);
    } catch {
      return String(value);
    }
  };
  const log = (...values) => emit({ type: "log", text: values.map(format).join(" ") });
  globalThis.console = { log, info: log, warn: log, error: log, debug: log };
  const timers = new Map();
  let timerId = 1;
  globalThis.setTimeout = (callback, ms, ...rest) => {
    const id = timerId++;
    timers.set(id, () => callback(...rest));
    timer(id, Math.max(0, Number(ms) || 0));
    return id;
  };
  globalThis.clearTimeout = (id) => {
    if (timers.delete(id)) timer(id, -1);
  };
  const denied = (what) => () => {
    throw new Error(what + " is not available in the sandbox; use the client.");
  };
  globalThis.fetch = denied("fetch");
  const base = {
    invoke: (name, args) => request("invoke", name, args),
    preview: (name, args) => request("preview", name, args),
  };
  const client = new Proxy(base, {
    get(target, prop) {
      if (typeof prop === "string" && prop in target) return target[prop];
      if (typeof prop !== "string" || prop === "then") return undefined;
      return (args) => request("invoke", prop, args);
    },
  });
  globalThis.__hooks = {
    deliver(line) {
      const message = JSON.parse(line);
      const resolve = pending.get(message.id);
      pending.delete(message.id);
      if (resolve) resolve(message.result);
    },
    fire(id) {
      const callback = timers.get(id);
      timers.delete(id);
      if (callback) callback();
    },
    start(script) {
      (async () => {
        try {
          if (typeof script !== "function") throw new SyntaxError("The script is not a function body.");
          const value = await script(client);
          emit({ type: "done", value: value === undefined ? null : value });
        } catch (error) {
          emit({ type: "error", message: error instanceof Error ? error.name + ": " + error.message : format(error) });
        }
      })();
    },
  };
})();
`;

/** The synchronous variant the package exports (its types name it badly under NodeNext). */
type SyncVariant = Extract<Parameters<typeof newVariant>[0], { type: "sync" }>;

const ERROR_PREFIX = '{"type":"error","message":"';

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

function describe(vm: QuickJSContext, handle: QuickJSHandle): { name: string; message: string } {
  const dumped = vm.dump(handle) as unknown;
  if (typeof dumped === "object" && dumped !== null) {
    const e = dumped as Record<string, unknown>;
    return { name: typeof e.name === "string" ? e.name : "Error", message: typeof e.message === "string" ? e.message : String(dumped) };
  }
  return { name: "Error", message: String(dumped) };
}

/** Run one script in a fresh QuickJS instance; never throws. */
export async function runWasm(run: SandboxRunBase): Promise<SandboxOutcome> {
  let source: string;
  try {
    source = wasmScript(run.code);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return syntaxFailure(run, message);
  }
  const memoryMb = Math.max(MIN_MEMORY_MB, Math.floor(run.memoryMb));
  let rt: QuickJSRuntime;
  let vm: QuickJSContext;
  try {
    // A fresh module instance and memory per script: nothing, not even the
    // heap of the WebAssembly instance, is shared between two scripts.
    const wasmMemory = new WebAssembly.Memory({ initial: BASE_PAGES, maximum: Math.min(MAX_PAGES, BASE_PAGES + memoryMb * PAGES_PER_MB) });
    const variant = newVariant(releaseSync as unknown as SyncVariant, { wasmMemory });
    const module = await newQuickJSWASMModuleFromVariant(variant);
    rt = module.newRuntime();
    vm = rt.newContext();
  } catch (error) {
    return startFailure(`The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`);
  }
  rt.setMaxStackSize(STACK_BYTES);

  let disposed = false;
  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    for (const t of timers.values()) clearTimeout(t);
    timers.clear();
    try {
      for (const hook of [hooks.deliver, hooks.fire, hooks.start]) if (hook.alive) hook.dispose();
      vm.dispose();
      rt.dispose();
    } catch {
      // The instance is dropped either way.
    }
  };
  const timers = new Map<number, NodeJS.Timeout>();
  const driver = createDriver(run, {
    reply: (line) => {
      if (disposed) return;
      call(hooks.deliver, vm.newString(line));
      pump();
    },
    // The run may end inside a call from the isolate: dispose once the
    // stack has unwound.
    stop: () => queueMicrotask(dispose),
  });
  rt.setInterruptHandler(() => driver.ended || Date.now() > driver.deadline);

  const stopped = (): boolean => disposed || driver.ended;
  const fromVm = (error: QuickJSHandle): void => {
    const { name, message } = describe(vm, error);
    error.dispose();
    if (Date.now() > driver.deadline && /interrupted/i.test(message)) driver.fail("timeout", timeoutMessage(run.timeoutMs));
    else if (/out of memory/i.test(message)) driver.fail("memory", memoryMessage(memoryMb));
    else driver.message(JSON.stringify({ type: "error", message: `${name}: ${message}` }));
  };
  const hooks = { deliver: vm.undefined as QuickJSHandle, fire: vm.undefined as QuickJSHandle, start: vm.undefined as QuickJSHandle };
  /** Call a hook of the isolate with `argument` (consumed). */
  const call = (hook: QuickJSHandle, argument: QuickJSHandle): void => {
    if (stopped()) {
      argument.dispose();
      return;
    }
    const result = vm.callFunction(hook, vm.undefined, argument);
    argument.dispose();
    if (result.error) fromVm(result.error);
    else result.value.dispose();
  };
  /** Run the isolate's pending jobs, then check, once the event loop has
   * turned, that the script still has something to wait for. */
  const pump = (): void => {
    if (stopped()) return;
    const jobs = rt.executePendingJobs();
    if (jobs.error) fromVm(jobs.error);
    setImmediate(() => {
      if (stopped() || driver.busy() || timers.size > 0 || rt.hasPendingJob()) return;
      driver.fail("exit", "The script exited (code 0) without returning");
    });
  };

  try {
    const send = vm.newFunction("__send", (text) => {
      const line = vm.getString(text);
      // QuickJS lets a script catch its own interrupt and out-of-memory
      // errors, so the script's wrapper reports them as thrown errors.
      if (line.startsWith(ERROR_PREFIX + "InternalError: ")) {
        if (Date.now() > driver.deadline && line.includes("interrupted")) return driver.fail("timeout", timeoutMessage(run.timeoutMs));
        if (line.includes("out of memory")) return driver.fail("memory", memoryMessage(memoryMb));
      }
      driver.line(line);
    });
    vm.setProp(vm.global, "__send", send);
    send.dispose();
    const timer = vm.newFunction("__timer", (idHandle, msHandle) => {
      const id = vm.getNumber(idHandle);
      const ms = vm.getNumber(msHandle);
      const old = timers.get(id);
      if (old) clearTimeout(old);
      timers.delete(id);
      if (ms < 0 || stopped()) return;
      if (timers.size >= MAX_TIMERS) throw new Error(`A script may have at most ${MAX_TIMERS} timers pending.`);
      timers.set(
        id,
        setTimeout(() => {
          timers.delete(id);
          call(hooks.fire, vm.newNumber(id));
          pump();
        }, ms),
      );
    });
    vm.setProp(vm.global, "__timer", timer);
    timer.dispose();

    const prelude = vm.evalCode(PRELUDE, "sandbox.js");
    if (prelude.error) throw new Error(describe(vm, prelude.error).message);
    prelude.value.dispose();
    const table = vm.getProp(vm.global, "__hooks");
    hooks.deliver = vm.getProp(table, "deliver");
    hooks.fire = vm.getProp(table, "fire");
    hooks.start = vm.getProp(table, "start");
    table.dispose();
    const cleanup = vm.evalCode("delete globalThis.__hooks;", "sandbox.js");
    if (cleanup.error) cleanup.error.dispose();
    else cleanup.value.dispose();
  } catch (error) {
    driver.fail("spawn", `The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`);
    dispose();
    return driver.outcome;
  }

  const compiled = vm.evalCode(source, "script.ts");
  if (compiled.error) {
    fromVm(compiled.error);
  } else {
    call(hooks.start, compiled.value);
    pump();
  }
  const outcome = await driver.outcome;
  dispose();
  return outcome;
}

function memoryMessage(memoryMb: number): string {
  return `The script used more than ${memoryMb} MB of memory and was stopped.`;
}

/** A script that could not be parsed: the same envelope as a thrown error. */
async function syntaxFailure(run: SandboxRunBase, message: string): Promise<SandboxOutcome> {
  const driver = createDriver(run, { reply: () => undefined, stop: () => undefined });
  driver.message(JSON.stringify({ type: "error", message: `SyntaxError: ${message.split("\n")[0]}` }));
  return driver.outcome;
}
