# SPDX-License-Identifier: Apache-2.0
"""Keyword search over the manifest's precomputed BM25 index and "did you
mean" suggestions for unknown names. Mirrors ``search.ts``: the same
tokenizer as the Rust index builder, the same scoring and the same order."""

from __future__ import annotations

import math
import re
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any

#: Words the shared tokenizer drops.
STOP_WORDS = frozenset(
    [
        "a",
        "an",
        "and",
        "are",
        "as",
        "at",
        "be",
        "by",
        "for",
        "from",
        "in",
        "is",
        "it",
        "of",
        "on",
        "or",
        "the",
        "this",
        "to",
        "with",
    ]
)

_CASE = re.compile(r"([a-z0-9])([A-Z])")
_SPLIT = re.compile(r"[^A-Za-z0-9]+")


def tokenize(text: str) -> list[str]:
    """Split on any non-alphanumeric character and on lower-to-upper case
    boundaries, lowercase, and drop words shorter than 2 characters and stop
    words. No stemming."""
    words = _SPLIT.split(_CASE.sub(r"\1 \2", text))
    return [w.lower() for w in words if len(w) >= 2 and w.lower() not in STOP_WORDS]


@dataclass(frozen=True)
class SearchIndex:
    """The manifest's BM25 index."""

    k1: float
    b: float
    avg_doc_length: float
    doc_lengths: tuple[float, ...]
    postings: Mapping[str, Any]

    @staticmethod
    def read(raw: Any) -> SearchIndex:
        """The index of a manifest; an empty one when it is missing."""
        if not isinstance(raw, Mapping) or not isinstance(raw.get("postings"), Mapping):
            return SearchIndex(1.2, 0.75, 0.0, (), {})

        def number(value: Any, fallback: float) -> float:
            ok = isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)
            return float(value) if ok else fallback

        lengths = raw.get("docLengths")
        return SearchIndex(
            k1=number(raw.get("k1"), 1.2),
            b=number(raw.get("b"), 0.75),
            avg_doc_length=number(raw.get("avgDocLength"), 0.0),
            doc_lengths=tuple(number(n, 0.0) for n in lengths) if isinstance(lengths, list) else (),
            postings=raw["postings"],
        )


def bm25(index: SearchIndex, documents: int, query: str) -> list[tuple[int, float]]:
    """BM25 scores of the documents for ``query`` (Okapi BM25 with the index's
    ``k1`` and ``b``; idf = ln(1 + (N - n + 0.5) / (n + 0.5))). Each distinct
    query term counts once. Hits ``(document, score)`` with a positive score,
    best first; ties keep document order."""
    terms = list(dict.fromkeys(tokenize(query)))
    scores = [0.0] * documents
    avg = index.avg_doc_length
    for term in terms:
        postings = index.postings.get(term)
        if not isinstance(postings, list):
            continue
        valid = [
            (p[0], p[1])
            for p in postings
            if isinstance(p, list)
            and len(p) >= 2
            and isinstance(p[0], int)
            and not isinstance(p[0], bool)
            and 0 <= p[0] < documents
            and isinstance(p[1], (int, float))
            and p[1] > 0
        ]
        n = len(valid)
        if n == 0:
            continue
        idf = math.log(1 + (documents - n + 0.5) / (n + 0.5))
        for doc, tf in valid:
            length = index.doc_lengths[doc] if doc < len(index.doc_lengths) else avg
            norm = 1 - index.b + index.b * length / avg if avg > 0 else 1
            scores[doc] += idf * tf * (index.k1 + 1) / (tf + index.k1 * norm)
    hits = [(i, s) for i, s in enumerate(scores) if s > 0]
    return sorted(hits, key=lambda hit: (-hit[1], hit[0]))


def _distance(a: str, b: str, limit: int) -> int:
    """Levenshtein distance, bounded: ``limit + 1`` once it is exceeded."""
    if abs(len(a) - len(b)) > limit:
        return limit + 1
    previous = list(range(len(b) + 1))
    for i in range(1, len(a) + 1):
        current = [i]
        best = i
        for j in range(1, len(b) + 1):
            cost = 0 if a[i - 1] == b[j - 1] else 1
            value = min(previous[j] + 1, current[j - 1] + 1, previous[j - 1] + cost)
            current.append(value)
            best = min(best, value)
        if best > limit:
            return limit + 1
        previous = current
    return previous[len(b)]


def suggest(wanted: str, names: Sequence[str], ranked: Sequence[str] = (), limit: int = 3) -> list[str]:
    """Up to ``limit`` names close to ``wanted``: near spellings first (edit
    distance within a quarter of the length, at least 2), then names it
    abbreviates or extends, then names sharing words with it (``ranked``, best
    first). Deterministic."""
    probe = wanted.lower()[:200]
    bound = max(2, len(probe) // 4)
    near = sorted(
        ((d, name) for name in names if (d := _distance(probe, name.lower(), bound)) <= bound),
        key=lambda pair: (pair[0], pair[1]),
    )

    def abbreviates(name: str) -> bool:
        if len(probe) < 3 or not name or name[0] != probe[0]:
            return False
        at = 0
        for ch in name:
            if at < len(probe) and ch == probe[at]:
                at += 1
        return at >= len(probe)

    prefixed = sorted(
        name for name in names if abbreviates(name.lower()) or (len(name) >= 3 and probe.startswith(name.lower()))
    )
    out: list[str] = []
    for name in [*(n for _, n in near), *prefixed, *ranked]:
        if name not in out:
            out.append(name)
        if len(out) >= limit:
            break
    return out
