// SPDX-License-Identifier: Apache-2.0
/**
 * The manifest read defensively into the tool catalog every session
 * shares, and the `tools/list` answer of each disclosure mode
 * (planning/05 "MCP server", planning/07 "MCP tool surface").
 */
import type { Safety } from "@tungsten/runtime";
import type { Tool as McpTool } from "@modelcontextprotocol/sdk/types.js";

import { REDACTED_REPEAT, isRecord } from "./render.js";
import type { ClusterEntry, JsonSchema, McpManifest, SearchIndex, ServerOptions, ToolAnnotations, ToolEntry } from "./types.js";

export type Mode = "discrete" | "progressive";

/** A manifest tool with every field checked; `summary` is the description
 * without its bracketed tier and key rule. */
export interface CatalogTool extends ToolEntry {
  summary: string;
}

export interface Catalog {
  mode: Mode;
  /** Valid tools in manifest order. */
  tools: CatalogTool[];
  byName: Map<string, CatalogTool>;
  /** Manifest position → tool (null for a skipped entry), for the index. */
  documents: Array<CatalogTool | null>;
  clusters: ClusterEntry[];
  index: SearchIndex;
  instructions: string;
  warnings: string[];
}

const SAFETIES = new Set<string>(["read_only", "mutating", "destructive", "irreversible"]);
/** MCP tool names: 1-128 of `[A-Za-z0-9_.-]`. */
const TOOL_NAME = /^[A-Za-z0-9_.-]{1,128}$/;

export const PROGRESSIVE_TOOLS = ["search_tools", "describe_tool", "invoke", "invoke_read", "preview", "list_clusters", "run_script"] as const;
export const DISCRETE_META_TOOLS = ["preview", "run_script"] as const;

function annotationsFor(safety: Safety): ToolAnnotations {
  return {
    readOnlyHint: safety === "read_only",
    destructiveHint: safety === "destructive" || safety === "irreversible",
    idempotentHint: safety === "read_only",
    openWorldHint: false,
  };
}

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" && value !== "" ? value : null;
}

/** The description without a trailing `[...]` tier and key rule. */
export function summaryOf(description: string): string {
  return description.replace(/\s*\[[^\]]*\]\s*$/, "").trim();
}

function readTool(raw: unknown, position: number, warnings: string[]): CatalogTool | null {
  const skip = (why: string): null => {
    warnings.push(`manifest tool ${position} skipped: ${why}`);
    return null;
  };
  if (!isRecord(raw)) return skip("not an object");
  const name = raw.name;
  if (typeof name !== "string" || !TOOL_NAME.test(name)) return skip("name is not a valid MCP tool name");
  if (raw.kind !== "operation" && raw.kind !== "macro") return skip(`${name}: kind is neither operation nor macro`);
  if (typeof raw.target !== "string" || raw.target === "") return skip(`${name}: no target`);
  if (!isRecord(raw.inputSchema) || raw.inputSchema.type !== "object") return skip(`${name}: inputSchema is not an object schema`);
  const safety: Safety = typeof raw.safety === "string" && SAFETIES.has(raw.safety) ? (raw.safety as Safety) : "irreversible";
  if (safety !== raw.safety) warnings.push(`manifest tool ${name}: unknown safety tier, treated as irreversible`);
  const derived = annotationsFor(safety);
  const a = isRecord(raw.annotations) ? raw.annotations : {};
  const flag = (key: keyof Omit<ToolAnnotations, "title">): boolean => (typeof a[key] === "boolean" ? (a[key] as boolean) : derived[key]);
  const annotations: ToolAnnotations = {
    readOnlyHint: flag("readOnlyHint"),
    destructiveHint: flag("destructiveHint"),
    idempotentHint: flag("idempotentHint"),
    openWorldHint: flag("openWorldHint"),
  };
  if (typeof a.title === "string" && a.title !== "") annotations.title = a.title;
  const reserved = isRecord(raw.reserved) ? raw.reserved : {};
  const description = typeof raw.description === "string" ? raw.description : "";
  return {
    name,
    kind: raw.kind,
    target: raw.target,
    description,
    inputSchema: raw.inputSchema,
    outputSchema: isRecord(raw.outputSchema) ? raw.outputSchema : null,
    annotations,
    safety,
    idempotency: typeof raw.idempotency === "string" ? raw.idempotency : "none",
    reserved: { idempotencyKey: stringOrNull(reserved.idempotencyKey), confirmationToken: stringOrNull(reserved.confirmationToken) },
    cluster: stringOrNull(raw.cluster),
    sensitiveResponseFields: Array.isArray(raw.sensitiveResponseFields)
      ? raw.sensitiveResponseFields.filter((f): f is string => typeof f === "string" && f !== "")
      : [],
    shownOnce: raw.shownOnce === true,
    schemaTokens: typeof raw.schemaTokens === "number" && Number.isFinite(raw.schemaTokens) && raw.schemaTokens >= 0 ? raw.schemaTokens : 0,
    summary: summaryOf(description),
  };
}

