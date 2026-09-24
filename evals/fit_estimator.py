#!/usr/bin/env python3
"""Fit speedread's content-aware token estimator (src/tokens.rs).

Tokens are modelled as a linear function of character classes in the text the
model will see: ASCII letters, digits, punctuation, horizontal whitespace,
newlines, non-ASCII characters, letter runs ("words") and digit runs. The
target is the larger of o200k_base and the (legacy) Claude tokenizer count, so
one estimate serves both families; the coefficients are then scaled so 99.5% of
samples are not undercounted. Samples are line-numbered windows of files —
the shape `read` returns — from the same corpus as evals/budget_eval.py.

Usage:
  python3 evals/fit_estimator.py <bench-dir> --claude-counter tok/claude_count.js
"""

import argparse
import random
import shutil
import sys
import tempfile
from pathlib import Path

import numpy as np
import tiktoken

sys.path.insert(0, str(Path(__file__).resolve().parent))
from budget_eval import CORPUS, MAX_BYTES, Claude, pct, synthetic  # noqa: E402

FEATURES = ["letters", "digits", "punct", "ws_runs", "newlines", "non_ascii", "words", "digit_runs",
            "case_breaks", "dense_runs"]


def features(text: str):
    """One pass over UTF-8 bytes; mirrors src/tokens.rs exactly."""
    b = text.encode("utf-8", "replace")
    f = [0] * len(FEATURES)
    prev = 0  # 1 lower, 2 upper, 3 digit, 4 space, 0 other
    run_len = 0  # current [A-Za-z0-9+/=_-] run
    run_alpha = run_digit = False
    for c in b:
        lower, upper, digit = 97 <= c <= 122, 65 <= c <= 90, 48 <= c <= 57
        if lower or upper or digit or c in (43, 47, 61, 95, 45):
            run_len += 1
            run_alpha |= lower or upper
            run_digit |= digit
        else:
            if run_len >= 16 and run_alpha and run_digit:
                f[9] += run_len
            run_len, run_alpha, run_digit = 0, False, False
        if lower or upper:
            f[0] += 1
            if prev not in (1, 2):
                f[6] += 1
            elif upper and prev == 1:
                f[8] += 1
            prev = 1 if lower else 2
        elif digit:
            f[1] += 1
            if prev != 3:
                f[7] += 1
            prev = 3
        elif c == 10:
            f[4] += 1
            prev = 0
        elif c in (32, 9, 13):
            if prev != 4:
                f[3] += 1
            prev = 4
        elif c >= 0xC0:
            f[5] += 1
            prev = 0
        elif c < 0x80:
            f[2] += 1
            prev = 0
        else:
            prev = 0
    if run_len >= 16 and run_alpha and run_digit:
        f[9] += run_len
    return f


def windows(text: str, rng: random.Random, k=3):
    lines = text.split("\n")
    out = []
    for _ in range(k):
        n = rng.randint(40, 220)
        a = rng.randint(0, max(0, len(lines) - n))
        chunk = lines[a:a + n]
        if rng.random() < 0.7:
            out.append("\n".join(f"{a + i + 1}\t{l[:2000]}" for i, l in enumerate(chunk)) + "\n")
        else:
            out.append("\n".join(l[:2000] for l in chunk) + "\n")
    return out


def nnls(X, y):
    active = list(range(X.shape[1]))
    while True:
        coef, *_ = np.linalg.lstsq(X[:, active], y, rcond=None)
        if (coef >= 0).all():
            full = np.zeros(X.shape[1])
            full[active] = coef
            return full
        active = [a for a, c in zip(active, coef) if c >= 0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bench")
    ap.add_argument("--claude-counter", default=None)
    ap.add_argument("--seed", type=int, default=5)
    ap.add_argument("--quantile", type=float, default=99.5)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    bench = Path(args.bench).resolve()
    o200k = tiktoken.get_encoding("o200k_base")
    claude = Claude(args.claude_counter)
    samples, cats = [], []
    for cat, (globs, n) in CORPUS.items():
        files = sorted({p for g in globs for p in bench.glob(g)
                        if p.is_file() and 64 < p.stat().st_size <= MAX_BYTES and "node_modules" not in p.parts})
        rng.shuffle(files)
        for p in files[:n]:
            for w in windows(p.read_text("utf-8", "replace"), rng):
                samples.append(w)
                cats.append(cat)
    tmp = Path(tempfile.mkdtemp(prefix="speedread-fit-"))
    for cat, paths in synthetic(tmp, rng).items():
        for w in windows(paths[0].read_text(), rng, k=12):
            samples.append(w)
            cats.append(cat)
    t_o = np.array([len(o200k.encode(s, disallowed_special=())) for s in samples], float)
    cl = claude.count(samples)
    t_c = np.array([c if c is not None else 0 for c in cl], float)
    y = np.maximum(t_o, t_c)
    X = np.array([features(s) for s in samples], float)
    coef = nnls(X, y)
    pred = X @ coef
    ratio = y / pred
    scale = float(np.percentile(ratio, args.quantile))
    coef_s = coef * scale
    pred_s = X @ coef_s
    r = y / pred_s
    print(f"{len(samples)} samples; fitted (scaled ×{scale:.3f} for p{args.quantile}):")
    for f, c in zip(FEATURES, coef_s):
        print(f"  {f:<10} {c:.4f}")
    old = y / (np.array([len(s.encode()) for s in samples]) / 2.6)
    print(f"\nundercount rate: new {np.mean(r > 1):.2%} (p99 {pct(list(r), 99):.3f}, max {r.max():.2f}) vs "
          f"fixed 2.6 B/tok {np.mean(old > 1):.2%} (p99 {pct(list(old), 99):.3f}, max {old.max():.2f})")
    print(f"mean overestimate (tokens left unused): new {np.mean(1 - np.minimum(r, 1)):.1%} vs fixed {np.mean(1 - np.minimum(old, 1)):.1%}\n")
    print(f"{'category':<34} {'n':>4} {'new p50':>8} {'new p99':>8} {'new max':>8} {'old p99':>8} {'old max':>8}")
    for cat in dict.fromkeys(cats):
        idx = [i for i, c in enumerate(cats) if c == cat]
        rr, oo = [r[i] for i in idx], [old[i] for i in idx]
        print(f"{cat:<34} {len(idx):>4} {pct(rr, 50):>8.2f} {pct(rr, 99):>8.2f} {max(rr):>8.2f} {pct(oo, 99):>8.2f} {max(oo):>8.2f}")
    print("\nRust:\n" + "\n".join(f"const W_{f.upper()}: f32 = {c:.4f};" for f, c in zip(FEATURES, coef_s)))
    shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
