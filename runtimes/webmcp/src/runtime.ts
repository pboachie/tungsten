// SPDX-License-Identifier: Apache-2.0
import type { CallOptions, Diagnostic, MacroDescriptor, OperationDescriptor, Safety } from "@elysra/tungsten-runtime";
import type { DocumentModelContext, LegacyModelContext, RegisterTungstenWebMcpOptions, WebMcpBackend, WebMcpRegistration, WebMcpTool, WebMcpToolEntry } from "./types.js";
import { matchesSchema } from "./schema.js";
import { binaryMediaType, collectStream, renderBinary } from "./results.js";

export const DEFAULT_MAX_RESULT_BYTES = 1024 * 1024;
const NAME = /^[A-Za-z0-9_.-]{1,128}$/;
const SAFETY: readonly Safety[] = ["read_only", "mutating", "destructive", "irreversible"];
const owned = new WeakMap<object, Set<string>>();

export class WebMcpError extends Error {
  constructor(readonly code: "UNSUPPORTED" | "INVALID_CONFIGURATION" | "REGISTRATION_FAILED" | "CANCELLED", message: string) {
    super(message);
    this.name = "WebMcpError";
  }
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Does not invoke browser APIs, and is safe during SSR/module import. */
export function detectWebMcpBackend(): WebMcpBackend | null {
  const current: unknown = typeof document === "undefined" ? undefined : (document as unknown as Record<string, unknown>).modelContext;
  if (record(current) && typeof current.registerTool === "function") return { kind: "document", modelContext: current as unknown as DocumentModelContext };
  const legacy: unknown = typeof navigator === "undefined" ? undefined : (navigator as unknown as Record<string, unknown>).modelContext;
  if (record(legacy) && typeof legacy.registerTool === "function" && typeof legacy.unregisterTool === "function") {
    return { kind: "navigator", modelContext: legacy as unknown as LegacyModelContext };
  }
  return null;
}

function invalid(message: string): never { throw new WebMcpError("INVALID_CONFIGURATION", message); }

function failure(operation: string, code: string, remediation: string, category: Diagnostic["category"] = "VALIDATION_FAILED"): { ok: false; error: Diagnostic } {
  return { ok: false, error: {
    status: "error", category, operation, code, http_status: null, failed_parameter: null,
    received_value: null, expected: null, remediation, retryable: "never", retry_after_ms: null,
    next_action: null, request_id: null, trace: { attempts: 0 },
  } };
}

/** Strict JSON conversion: byte buffers, iterators, cycles and exotic objects
 * must not silently become empty objects or corrupt API outcomes. */
function json(value: unknown, maxBytes: number): unknown {
  let bytes = 0;
  let nodes = 0;
  const encoder = new TextEncoder();
  const active = new Set<object>();
  const add = (part: string): void => {
    bytes += encoder.encode(part).byteLength;
    if (bytes > maxBytes) throw new Error("RESULT_TOO_LARGE");
  };
  const visit = (item: unknown, depth: number): unknown => {
    if (++nodes > 100000 || depth > 100) throw new Error("RESULT_TOO_COMPLEX");
    if (item === undefined || item === null) { add("null"); return null; }
    if (typeof item === "string" || typeof item === "boolean" || (typeof item === "number" && Number.isFinite(item))) {
      add(JSON.stringify(item)); return item;
    }
    if (typeof item !== "object") throw new Error("NON_JSON_RESULT");
    if (active.has(item)) throw new Error("NON_JSON_RESULT");
    if (!Array.isArray(item) && Object.getPrototypeOf(item) !== Object.prototype && Object.getPrototypeOf(item) !== null) throw new Error("NON_JSON_RESULT");
    if (Symbol.asyncIterator in item || Symbol.iterator in item && !Array.isArray(item)) throw new Error("STREAMING_UNSUPPORTED");
    active.add(item);
    add(Array.isArray(item) ? "[]" : "{}");
    let result: unknown;
    if (Array.isArray(item)) {
      result = item.map((entry, index) => { if (index) add(","); return visit(entry, depth + 1); });
    } else {
      const object: Record<string, unknown> = Object.create(null) as Record<string, unknown>;
      let count = 0;
      for (const [key, entry] of Object.entries(item)) {
        if (entry === undefined) continue;
        if (count++) add(",");
        add(`${JSON.stringify(key)}:`);
        object[key] = visit(entry, depth + 1);
      }
      result = object;
    }
    active.delete(item);
    return result;
  };
  return visit(value, 0);
}

interface Resolved { entry: WebMcpToolEntry; op?: OperationDescriptor; macro?: MacroDescriptor; safety: Safety }

function resolve(options: RegisterTungstenWebMcpOptions): Resolved[] {
  if (!record(options.manifest) || options.manifest.manifestVersion !== 1 || !Array.isArray(options.manifest.tools)) invalid("Expected manifestVersion: 1 and a tools array.");
  const entries = new Map<string, Resolved>();
  for (const entry of options.manifest.tools) {
    if (!record(entry) || typeof entry.name !== "string" || !NAME.test(entry.name) || entries.has(entry.name)) invalid("Invalid or duplicate manifest tool name.");
    if (entry.kind !== "operation" && entry.kind !== "macro") invalid(`Invalid tool kind: ${entry.name}.`);
    if (typeof entry.target !== "string" || !entry.target || typeof entry.description !== "string" || !entry.description.trim()) invalid(`Tool ${entry.name} needs a target and nonempty description.`);
    if (!SAFETY.includes(entry.safety)) invalid(`Invalid safety tier: ${entry.name}.`);
    if (!record(entry.inputSchema) || entry.inputSchema.type !== "object") invalid(`Tool ${entry.name} needs an object input schema.`);
    try { json(entry.inputSchema, DEFAULT_MAX_RESULT_BYTES); } catch { invalid(`Tool ${entry.name} has a non-JSON or oversized input schema.`); }
    if (!record(entry.reserved)) invalid(`Tool ${entry.name} needs reserved field metadata.`);
    const fields = [entry.reserved.idempotencyKey, entry.reserved.confirmationToken];
    if (fields.some((v) => v !== null && (typeof v !== "string" || !v)) || (fields[0] !== null && fields[0] === fields[1])) invalid(`Invalid reserved fields: ${entry.name}.`);
    const resolved: Resolved = { entry, safety: entry.safety };
    // Resolve only selected targets below, so page-scoped catalogs may include
    // operations whose descriptors were deliberately omitted from this page.
    entries.set(entry.name, resolved);
  }
  const names = options.allowlist ?? [...entries.values()].filter(({ entry }) => entry.kind === "operation" && entry.safety === "read_only").map(({ entry }) => entry.name);
  if (!Array.isArray(names) || new Set(names).size !== names.length) invalid("allowlist must contain unique manifest names.");
  return names.map((name) => {
    const resolved = entries.get(name);
    if (!resolved) invalid(`Unknown allowlist tool: ${name}.`);
    const entry = resolved.entry;
    if (entry.kind === "operation") {
      const op = options.core.operation(entry.target);
      if (!op) invalid(`No registered operation for ${entry.name}: ${entry.target}.`);
      if (!SAFETY.includes(op.agent.safety)) invalid(`Invalid operation safety: ${entry.target}.`);
      resolved.op = op;
      resolved.safety = SAFETY[Math.max(SAFETY.indexOf(op.agent.safety), SAFETY.indexOf(entry.safety))] ?? "irreversible";
    } else {
      const macro = options.macros?.[entry.target];
      if (!macro) invalid(`No macro descriptor for ${entry.name}: ${entry.target}.`);
      if (!SAFETY.includes(macro.safety)) invalid(`Invalid macro safety: ${entry.target}.`);
      resolved.macro = macro;
      let tier = Math.max(SAFETY.indexOf(macro.safety), SAFETY.indexOf(entry.safety));
      for (const step of macro.steps) {
        const op = options.core.operation(step.operation);
        if (!op) invalid(`Unregistered macro step: ${step.operation}.`);
        if (!SAFETY.includes(op.agent.safety)) invalid(`Invalid macro step safety: ${step.operation}.`);
        tier = Math.max(tier, SAFETY.indexOf(op.agent.safety));
      }
      resolved.safety = SAFETY[tier] ?? "irreversible";
    }
    // Metadata must never advertise or default-expose a safer tier than the
    // actual execution. Fail before registering any tool.
    if (resolved.safety !== entry.safety) invalid(`Manifest safety understates descriptor safety: ${entry.name}.`);
    return resolved;
  });
}

function cancelled(signal: AbortSignal): void {
  if (signal.aborted) throw new WebMcpError("CANCELLED", "WebMCP registration was cancelled.");
}

function previewSchema(entry: WebMcpToolEntry): Record<string, unknown> {
  const token = entry.reserved.confirmationToken;
  if (token === null) return entry.inputSchema;
  const schema = { ...entry.inputSchema };
  if (Array.isArray(schema.required)) schema.required = schema.required.filter((name) => name !== token);
  if (record(schema.properties)) {
    schema.properties = { ...schema.properties };
    delete (schema.properties as Record<string, unknown>)[token];
  }
  return schema;
}

/** Register only the active page's chosen tools. No browser side effects until
 * the entire catalog, targets, generated preview names and options are valid. */
export async function registerTungstenWebMcp(options: RegisterTungstenWebMcpOptions): Promise<WebMcpRegistration> {
  const backend = options.backend ?? detectWebMcpBackend();
  if (!backend) throw new WebMcpError("UNSUPPORTED", "WebMCP is unavailable in this browser. Use a supported secure-context browser or supply an explicit backend.");
  if ((backend.kind !== "document" && backend.kind !== "navigator") || !backend.modelContext || typeof backend.modelContext.registerTool !== "function" || (backend.kind === "navigator" && typeof backend.modelContext.unregisterTool !== "function")) invalid("Invalid WebMCP backend contract.");
  const maxBytes = options.maxResultBytes ?? DEFAULT_MAX_RESULT_BYTES;
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1024) invalid("maxResultBytes must be a safe integer of at least 1024.");
  for (const limits of [options.streamCollection, options.binaryResults]) {
    if (!limits) continue;
    if (!Number.isSafeInteger(limits.maxBytes) || limits.maxBytes < 1) invalid("Result policy maxBytes must be a positive safe integer.");
  }
  if (options.streamCollection && (!Number.isSafeInteger(options.streamCollection.maxEvents) || options.streamCollection.maxEvents < 1 || !Number.isSafeInteger(options.streamCollection.maxDurationMs) || options.streamCollection.maxDurationMs < 1 || options.streamCollection.maxDurationMs > 2 ** 31 - 1 || maxBytes <= 1024)) invalid("streamCollection requires positive maxEvents, bounded maxDurationMs and maxResultBytes greater than 1024.");
  const deadlineMs = options.toolDeadlineMs ?? 0;
  if (!Number.isSafeInteger(deadlineMs) || deadlineMs < 0) invalid("toolDeadlineMs must be a non-negative safe integer.");
  if (options.binaryResults && (options.binaryResults.mode !== "base64" && options.binaryResults.mode !== "download" || options.binaryResults.mode === "download" && typeof options.binaryResults.render !== "function")) invalid("Invalid binaryResults renderer.");
  const selected = resolve(options);
  const controller = new AbortController();
  const abort = (): void => { controller.abort(options.signal?.reason); };
  if (options.signal?.aborted) abort();
  cancelled(controller.signal);
  const tools: WebMcpTool[] = [];
  for (const resolved of selected) {
    const { entry, op, macro, safety } = resolved;
    const create = (preview: boolean): WebMcpTool => ({
      name: preview ? `${entry.name}__preview` : entry.name,
      description: preview ? `Preview ${entry.name} and obtain a confirmation_token bound to these exact arguments. Review the effects before executing. ${entry.description}` : entry.description,
      inputSchema: preview ? previewSchema(entry) : entry.inputSchema,
      annotations: { readOnlyHint: preview || safety === "read_only", untrustedContentHint: true, consequentialHint: !preview && safety !== "read_only", debugging: false },
      async execute(raw, context) {
        if (controller.signal.aborted || context?.signal?.aborted) return failure(entry.target, "CANCELLED", "The tool call was cancelled; do not assume an in-flight mutation had no effect.", "TRANSPORT_FAILED");
        if (!record(raw) || (Object.getPrototypeOf(raw) !== Object.prototype && Object.getPrototypeOf(raw) !== null)) return failure(entry.target, "INVALID_ARGUMENTS", "Pass a JSON object matching the tool input schema.");
        try {
          json(raw, maxBytes);
          if (!matchesSchema(raw, preview ? previewSchema(entry) : entry.inputSchema)) return failure(entry.target, "INVALID_ARGUMENTS", "Pass arguments matching the published tool input schema.");
        } catch {
          return failure(entry.target, "INVALID_ARGUMENTS", "Pass bounded JSON arguments matching the supported input schema.");
        }
        const args = { ...raw };
        const opts: CallOptions = { allowConfirmTrue: false, verify: !preview && safety === "irreversible" };
        if (deadlineMs > 0) opts.deadlineMs = deadlineMs;
        for (const [field, option] of [[entry.reserved.idempotencyKey, "idempotencyKey"], [entry.reserved.confirmationToken, "confirm"]] as const) {
          if (field === null) continue;
          const value = args[field];
          delete args[field];
          if (value === undefined) continue;
          if (typeof value !== "string" || !value) return failure(entry.target, "INVALID_RESERVED_FIELD", `Reserved field ${field} must be a nonempty string; use the preview token for confirmation.`);
          opts[option] = value;
        }
        const calls = macro ? macro.steps.map((s) => options.core.operation(s.operation)) : [op];
        if (!preview && calls.some((operation) => operation?.stream !== undefined) && (!options.streamCollection || macro)) return failure(entry.target, "STREAMING_UNSUPPORTED", "Enable bounded streamCollection for a single operation, or use the SDK stream API; streaming macros are unsupported.");
        if (!preview && !options.binaryResults && calls.some((operation) => operation?.responses.some((response) => response.kind === "success" && binaryMediaType(response.mediaType)))) return failure(entry.target, "BINARY_UNSUPPORTED", "Enable bounded binaryResults or use the SDK directly for binary output.");
        const secretResult = entry.shownOnce || entry.sensitiveResponseFields?.length || macro?.shownOnce || macro?.sensitiveResponseFields?.length || calls.some((operation) => operation?.agent.shownOnce || operation?.agent.sensitiveResponseFields.length);
        if (!preview && secretResult) {
          return failure(entry.target, "SENSITIVE_RESULT_UNSUPPORTED", "This tool can return credentials or values shown only once. Use the SDK directly in trusted application code; this WebMCP adapter does not expose secret-bearing results.");
        }
        if (preview && op?.agent.preview.mode === "header" && secretResult) return failure(entry.target, "SENSITIVE_RESULT_UNSUPPORTED", "Server previews of secret-bearing tools are not exposed by this WebMCP adapter; use the SDK directly.");
        if (preview && op?.agent.preview.mode === "endpoint") {
          const target = options.core.operation(op.agent.preview.operation);
          if (target && (target.agent.safety !== "read_only" || target.stream || target.agent.shownOnce || target.agent.sensitiveResponseFields.length || secretResult)) {
            return failure(entry.target, "PREVIEW_UNSUPPORTED", "The configured server-preview endpoint is consequential, streaming, or secret-bearing. Use the SDK directly in trusted application code.");
          }
        }
        const callController = new AbortController();
        const stop = (): void => { callController.abort(); };
        controller.signal.addEventListener("abort", stop, { once: true });
        context?.signal?.addEventListener("abort", stop, { once: true });
        opts.signal = callController.signal;
        // Bound actual HTTP bytes before binary/JSON materialization. The SDK
        // applies this to fallback non-SSE responses as well.
        opts.maxResponseBytes = !preview && op && options.binaryResults && op.responses.some((response) => response.kind === "success" && binaryMediaType(response.mediaType)) ? options.binaryResults.maxBytes : maxBytes;
        try {
          if (!preview && op?.stream && options.streamCollection) {
            const result = await collectStream(options.core.stream(op, args, opts), options.streamCollection, entry.target, safety, callController.signal, json, maxBytes);
            return json(result, maxBytes);
          }
          const result = macro
            ? await (preview ? options.core.previewMacro(macro, args, opts) : options.core.runMacro(macro, args, opts))
            : await (preview ? options.core.preview(op!, args, opts) : options.core.call(op!, args, opts));
          if (result.ok && result.value instanceof Uint8Array) {
            if (!options.binaryResults) return failure(entry.target, "BINARY_UNSUPPORTED", "Unexpected binary output was withheld. The call may have taken effect; inspect API state before retrying.", "UNEXPECTED_RESPONSE");
            try {
              const mediaType = result.meta.headers["content-type"] ?? op?.responses.find((response) => response.kind === "success")?.mediaType ?? "application/octet-stream";
              return json({ ...result, value: await renderBinary(result.value, mediaType, entry.target, options.binaryResults, callController.signal, maxBytes) }, maxBytes);
            } catch (error) {
              return failure(entry.target, error instanceof Error ? error.message : "BINARY_RENDER_FAILED", "Binary output was withheld. The call may have taken effect; inspect API state before retrying.", "UNEXPECTED_RESPONSE");
            }
          }
          const normalized = result.ok && result.value === undefined ? { ...result, value: null } : result;
          try { return json(normalized, maxBytes); } catch (error) {
            const reason = error instanceof Error ? error.message : "";
            const code = ["RESULT_TOO_LARGE", "RESULT_TOO_COMPLEX", "NON_JSON_RESULT", "STREAMING_UNSUPPORTED"].includes(reason) ? reason : "NON_JSON_RESULT";
            return failure(entry.target, code, "The API result cannot be returned as bounded JSON. The call may already have taken effect; inspect API state before retrying.", "UNEXPECTED_RESPONSE");
          }
        } catch {
          return failure(entry.target, "EXECUTION_FAILED", "The SDK execution failed unexpectedly. Inspect API state before retrying a mutation.", "TRANSPORT_FAILED");
        } finally {
          controller.signal.removeEventListener("abort", stop);
          context?.signal?.removeEventListener("abort", stop);
        }
      },
    });
    tools.push(create(false));
    if (entry.reserved.confirmationToken !== null) tools.push(create(true));
  }
  const names = tools.map((tool) => tool.name);
  if (names.some((name) => !NAME.test(name)) || new Set(names).size !== names.length) invalid("Invalid or colliding generated preview tool names.");
  let claims = owned.get(backend.modelContext);
  if (!claims) { claims = new Set(); owned.set(backend.modelContext, claims); }
  if (names.some((name) => claims.has(name))) invalid("A tool name is already owned by another Tungsten registration.");
  for (const name of names) claims.add(name);
  const registered: string[] = [];
  let registering = true;
  let disposed = false;
  let disposal: Promise<void> | undefined;
  const dispose = (): Promise<void> => {
    if (disposal) return disposal;
    disposed = true;
    controller.abort();
    options.signal?.removeEventListener("abort", abort);
    // Never clear the host's catalog: legacy cleanup is only for names whose
    // registration completed successfully. Current cleanup is signal-owned.
    const failed = new Set<string>();
    if (backend.kind === "navigator") for (const name of [...registered].reverse()) {
      try { backend.modelContext.unregisterTool(name); } catch { failed.add(name); }
    }
    // Keep failed legacy cleanup names claimed: a later registration must not
    // overwrite a tool still owned by this adapter.
    for (const name of names) if (!failed.has(name)) claims!.delete(name);
    if (failed.size) {
      registered.splice(0, registered.length, ...failed);
      // A caller can retry a failed cleanup. Successfully removed names are
      // never unregistered again, and unresolved names stay reserved.
      return Promise.reject(new WebMcpError("REGISTRATION_FAILED", "Some owned legacy tools could not be unregistered; retry dispose()."));
    }
    disposal = Promise.resolve();
    return disposal;
  };
  options.signal?.addEventListener("abort", abort, { once: true });
  const cleanupOnAbort = (): void => { if (!disposed && !registering) void dispose().catch(() => undefined); };
  controller.signal.addEventListener("abort", cleanupOnAbort, { once: true });
  try {
    for (const tool of tools) {
      cancelled(controller.signal);
      if (backend.kind === "document") await backend.modelContext.registerTool(tool, { signal: controller.signal });
      else backend.modelContext.registerTool(tool);
      registered.push(tool.name);
      cancelled(controller.signal);
    }
  } catch (error) {
    registering = false;
    await dispose().catch(() => undefined);
    if (error instanceof WebMcpError) throw error;
    throw new WebMcpError("REGISTRATION_FAILED", "WebMCP tool registration failed; owned registrations were rolled back.");
  }
  registering = false;
  return Object.freeze({ backend: backend.kind, registeredNames: Object.freeze([...registered]), dispose });
}