function readIndex(raw: unknown, warnings: string[]): SearchIndex {
  if (!isRecord(raw) || !isRecord(raw.postings)) {
    warnings.push("manifest index is missing: search_tools finds nothing");
    return { k1: 1.2, b: 0.75, avgDocLength: 0, docLengths: [], postings: {} };
  }
  return {
    k1: typeof raw.k1 === "number" ? raw.k1 : 1.2,
    b: typeof raw.b === "number" ? raw.b : 0.75,
    avgDocLength: typeof raw.avgDocLength === "number" ? raw.avgDocLength : 0,
    docLengths: Array.isArray(raw.docLengths) ? raw.docLengths.map((n) => (typeof n === "number" ? n : 0)) : [],
    postings: raw.postings as SearchIndex["postings"],
  };
}

/** The instructions of `mode`: its `instructionsByMode` entry, else `instructions`. */
function instructionsFor(manifest: Partial<McpManifest>, mode: Mode): string {
  const byMode: unknown = manifest.instructionsByMode;
  if (isRecord(byMode) && typeof byMode[mode] === "string") return byMode[mode] as string;
  return typeof manifest.instructions === "string" ? manifest.instructions : "";
}

/** Read the manifest; never throws. Invalid entries are skipped with a warning. */
export function buildCatalog(options: ServerOptions): Catalog {
  const warnings: string[] = [];
  const manifest: Partial<McpManifest> = isRecord(options.manifest) ? options.manifest : {};
  if (!isRecord(options.manifest)) warnings.push("manifest is not an object: no tools");
  const rawTools: unknown[] = Array.isArray(manifest.tools) ? manifest.tools : [];
  const threshold = typeof manifest.threshold === "number" ? manifest.threshold : 24;
  const requested = options.mode ?? manifest.mode;
  const mode: Mode = requested === "discrete" || requested === "progressive" ? requested : rawTools.length > threshold ? "progressive" : "discrete";
  const reservedNames = new Set<string>(mode === "discrete" ? DISCRETE_META_TOOLS : []);
  const tools: CatalogTool[] = [];
  const byName = new Map<string, CatalogTool>();
  const documents: Array<CatalogTool | null> = [];
  for (const [position, raw] of rawTools.entries()) {
    let tool = readTool(raw, position, warnings);
    if (tool && byName.has(tool.name)) {
      warnings.push(`manifest tool ${position} skipped: duplicate name ${tool.name}`);
      tool = null;
    }
    if (tool && reservedNames.has(tool.name)) {
      warnings.push(`manifest tool ${tool.name} skipped: the name is a ${mode}-mode meta tool`);
      tool = null;
    }
    documents.push(tool);
    if (tool) {
      tools.push(tool);
      byName.set(tool.name, tool);
    }
  }
  const clusters: ClusterEntry[] = (Array.isArray(manifest.clusters) ? manifest.clusters : [])
    .filter((c): c is ClusterEntry => isRecord(c) && typeof c.name === "string" && c.name !== "")
    .map((c) => ({
      name: c.name,
      summary: typeof c.summary === "string" ? c.summary : null,
      tools: Array.isArray(c.tools) ? c.tools.filter((t): t is string => typeof t === "string") : [],
    }))
    .sort((x, y) => (x.name < y.name ? -1 : x.name > y.name ? 1 : 0));
  return {
    mode,
    tools,
    byName,
    documents,
    clusters,
    index: readIndex(manifest.index, warnings),
    instructions: instructionsFor(manifest, mode),
    warnings,
  };
}

