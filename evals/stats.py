#!/usr/bin/env python3
"""Statistics for real-agent runs: bootstrap CIs and paired per-task wins.

  python3 evals/stats.py evals/results/coding-claude-sonnet-5 [--baseline baseline]

Prints a Markdown table: for each condition vs the baseline, the ratio of
medians and of means with 95% bootstrap intervals (resampling trials within
each condition), and on how many tasks the condition's per-task mean is lower.
Small runs give wide intervals; this is what separates a direction from a result.
"""

import argparse
import json
import sys
from pathlib import Path

import numpy as np

METRICS = [("input_tokens", "Input tokens"), ("api_ms", "Model time"), ("model_calls", "Model calls"),
           ("cost_aiu", "Cost (AIU)"), ("session_ms", "Session time")]


def table(recs, base, seed=0, n=20000):
    rng = np.random.default_rng(seed)
    conds = [c for c in dict.fromkeys(r["condition"] for r in recs) if c != base]
    order = ["available", "preferred", "exclusive", "speedread", "dropin"]
    conds.sort(key=lambda c: order.index(c) if c in order else 99)
    tasks = sorted({r["task"] for r in recs})
    rows = ["| Condition vs baseline | Metric | Ratio of medians [95% CI] | Ratio of means [95% CI] | Lower on tasks |",
            "|---|---|---:|---:|---:|"]
    for c in conds:
        for key, label in METRICS:
            a = np.array([r[key] for r in recs if r["condition"] == c and r.get(key) is not None], float)
            b = np.array([r[key] for r in recs if r["condition"] == base and r.get(key) is not None], float)
            if not len(a) or not len(b):
                continue
            ra, rb = rng.choice(a, (n, len(a))), rng.choice(b, (n, len(b)))
            med = np.percentile(np.median(ra, 1) / np.median(rb, 1), [2.5, 97.5])
            mea = np.percentile(ra.mean(1) / rb.mean(1), [2.5, 97.5])
            per = lambda cond, t: np.mean([r[key] for r in recs if r["condition"] == cond and r["task"] == t])
            wins = sum(per(c, t) < per(base, t) for t in tasks)
            rows.append(f"| {c} | {label} | {np.median(a) / np.median(b):.2f} [{med[0]:.2f}–{med[1]:.2f}] | "
                        f"{a.mean() / b.mean():.2f} [{mea[0]:.2f}–{mea[1]:.2f}] | {wins}/{len(tasks)} |")
    return "\n".join(rows)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run", type=Path)
    ap.add_argument("--baseline", default="baseline")
    args = ap.parse_args()
    recs = [json.loads(l) for l in (args.run / "trials.jsonl").read_text().splitlines() if l.strip()]
    print(table(recs, args.baseline))
    return 0


if __name__ == "__main__":
    sys.exit(main())
