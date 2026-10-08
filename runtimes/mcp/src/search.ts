// SPDX-License-Identifier: Apache-2.0
/**
 * Keyword search over the manifest's precomputed BM25 index (planning/02
 * D6) and "did you mean" suggestions for unknown names.
 */
import { STOP_WORDS, type SearchIndex } from "./types.js";

/** The shared tokenizer (see `STOP_WORDS`). */
export function tokenize(text: string): string[] {
  const words = text
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .split(/[^A-Za-z0-9]+/)
    .map((w) => w.toLowerCase())
    .filter((w) => w.length >= 2 && !STOP_WORDS.includes(w));
  return words;
}

export interface Hit {
  index: number;
  score: number;
}

/**
 * BM25 scores of the documents of `index` for `query` (Okapi BM25 with the
 * index's `k1` and `b`; idf = ln(1 + (N - n + 0.5) / (n + 0.5))). Each
 * distinct query term counts once. Hits with a positive score, best first;
 * ties keep document order.
 */
export function bm25(index: SearchIndex, documents: number, query: string): Hit[] {
  const terms = [...new Set(tokenize(query))];
  const scores = new Float64Array(documents);
  const k1 = finiteOr(index.k1, 1.2);
  const b = finiteOr(index.b, 0.75);
  const avg = finiteOr(index.avgDocLength, 0);
  for (const term of terms) {
    const postings = Object.prototype.hasOwnProperty.call(index.postings, term) ? index.postings[term] : undefined;
    if (!Array.isArray(postings)) continue;
    const valid = postings.filter(
      (p): p is [number, number] =>
        Array.isArray(p) && Number.isInteger(p[0]) && p[0] >= 0 && p[0] < documents && typeof p[1] === "number" && p[1] > 0,
    );
    const n = valid.length;
    if (n === 0) continue;
    const idf = Math.log(1 + (documents - n + 0.5) / (n + 0.5));
    for (const [doc, tf] of valid) {
      const length = finiteOr(index.docLengths[doc], avg);
      const norm = avg > 0 ? 1 - b + (b * length) / avg : 1;
      scores[doc] = (scores[doc] ?? 0) + (idf * tf * (k1 + 1)) / (tf + k1 * norm);
    }
  }
  const hits: Hit[] = [];
  for (let i = 0; i < documents; i++) {
    const score = scores[i] ?? 0;
    if (score > 0) hits.push({ index: i, score });
  }
  return hits.sort((x, y) => y.score - x.score || x.index - y.index);
}

function finiteOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

/** Levenshtein distance, bounded: returns `limit + 1` once it is exceeded. */
function distance(a: string, b: string, limit: number): number {
  if (Math.abs(a.length - b.length) > limit) return limit + 1;
  let previous = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const current = [i];
    let best = i;
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      const value = Math.min((previous[j] ?? 0) + 1, (current[j - 1] ?? 0) + 1, (previous[j - 1] ?? 0) + cost);
      current.push(value);
      if (value < best) best = value;
    }
    if (best > limit) return limit + 1;
    previous = current;
  }
  return previous[b.length] ?? limit + 1;
}

/**
 * Up to `max` names close to `wanted`: near spellings first (edit distance
 * within a quarter of the length, at least 2), then names it abbreviates
 * or extends, then names sharing words with it (`ranked`, best first).
 * Deterministic.
 */
export function suggest(wanted: string, names: readonly string[], ranked: readonly string[] = [], max = 3): string[] {
  const probe = wanted.toLowerCase().slice(0, 200);
  const limit = Math.max(2, Math.floor(probe.length / 4));
  const near = names
    .map((name) => ({ name, d: distance(probe, name.toLowerCase(), limit) }))
    .filter((c) => c.d <= limit)
    .sort((x, y) => x.d - y.d || (x.name < y.name ? -1 : x.name > y.name ? 1 : 0))
    .map((c) => c.name);
  // Abbreviations (`args` for `arguments`: same first letter, letters in
  // order) and extensions (`public_webhooks_list_all` for `public_webhooks_list`).
  const abbreviates = (name: string): boolean => {
    if (probe.length < 3 || name[0] !== probe[0]) return false;
    let at = 0;
    for (const ch of name) if (ch === probe[at]) at += 1;
    return at >= probe.length;
  };
  const prefixed = names.filter((name) => abbreviates(name.toLowerCase()) || (name.length >= 3 && probe.startsWith(name.toLowerCase()))).sort();
  const out: string[] = [];
  for (const name of [...near, ...prefixed, ...ranked]) {
    if (!out.includes(name)) out.push(name);
    if (out.length >= max) break;
  }
  return out;
}
