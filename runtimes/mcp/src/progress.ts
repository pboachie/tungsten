// SPDX-License-Identifier: Apache-2.0
/**
 * Progress of the tool call being served. A `tools/call` that carries a
 * `progressToken` runs inside {@link progressSink}; a streamed tool call
 * (session.ts) reports each event it collects through it, so a client sees
 * progress across the reconnects of a dropped stream.
 */
import { AsyncLocalStorage } from "node:async_hooks";

/** Report that `progress` events have been collected so far. */
export type ProgressSink = (progress: number, message: string) => void;

export const progressSink = new AsyncLocalStorage<ProgressSink>();
