// SPDX-License-Identifier: Apache-2.0
/**
 * Streamable HTTP transport (planning/05 "MCP server"): one MCP session,
 * with its own `ClientCore`, per `Mcp-Session-Id`. Loopback by default,
 * with Host and Origin checks against DNS rebinding.
 */
import { randomUUID } from "node:crypto";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";

import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { isInitializeRequest } from "@modelcontextprotocol/sdk/types.js";

import type { HttpEndpoint, HttpOptions } from "./types.js";

/** What the HTTP endpoint needs from the server: a new connected session. */
export interface SessionFactory {
  open(transport: StreamableHTTPServerTransport): Promise<{ close(): Promise<void> }>;
}

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]", "::1"]);
const DEFAULT_MAX_BODY = 4 * 1024 * 1024;

function hostname(value: string): string | null {
  try {
    return new URL(`http://${value}`).hostname.toLowerCase();
  } catch {
    return null;
  }
}

function sendError(res: ServerResponse, status: number, code: number, message: string, headers: Record<string, string> = {}): void {
  if (res.headersSent) {
    res.end();
    return;
  }
  res.writeHead(status, { "content-type": "application/json", ...headers });
  res.end(JSON.stringify({ jsonrpc: "2.0", error: { code, message }, id: null }));
}

async function readBody(req: IncomingMessage, limit: number): Promise<{ ok: true; text: string } | { ok: false; status: number; message: string }> {
  const declared = Number(req.headers["content-length"]);
  if (Number.isFinite(declared) && declared > limit) return { ok: false, status: 413, message: `Request body over ${limit} bytes` };
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const buffer = chunk as Buffer;
    size += buffer.length;
    if (size > limit) return { ok: false, status: 413, message: `Request body over ${limit} bytes` };
    chunks.push(buffer);
  }
  return { ok: true, text: Buffer.concat(chunks).toString("utf8") };
}

function initializes(body: unknown): boolean {
  return Array.isArray(body) ? body.some((m) => isInitializeRequest(m)) : isInitializeRequest(body);
}

/** Listen for Streamable HTTP; resolves once listening. */
export async function serveHttp(options: HttpOptions, factory: SessionFactory): Promise<HttpEndpoint> {
  const host = typeof options.host === "string" && options.host !== "" ? options.host : "127.0.0.1";
  const port = typeof options.port === "number" && Number.isInteger(options.port) && options.port >= 0 ? options.port : 0;
  const path = typeof options.path === "string" && options.path.startsWith("/") ? options.path : "/mcp";
  const maxSessions = typeof options.maxSessions === "number" && options.maxSessions >= 1 ? Math.floor(options.maxSessions) : 64;
  const maxBody = typeof options.maxBodyBytes === "number" && options.maxBodyBytes >= 1 ? Math.floor(options.maxBodyBytes) : DEFAULT_MAX_BODY;
  const allowedHosts = new Set((options.allowedHosts ?? []).map((h) => h.toLowerCase()));
  const allowedOrigins = new Set(options.allowedOrigins ?? []);
  const boundLoopback = LOOPBACK.has(host.toLowerCase());
  const sessions = new Map<string, { transport: StreamableHTTPServerTransport; session: { close(): Promise<void> } }>();
  const pending = new Set<{ close(): Promise<void> }>();

  const hostAllowed = (value: string | undefined): boolean => {
    if (!boundLoopback && allowedHosts.size === 0) return true;
    if (typeof value !== "string") return false;
    const name = hostname(value);
    return (name !== null && LOOPBACK.has(name)) || allowedHosts.has(value.toLowerCase()) || (name !== null && allowedHosts.has(name));
  };
  const originAllowed = (value: string | undefined): boolean => {
    if (typeof value !== "string") return true;
    if (allowedOrigins.has(value)) return true;
    try {
      return LOOPBACK.has(new URL(value).hostname.toLowerCase());
    } catch {
      return false;
    }
  };

  const handle = async (req: IncomingMessage, res: ServerResponse): Promise<void> => {
    const url = new URL(req.url ?? "/", "http://localhost");
    if (url.pathname !== path) return sendError(res, 404, -32000, "Not found");
    if (!hostAllowed(req.headers.host)) return sendError(res, 403, -32000, "Host not allowed");
    if (!originAllowed(req.headers.origin)) return sendError(res, 403, -32000, "Origin not allowed");
    const header = req.headers["mcp-session-id"];
    const sessionId = typeof header === "string" ? header : undefined;
    if (req.method === "POST") {
      const read = await readBody(req, maxBody);
      if (!read.ok) return sendError(res, read.status, -32000, read.message);
      let body: unknown;
      try {
        body = JSON.parse(read.text);
      } catch {
        return sendError(res, 400, -32700, "Parse error: the body is not JSON");
      }
      if (sessionId !== undefined) {
        const entry = sessions.get(sessionId);
        if (!entry) return sendError(res, 404, -32001, "Session not found");
        return entry.transport.handleRequest(req, res, body);
      }
      if (!initializes(body)) return sendError(res, 400, -32000, "Bad Request: no valid Mcp-Session-Id; start with initialize");
      if (sessions.size + pending.size >= maxSessions) return sendError(res, 503, -32000, "Too many sessions", { "retry-after": "5" });
      let opened: { close(): Promise<void> } | null = null;
      const transport = new StreamableHTTPServerTransport({
        sessionIdGenerator: () => randomUUID(),
        onsessioninitialized: (id) => {
          if (opened) sessions.set(id, { transport, session: opened });
        },
      });
      transport.onclose = () => {
        const id = transport.sessionId;
        if (id !== undefined) sessions.delete(id);
      };
      opened = await factory.open(transport);
      pending.add(opened);
      try {
        await transport.handleRequest(req, res, body);
      } finally {
        pending.delete(opened);
        if (transport.sessionId === undefined || !sessions.has(transport.sessionId)) await opened.close();
      }
      return;
    }
    if (req.method === "GET" || req.method === "DELETE") {
      if (sessionId === undefined) return sendError(res, 400, -32000, "Bad Request: Mcp-Session-Id required");
      const entry = sessions.get(sessionId);
      if (!entry) return sendError(res, 404, -32001, "Session not found");
      await entry.transport.handleRequest(req, res);
      if (req.method === "DELETE") {
        sessions.delete(sessionId);
        await entry.session.close();
      }
      return;
    }
    return sendError(res, 405, -32000, "Method not allowed", { allow: "GET, POST, DELETE" });
  };

  const server = createServer((req, res) => {
    handle(req, res).catch(() => sendError(res, 500, -32603, "Internal error"));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => {
      server.off("error", reject);
      resolve();
    });
  });
  const address = server.address();
  const actualPort = typeof address === "object" && address !== null ? address.port : port;
  const shown = host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
  let closed = false;
  return {
    url: `http://${shown}:${actualPort}${path}`,
    host,
    port: actualPort,
    async close() {
      if (closed) return;
      closed = true;
      const open = [...sessions.values()];
      sessions.clear();
      await Promise.all(open.map((entry) => entry.session.close().catch(() => undefined)));
      await Promise.all([...pending].map((s) => s.close().catch(() => undefined)));
      server.closeAllConnections();
      await new Promise<void>((resolve) => server.close(() => resolve()));
    },
  };
}
