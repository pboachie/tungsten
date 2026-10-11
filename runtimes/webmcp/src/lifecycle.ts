// SPDX-License-Identifier: Apache-2.0
import { detectWebMcpBackend, registerTungstenWebMcp, WebMcpError } from "./runtime.js";
import type { RegisterTungstenWebMcpOptions, WebMcpBackend, WebMcpRegistration } from "./types.js";

/** Construct a scoped core and manifest inside the loader, after dynamic import.
 * Keep a core alive for this scope's lifetime so preview tokens and execution
 * share the same SDK session. Backend and lifetime belong to the controller. */
export type WebMcpScope = Omit<RegisterTungstenWebMcpOptions, "backend" | "signal">;
export type WebMcpScopeLoader = (context: { signal: AbortSignal }) => WebMcpScope | Promise<WebMcpScope>;

export interface WebMcpRegistrationControllerOptions {
  backend?: WebMcpBackend;
  /** Aborting closes the controller and cancels loading and active calls. */
  signal?: AbortSignal;
}

export interface WebMcpRegistrationController {
  /** Replace the route scope. Unsupported browsers and superseded loads return
   * null. Current loader/registration errors reject. No loader runs during SSR
   * when there is no backend. Mutations still require an explicit allowlist. */
  replace(loader: WebMcpScopeLoader): Promise<WebMcpRegistration | null>;
  /** Unload tools and cancel loading/calls; the controller remains reusable. */
  clear(): Promise<void>;
  /** Close permanently. Idempotent; failed owned cleanup may be retried. */
  dispose(): Promise<void>;
}

/** Framework-neutral route lifecycle. Loads can run concurrently, but owned
 * cleanup and browser registration are serialized. Even loaders that ignore
 * cancellation cannot restore stale tools or block later routes. */
export function createWebMcpRegistrationController(options: WebMcpRegistrationControllerOptions = {}): WebMcpRegistrationController {
  let generation = 0;
  let closed = false;
  let pending: AbortController | undefined;
  let active: WebMcpRegistration | undefined;
  let queue: Promise<unknown> = Promise.resolve();
  const serial = <T>(action: () => Promise<T>): Promise<T> => {
    const result = queue.then(action);
    queue = result.catch(() => undefined);
    return result;
  };
  const cleanup = async (): Promise<void> => {
    if (active) {
      await active.dispose();
      active = undefined;
    }
  };
  const invalidate = (): number => {
    generation++;
    pending?.abort();
    pending = undefined;
    return generation;
  };
  const clear = (): Promise<void> => {
    invalidate();
    return serial(cleanup);
  };
  const dispose = (): Promise<void> => {
    closed = true;
    options.signal?.removeEventListener("abort", onAbort);
    return clear();
  };
  const onAbort = (): void => { void dispose().catch(() => undefined); };
  if (options.signal?.aborted) closed = true;
  else options.signal?.addEventListener("abort", onAbort, { once: true });

  const replace = async (loader: WebMcpScopeLoader): Promise<WebMcpRegistration | null> => {
    if (closed) throw new WebMcpError("CANCELLED", "The WebMCP registration controller is disposed.");
    const current = invalidate();
    const controller = new AbortController();
    pending = controller;
    const stale = (): boolean => closed || current !== generation || controller.signal.aborted;
    await serial(cleanup);
    if (stale()) return null;
    const backend = options.backend ?? detectWebMcpBackend();
    if (!backend) return null;
    // Dynamic import itself is not abortable. Race the loader so an ignored
    // signal does not keep framework cleanup or a superseded replace pending.
    let stop: (() => void) | undefined;
    const cancelled = new Promise<null>((resolve) => {
      stop = () => resolve(null);
      controller.signal.addEventListener("abort", stop, { once: true });
    });
    let scope: WebMcpScope | null;
    try {
      scope = await Promise.race([Promise.resolve().then(() => stale() ? null : loader({ signal: controller.signal })), cancelled]);
    } catch (error) {
      if (stale()) return null;
      throw error;
    } finally {
      if (stop) controller.signal.removeEventListener("abort", stop);
    }
    if (!scope || stale()) return null;
    return serial(async () => {
      if (stale()) return null;
      try {
        const registration = await registerTungstenWebMcp({ ...scope, backend, signal: controller.signal });
        active = registration;
        if (stale()) {
          await cleanup();
          return null;
        }
        return registration;
      } catch (error) {
        if (stale() && error instanceof WebMcpError && error.code === "CANCELLED") return null;
        throw error;
      }
    });
  };
  return Object.freeze({ replace, clear, dispose });
}
