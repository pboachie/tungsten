// SPDX-License-Identifier: Apache-2.0
/**
 * The optional embedding index of `search_tools` (planning/02 D6 "As built:
 * embedding index"). When the server options carry `search.embeddings`, the
 * index the compiler wrote next to `manifest.json` (`index.embeddings.json`
 * and `.bin`) is loaded and a query embedder is built from the same provider
 * kinds the compiler knows; `search_tools` then ranks hybrid: BM25 and cosine
 * similarity fused with reciprocal rank fusion (default) or a weighted sum.
 * Without the option nothing here runs and search is BM25 only. The embedder
 * is called with a timeout; when it fails (or the index cannot be used) the
 * result falls back to BM25 and says why in its `ranking` field.
 */
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import type { Catalog } from "./catalog.js";
import { isRecord } from "./render.js";
import type { Hit } from "./search.js";
import type { EmbeddingsOptions } from "./types.js";

/** Default wall-clock limit of one query embedding. */
export const DEFAULT_EMBED_TIMEOUT_MS = 5000;
/** Default constant of reciprocal rank fusion. */
export const DEFAULT_RRF_K = 60;
/** Documents of each ranking that enter the fusion. */
const FUSION_DEPTH = 100;
const MAX_DIMENSIONS = 8192;
const MAX_OUTPUT_BYTES = 8 * 1024 * 1024;
const CACHE_SIZE = 64;

/** The loaded index: row-major unit vectors, one row per manifest position. */
export interface EmbeddingIndex {
  model: string;
  dimensions: number;
  count: number;
  vectors: Float32Array;
}

export interface Ranking {
  mode: "hybrid" | "bm25";
  fusion?: "rrf" | "weighted";
  model?: string;
  /** Why the result is BM25 only although embeddings are configured. */
  fallback?: string;
}

/** Embeddings configured for this server. */
export interface Semantic {
  /** Why hybrid ranking is not available (index or embedder), if so. */
  readonly unavailable: string | null;
  readonly model: string | null;
  readonly fusion: "rrf" | "weighted";
  /**
   * Rank `bm25` hits (best first) together with the cosine ranking of
   * `query` restricted to the manifest positions `allowed` (all when
   * null). Throws an `Error` whose message says why when the query cannot
   * be embedded.
   */
  rank(query: string, bm25: Hit[], allowed: ReadonlySet<number> | null): Promise<Hit[]>;
}

function finiteOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function positiveOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}

function nonNegativeOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : fallback;
}

function normalize(vector: ArrayLike<number>): Float32Array {
  let sum = 0;
  for (let i = 0; i < vector.length; i++) sum += (vector[i] as number) ** 2;
  const norm = Math.sqrt(sum);
  const out = new Float32Array(vector.length);
  if (norm > 0) for (let i = 0; i < vector.length; i++) out[i] = (vector[i] as number) / norm;
  return out;
}

/** Read `index.embeddings.json` and `.bin`; the reason when unusable. */
export function readEmbeddingIndex(source: string | URL, catalog: Catalog): EmbeddingIndex | string {
  const headerPath = source instanceof URL ? fileURLToPath(source) : source;
  let header: unknown;
  try {
    header = JSON.parse(readFileSync(headerPath, "utf8"));
  } catch {
    return `the embedding index ${headerPath} cannot be read`;
  }
  if (!isRecord(header) || header.format !== 1) return `${headerPath} is not a version 1 embedding index`;
  const { model, dimensions, count, docs } = header;
  if (typeof model !== "string" || !Number.isInteger(dimensions) || !Number.isInteger(count) || !Array.isArray(docs)) {
    return `${headerPath} has an invalid header`;
  }
  const dims = dimensions as number;
  const rows = count as number;
  if (dims < 1 || dims > MAX_DIMENSIONS || rows !== docs.length || rows !== catalog.documents.length) {
    return "the embedding index is for a different set of tools: regenerate the server";
  }
  for (const [i, doc] of docs.entries()) {
    const tool = catalog.documents[i];
    if (!isRecord(doc) || (tool && doc.id !== tool.name)) return "the embedding index is for different tools: regenerate the server";
  }
  const binPath = headerPath.replace(/\.json$/, ".bin");
  let bin: Buffer;
  try {
    bin = readFileSync(binPath);
  } catch {
    return `the embedding vectors ${binPath} cannot be read`;
  }
  if (bin.length !== rows * dims * 4) return `${binPath} has ${bin.length} bytes, expected ${rows * dims * 4}`;
  const vectors = new Float32Array(rows * dims);
  for (let i = 0; i < vectors.length; i++) {
    const x = bin.readFloatLE(i * 4);
    if (!Number.isFinite(x)) return `${binPath} holds a value that is not a finite number`;
    vectors[i] = x;
  }
  return { model, dimensions: dims, count: rows, vectors };
}

