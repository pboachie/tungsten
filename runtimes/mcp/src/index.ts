// SPDX-License-Identifier: Apache-2.0
/**
 * `@tungsten/mcp`: MCP server runtime for tungsten-generated APIs.
 *
 * PHASE-3 STUB: exports the contract and a factory with the final
 * signature. The mcp-runtime work package implements the server.
 */

export * from "./types.js";
export { createTungstenMcpServer, tokenize } from "./server.js";
export type { TungstenMcpServer } from "./server.js";
