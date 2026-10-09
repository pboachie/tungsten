// SPDX-License-Identifier: Apache-2.0
/**
 * One MCP session: its own `ClientCore` (so confirmation tokens and
 * shown-once state never cross sessions) and the tool handlers of both
 * disclosure modes (planning/05 "MCP server", planning/07 "MCP tool
 * surface").
 *
 * Every outcome is a tool result, never a protocol error: failures are
 * `isError: true` with the diagnostic envelope verbatim as
 * `structuredContent` and its remediation in the text, so the model sees
 * what to do next.
 */
import { createHash } from "node:crypto";

import type { ClientCore, CallOptions, Diagnostic, MacroDescriptor, MacroStepPreview, OperationDescriptor, PreviewResult, Result } from "@tungsten/runtime";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";

import { inCluster, PROGRESSIVE_TOOLS, TOOL_META_KEY, verifies, type Catalog, type CatalogTool } from "./catalog.js";
import {
  REDACTED_REPEAT,
  SHOWN_ONCE_LINE,
  canonicalJson,
  clip,
  compactJson,
  envelope,
  holdsAny,
  isRecord,
  redactPaths,
  skeleton,
  truncate,
} from "./render.js";
import { apiHost, runSandboxed, type SandboxOutcome } from "./sandbox.js";
import { bm25, suggest } from "./search.js";
import type { ClusterEntry, ServerOptions } from "./types.js";

export const DEFAULT_MAX_RESULT_CHARS = 50000;
/** The `maxLength` of run_script's `code`. */
const MAX_SCRIPT_CHARS = 100000;
const DEFAULT_SEARCH_LIMIT = 10;
const SEARCH_HINT =
  "Call describe_tool(name) for the schema, preview(name, arguments) before destructive or irreversible calls, invoke(name, arguments) to execute (invoke_read for read_only tools).";

/** What `run_script` needs once the server found deno. */
export interface SandboxConfig {
  deno: string;
  timeoutMs: number;
  memoryMb: number;
  maxCalls: number;
}

type Outcome =
  | { ok: true; value: unknown; verification?: unknown }
  | { ok: false; error: Diagnostic; partial?: unknown };

/** A tool call with its SDK arguments and options separated. */
interface Prepared {
  tool: CatalogTool;
  /** The SDK args object, or the unparsable input (passed through so the
   * runtime answers its own VALIDATION_FAILED envelope). */
  args: unknown;
  idempotencyKey: unknown;
  confirm: string | undefined;
  /** An argument this layer could not decode (nothing is sent). */
  invalid?: Diagnostic;
}

const BASE64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;
const BASE64URL = /^[A-Za-z0-9_-]*$/;

/** The hints a host gates by: the tool's own annotations without the title. */
function toolHints(tool: CatalogTool): { readOnlyHint: boolean; destructiveHint: boolean; idempotentHint: boolean } {
  const { readOnlyHint, destructiveHint, idempotentHint } = tool.annotations;
  return { readOnlyHint, destructiveHint, idempotentHint };
}

/** The progressive meta tool that calls `tool`. */
function callTool(tool: CatalogTool): "invoke_read" | "invoke" {
  return tool.safety === "read_only" ? "invoke_read" : "invoke";
}

/** The bytes of base64 (standard, padded) or base64url (unpadded) text, or
 * null when `text` is neither. */
function decodeBase64(text: string): Uint8Array | null {
  if (BASE64.test(text)) return new Uint8Array(Buffer.from(text, "base64"));
  if (BASE64URL.test(text) && text.length % 4 !== 1) return new Uint8Array(Buffer.from(text, "base64url"));
  return null;
}

type ArgSpec = Record<string, { type: "string" | "integer" | "object"; required?: boolean }>;

function describeRequest(request: MacroStepPreview["request"]): string {
  if (request === null) return "its request is built from an earlier step's whole result, so it cannot be shown before it runs";
  const body = request.body === undefined || request.body === null ? "" : ` with body ${compactJson(request.body)}`;
  return `${request.method} ${request.url}${body}`;
}

/** A macro preview's steps as text: each step's request and effects, with
 * the placeholders of values that earlier steps produce. */
export function stepLines(preview: PreviewResult): string[] {
  const steps = Array.isArray(preview.steps) ? preview.steps : [];
  if (steps.length === 0) return [];
  const lines = [`Steps (nothing is sent by this preview; <from step NAME: path> is a value an earlier step returns):`];
  for (const step of steps) {
    const bound = step.as ? `, result as ${step.as}` : "";
    lines.push(`${step.step}. ${step.kind} ${step.operation} (${step.safety}${bound}): ${describeRequest(step.request)}`);
    for (const effect of Array.isArray(step.effects) ? step.effects : []) lines.push(`   - ${effect}`);
  }
  return lines;
}

/** Events a streamed call collects before it stops reading (the result then
 * has `truncated: true`). */
const MAX_STREAM_EVENTS = 1000;

/** Default `ServerOptions.maxStreamBytes`: the JSON bytes of the events a
 * streamed call collects (4 MiB). */
const DEFAULT_MAX_STREAM_BYTES = 4 * 1024 * 1024;

/** Default `ServerOptions.maxStreamMs`: the wall-clock time a streamed call
 * may take to collect its events (60 s), whatever the idle timeout. */
const DEFAULT_MAX_STREAM_MS = 60000;