/** A command line split into words (spaces separate; quotes group). */
export function commandWords(text: string): string[] | null {
  const words: string[] = [];
  let current = "";
  let quote: string | null = null;
  let started = false;
  for (const c of text) {
    if (quote !== null) {
      if (c === quote) quote = null;
      else current += c;
    } else if (c === '"' || c === "'") {
      quote = c;
      started = true;
    } else if (c === " " || c === "\t") {
      if (started) {
        words.push(current);
        current = "";
        started = false;
      }
    } else {
      current += c;
      started = true;
    }
  }
  if (quote !== null) return null;
  if (started) words.push(current);
  return words.length > 0 ? words : null;
}

type Embedder = (query: string) => Promise<ArrayLike<number>>;

function commandEmbedder(command: string, model: string, dimensions: number | null, timeoutMs: number): Embedder | string {
  const words = commandWords(command);
  if (words === null) return "search.embeddings.command is empty or has an unbalanced quote";
  const [program, ...args] = words as [string, ...string[]];
  return (query) =>
    new Promise((resolve, reject) => {
      let child;
      try {
        child = spawn(program, args, { stdio: ["pipe", "pipe", "ignore"], shell: false });
      } catch (error) {
        reject(new Error(`the embedding command could not be started (${error instanceof Error ? error.message : String(error)})`));
        return;
      }
      const chunks: Buffer[] = [];
      let size = 0;
      let done = false;
      const finish = (error: Error | null, value?: ArrayLike<number>): void => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        child.kill();
        if (error) reject(error);
        else resolve(value as ArrayLike<number>);
      };
      const timer = setTimeout(() => finish(new Error(`the embedding command did not answer within ${timeoutMs} ms`)), timeoutMs);
      child.on("error", (error) => finish(new Error(`the embedding command could not be run (${error.message})`)));
      child.stdout.on("data", (chunk: Buffer) => {
        size += chunk.length;
        if (size > MAX_OUTPUT_BYTES) finish(new Error("the embedding command wrote more than 8 MiB"));
        else chunks.push(chunk);
      });
      child.on("close", (code) => {
        if (done) return;
        if (code !== 0) return finish(new Error(`the embedding command exited with status ${code}`));
        try {
          const body: unknown = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          const row = isRecord(body) && Array.isArray(body.embeddings) ? (body.embeddings[0] as unknown) : undefined;
          if (!Array.isArray(row)) return finish(new Error("the embedding command's response has no `embeddings` array"));
          finish(null, row as number[]);
        } catch {
          finish(new Error("the embedding command's response is not JSON"));
        }
      });
      child.stdin.on("error", () => undefined);
      child.stdin.end(JSON.stringify({ protocol: 1, kind: "query", model, dimensions, inputs: [query] }));
    });
}

function httpEmbedder(url: string, model: string, dimensions: number | null, apiKeyEnv: string | undefined, timeoutMs: number): Embedder {
  return async (query) => {
    const headers: Record<string, string> = { "content-type": "application/json", accept: "application/json" };
    if (apiKeyEnv !== undefined) {
      const key = process.env[apiKeyEnv];
      if (key === undefined || key === "") throw new Error(`the environment variable ${apiKeyEnv} (search.embeddings.apiKeyEnv) is not set`);
      headers.authorization = `Bearer ${key}`;
    }
    const body: Record<string, unknown> = { model, input: [query], encoding_format: "float" };
    if (dimensions !== null) body.dimensions = dimensions;
    let response: Response;
    try {
      response = await fetch(url, { method: "POST", headers, body: JSON.stringify(body), signal: AbortSignal.timeout(timeoutMs) });
    } catch (error) {
      const timedOut = error instanceof Error && (error.name === "TimeoutError" || error.name === "AbortError");
      throw new Error(timedOut ? `the embedding endpoint did not answer within ${timeoutMs} ms` : "the embedding endpoint could not be reached");
    }
    if (!response.ok) throw new Error(`the embedding endpoint answered HTTP ${response.status}`);
    let parsed: unknown;
    try {
      parsed = await response.json();
    } catch {
      throw new Error("the embedding endpoint's response is not JSON");
    }
    const first = isRecord(parsed) && Array.isArray(parsed.data) && isRecord(parsed.data[0]) ? parsed.data[0].embedding : undefined;
    if (!Array.isArray(first)) throw new Error("the embedding endpoint's response has no data[0].embedding");
    return first as number[];
  };
}

