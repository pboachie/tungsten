// SPDX-License-Identifier: Apache-2.0
import type { ClientCore, MacroDescriptor, Safety } from "@elysra/tungsten-runtime";

export type JsonSchema = Record<string, unknown>;

/** Shared catalogue entry with optional legacy MCP transport metadata. */
export interface WebMcpToolEntry {
  name: string;
  kind: "operation" | "macro";
  target: string;
  descriptor?: { kind: "operation" | "macro"; id: string };
  description: string;
  inputSchema: JsonSchema;
  safety: Safety;
  reserved: { idempotencyKey: string | null; confirmationToken: string | null };
  sensitiveResponseFields?: string[];
  shownOnce?: boolean;
  outputSchema?: JsonSchema | null;
  annotations?: {
    title?: string;
    readOnlyHint?: boolean;
    destructiveHint?: boolean;
    idempotentHint?: boolean;
    openWorldHint?: boolean;
  };
  idempotency?: string;
  cluster?: string | null;
  schemaTokens?: number;
}

export interface WebMcpManifest {
  manifestVersion: 1;
  catalogVersion?: 1;
  groups?: { name: string; moduleKey: string; tools: string[] }[];
  tools: WebMcpToolEntry[];
  api?: string;
  apiVersion?: string;
  tungstenVersion?: string;
  mode?: "discrete" | "progressive";
  threshold?: number;
  listBudgetTokens?: number;
  tokenCounter?: string;
  instructions?: string;
  instructionsByMode?: { discrete: string; progressive: string };
  clusters?: object[];
  index?: object;
}

/** Structural types, without changing experimental global DOM interfaces. */
export interface WebMcpTool {
  name: string;
  description: string;
  inputSchema: JsonSchema;
  annotations: {
    readOnlyHint: boolean;
    untrustedContentHint: boolean;
    consequentialHint: boolean;
    debugging: boolean;
  };
  execute(args: unknown, options: { signal: AbortSignal }): Promise<unknown>;
}

/** Current document.modelContext contract: async registration, abort cleanup. */
export interface DocumentModelContext {
  registerTool(tool: WebMcpTool, options: { signal: AbortSignal }): Promise<void>;
}

/** Legacy navigator.modelContext contract: sync registration, named cleanup. */
export interface LegacyModelContext {
  registerTool(tool: WebMcpTool): void;
  unregisterTool(name: string): void;
}

export type WebMcpBackend =
  | { kind: "document"; modelContext: DocumentModelContext }
  | { kind: "navigator"; modelContext: LegacyModelContext };

/** Explicit collection: all limits must be finite positive integers. */
export interface WebMcpStreamCollection {
  maxEvents: number;
  /** UTF-8 bytes of collected event envelopes; also capped by maxResultBytes. */
  maxBytes: number;
  maxDurationMs: number;
}

export interface WebMcpDownloadHandle {
  /** Application-owned HTTPS URL; the application enforces authorization. */
  url: string;
  /** Unix epoch milliseconds, in the future. */
  expiresAt: number;
}

export type WebMcpBinaryResultOptions =
  | { mode: "base64"; maxBytes: number }
  | { mode: "download"; maxBytes: number; render: (input: { bytes: Uint8Array; mediaType: string; operation: string; signal: AbortSignal }) => Promise<WebMcpDownloadHandle> };

export interface RegisterTungstenWebMcpOptions {
  core: ClientCore;
  manifest: WebMcpManifest;
  macros?: Record<string, MacroDescriptor>;
  /** Manifest names. Defaults to read-only operations; macros require opt-in. */
  allowlist?: readonly string[];
  /** Explicitly distinguish the current API from the legacy API. */
  backend?: WebMcpBackend;
  /** Aborting unregisters this registration and cancels its active calls. */
  signal?: AbortSignal;
  /** UTF-8 JSON byte limit, at least 1024; default 1 MiB. No silent clipping.
   * Also bounds input JSON and ordinary upstream HTTP response bodies before
   * parsing; binaryResults.maxBytes overrides the latter for binary calls. */
  maxResultBytes?: number;
  /** Absent: streaming operations reject before sending requests. */
  streamCollection?: WebMcpStreamCollection;
  /** Absent: declared binary outputs reject before sending requests. Secret
   * and shown-once descriptors remain blocked even with this option. */
  binaryResults?: WebMcpBinaryResultOptions;
}

export interface WebMcpRegistration {
  readonly backend: WebMcpBackend["kind"];
  readonly registeredNames: readonly string[];
  /** Idempotent. Removes only owned tools and cancels active executions. */
  dispose(): Promise<void>;
}
