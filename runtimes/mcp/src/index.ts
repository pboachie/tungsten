// SPDX-License-Identifier: Apache-2.0
/**
 * `@tungsten/mcp`: MCP server runtime for tungsten-generated APIs
 * (planning/05 "MCP server"): discrete and progressive tool disclosure,
 * BM25 search, previews and per-session confirmation, envelope results,
 * sensitive-field redaction, Streamable HTTP and stdio, and the optional
 * Deno sandbox for `run_script`.
 */

export * from "./types.js";
export { createTungstenMcpServer, tokenize } from "./server.js";
export type { TungstenMcpServer } from "./server.js";
export { bm25, suggest, type Hit } from "./search.js";
export { apiHost, denoArguments, resolveDeno, SANDBOX_DEFAULTS } from "./sandbox.js";
export { REDACTED_REPEAT, SHOWN_ONCE_LINE } from "./render.js";
export { loadManifest, parseServeArgs, serve, serveUsage, type ServeArgs } from "./serve.js";
