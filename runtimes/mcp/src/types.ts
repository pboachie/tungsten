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
  /** Selected mode: `discrete` when the tool count is within the
   * threshold, otherwise `progressive` (agent.yml disclosure.mode/threshold). */
  mode: "discrete" | "progressive";
  threshold: number;
  /** How token counts in this manifest were computed. */
  tokenCounter: string;
  tools: ToolEntry[];
  clusters: ClusterEntry[];
  index: SearchIndex;
  /** Instructions sent in the MCP `initialize` result (short). */
  instructions: string;
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
  /** Enable `run_script`. Default false. Requires `deno` on PATH. */
  enabled: boolean;
  /** Path to the deno executable. Default "deno". */
  denoPath?: string;
  timeoutMs?: number;
  memoryMb?: number;
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
}
