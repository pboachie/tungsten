// SPDX-License-Identifier: Apache-2.0
import type { Diagnostic } from "@elysra/tungsten-runtime";
import { detectWebMcpBackend, registerOwned, WebMcpError, DEFAULT_MAX_RESULT_BYTES } from "./runtime.js";
import type { WebMcpBackend, WebMcpRegistration, WebMcpTool } from "./types.js";

/** Version of the dispatch contract this adapter speaks. */
export const WEBMCP_DISPATCH_VERSION = 1;
/** The header every dispatch request carries (the server's CSRF check). */
export const WEBMCP_DISPATCH_HEADER = "X-Tungsten-WebMCP";
const NAME = /^[A-Za-z0-9_.-]{1,128}$/;

export interface RegisterRemoteWebMcpOptions {
  /** The application's dispatch prefix on this page's own origin, such as
   * `/webmcp` (a path, or a same-origin URL). Another origin is refused. */
  endpoint: string;
  /** Explicitly distinguish the current API from the legacy API. */
  backend?: WebMcpBackend;
  /** Aborting unregisters this registration and cancels its active calls. */
  signal?: AbortSignal;
  /** Defaults to the global fetch, always with same-origin credentials. */
  fetch?: typeof fetch;
  /** UTF-8 bytes of one response; default 1 MiB. No silent clipping. */
  maxResultBytes?: number;
}