/** Whether `tool` belongs to `cluster` (by its own field or the cluster's list). */
export function inCluster(tool: CatalogTool, cluster: ClusterEntry): boolean {
  return tool.cluster === cluster.name || cluster.tools.includes(tool.name);
}

/** Whether a call of this tool runs its verification hook (planning/06:
 * on by default in the MCP server for irreversible operations). */
export function verifies(tool: CatalogTool): boolean {
  return tool.kind === "operation" && tool.safety === "irreversible";
}

const VERIFICATION_SCHEMA: JsonSchema = {
  type: "object",
  description: "Result of the operation's verification hook.",
  properties: { checked: { type: "boolean" }, passed: { type: "boolean" }, observed: {}, timedOut: { type: "boolean" }, error: { type: "object" } },
};

/** `schema` with the field at a dotted path also allowed to be the redaction marker. */
function allowRedaction(schema: JsonSchema, segments: string[]): JsonSchema {
  if (segments.length === 0) return { anyOf: [schema, { const: REDACTED_REPEAT }] };
  if (schema.type === "array" && isRecord(schema.items)) return { ...schema, items: allowRedaction(schema.items, segments) };
  const properties = schema.properties;
  const key = segments[0] as string;
  if (!isRecord(properties) || !isRecord(properties[key])) return schema;
  return { ...schema, properties: { ...properties, [key]: allowRedaction(properties[key], segments.slice(1)) } };
}

/** What an error result's `structuredContent` (the envelope) satisfies. */
const ENVELOPE_BRANCH: JsonSchema = { properties: { status: { const: "error" } }, required: ["status", "category", "remediation"] };

/**
 * The `outputSchema` advertised for a tool: the manifest's schema when it
 * describes an object (MCP requires `type: "object"`), widened for what
 * the server adds: the verification result of verified calls, the
 * redaction marker of sensitive fields, and the error envelope (clients
 * validate the `structuredContent` of error results too). The body
 * schema's `$defs` stay at the root so its references resolve. Otherwise
 * none (the body is then returned as `{value}`).
 */
export function advertisedOutputSchema(tool: CatalogTool): JsonSchema | null {
  const schema = tool.outputSchema;
  if (!schema || schema.type !== "object") return null;
  const { $schema, $defs, ...rest } = schema;
  let body: JsonSchema = rest;
  if (verifies(tool)) {
    const properties = isRecord(body.properties) ? body.properties : {};
    if (!("verification" in properties)) body = { ...body, properties: { ...properties, verification: VERIFICATION_SCHEMA } };
  }
  for (const path of tool.sensitiveResponseFields) body = allowRedaction(body, path.split(".").filter((s) => s !== ""));
  const out: JsonSchema = {};
  if ($schema !== undefined) out.$schema = $schema;
  out.type = "object";
  if ($defs !== undefined) out.$defs = $defs;
  out.anyOf = [body, ENVELOPE_BRANCH];
  return out;
}

const CLOSED = { additionalProperties: false } as const;
/** The MCP annotations of the tool call a result belongs to, for hosts that
 * gate by tier (`_meta["tungsten/tool"]` of invoke and preview results). */
export const TOOL_META_KEY = "tungsten/tool";

const META_READ = { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false };

function argumentsSchema(what: string): JsonSchema {
  return {
    type: "object",
    description: `${what}: the tool's input object, including its reserved fields (idempotency key, confirmation token) when it has them.`,
  };
}

function runScriptTool(): McpTool {
  return {
    name: "run_script",
    description:
      "Run TypeScript in an isolated sandbox (no files or environment; the API is reached only through `client`). The code is the body of an async function receiving `client`: `await client.invoke(name, args)`, `await client.preview(name, args)` or `await client.<tool_name>(args)`, each returning {ok, value} or {ok: false, error}. Return a JSON value.",
    inputSchema: { type: "object", properties: { code: { type: "string", minLength: 1, maxLength: 100000 } }, required: ["code"], ...CLOSED },
    annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
  };
}

function byName(a: { name: string }, b: { name: string }): number {
  return a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
}