function positive(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 1 ? Math.floor(value) : fallback;
}

/** Whether a call of `op` reads its event stream: when the operation only
 * answers a stream, or when it also answers a plain body and the arguments
 * set the stream's request flag to `true`. */
function streamRequested(op: OperationDescriptor, args: Record<string, unknown>): boolean {
  const spec = op.stream;
  if (!spec) return false;
  const first = op.responses.find((r) => r.kind === "success")?.mediaType ?? "";
  if (first.split(";")[0]?.trim().toLowerCase() === "text/event-stream") return true;
  const flag = spec.flag;
  const body = op.body;
  if (flag === undefined || !body) return false;
  if (body.shape.kind === "merged") return args[flag] === true;
  const inner = args[body.shape.arg];
  return isRecord(inner) && inner[flag] === true;
}

export class Session {
  readonly #catalog: Catalog;
  readonly #options: ServerOptions;
  readonly #sandbox: SandboxConfig | null;
  readonly #maxChars: number;
  readonly #maxStreamBytes: number;
  readonly #maxStreamMs: number;
  readonly #core: ClientCore | null;
  readonly #coreError: string | null;
  /** Digests of identical replayable calls whose sensitive or shown-once
   * results this session has already returned. Digests only: no argument
   * or secret is kept. */
  readonly #shown = new Set<string>();

  constructor(catalog: Catalog, options: ServerOptions, sandbox: SandboxConfig | null) {
    this.#catalog = catalog;
    this.#options = options;
    this.#sandbox = sandbox;
    const cap = options.maxResultChars;
    this.#maxChars = typeof cap === "number" && Number.isFinite(cap) && cap >= 200 ? Math.floor(cap) : DEFAULT_MAX_RESULT_CHARS;
    this.#maxStreamBytes = positive(options.maxStreamBytes, DEFAULT_MAX_STREAM_BYTES);
    this.#maxStreamMs = positive(options.maxStreamMs, DEFAULT_MAX_STREAM_MS);
    let core: ClientCore | null = null;
    let coreError: string | null = null;
    try {
      const created = typeof options.createCore === "function" ? options.createCore() : null;
      if (isRecord(created) && typeof created.call === "function") core = created;
      else coreError = "createCore did not return a ClientCore";
    } catch (error) {
      coreError = `createCore failed: ${error instanceof Error ? error.message : String(error)}`.slice(0, 300);
    }
    this.#core = core;
    this.#coreError = coreError;
  }

