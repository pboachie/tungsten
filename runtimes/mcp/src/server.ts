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
import { CallToolRequestSchema, ListToolsRequestSchema, type Tool as McpTool } from "@modelcontextprotocol/sdk/types.js";

import { buildCatalog, listTools, type Mode } from "./catalog.js";
import { serveHttp } from "./http.js";
import { isRecord } from "./render.js";
import { resolveDeno, SANDBOX_DEFAULTS } from "./sandbox.js";
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
  /** Whether `run_script` is offered (enabled and deno found). */
  readonly sandboxEnabled: boolean;
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
  if (!deno) {
    warnings.push(`sandbox.enabled is true but ${path} was not found: run_script is not offered`);
    return null;
  }
  return {
    deno,
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
    server.setRequestHandler(CallToolRequestSchema, (request) => session.call(request.params.name, request.params.arguments));
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
