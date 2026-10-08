// SPDX-License-Identifier: Apache-2.0
/**
 * What a generated server's entry point needs besides the server itself:
 * reading its `manifest.json` and choosing the transport from its command
 * line (stdio by default, Streamable HTTP with `--http <port>`).
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { TungstenMcpServer } from "./server.js";
import type { HttpEndpoint, McpManifest } from "./types.js";

/**
 * Read a manifest file (a path, or a `file:` URL such as
 * `new URL("../manifest.json", import.meta.url)`). Throws an `Error`
 * naming the file when it cannot be read, is not JSON, or is not a
 * version 1 manifest; entries are checked later by
 * `createTungstenMcpServer`.
 */
export function loadManifest(source: string | URL): McpManifest {
  const path = source instanceof URL ? fileURLToPath(source) : source;
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (error) {
    const code = error instanceof Error && "code" in error ? ` (${String((error as { code?: unknown }).code)})` : "";
    throw new Error(`cannot read the MCP manifest ${path}${code}; reinstall the package`);
  }
  let manifest: unknown;
  try {
    manifest = JSON.parse(text);
  } catch {
    throw new Error(`${path} is not JSON; reinstall the package`);
  }
  if (typeof manifest !== "object" || manifest === null || Array.isArray(manifest) || (manifest as { manifestVersion?: unknown }).manifestVersion !== 1) {
    throw new Error(`${path} is not a version 1 tungsten MCP manifest`);
  }
  return manifest as McpManifest;
}

/** The transport chosen on a server's command line. */
export type ServeArgs =
  | { transport: "stdio" }
  | { transport: "http"; port: number; host: string }
  | { transport: "help" };

/** Usage text of a server's command line. */
export function serveUsage(bin: string): string {
  return [
    `usage: ${bin} [--stdio | --http <port> [--host <host>]]`,
    "",
    "  --stdio          serve one MCP session over stdin/stdout (default)",
    "  --http <port>    serve Streamable HTTP at http://<host>:<port>/mcp (0 picks a free port)",
    "  --host <host>    interface to bind with --http (default 127.0.0.1; other hosts",
    "                   accept any Host header: put the server behind your own access control)",
    "  --help           print this text",
    "",
  ].join("\n");
}

/**
 * Parse a server's arguments (`process.argv.slice(2)`): `--stdio`,
 * `--http <port>` or `--http=<port>`, `--host <host>` (only with
 * `--http`), `--help`. Throws an `Error` for anything else.
 */
export function parseServeArgs(argv: readonly string[]): ServeArgs {
  let http: number | null = null;
  let host: string | null = null;
  let stdio = false;
  const value = (flag: string, i: number, inline: string | undefined): [string, number] => {
    if (inline !== undefined) return [inline, i];
    const next = argv[i + 1];
    if (next === undefined || next.startsWith("--")) throw new Error(`${flag} needs a value`);
    return [next, i + 1];
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i] as string;
    const eq = arg.indexOf("=");
    const flag = arg.startsWith("--") && eq > 0 ? arg.slice(0, eq) : arg;
    const inline = arg.startsWith("--") && eq > 0 ? arg.slice(eq + 1) : undefined;
    switch (flag) {
      case "--help":
      case "-h":
        return { transport: "help" };
      case "--stdio":
        stdio = true;
        break;
      case "--http": {
        const [text, next] = value(flag, i, inline);
        i = next;
        const port = /^\d{1,5}$/.test(text) ? Number(text) : NaN;
        if (!(port >= 0 && port <= 65535)) throw new Error(`--http needs a port from 0 to 65535, not ${JSON.stringify(text)}`);
        http = port;
        break;
      }
      case "--host": {
        const [text, next] = value(flag, i, inline);
        i = next;
        if (text === "") throw new Error("--host needs a host name or address");
        host = text;
        break;
      }
      default:
        throw new Error(`unknown argument ${JSON.stringify(arg)} (see --help)`);
    }
  }
  if (http !== null && stdio) throw new Error("--stdio and --http exclude each other");
  if (http === null && host !== null) throw new Error("--host needs --http");
  return http === null ? { transport: "stdio" } : { transport: "http", port: http, host: host ?? "127.0.0.1" };
}

/**
 * Serve `server` as `args` say. Stdio resolves once connected; HTTP
 * resolves with the listening endpoint after writing its URL to stderr,
 * and closes it on SIGINT and SIGTERM. `help` prints the usage to stdout.
 */
export async function serve(server: TungstenMcpServer, args: ServeArgs, bin = "server"): Promise<HttpEndpoint | null> {
  if (args.transport === "help") {
    process.stdout.write(serveUsage(bin));
    return null;
  }
  if (args.transport === "stdio") {
    await server.connectStdio();
    return null;
  }
  const endpoint = await server.connectHttp({ port: args.port, host: args.host });
  process.stderr.write(`${bin}: MCP over Streamable HTTP at ${endpoint.url}\n`);
  const stop = (): void => {
    void server.close().finally(() => process.exit(0));
  };
  process.once("SIGINT", stop);
  process.once("SIGTERM", stop);
  return endpoint;
}