  /** Handle one `tools/call` with its raw `name` and `arguments`; never
   * throws. Arguments may be an object or its JSON text. */
  async call(name: unknown, args: unknown): Promise<CallToolResult> {
    if (typeof name !== "string" || name === "") {
      return this.#failure(
        null,
        envelope("tools/call", "VALIDATION_FAILED", {
          failed_parameter: "name",
          received_value: name,
          expected: "a tool name from tools/list",
          remediation: "Pass the tool's name as a string, from tools/list.",
        }),
      );
    }
    let parsed: unknown = args === undefined || args === null ? {} : args;
    if (typeof parsed === "string") {
      try {
        parsed = JSON.parse(parsed);
      } catch {
        // Reported below.
      }
    }
    if (!isRecord(parsed)) {
      return this.#failure(
        null,
        envelope(name, "VALIDATION_FAILED", {
          failed_parameter: "arguments",
          received_value: args,
          expected: "a JSON object (or its JSON text)",
          remediation: `Pass arguments as a JSON object of ${clip(name)}'s input schema and call it again.`,
        }),
      );
    }
    try {
      return await this.#dispatch(name, parsed);
    } catch (error) {
      return this.#failure(
        null,
        envelope(name, "UNEXPECTED_RESPONSE", {
          remediation: `The MCP server failed while handling ${clip(name)} (${error instanceof Error ? error.message : String(error)}). This is a bug in @tungsten/mcp; report it. If the call may have been sent, check its effect before repeating it.`,
        }),
      );
    }
  }

  async #dispatch(name: string, args: Record<string, unknown>): Promise<CallToolResult> {
    const catalog = this.#catalog;
    if (name === "run_script" && this.#sandbox) {
      const bad = this.#checkArgs(name, args, { code: { type: "string", required: true } });
      if (bad) return bad;
      if ((args.code as string).length > MAX_SCRIPT_CHARS) {
        return this.#failure(
          null,
          envelope(name, "VALIDATION_FAILED", {
            failed_parameter: "code",
            expected: `at most ${MAX_SCRIPT_CHARS} characters`,
            remediation: `The script is longer than ${MAX_SCRIPT_CHARS} characters; split the work into several scripts. Nothing ran.`,
          }),
        );
      }
      return await this.#runScript(args.code as string);
    }
    if (catalog.mode === "discrete") {
      if (name === "preview" && catalog.tools.some((t) => t.safety !== "read_only")) {
        const bad = this.#checkArgs(name, args, { tool: { type: "string", required: true }, arguments: { type: "object" } });
        if (bad) return bad;
        const tool = this.#lookup(args.tool as string, "tool", name);
        return "content" in tool ? tool : await this.#preview(tool, args.arguments);
      }
      const tool = catalog.byName.get(name);
      if (!tool) return this.#unknownTool(name);
      const prepared = this.#prepare(tool, args);
      return this.#render(tool, await this.#execute(prepared), prepared);
    }
    if (name === "invoke_read" && !catalog.tools.some((t) => t.safety === "read_only")) return this.#unknownTool(name);
    switch (name) {
      case "search_tools": {
        const bad = this.#checkArgs(name, args, { query: { type: "string", required: true }, cluster: { type: "string" }, limit: { type: "integer" } });
        return bad ?? this.#search(args);
      }
      case "describe_tool": {
        const bad = this.#checkArgs(name, args, { name: { type: "string", required: true } });
        if (bad) return bad;
        const tool = this.#lookup(args.name as string, "name", name);
        return "content" in tool ? tool : this.#describe(tool);
      }
      case "invoke_read":
      case "invoke":
      case "preview": {
        const bad = this.#checkArgs(name, args, { name: { type: "string", required: true }, arguments: { type: "object" } });
        if (bad) return bad;
        const tool = this.#lookup(args.name as string, "name", name);
        if ("content" in tool) return tool;
        if (name === "invoke_read" && tool.safety !== "read_only") {
          return this.#withToolMeta(
            this.#failure(
              null,
              envelope(name, "VALIDATION_FAILED", {
                failed_parameter: "name",
                received_value: tool.name,
                expected: "the name of a read_only tool",
                remediation: `${tool.name} is ${tool.safety}, so invoke_read refuses it; nothing ran. Call preview(name, arguments) and then invoke(name, arguments) instead.`,
              }),
            ),
            tool,
          );
        }
        if (name === "preview") return this.#withToolMeta(await this.#preview(tool, args.arguments), tool);
        const prepared = this.#prepare(tool, args.arguments);
        return this.#withToolMeta(this.#render(tool, await this.#execute(prepared), prepared), tool);
      }
      case "list_clusters": {
        const bad = this.#checkArgs(name, args, {});
        return bad ?? this.#listClusters();
      }
      default:
        return this.#unknownTool(name);
    }
  }

  // ------------------------------------------------------------- results

  /** `result` with the tool's tier and annotations in `_meta`, so a host that
   * sees only `invoke` or `preview` still learns what it just ran. */
  #withToolMeta(result: CallToolResult, tool: CatalogTool): CallToolResult {
    return { ...result, _meta: { ...(isRecord(result._meta) ? result._meta : {}), [TOOL_META_KEY]: { name: tool.name, safety: tool.safety, annotations: toolHints(tool) } } };
  }

  /** A success result. `rendered` names a member of `structured` that
   * `lines` already render, left out of the text's JSON (it stays in
   * `structuredContent`). */
  #success(structured: Record<string, unknown> | null, lines: string[], rendered: string | null = null): CallToolResult {
    const { value, note } = truncate(structured, this.#maxChars);
    let shown: unknown = value;
    if (rendered !== null && isRecord(value) && rendered in value) {
      const { [rendered]: _omitted, ...rest } = value;
      shown = rest;
    }
    const text = [structured === null ? "OK (no content)." : compactJson(shown), ...lines, ...(note ? [note] : [])].join("\n");
    const result: CallToolResult = { content: [{ type: "text", text }] };
    if (structured !== null) result.structuredContent = value as Record<string, unknown>;
    return result;
  }

  /** An error result: the envelope verbatim and its remediation as text. */
  #failure(tool: CatalogTool | null, error: Diagnostic, partial?: unknown): CallToolResult {
    const lines = [`${error.category} (retryable: ${error.retryable}): ${error.remediation}`];
    if (error.next_action) lines.push(`next_action: ${error.next_action}`);
    const detail = [
      error.failed_parameter ? `failed_parameter: ${error.failed_parameter}` : null,
      error.expected ? `expected: ${error.expected}` : null,
      error.retry_after_ms !== null ? `retry_after_ms: ${error.retry_after_ms}` : null,
    ].filter((x): x is string => x !== null);
    if (detail.length > 0) lines.push(detail.join("; "));
    if (tool) lines.push(...this.#mcpHints(tool, error));
    if (partial !== undefined) {
      const { value, note } = truncate(partial, this.#maxChars);
      lines.push(`Already produced before the failure (store any one-time secret in it now; it is not repeated): ${compactJson(value)}`);
      if (note) lines.push(note);
    }
    return { content: [{ type: "text", text: lines.join("\n") }], structuredContent: error as unknown as Record<string, unknown>, isError: true };
  }

  /** The runtime names `CallOptions` and SDK operations; say which tool
   * fields and tools they are here. */
  #mcpHints(tool: CatalogTool, error: Diagnostic): string[] {
    const hints: string[] = [];
    const named = this.#namedTools(`${error.remediation} ${error.next_action ?? ""}`);
    if (named.length > 0) hints.push(`Here ${named.map((t) => `${t.target} is called as ${this.#callForm(t)}`).join("; ")}.`);
    const keyField = tool.reserved.idempotencyKey;
    if (error.failed_parameter === "idempotencyKey" && keyField) hints.push(`In this tool the idempotency key is the argument "${keyField}".`);
    if (error.category === "CONFIRMATION_REQUIRED") {
      const field = tool.reserved.confirmationToken;
      hints.push(
        field
          ? `Call ${this.#previewForm(tool)} with the same arguments, then call ${this.#callForm(tool)} again with "${field}" set to its confirmation_token (only a token from this session is accepted, never true).`
          : `This tool takes no confirmation token, so it cannot be confirmed through this server.`,
      );
    }
    return hints;
  }

  /** Tools whose SDK operation id or macro name `text` mentions (as a
   * whole dotted name), at most three, in order of first mention. */
  #namedTools(text: string): CatalogTool[] {
    const found: Array<[number, CatalogTool]> = [];
    for (const t of this.#catalog.tools) {
      if (t.target === t.name || !t.target.includes(".")) continue;
      const at = text.search(new RegExp(`(?<![\\w.])${t.target.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\w])`));
      if (at >= 0) found.push([at, t]);
    }
    return found.sort((a, b) => a[0] - b[0]).slice(0, 3).map(([, t]) => t);
  }

  #previewForm(tool: CatalogTool): string {
    return this.#catalog.mode === "discrete" ? `preview({"tool": "${tool.name}", "arguments": ...})` : `preview({"name": "${tool.name}", "arguments": ...})`;
  }

  #callForm(tool: CatalogTool): string {
    return this.#catalog.mode === "discrete" ? tool.name : `invoke({"name": "${tool.name}", "arguments": ...})`;
  }

  // ------------------------------------------------------- argument checks

  #checkArgs(tool: string, args: Record<string, unknown>, spec: ArgSpec): CallToolResult | null {
    const allowed = Object.keys(spec);
    for (const key of Object.keys(args)) {
      if (!(key in spec) && args[key] !== undefined) {
        const close = suggest(key, allowed, [], 1);
        return this.#failure(
          null,
          envelope(tool, "VALIDATION_FAILED", {
            failed_parameter: key,
            received_value: args[key],
            expected: allowed.length > 0 ? `one of: ${allowed.join(", ")}` : "no arguments",
            remediation: `Remove ${clip(key)}; ${tool} does not accept it${close.length > 0 ? ` (did you mean ${close[0]}?)` : ""}.`,
          }),
        );
      }
    }
    for (const [key, rule] of Object.entries(spec)) {
      const value = args[key];
      if (value === undefined || value === null) {
        if (!rule.required) continue;
        return this.#failure(
          null,
          envelope(tool, "VALIDATION_FAILED", { failed_parameter: key, expected: `a ${rule.type}`, remediation: `Pass ${key}; ${tool} cannot be called without it.` }),
        );
      }
      const ok =
        rule.type === "string"
          ? typeof value === "string" && value !== ""
          : rule.type === "integer"
            ? Number.isInteger(value)
            : isRecord(value) || typeof value === "string";
      if (!ok) {
        return this.#failure(
          null,
          envelope(tool, "VALIDATION_FAILED", {
            failed_parameter: key,
            received_value: value,
            expected: rule.type === "object" ? "an object (or its JSON text)" : `a${rule.type === "integer" ? "n" : ""} ${rule.type === "string" ? "non-empty string" : rule.type}`,
            remediation: `Pass ${key} as ${rule.type === "object" ? "an object" : `a${rule.type === "integer" ? "n" : ""} ${rule.type}`} and call ${tool} again.`,
          }),
        );
      }
    }
    return null;
  }

  /** The catalog tool called `name`, or an error result with suggestions. */
  #lookup(name: string, parameter: string, via: string): CatalogTool | CallToolResult {
    const tool = this.#catalog.byName.get(name);
    if (tool) return tool;
    const names = this.#catalog.tools.map((t) => t.name);
    const close = suggest(name, names, this.#ranked(name));
    const where = this.#catalog.mode === "discrete" ? "tools/list" : "search_tools(query)";
    return this.#failure(
      null,
      envelope(via, "VALIDATION_FAILED", {
        failed_parameter: parameter,
        received_value: name,
        expected: "the name of a tool of this server",
        remediation: `No tool is named ${clip(name)}.${close.length > 0 ? ` Did you mean: ${close.join(", ")}?` : ""} Use ${where} to find tools.`,
      }),
    );
  }

  /** Tool names sharing words with `text`, best first. */
  #ranked(text: string): string[] {
    const docs = this.#catalog.documents;
    return bm25(this.#catalog.index, docs.length, text)
      .map((h) => docs[h.index]?.name)
      .filter((n): n is string => typeof n === "string");
  }

  #unknownTool(name: string): CallToolResult {
    const catalog = this.#catalog;
    if (catalog.mode === "progressive" && catalog.byName.has(name)) {
      return this.#failure(
        null,
        envelope(name, "VALIDATION_FAILED", {
          failed_parameter: "name",
          received_value: name,
          expected: "a tool from tools/list",
          remediation: `${name} is reached through invoke: call invoke({"name": "${name}", "arguments": {...}}) (describe_tool gives its schema).`,
        }),
      );
    }
    const listed: string[] =
      catalog.mode === "discrete"
        ? [...catalog.tools.map((t) => t.name), ...(catalog.tools.some((t) => t.safety !== "read_only") ? ["preview"] : [])]
        : PROGRESSIVE_TOOLS.filter((t) => t !== "run_script" && (t !== "invoke_read" || catalog.tools.some((x) => x.safety === "read_only")));
    if (this.#sandbox) listed.push("run_script");
    const close = suggest(name, listed, catalog.mode === "discrete" ? this.#ranked(name) : []);
    return this.#failure(
      null,
      envelope(name, "VALIDATION_FAILED", {
        failed_parameter: "name",
        received_value: name,
        expected: "a tool from tools/list",
        remediation: `No tool is named ${clip(name)}.${close.length > 0 ? ` Did you mean: ${close.join(", ")}?` : ""} Call tools/list for the available tools.`,
      }),
    );
  }

  // -------------------------------------------------------------- calling

  #prepare(tool: CatalogTool, raw: unknown): Prepared {
    let args: unknown = raw === undefined || raw === null ? {} : raw;
    if (typeof args === "string") {
      try {
        args = JSON.parse(args);
      } catch {
        // Not JSON: the runtime reports the non-object arguments.
      }
    }
    if (!isRecord(args)) return { tool, args, idempotencyKey: undefined, confirm: undefined };
    const { idempotencyKey: keyField, confirmationToken: tokenField } = tool.reserved;
    const sdkArgs: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(args)) if (key !== keyField && key !== tokenField) sdkArgs[key] = value;
    const token = tokenField ? args[tokenField] : undefined;
    const prepared: Prepared = {
      tool,
      args: sdkArgs,
      idempotencyKey: keyField ? args[keyField] : undefined,
      // Only a string token is forwarded: `true` never confirms through MCP.
      confirm: typeof token === "string" && token !== "" ? token : undefined,
    };
    // A binary body travels as base64 text in JSON (its input schema says
    // `contentEncoding: base64`); the SDK takes the bytes. Decoded here, so
    // preview and call digest the same value.
    const op = tool.kind === "operation" && this.#core && typeof this.#core.operation === "function" ? this.#core.operation(tool.target) : undefined;
    const body = op?.body;
    if (body && body.encoding === "bytes" && body.shape.kind === "arg" && typeof sdkArgs[body.shape.arg] === "string") {
      const name = body.shape.arg;
      const bytes = decodeBase64(sdkArgs[name] as string);
      if (bytes) sdkArgs[name] = bytes;
      else {
        prepared.invalid = envelope(tool.target, "VALIDATION_FAILED", {
          failed_parameter: name,
          received_value: clip(sdkArgs[name] as string),
          expected: "the raw bytes as base64 text",
          remediation: `Pass ${name} as the base64 encoding of the raw bytes (standard with padding, or base64url); this server decodes it and sends the bytes. Nothing was sent.`,
        });
      }
    }
    return prepared;
  }

  #callOptions(prepared: Prepared, withConfirm: boolean): CallOptions {
    // Only preview tokens are forwarded, so the runtime must not offer `true`.
    const opts: CallOptions = { allowConfirmTrue: false };
    // A non-string key is passed through so the runtime reports it.
    if (prepared.idempotencyKey !== undefined && prepared.idempotencyKey !== null) opts.idempotencyKey = prepared.idempotencyKey as string;
    if (withConfirm && prepared.confirm !== undefined) opts.confirm = prepared.confirm;
    return opts;
  }

  #unavailable(tool: CatalogTool, what: string): Outcome {
    return {
      ok: false,
      error: envelope(tool.target, "UNEXPECTED_RESPONSE", {
        remediation: `The MCP server is misconfigured: ${what}. Nothing was sent; report it and do not retry.`,
      }),
    };
  }

  #resolve(tool: CatalogTool): { op: OperationDescriptor } | { macro: MacroDescriptor } | { error: Outcome } {
    const core = this.#core;
    if (!core) return { error: this.#unavailable(tool, this.#coreError ?? "no API client") };
    if (tool.kind === "macro") {
      const macros = isRecord(this.#options.macros) ? this.#options.macros : {};
      const macro = Object.prototype.hasOwnProperty.call(macros, tool.target) ? macros[tool.target] : undefined;
      return macro ? { macro } : { error: this.#unavailable(tool, `macro ${tool.target} of tool ${tool.name} is not in ServerOptions.macros`) };
    }
    const op = typeof core.operation === "function" ? core.operation(tool.target) : undefined;
    return op ? { op } : { error: this.#unavailable(tool, `operation ${tool.target} of tool ${tool.name} is not registered with the client core`) };
  }

  async #execute(prepared: Prepared): Promise<Outcome> {
    if (prepared.invalid) return { ok: false, error: prepared.invalid };
    const target = this.#resolve(prepared.tool);
    if ("error" in target) return target.error;
    const core = this.#core as ClientCore;
    const opts = this.#callOptions(prepared, true);
    const args = prepared.args as Record<string, unknown>;
    let result: Result<unknown>;
    if ("macro" in target) {
      result = await core.runMacro<unknown>(target.macro, args, opts);
    } else if (streamRequested(target.op, args)) {
      return this.#collectStream(target.op, args, opts, prepared.tool);
    } else {
      if (verifies(prepared.tool)) opts.verify = true;
      result = await core.call<unknown>(target.op, args, opts);
    }
    if (result.ok) return result.verification === undefined ? { ok: true, value: result.value } : { ok: true, value: result.value, verification: result.verification };
    return result.partial === undefined ? { ok: false, error: result.error } : { ok: false, error: result.error, partial: result.partial };
  }

  /** Collect the events of a stream in order: the events, or the failure
   * that ended the stream with the events before it as `partial`. Stops at
   * `MAX_STREAM_EVENTS` events (`truncated: true`) and fails when the events
   * exceed `maxStreamBytes` of JSON or the collection runs past
   * `maxStreamMs`. */
  async #collectStream(op: OperationDescriptor, args: Record<string, unknown>, opts: CallOptions, tool: CatalogTool): Promise<Outcome> {
    const core = this.#core as ClientCore;
    const events: unknown[] = [];
    let bytes = 0;
    let expired = false;
    const controller = new AbortController();
    const started = Date.now();
    const timer = setTimeout(() => {
      expired = true;
      controller.abort();
    }, this.#maxStreamMs);
    // The caller's own cancellation still ends the stream.
    const caller = opts.signal;
    const forward = (): void => controller.abort();
    if (caller?.aborted === true) controller.abort();
    else caller?.addEventListener("abort", forward, { once: true });
    const exceeded = (what: string): Outcome => ({
      ok: false,
      error: envelope(op.id, "UNEXPECTED_RESPONSE", {
        failed_parameter: "events",
        expected: what,
        remediation: `The stream was abandoned after ${events.length} ${events.length === 1 ? "event" : "events"} (${what}; ServerOptions.maxStreamBytes and maxStreamMs).${tool.safety === "read_only" ? " The events collected so far are in partial." : " The call took effect; do not repeat it. The events collected so far are in partial."}`,
      }),
      partial: { events },
    });
    try {
      const streamOpts: CallOptions = { ...opts, signal: controller.signal };
      for await (const item of core.stream<unknown>(op, args, streamOpts)) {
        if (expired || Date.now() - started >= this.#maxStreamMs) return exceeded(`at most ${this.#maxStreamMs} ms to collect the events`);
        if (!item.ok) return { ok: false, error: item.error, partial: { events } };
        bytes += Buffer.byteLength(JSON.stringify(item.value) ?? "null", "utf8");
        if (bytes > this.#maxStreamBytes) return exceeded(`at most ${this.#maxStreamBytes} bytes of events`);
        events.push(item.value);
        if (events.length >= MAX_STREAM_EVENTS) return { ok: true, value: { events, truncated: true } };
      }
      return expired ? exceeded(`at most ${this.#maxStreamMs} ms to collect the events`) : { ok: true, value: { events } };
    } finally {
      clearTimeout(timer);
      caller?.removeEventListener("abort", forward);
    }
  }

  /** Digest identifying an identical call: tool, SDK arguments and key. */
  #identity(prepared: Prepared): string {
    let args: string;
    try {
      args = canonicalJson(prepared.args);
    } catch {
      args = "<unserializable>";
    }
    const key = typeof prepared.idempotencyKey === "string" ? prepared.idempotencyKey : "";
    return createHash("sha256").update(`${prepared.tool.name}\n${args}\n${key}`).digest("hex");
  }

  /** Whether an identical repeat of this call is a replay of the first
   * (the same answer, no new effect): a read; an operation the server
   * deduplicates by content; or a keyed one called with the caller's key
   * (without one, `auto` keys are fresh per call). */
  #replays(prepared: Prepared): boolean {
    const tool = prepared.tool;
    if (tool.safety === "read_only") return true;
    if (tool.idempotency === "content_identity" || tool.idempotency === "content_hash") return true;
    if (tool.idempotency === "caller_owned" || tool.idempotency === "auto") return typeof prepared.idempotencyKey === "string" && prepared.idempotencyKey !== "";
    return false;
  }

  #render(tool: CatalogTool, outcome: Outcome, prepared: Prepared): CallToolResult {
    if (!outcome.ok) return this.#failure(tool, outcome.error, outcome.partial);
    let body = outcome.value;
    const lines: string[] = [];
    const sensitive = tool.sensitiveResponseFields;
    if (sensitive.length > 0 || tool.shownOnce) {
      // Only a replay returns what an earlier identical call returned; a
      // repeat that is a new effect (a second rotate or create) returns a
      // new secret, which must be shown.
      const id = this.#replays(prepared) ? this.#identity(prepared) : null;
      if (id !== null && this.#shown.has(id)) {
        const redacted = redactPaths(body, sensitive, REDACTED_REPEAT);
        body = redacted.value;
        if (redacted.found.length > 0) {
          lines.push(`${redacted.found.join(", ")} ${redacted.found.length === 1 ? "was" : "were"} returned by the first identical call in this session and ${redacted.found.length === 1 ? "is" : "are"} redacted here.`);
        }
      } else if (sensitive.length === 0 || holdsAny(body, sensitive)) {
        if (id !== null) this.#shown.add(id);
        if (tool.shownOnce) lines.push(SHOWN_ONCE_LINE);
      }
    }
    let structured: Record<string, unknown> | null;
    if (isRecord(body)) structured = { ...body };
    else structured = body === undefined ? null : { value: body };
    if (outcome.verification !== undefined) {
      structured = structured ?? {};
      if (!("verification" in structured)) structured.verification = outcome.verification;
      else lines.push(`verification: ${compactJson(outcome.verification)}`);
    }
    return this.#success(structured, lines);
  }

  async #preview(tool: CatalogTool, raw: unknown): Promise<CallToolResult> {
    const prepared = this.#prepare(tool, raw);
    if (prepared.invalid) return this.#failure(tool, prepared.invalid);
    const target = this.#resolve(tool);
    if ("error" in target) return this.#render(tool, target.error, prepared);
    const core = this.#core as ClientCore;
    const opts = this.#callOptions(prepared, false);
    const args = prepared.args as Record<string, unknown>;
    const result: Result<PreviewResult> =
      "macro" in target ? await core.previewMacro(target.macro, args, opts) : await core.preview(target.op, args, opts);
    if (!result.ok) return this.#failure(tool, result.error, result.partial);
    const preview = result.value;
    const field = tool.reserved.confirmationToken;
    const lines: string[] = stepLines(preview);
    const confirmed = [tool.safety, preview.safety].some((tier) => tier === "destructive" || tier === "irreversible");
    if (confirmed && field && preview.confirmation_token !== null) {
      const seconds = preview.expires_in_ms === null ? null : Math.round(preview.expires_in_ms / 1000);
      lines.push(
        `To execute, call ${this.#callForm(tool)} with the same arguments plus "${field}": "${preview.confirmation_token}" (valid ${seconds === null ? "briefly" : `${seconds} s`}, for these exact arguments, in this session only).`,
      );
    } else if (confirmed) {
      lines.push("This tool needs confirmation but takes no confirmation token, so it cannot be executed through this server.");
    } else if (preview.safety === "read_only") {
      lines.push(`Read-only: call ${this.#callForm(tool)} directly.`);
    } else {
      lines.push(`No confirmation is needed: call ${this.#callForm(tool)} with the same arguments to execute.`);
    }
    return this.#success(preview as unknown as Record<string, unknown>, lines, Array.isArray(preview.steps) ? "steps" : null);
  }

  // ------------------------------------------------------ progressive mode

  #search(args: Record<string, unknown>): CallToolResult {
    const catalog = this.#catalog;
    const query = args.query as string;
    const limit = args.limit === undefined || args.limit === null ? DEFAULT_SEARCH_LIMIT : (args.limit as number);
    if (limit < 1 || limit > 50) {
      return this.#failure(
        null,
        envelope("search_tools", "VALIDATION_FAILED", { failed_parameter: "limit", received_value: limit, expected: "an integer from 1 to 50", remediation: "Pass limit between 1 and 50." }),
      );
    }
    let cluster: ClusterEntry | null = null;
    if (typeof args.cluster === "string") {
      cluster = catalog.clusters.find((c) => c.name === args.cluster) ?? null;
      if (!cluster) {
        const names = catalog.clusters.map((c) => c.name);
        const close = suggest(args.cluster, names);
        return this.#failure(
          null,
          envelope("search_tools", "VALIDATION_FAILED", {
            failed_parameter: "cluster",
            received_value: args.cluster,
            expected: names.length > 0 ? `one of: ${names.join(", ")}` : "no cluster (this API has none)",
            remediation: `No cluster is named ${clip(args.cluster)}.${close.length > 0 ? ` Did you mean: ${close.join(", ")}?` : ""} Call list_clusters() for the clusters, or search without one.`,
          }),
        );
      }
    }
    const docs = catalog.documents;
    const c = cluster;
    let tools = bm25(catalog.index, docs.length, query)
      .map((h) => docs[h.index])
      .filter((t): t is CatalogTool => t !== null && t !== undefined && (c === null || inCluster(t, c)));
    let hint = SEARCH_HINT;
    if (tools.length === 0 && c) {
      tools = catalog.tools.filter((t) => inCluster(t, c));
      hint = `No tool matched the query; these are the tools of cluster ${c.name}. ${SEARCH_HINT}`;
    } else if (tools.length === 0) {
      hint = "No tool matched. Use other words from the task (resource names, verbs), or call list_clusters() and search one cluster.";
    }
    const results = tools.slice(0, limit).map((t) => ({
      name: t.name,
      summary: t.summary,
      safety: t.safety,
      annotations: toolHints(t),
      idempotency: t.idempotency,
      schema_tokens: t.schemaTokens,
      call_with: callTool(t),
    }));
    return this.#success({ results, hint }, []);
  }

  #describe(tool: CatalogTool): CallToolResult {
    const example = skeleton(tool.inputSchema);
    const call: Record<string, unknown> = isRecord(example) ? example : {};
    const { idempotencyKey: keyField, confirmationToken: tokenField } = tool.reserved;
    if (keyField) call[keyField] = "<UUIDv4: generate once, persist with the intent, reuse on every retry>";
    if (tokenField) {
      if (tool.safety === "destructive" || tool.safety === "irreversible") call[tokenField] = "<confirmation_token from preview with these arguments>";
      else delete call[tokenField];
    }
    const op = tool.kind === "operation" && this.#core && typeof this.#core.operation === "function" ? this.#core.operation(tool.target) : undefined;
    const remediation = op && isRecord(op.agent) && isRecord(op.agent.remediation) ? op.agent.remediation : {};
    const steps =
      tool.safety === "read_only"
        ? "Read-only: invoke it directly."
        : tool.safety === "mutating"
          ? `Mutating${keyField ? `: pass "${keyField}" and reuse it on every retry of the same intent` : ""}; preview(name, arguments) shows the request first.`
          : `${tool.safety === "irreversible" ? "Irreversible" : "Destructive"}: call preview(name, arguments) first, then invoke with the same arguments and ${tokenField ? `"${tokenField}" set to` : ""} its confirmation_token.`;
    return this.#success(
      {
        name: tool.name,
        kind: tool.kind,
        summary: tool.summary,
        description: tool.description,
        safety: tool.safety,
        idempotency: tool.idempotency,
        cluster: tool.cluster,
        annotations: tool.annotations,
        input_schema: tool.inputSchema,
        output_schema: tool.outputSchema,
        reserved_fields: { idempotency_key: keyField, confirmation_token: tokenField },
        sensitive_response_fields: tool.sensitiveResponseFields,
        shown_once: tool.shownOnce,
        remediation,
        schema_tokens: tool.schemaTokens,
        call_with: callTool(tool),
        example: { tool: callTool(tool), arguments: { name: tool.name, arguments: call } },
        hint: steps,
      },
      [],
    );
  }

  #listClusters(): CallToolResult {
    const catalog = this.#catalog;
    const clusters = catalog.clusters.map((c) => ({ name: c.name, summary: c.summary, tools: catalog.tools.filter((t) => inCluster(t, c)).length }));
    const unclustered = catalog.tools.filter((t) => !catalog.clusters.some((c) => inCluster(t, c))).length;
    return this.#success({ clusters, unclustered, hint: "search_tools(query, cluster) searches one cluster." }, []);
  }

  // -------------------------------------------------------------- sandbox

  /** One client call from a script, through the same handlers as MCP calls. */
  async #scriptCall(method: "invoke" | "preview", name: unknown, args: unknown): Promise<{ ok: boolean; [key: string]: unknown }> {
    const toolName = typeof name === "string" ? name : String(name);
    const tool = this.#lookup(toolName, "name", method);
    let result: CallToolResult;
    if ("content" in tool) result = tool;
    else if (method === "preview") result = await this.#preview(tool, args);
    else {
      const prepared = this.#prepare(tool, args);
      result = this.#render(tool, await this.#execute(prepared), prepared);
    }
    const text = result.content.map((c) => (c.type === "text" ? c.text : "")).join("\n");
    return result.isError ? { ok: false, error: result.structuredContent, text } : { ok: true, value: result.structuredContent ?? null, text };
  }

  async #runScript(code: string): Promise<CallToolResult> {
    const sandbox = this.#sandbox as SandboxConfig;
    const core = this.#core;
    const base = core && isRecord(core.options) && typeof core.options.baseUrl === "string" ? core.options.baseUrl : core?.api?.servers?.[0];
    const host = apiHost(base);
    if (!host) {
      return this.#failure(
        null,
        envelope("run_script", "VALIDATION_FAILED", {
          failed_parameter: "baseUrl",
          expected: "an absolute http(s) API base URL",
          remediation: `run_script needs the API's base URL to scope the sandbox's network access${this.#coreError ? ` (${this.#coreError})` : ""}; configure it in the server. Nothing ran.`,
        }),
      );
    }
    const outcome: SandboxOutcome = await runSandboxed({
      deno: sandbox.deno,
      host,
      code,
      timeoutMs: sandbox.timeoutMs,
      memoryMb: sandbox.memoryMb,
      maxCalls: sandbox.maxCalls,
      call: (method, name, args) => this.#scriptCall(method, name, args),
      refuse: () => ({
        ok: false,
        error: envelope("run_script", "VALIDATION_FAILED", {
          expected: `at most ${sandbox.maxCalls} calls per script`,
          remediation: `The script made more than ${sandbox.maxCalls} calls; this one was not executed. Split the work into several scripts.`,
        }),
      }),
    });
    // Results of calls that completed after the script ended: the script
    // never saw them, so they (and any one-time secret) are returned here.
    const lateLines = outcome.unreceived.map((late) => `${late.method} ${late.name} (${late.result.ok ? "ok" : "error"}): ${String(late.result.text ?? "")}`);
    const unreceived = outcome.unreceived.map((late) => {
      const { text: _text, ...result } = late.result;
      return { method: late.method, name: late.name, ...result };
    });
    const open = outcome.calls.filter((c) => c.ok === null).map((c) => `${c.method} ${c.name}`);
    if (outcome.status === "done") {
      const structured: Record<string, unknown> = { value: outcome.value, logs: outcome.logs, calls: outcome.calls };
      if (unreceived.length > 0) structured.unreceived = unreceived;
      const lines: string[] = [];
      if (lateLines.length > 0) lines.push("Results of calls the script did not receive (it returned before they completed):", ...lateLines);
      if (open.length > 0) lines.push(`Still in flight when the script returned (outcome unknown): ${open.join(", ")}; check their effects before running it again.`);
      return this.#success(structured, lines);
    }
    const completed = outcome.calls.filter((c) => c.ok !== null).map((c) => `${c.method} ${c.name} (${c.ok ? "ok" : "error"})`);
    const parts: string[] = [];
    if (completed.length > 0) parts.push(`Completed calls: ${completed.join(", ")}`);
    if (open.length > 0) parts.push(`${completed.length > 0 ? "still" : "Still"} in flight (outcome unknown): ${open.join(", ")}`);
    const calls = parts.length > 0 ? ` ${parts.join("; ")}; check their effects before running it again.` : " No tool call completed.";
    const error = envelope("run_script", "VALIDATION_FAILED", {
      failed_parameter: "code",
      expected: "a script that returns a JSON value within the limits",
      remediation: `${outcome.message}${/[.!?]$/.test(outcome.message) ? "" : "."}${calls}`,
      retryable: "after_remediation",
    });
    const result = this.#failure(null, error);
    if (outcome.logs.length > 0) result.content.push({ type: "text", text: `logs:\n${outcome.logs.join("\n")}` });
    if (lateLines.length > 0) result.content.push({ type: "text", text: ["Results of calls the script did not receive (they completed after it ended):", ...lateLines].join("\n") });
    return result;
  }
}
