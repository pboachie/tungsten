// SPDX-License-Identifier: Apache-2.0
/**
 * The isolate side of the WASM engine: this module runs in a worker thread
 * (started by wasm.ts), so a script that computes never occupies the event
 * loop of the server. It owns the QuickJS instance, its timers and its
 * limits; everything the script sends out is posted to the parent as a line,
 * and the parent posts the answers of calls back. The parent terminates the
 * worker when the run ends or its deadline passes.
 */
import { parentPort, workerData, type MessagePort } from "node:worker_threads";

import releaseSync from "@jitl/quickjs-wasmfile-release-sync";
import { newQuickJSWASMModuleFromVariant, newVariant, type QuickJSContext, type QuickJSHandle, type QuickJSRuntime } from "quickjs-emscripten-core";

import type { FailReason } from "./protocol.js";
import type { WorkerInit, WorkerToParent } from "./wasm.js";

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

/** The synchronous variant the package exports (its types name it badly under NodeNext). */
type SyncVariant = Extract<Parameters<typeof newVariant>[0], { type: "sync" }>;

const ERROR_PREFIX = '{"type":"error","message":"';

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
  // Like the real one, fetch never throws: it rejects.
  globalThis.fetch = () => Promise.reject(new Error("fetch is not available in the sandbox; use the client."));
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

function describe(vm: QuickJSContext, handle: QuickJSHandle): { name: string; message: string } {
  const dumped = vm.dump(handle) as unknown;
  if (typeof dumped === "object" && dumped !== null) {
    const e = dumped as Record<string, unknown>;
    return { name: typeof e.name === "string" ? e.name : "Error", message: typeof e.message === "string" ? e.message : String(dumped) };
  }
  return { name: "Error", message: String(dumped) };
}

if (parentPort === null) throw new Error("wasm-worker must run in a worker thread");
const port: MessagePort = parentPort;
const init = workerData as WorkerInit;
const post = (message: WorkerToParent): void => port.postMessage(message);

async function main(): Promise<void> {
  const { source, deadline } = init;
  const memoryMb = Math.max(MIN_MEMORY_MB, Math.floor(init.memoryMb));
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
    return fail("spawn", `The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`);
  }
  rt.setMaxStackSize(STACK_BYTES);

  let ended = false;
  /** Replies received from the parent, reported with every idle check. */
  let replies = 0;
  const timers = new Map<number, NodeJS.Timeout>();
  function fail(reason: FailReason, message: string): void {
    if (ended) return;
    ended = true;
    post({ type: "fail", reason, message });
  }
  rt.setInterruptHandler(() => ended || Date.now() > deadline);

  const hooks = { deliver: vm.undefined as QuickJSHandle, fire: vm.undefined as QuickJSHandle, start: vm.undefined as QuickJSHandle };
  const fromVm = (error: QuickJSHandle): void => {
    const { name, message } = describe(vm, error);
    error.dispose();
    if (Date.now() > deadline && /interrupted/i.test(message)) fail("timeout", timeoutMessage(init.timeoutMs));
    else if (/out of memory/i.test(message)) fail("memory", memoryMessage(memoryMb));
    else post({ type: "line", text: JSON.stringify({ type: "error", message: `${name}: ${message}` }) });
  };
  /** Call a hook of the isolate with `argument` (consumed). */
  const call = (hook: QuickJSHandle, argument: QuickJSHandle): void => {
    if (ended) {
      argument.dispose();
      return;
    }
    const result = vm.callFunction(hook, vm.undefined, argument);
    argument.dispose();
    if (result.error) fromVm(result.error);
    else result.value.dispose();
  };
  /** Run the isolate's pending jobs, then ask the parent, once this thread's
   * event loop has turned, whether the script still has something to wait for. */
  const pump = (): void => {
    if (ended) return;
    const jobs = rt.executePendingJobs();
    if (jobs.error) fromVm(jobs.error);
    setImmediate(() => {
      if (ended || timers.size > 0 || rt.hasPendingJob()) return;
      post({ type: "idle", replies });
    });
  };

  try {
    const send = vm.newFunction("__send", (text) => {
      const line = vm.getString(text);
      // QuickJS lets a script catch its own interrupt and out-of-memory
      // errors, so the script's wrapper reports them as thrown errors.
      if (line.startsWith(ERROR_PREFIX + "InternalError: ")) {
        if (Date.now() > deadline && line.includes("interrupted")) return fail("timeout", timeoutMessage(init.timeoutMs));
        if (line.includes("out of memory")) return fail("memory", memoryMessage(memoryMb));
      }
      post({ type: "line", text: line });
    });
    vm.setProp(vm.global, "__send", send);
    send.dispose();
    const timer = vm.newFunction("__timer", (idHandle, msHandle) => {
      const id = vm.getNumber(idHandle);
      const ms = vm.getNumber(msHandle);
      const old = timers.get(id);
      if (old) clearTimeout(old);
      timers.delete(id);
      if (ms < 0 || ended) return;
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
    return fail("spawn", `The sandbox could not start (${error instanceof Error ? error.message : String(error)}).`);
  }

  port.on("message", (message: { type: "reply"; line: string }) => {
    if (message.type !== "reply" || ended) return;
    replies += 1;
    call(hooks.deliver, vm.newString(message.line));
    pump();
  });
  const compiled = vm.evalCode(source, "script.ts");
  if (compiled.error) {
    fromVm(compiled.error);
  } else {
    call(hooks.start, compiled.value);
    pump();
  }
}

function memoryMessage(memoryMb: number): string {
  return `The script used more than ${memoryMb} MB of memory and was stopped.`;
}

function timeoutMessage(timeoutMs: number): string {
  return `The script ran longer than ${timeoutMs} ms and was stopped.`;
}

main().catch((error: unknown) => post({ type: "fail", reason: "spawn", message: `The sandbox could not start (${error instanceof Error ? error.message : String(error)}).` }));
