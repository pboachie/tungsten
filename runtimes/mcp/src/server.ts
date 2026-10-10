// SPDX-License-Identifier: Apache-2.0
/**
 * `createTungstenMcpServer`: serves an `McpManifest` over MCP
 * (planning/05 "MCP server"). Each MCP session (a stdio connection, an
 * HTTP `Mcp-Session-Id`, or any transport passed to `connect`) gets its
 * own `ClientCore` from `options.createCore`. Only tools are advertised
 * (`listChanged: false`); protocol versions are negotiated by the SDK.
 */
import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import type { Transport } from "@modelcontextprotocol/sdk/shared/transport.js";
import { ErrorCode, ListToolsRequestSchema, McpError, type Tool as McpTool } from "@modelcontextprotocol/sdk/types.js";

import { buildCatalog, listTools, type Mode } from "./catalog.js";
import { serveHttp } from "./http.js";
import { isRecord } from "./render.js";
import { resolveDeno, SANDBOX_DEFAULTS, selectEngine } from "./sandbox.js";
import { Session, type SandboxConfig } from "./session.js";
import type { HttpEndpoint, HttpOptions, ServerOptions } from "./types.js";

export { tokenize } from "./search.js";

/** A configured server. `connectStdio` serves one session over stdin/stdout. */
export interface TungstenMcpServer {
  readonly options: ServerOptions;
  /** The disclosure mode in effect (the option, else the manifest's). */
  readonly mode: Mode;
  /** Manifest entries that were skipped or adjusted, and sandbox notes. */
  readonly warnings: readonly string[];
  /** Whether `run_script` is offered (enabled, and an isolate is available). */
  readonly sandboxEnabled: boolean;
  /** The isolate `run_script` uses, or null when it is not offered. */
  readonly sandboxEngine: "deno" | "wasm" | null;
  /** The `tools/list` answer, identical for every session. */
  tools(): McpTool[];
  /** Serve one session over `transport` (in-memory, custom). */
  connect(transport: Transport): Promise<void>;
  connectStdio(): Promise<void>;
  /** Serve Streamable HTTP: one session per `Mcp-Session-Id`. */
  connectHttp(options?: HttpOptions): Promise<HttpEndpoint>;
  /** Close every session and HTTP endpoint. */
  close(): Promise<void>;
}

function positive(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}

function sandboxConfig(options: ServerOptions, warnings: string[]): SandboxConfig | null {
  const sandbox = isRecord(options.sandbox) ? options.sandbox : null;
  if (!sandbox || sandbox.enabled !== true) return null;
  const path = typeof sandbox.denoPath === "string" ? sandbox.denoPath : "deno";
  const deno = resolveDeno(path);
  const engine = selectEngine(sandbox.engine, deno !== null);
  if (engine === "off") return null;
  if (engine === "missing") {
    warnings.push(`sandbox.enabled is true but ${path} was not found: run_script is not offered`);
    return null;
  }
  return {
    engine,
    deno: deno ?? "",
    timeoutMs: positive(sandbox.timeoutMs, SANDBOX_DEFAULTS.timeoutMs),
    memoryMb: positive(sandbox.memoryMb, SANDBOX_DEFAULTS.memoryMb),
    maxCalls: Math.floor(positive(sandbox.maxCalls, SANDBOX_DEFAULTS.maxCalls)),
  };
}

/** Create the server for a manifest. Never throws: invalid manifest
 * entries are skipped and listed in `warnings`. */
export function createTungstenMcpServer(options: ServerOptions): TungstenMcpServer {
  const opts: ServerOptions = isRecord(options) ? options : ({} as ServerOptions);
  const catalog = buildCatalog(opts);
  const warnings = [...catalog.warnings];
  const sandbox = sandboxConfig(opts, warnings);
  const listed = listTools(catalog, sandbox !== null);
  const name = typeof opts.name === "string" && opts.name !== "" ? opts.name : "tungsten-mcp";
  const version = typeof opts.version === "string" && opts.version !== "" ? opts.version : "0.0.0";
  const open = new Set<Server>();
  const endpoints = new Set<HttpEndpoint>();

  const start = async (transport: Transport): Promise<{ close(): Promise<void> }> => {
    const session = new Session(catalog, opts, sandbox);
    const server = new Server(
      { name, version },
      catalog.instructions === "" ? { capabilities: { tools: { listChanged: false } } } : { capabilities: { tools: { listChanged: false } }, instructions: catalog.instructions },
    );
    server.setRequestHandler(ListToolsRequestSchema, () => ({ tools: structuredClone(listed) }));
    // tools/call is answered by the fallback handler, not setRequestHandler:
    // the SDK validates a registered tools/call handler's params itself and
    // answers a protocol error for arguments sent as JSON text or a
    // non-string name, where this server answers an envelope (planning/05).
    server.fallbackRequestHandler = async (request) => {
      if (request.method !== "tools/call") throw new McpError(ErrorCode.MethodNotFound, "Method not found");
      const params = isRecord(request.params) ? request.params : {};
      return session.call(params.name, params.arguments);
    };
    server.onclose = () => {
      open.delete(server);
    };
    open.add(server);
    await server.connect(transport);
    return {
      async close() {
        open.delete(server);
        await server.close();
      },
    };
  };

  return {
    options,
    mode: catalog.mode,
    warnings,
    sandboxEnabled: sandbox !== null,
    sandboxEngine: sandbox?.engine ?? null,
    tools: () => structuredClone(listed),
    async connect(transport) {
      await start(transport);
    },
    async connectStdio() {
      await start(new StdioServerTransport());
    },
    async connectHttp(httpOptions = {}) {
      const endpoint = await serveHttp(httpOptions, { open: (transport) => start(transport as Transport) });
      endpoints.add(endpoint);
      return endpoint;
    },
    async close() {
      const pendingEndpoints = [...endpoints];
      endpoints.clear();
      await Promise.all(pendingEndpoints.map((e) => e.close()));
      const servers = [...open];
      open.clear();
      await Promise.all(servers.map((s) => s.close().catch(() => undefined)));
    },
  };
}
