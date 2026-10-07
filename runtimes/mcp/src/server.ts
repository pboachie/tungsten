// SPDX-License-Identifier: Apache-2.0
import { STOP_WORDS, type ServerOptions } from "./types.js";

/** A configured server. `connectStdio` serves one session over stdin/stdout. */
export interface TungstenMcpServer {
  readonly options: ServerOptions;
  connectStdio(): Promise<void>;
  close(): Promise<void>;
}

/** Create the server for a manifest. */
export function createTungstenMcpServer(options: ServerOptions): TungstenMcpServer {
  return {
    options,
    async connectStdio() {
      throw new Error("@tungsten/mcp is not implemented yet");
    },
    async close() {},
  };
}

/** The shared tokenizer (see `STOP_WORDS`). */
export function tokenize(text: string): string[] {
  const words = text
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .split(/[^A-Za-z0-9]+/)
    .map((w) => w.toLowerCase())
    .filter((w) => w.length >= 2 && !STOP_WORDS.includes(w));
  return words;
}