/** The `tools/list` answer: identical for every session and deterministic. */
export function listTools(catalog: Catalog, sandbox: boolean): McpTool[] {
  if (catalog.mode === "discrete") {
    const listed: McpTool[] = [...catalog.tools].sort(byName).map((tool) => {
      const entry: McpTool = {
        name: tool.name,
        description: tool.description,
        inputSchema: tool.inputSchema as McpTool["inputSchema"],
        annotations: { ...tool.annotations },
      };
      const output = advertisedOutputSchema(tool);
      if (output) entry.outputSchema = output as NonNullable<McpTool["outputSchema"]>;
      return entry;
    });
    const previewable = catalog.tools.filter((t) => t.safety !== "read_only").map((t) => t.name).sort();
    if (previewable.length > 0) {
      listed.push({
        name: "preview",
        description:
          "Preview a non-read-only tool call without executing it: the rendered request, its effects and a confirmation_token bound to these exact arguments (required by destructive and irreversible tools).",
        inputSchema: {
          type: "object",
          properties: { tool: { type: "string", enum: previewable }, arguments: argumentsSchema("Arguments of the call to preview") },
          required: ["tool"],
          ...CLOSED,
        },
        annotations: { ...META_READ, idempotentHint: false },
      });
    }
    if (sandbox) listed.push(runScriptTool());
    return listed;
  }
  const clusterNames = catalog.clusters.map((c) => c.name);
  const cluster: JsonSchema = { type: "string", description: "Restrict the search to one cluster (see list_clusters)." };
  if (clusterNames.length > 0) cluster.enum = clusterNames;
  const listed: McpTool[] = [
    {
      name: "search_tools",
      description: `Search the ${catalog.tools.length} API tools by keywords. Returns name, summary, safety tier, MCP annotations, idempotency policy, schema token cost and the tool to call it with.`,
      inputSchema: {
        type: "object",
        properties: {
          query: { type: "string", minLength: 1, description: "Words describing the task, e.g. \"replay a failed webhook\"." },
          cluster,
          limit: { type: "integer", minimum: 1, maximum: 50, default: 10 },
        },
        required: ["query"],
        ...CLOSED,
      },
      annotations: META_READ,
    },
    {
      name: "describe_tool",
      description: "Full input and output schema of one tool, its safety tier and annotations, reserved fields, remediation table and an example call.",
      inputSchema: { type: "object", properties: { name: { type: "string", minLength: 1 } }, required: ["name"], ...CLOSED },
      annotations: META_READ,
    },
    {
      name: "invoke",
      description:
        "Execute any tool. Returns the response body, or the error envelope with remediation. Destructive and irreversible tools need the confirmation_token from preview with the same arguments. Read-only tools: use invoke_read.",
      inputSchema: {
        type: "object",
        properties: { name: { type: "string", minLength: 1 }, arguments: argumentsSchema("Arguments of the tool") },
        required: ["name"],
        ...CLOSED,
      },
      annotations: { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
    },
    ...(catalog.tools.some((t) => t.safety === "read_only")
      ? [
          {
            name: "invoke_read",
            description: "Execute one read-only tool (safety tier read_only); any other tier is refused, use invoke. Safe to allow without confirmation.",
            inputSchema: {
              type: "object" as const,
              properties: { name: { type: "string", minLength: 1 }, arguments: argumentsSchema("Arguments of the tool") },
              required: ["name"],
              ...CLOSED,
            },
            annotations: META_READ,
          } satisfies McpTool,
        ]
      : []),
    {
      name: "preview",
      description:
        "Preview a tool call without executing it: the rendered request, its effects and a confirmation_token bound to these exact arguments.",
      inputSchema: {
        type: "object",
        properties: { name: { type: "string", minLength: 1 }, arguments: argumentsSchema("Arguments of the call to preview") },
        required: ["name"],
        ...CLOSED,
      },
      annotations: { ...META_READ, idempotentHint: false },
    },
    {
      name: "list_clusters",
      description: "The tool clusters with their summaries and tool counts.",
      inputSchema: { type: "object", properties: {}, ...CLOSED },
      annotations: META_READ,
    },
  ];
  if (sandbox) listed.push(runScriptTool());
  return listed;
}