interface RemoteTool {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  annotations: WebMcpTool["annotations"];
  call: { tool: string; preview: boolean };
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function invalid(message: string): never {
  throw new WebMcpError("INVALID_CONFIGURATION", message);
}

function failure(operation: string, code: string, remediation: string, category: Diagnostic["category"]): { ok: false; error: Diagnostic } {
  return { ok: false, error: {
    status: "error", category, operation, code, http_status: null, failed_parameter: null,
    received_value: null, expected: null, remediation, retryable: "never", retry_after_ms: null,
    next_action: null, request_id: null, trace: { attempts: 0 },
  } };
}

/** The endpoint as an absolute URL of this page's origin, without a trailing slash. */
function sameOrigin(endpoint: string): string {
  if (typeof endpoint !== "string" || endpoint === "") invalid("endpoint must be a non-empty path or same-origin URL.");
  const here = typeof location === "undefined" ? undefined : location.href;
  if (here === undefined) invalid("registerRemoteWebMcp needs a browser page (location).");
  const url = new URL(endpoint, here);
  if (url.origin !== new URL(here).origin) invalid("endpoint must be on this page's own origin.");
  if (url.search || url.hash) invalid("endpoint must not have a query or fragment.");
  return url.href.replace(/\/+$/, "");
}

function readTools(body: unknown): RemoteTool[] {
  if (!record(body) || body.webmcpDispatchVersion !== WEBMCP_DISPATCH_VERSION || !Array.isArray(body.tools)) {
    invalid(`The dispatch endpoint did not answer webmcpDispatchVersion ${WEBMCP_DISPATCH_VERSION} tools.`);
  }
  return body.tools.map((tool: unknown) => {
    if (!record(tool) || typeof tool.name !== "string" || !NAME.test(tool.name) || typeof tool.description !== "string") invalid("Invalid dispatch tool.");
    if (!record(tool.inputSchema) || tool.inputSchema.type !== "object" || !record(tool.annotations) || !record(tool.call)) invalid(`Invalid dispatch tool ${tool.name}.`);
    const a = tool.annotations;
    const call = tool.call;
    if (typeof call.tool !== "string" || typeof call.preview !== "boolean") invalid(`Invalid dispatch call of ${tool.name}.`);
    return {
      name: tool.name,
      description: tool.description,
      inputSchema: tool.inputSchema,
      // A tool is never presented as safer than the server says.
      annotations: { readOnlyHint: a.readOnlyHint === true, untrustedContentHint: a.untrustedContentHint !== false, consequentialHint: a.consequentialHint !== false || a.readOnlyHint !== true, debugging: false },
      call: { tool: call.tool, preview: call.preview },
    };
  });
}

/**
 * Register the tools a Python (or other) dispatch endpoint of this page's
 * application exposes to the signed-in user. Tool calls go to that endpoint
 * with the page's own session credentials; the server lists the tools,
 * authorizes and runs every call, and keeps API credentials. Registration and
 * cleanup are the same as `registerTungstenWebMcp`'s: page-scoped, owned, and
 * removed on `dispose()` or abort.
 */
export async function registerRemoteWebMcp(options: RegisterRemoteWebMcpOptions): Promise<WebMcpRegistration> {
  const backend = options.backend ?? detectWebMcpBackend();
  if (!backend) throw new WebMcpError("UNSUPPORTED", "WebMCP is unavailable in this browser. Use a supported secure-context browser or supply an explicit backend.");
  const base = sameOrigin(options.endpoint);
  const maxBytes = options.maxResultBytes ?? DEFAULT_MAX_RESULT_BYTES;
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1024) invalid("maxResultBytes must be a safe integer of at least 1024.");
  const doFetch = options.fetch ?? globalThis.fetch;
  if (typeof doFetch !== "function") invalid("No fetch is available.");
  const controller = new AbortController();
  const abort = (): void => { controller.abort(options.signal?.reason); };
  if (options.signal?.aborted) abort();
  const headers = { [WEBMCP_DISPATCH_HEADER]: "1", accept: "application/json" };
  let listing: Response;
  try {
    listing = await doFetch(`${base}/tools`, { method: "GET", headers, credentials: "same-origin", signal: controller.signal });
  } catch {
    throw new WebMcpError("REGISTRATION_FAILED", "The dispatch endpoint could not be reached.");
  }
  if (!listing.ok) throw new WebMcpError("REGISTRATION_FAILED", `The dispatch endpoint answered ${listing.status} to the tool listing.`);
  const remote = readTools(await listing.json());
  const tools: WebMcpTool[] = remote.map((tool) => ({
    name: tool.name,
    description: tool.description,
    inputSchema: tool.inputSchema,
    annotations: tool.annotations,
    async execute(raw, context) {
      const operation = tool.call.tool;
      const consequential = !tool.call.preview && !tool.annotations.readOnlyHint;
      const unknown = consequential ? " The call may have taken effect; inspect API state before retrying." : "";
      if (controller.signal.aborted || context?.signal?.aborted) return failure(operation, "CANCELLED", "The tool call was cancelled; do not assume an in-flight mutation had no effect.", "TRANSPORT_FAILED");
      if (!record(raw)) return failure(operation, "INVALID_ARGUMENTS", "Pass a JSON object matching the tool input schema.", "VALIDATION_FAILED");
      const call = new AbortController();
      const stop = (): void => { call.abort(); };
      controller.signal.addEventListener("abort", stop, { once: true });
      context?.signal?.addEventListener("abort", stop, { once: true });
      try {
        const body = JSON.stringify({ tool: tool.call.tool, preview: tool.call.preview, arguments: raw });
        if (new TextEncoder().encode(body).byteLength > maxBytes) return failure(operation, "INVALID_ARGUMENTS", "The arguments are larger than the dispatch limit.", "VALIDATION_FAILED");
        const response = await doFetch(`${base}/call`, { method: "POST", headers: { ...headers, "content-type": "application/json" }, body, credentials: "same-origin", signal: call.signal });
        const text = await response.text();
        if (new TextEncoder().encode(text).byteLength > maxBytes) return failure(operation, "RESULT_TOO_LARGE", `The dispatch result is larger than maxResultBytes.${unknown}`, "UNEXPECTED_RESPONSE");
        if (!response.ok) return failure(operation, `DISPATCH_${response.status}`, `The dispatch endpoint refused the call (${response.status}); sign in again or reload the page.${unknown}`, response.status === 401 || response.status === 403 ? "AUTH_FAILED" : "TRANSPORT_FAILED");
        const result: unknown = JSON.parse(text);
        if (!record(result) || typeof result.ok !== "boolean") return failure(operation, "INVALID_DISPATCH_RESULT", `The dispatch endpoint answered an invalid result.${unknown}`, "UNEXPECTED_RESPONSE");
        return result;
      } catch {
        return failure(operation, call.signal.aborted ? "CANCELLED" : "DISPATCH_FAILED", `The dispatch request failed.${unknown || " Retry the read."}`, "TRANSPORT_FAILED");
      } finally {
        controller.signal.removeEventListener("abort", stop);
        context?.signal?.removeEventListener("abort", stop);
      }
    },
  }));
  return registerOwned(backend, tools, controller, abort, options.signal);
}
