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

import { inCluster, PROGRESSIVE_TOOLS, verifies, type Catalog, type CatalogTool } from "./catalog.js";
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
const DEFAULT_SEARCH_LIMIT = 10;
const SEARCH_HINT =
  "Call describe_tool(name) for the schema, preview(name, arguments) before destructive or irreversible calls, invoke(name, arguments) to execute.";

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

export class Session {
  readonly #catalog: Catalog;
  readonly #options: ServerOptions;
  readonly #sandbox: SandboxConfig | null;
  readonly #maxChars: number;
  readonly #core: ClientCore | null;
  readonly #coreError: string | null;
  /** Digests of identical calls whose sensitive or shown-once results this
   * session has already returned. Digests only: no argument or secret is kept. */
  readonly #shown = new Set<string>();

  constructor(catalog: Catalog, options: ServerOptions, sandbox: SandboxConfig | null) {
    this.#catalog = catalog;
    this.#options = options;
    this.#sandbox = sandbox;
    const cap = options.maxResultChars;
    this.#maxChars = typeof cap === "number" && Number.isFinite(cap) && cap >= 200 ? Math.floor(cap) : DEFAULT_MAX_RESULT_CHARS;
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

  /** Handle one `tools/call`; never throws. */
  async call(name: string, args: Record<string, unknown> | undefined): Promise<CallToolResult> {
    try {
      return await this.#dispatch(name, args ?? {});
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
      return bad ?? (await this.#runScript(args.code as string));
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
      case "invoke":
      case "preview": {
        const bad = this.#checkArgs(name, args, { name: { type: "string", required: true }, arguments: { type: "object" } });
        if (bad) return bad;
        const tool = this.#lookup(args.name as string, "name", name);
        if ("content" in tool) return tool;
        if (name === "preview") return await this.#preview(tool, args.arguments);
        const prepared = this.#prepare(tool, args.arguments);
        return this.#render(tool, await this.#execute(prepared), prepared);
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
        : PROGRESSIVE_TOOLS.filter((t) => t !== "run_script");
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
    return {
      tool,
      args: sdkArgs,
      idempotencyKey: keyField ? args[keyField] : undefined,
      // Only a string token is forwarded: `true` never confirms through MCP.
      confirm: typeof token === "string" && token !== "" ? token : undefined,
    };
  }

  #callOptions(prepared: Prepared, withConfirm: boolean): CallOptions {
    const opts: CallOptions = {};
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
    const target = this.#resolve(prepared.tool);
    if ("error" in target) return target.error;
    const core = this.#core as ClientCore;
    const opts = this.#callOptions(prepared, true);
    const args = prepared.args as Record<string, unknown>;
    let result: Result<unknown>;
    if ("macro" in target) {
      result = await core.runMacro<unknown>(target.macro, args, opts);
    } else {
      if (verifies(prepared.tool)) opts.verify = true;
      result = await core.call<unknown>(target.op, args, opts);
    }
    if (result.ok) return result.verification === undefined ? { ok: true, value: result.value } : { ok: true, value: result.value, verification: result.verification };
    return result.partial === undefined ? { ok: false, error: result.error } : { ok: false, error: result.error, partial: result.partial };
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

  #render(tool: CatalogTool, outcome: Outcome, prepared: Prepared): CallToolResult {
    if (!outcome.ok) return this.#failure(tool, outcome.error, outcome.partial);
    let body = outcome.value;
    const lines: string[] = [];
    const sensitive = tool.sensitiveResponseFields;
    if (sensitive.length > 0 || tool.shownOnce) {
      const id = this.#identity(prepared);
      if (this.#shown.has(id)) {
        const redacted = redactPaths(body, sensitive, REDACTED_REPEAT);
        body = redacted.value;
        if (redacted.found.length > 0) {
          lines.push(`${redacted.found.join(", ")} ${redacted.found.length === 1 ? "was" : "were"} returned by the first identical call in this session and ${redacted.found.length === 1 ? "is" : "are"} redacted here.`);
        }
      } else if (sensitive.length === 0 || holdsAny(body, sensitive)) {
        this.#shown.add(id);
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
      idempotency: t.idempotency,
      schema_tokens: t.schemaTokens,
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
        example: { tool: "invoke", arguments: { name: tool.name, arguments: call } },
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
    if (outcome.status === "done") return this.#success({ value: outcome.value, logs: outcome.logs, calls: outcome.calls }, []);
    const calls = outcome.calls.length > 0 ? ` Completed calls: ${outcome.calls.map((c) => `${c.method} ${c.name} (${c.ok ? "ok" : "error"})`).join(", ")}; check their effects before running it again.` : " No tool call completed.";
    const error = envelope("run_script", "VALIDATION_FAILED", {
      failed_parameter: "code",
      expected: "a script that returns a JSON value within the limits",
      remediation: `${outcome.message}${/[.!?]$/.test(outcome.message) ? "" : "."}${calls}`,
      retryable: "after_remediation",
    });
    const result = this.#failure(null, error);
    if (outcome.logs.length > 0) result.content.push({ type: "text", text: `logs:\n${outcome.logs.join("\n")}` });
    return result;
  }
}