/** Reciprocal rank fusion or a weighted sum of two rankings, best first. */
export function fuse(bm25: Hit[], dense: Hit[], fusion: "rrf" | "weighted", weights: { bm25: number; embedding: number }, k: number): Hit[] {
  const scores = new Map<number, number>();
  const add = (index: number, value: number): void => {
    scores.set(index, (scores.get(index) ?? 0) + value);
  };
  if (fusion === "rrf") {
    bm25.slice(0, FUSION_DEPTH).forEach((h, rank) => add(h.index, weights.bm25 / (k + rank + 1)));
    dense.slice(0, FUSION_DEPTH).forEach((h, rank) => add(h.index, weights.embedding / (k + rank + 1)));
  } else {
    const top = bm25[0]?.score ?? 0;
    if (top > 0) for (const h of bm25) add(h.index, (weights.bm25 * h.score) / top);
    for (const h of dense) add(h.index, weights.embedding * h.score);
  }
  return [...scores.entries()].map(([index, score]) => ({ index, score })).sort((x, y) => y.score - x.score || x.index - y.index);
}

/**
 * The semantic search of a server, or null when `search.embeddings` is not
 * configured. Never throws: an unusable index or embedder is a warning and
 * a `Semantic` whose `unavailable` says why.
 */
export function buildSemantic(config: EmbeddingsOptions | undefined, catalog: Catalog, warnings: string[]): Semantic | null {
  if (typeof config !== "object" || config === null) return null;
  const fusion = config.fusion === "weighted" ? "weighted" : "rrf";
  const weights = { bm25: nonNegativeOr(config.weights?.bm25, 1), embedding: nonNegativeOr(config.weights?.embedding, 1) };
  const k = positiveOr(config.rrfK, DEFAULT_RRF_K);
  const timeoutMs = Math.floor(positiveOr(config.timeoutMs, DEFAULT_EMBED_TIMEOUT_MS));
  const off = (why: string): Semantic => {
    warnings.push(`embedding search is off: ${why}`);
    return { unavailable: why, model: null, fusion, rank: async (_q, bm25) => bm25 };
  };
  if (config.index === undefined) return off("search.embeddings.index is not set");
  const index = readEmbeddingIndex(config.index, catalog);
  if (typeof index === "string") return off(index);
  const model = typeof config.model === "string" && config.model !== "" ? config.model : index.model;
  const dimensions = Number.isInteger(config.dimensions) && (config.dimensions as number) > 0 ? (config.dimensions as number) : null;
  if (dimensions !== null && dimensions !== index.dimensions) {
    return off(`search.embeddings.dimensions is ${dimensions} but the index has ${index.dimensions}`);
  }
  let embedder: Embedder | string;
  if (typeof config.embed === "function") {
    const custom = config.embed;
    embedder = async (query) => {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), timeoutMs);
      try {
        return await Promise.race([
          custom(query, controller.signal),
          new Promise<never>((_, reject) => controller.signal.addEventListener("abort", () => reject(new Error(`the query embedder did not answer within ${timeoutMs} ms`)))),
        ]);
      } finally {
        clearTimeout(timer);
      }
    };
  } else if (config.provider === "command" && typeof config.command === "string") {
    embedder = commandEmbedder(config.command, model, dimensions, timeoutMs);
  } else if (config.provider === "http" && typeof config.url === "string") {
    embedder = httpEmbedder(config.url, model, dimensions, typeof config.apiKeyEnv === "string" ? config.apiKeyEnv : undefined, timeoutMs);
  } else {
    embedder = "no query embedder is configured (search.embeddings.provider and command or url, or embed)";
  }
  if (typeof embedder === "string") return off(embedder);
  const embed = embedder;
  const cache = new Map<string, Float32Array>();
  const embedQuery = async (query: string): Promise<Float32Array> => {
    const cached = cache.get(query);
    if (cached) return cached;
    let raw: ArrayLike<number>;
    try {
      raw = await embed(query);
    } catch (error) {
      throw error instanceof Error ? error : new Error(String(error));
    }
    if (raw.length !== index.dimensions) throw new Error(`the query embedding has ${raw.length} values, the index has ${index.dimensions}`);
    for (let i = 0; i < raw.length; i++) if (!Number.isFinite(raw[i])) throw new Error("the query embedding holds a value that is not a finite number");
    const vector = normalize(raw);
    if (cache.size >= CACHE_SIZE) cache.delete(cache.keys().next().value as string);
    cache.set(query, vector);
    return vector;
  };
  return {
    unavailable: null,
    model: index.model,
    fusion,
    async rank(query, bm25, allowed) {
      const q = await embedQuery(query);
      const dense: Hit[] = [];
      const d = index.dimensions;
      for (let row = 0; row < index.count; row++) {
        if (allowed !== null && !allowed.has(row)) continue;
        if (!catalog.documents[row]) continue;
        let dot = 0;
        const base = row * d;
        for (let j = 0; j < d; j++) dot += (index.vectors[base + j] as number) * (q[j] as number);
        if (dot > 0) dense.push({ index: row, score: finiteOr(dot, 0) });
      }
      dense.sort((x, y) => y.score - x.score || x.index - y.index);
      return fuse(bm25, dense, fusion, weights, k);
    },
  };
}
