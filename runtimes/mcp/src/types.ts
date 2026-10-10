// SPDX-License-Identifier: Apache-2.0
/**
 * The contract between the tungsten MCP emitter (Rust, `tungsten-emit-mcp`)
 * and this runtime (planning/05 "MCP server", planning/07 "MCP tool
 * surface", planning/01 NFR-3).
 *
 * The emitter writes an `McpManifest` as JSON (`manifest.json` in the
 * generated server package). The generated server imports it together with
 * the generated TypeScript SDK's descriptors and calls
 * `createTungstenMcpServer`. All behaviour lives here.
 *
 * PHASE-3 CONTRACT: additive changes only, with both sides updated.
 */

import type { Safety } from "@tungsten/runtime";

/** JSON Schema (draft 2020-12) as plain data. */
export type JsonSchema = Record<string, unknown>;

export interface ToolAnnotations {
  title?: string;
  readOnlyHint: boolean;
  destructiveHint: boolean;
  idempotentHint: boolean;
  openWorldHint: boolean;
}

export interface ToolEntry {
  /** `<namespace>_<resource path>_<method>` snake_case, <= 48 chars, unique
   * and stable across regenerations (planning/07). */
  name: string;
  kind: "operation" | "macro";
  /** IR operation id or macro name; the runtime resolves it against the
   * SDK descriptors (`OperationDescriptor.id`) or macros. */
  target: string;
  /** Compact description within the manifest's description budget:
   * summary plus a bracketed tier and key rule
   * (e.g. "[mutating; caller-owned Idempotency-Key UUIDv4]"). */
  description: string;
  /** Closed JSON Schema of the tool input: the SDK args object (same layout
   * as tools.json), plus the reserved fields named in `reserved`. */
  inputSchema: JsonSchema;
  /** JSON Schema of the success body, when known. */
  outputSchema: JsonSchema | null;
  annotations: ToolAnnotations;
  safety: Safety;
  /** IR idempotency policy kind (`none`, `auto`, `caller_owned`, ...). */
  idempotency: string;
  /** Names of the reserved input fields that are not SDK args. `null`
   * when the tool does not take that field. */
  reserved: { idempotencyKey: string | null; confirmationToken: string | null };
  cluster: string | null;
  /** Response fields to return once and redact on repeats (planning/05). */
  sensitiveResponseFields: string[];
  shownOnce: boolean;
  /** Token cost of `description` + `inputSchema` under the manifest's
   * counter (what `search_tools` reports as `schema_tokens`). */
  schemaTokens: number;
}

export interface ClusterEntry {
  name: string;
  summary: string | null;
  tools: string[];
}

/** Precomputed BM25 index over tool name words, summary, parameter names,
 * cluster and tags (planning/02 D6). Terms are lowercase ASCII words from
 * the same tokenizer the runtime applies to queries (`tokenize` below). */
export interface SearchIndex {
  k1: number;
  b: number;
  avgDocLength: number;
  /** One entry per tool, in `tools` order. */
  docLengths: number[];
  /** term -> postings [toolIndex, termFrequency], sorted by toolIndex. */
  postings: Record<string, Array<[number, number]>>;
}

export interface McpManifest {
  manifestVersion: 1;
  api: string;
  apiVersion: string;
  tungstenVersion: string;
  /** Selected mode: the agent.yml `disclosure.mode`; for `auto`, `discrete`
   * when the discrete tool list is within `listBudgetTokens`, otherwise
   * `progressive`. */
  mode: "discrete" | "progressive";
  /** Tool count; the fallback when a manifest has no `mode`. */
  threshold: number;
  /** Tokens of the discrete tool list above which `auto` selects progressive. */
  listBudgetTokens?: number;
  /** How token counts in this manifest were computed. */
  tokenCounter: string;
  tools: ToolEntry[];
  clusters: ClusterEntry[];
  index: SearchIndex;
  /** Instructions sent in the MCP `initialize` result (short), for `mode`. */
  instructions: string;
  /** The instructions of each mode; the runtime sends the entry of the mode
   * in effect (an overridden mode included), else `instructions`.
   * Additive: optional for manifests written before it. */
  instructionsByMode?: { discrete: string; progressive: string };
}

/** Query/document tokenizer shared with the Rust index builder: split on
 * any non-alphanumeric character and on lower->upper case boundaries,
 * lowercase, drop words shorter than 2 characters and the stop words in
 * `STOP_WORDS`. No stemming. The Rust side must produce identical terms. */
export const STOP_WORDS: readonly string[] = [
  "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it",
  "of", "on", "or", "the", "this", "to", "with",
];

export interface SandboxOptions {
  /** Enable `run_script`. Default false. */
  enabled: boolean;
  /** The isolate that runs scripts: `"wasm"` (QuickJS compiled to
   * WebAssembly, in this process, nothing to install), `"deno"` (a Deno
   * subprocess; needs the deno executable), `"off"` (no `run_script`) or
   * `"auto"`: deno when the executable is found, else wasm. Default
   * `"auto"`. */
  engine?: "auto" | "wasm" | "deno" | "off";
  /** Path to the deno executable. Default "deno". Used by the deno engine. */
  denoPath?: string;
  /** Wall-clock limit of one script. Default 30000. */
  timeoutMs?: number;
  /** Memory limit of one script (the V8 heap for deno, the isolate's heap
   * for wasm). Default 128. */
  memoryMb?: number;
  /** Tool calls one script may make. Default 50. */
  maxCalls?: number;
}

export interface ServerOptions {
  manifest: McpManifest;
  /** Factory for one `ClientCore` per MCP session (confirmation tokens and
   * shown-once state are per session). */
  createCore: () => import("@tungsten/runtime").ClientCore;
  /** Generated macro descriptors, by name. */
  macros?: Record<string, import("@tungsten/runtime").MacroDescriptor>;
  /** Server name and version reported in `initialize`. */
  name: string;
  version: string;
  /** Override the manifest's mode. */
  mode?: "discrete" | "progressive";
  sandbox?: SandboxOptions;
  /** Cap, in characters, on the JSON of one tool result; larger results
   * are cut (arrays to their first items, then long strings) with a note.
   * Default 50000. */
  maxResultChars?: number;
  /** Cap, in bytes of JSON, on the events one streamed tool call collects;
   * past it the call fails with an `UNEXPECTED_RESPONSE` envelope and the
   * events so far as `partial`. Default 4194304 (4 MiB). */
  maxStreamBytes?: number;
  /** Cap, in milliseconds, on the wall-clock time one streamed tool call
   * may spend collecting events (same failure). Default 60000. A streamed
   * call also stops after 1000 events (`truncated: true`). */
  maxStreamMs?: number;
}

/** Options of `TungstenMcpServer.connectHttp` (Streamable HTTP). */
export interface HttpOptions {
  /** Interface to bind. Default "127.0.0.1". */
  host?: string;
  /** Port to bind; 0 picks a free one. Default 0. */
  port?: number;
  /** Endpoint path. Default "/mcp". */
  path?: string;
  /** Host header values accepted besides the loopback names (DNS
   * rebinding protection). Entries are `host` or `host:port`. */
  allowedHosts?: string[];
  /** Origin header values accepted besides loopback origins. */
  allowedOrigins?: string[];
  /** Concurrent sessions; further initialize requests get 503. Default 64. */
  maxSessions?: number;
  /** Largest request body in bytes. Default 4 MiB. */
  maxBodyBytes?: number;
  /** A session with no open request or stream for this long is closed
   * (clients that vanish without DELETE). 0 disables. Default 1800000. */
  idleTimeoutMs?: number;
}

/** A listening Streamable HTTP endpoint. */
export interface HttpEndpoint {
  /** `http://<host>:<port><path>`. */
  url: string;
  host: string;
  port: number;
  /** Stop listening and close every session of this endpoint. */
  close(): Promise<void>;
}
