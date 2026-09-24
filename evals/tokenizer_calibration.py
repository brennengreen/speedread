#!/usr/bin/env python3
"""Calibrate token estimates against a production tokenizer (Suite 2b in evals/README.md).

Anthropic has not published the tokenizer of current Claude models (Anthropic
states Claude 4.7+ tokenizers produce ~30% more tokens than earlier models), so
offline counts use the legacy tokenizer as a proxy. This script recovers exact
counts from real sessions instead: the harness logs input and output tokens
of every model call, so the tokens added by the tool results returned between
call k and call k+1 are

    input[k+1] - input[k] - output[k] (+ reasoning[k], which is not re-sent)

minus a small per-result framing overhead, fitted jointly. The texts come from
the saved transcripts. Each gap is compared with the content-aware estimate
(src/tokens.rs, ported in fit_estimator.py), o200k_base, the legacy Claude
tokenizer, and a fixed 2.6 bytes/token.

Usage:
  python3 evals/tokenizer_calibration.py evals/results/agent-claude-sonnet-5 [more run dirs]
          [--claude-counter tok/claude_count.js] [--json out.json]
"""

import argparse
import json
import os
import sqlite3
import sys
from pathlib import Path

import numpy as np
import tiktoken

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from budget_eval import Claude, pct  # noqa: E402
from fit_estimator import FEATURES, features  # noqa: E402

DB = Path.home() / ".copilot" / "session-store.db"
# Only tools whose recorded result is exactly what the model saw (bash and
# edit results can be re-rendered or extended by the harness).
READ_TOOLS = {"view", "grep", "glob", "rg", "speedread-read", "speedread-search", "speedread-map", "speedread-trace"}
# src/tokens.rs coefficients, in FEATURES order (letters has weight 0).
W = {"letters": 0.0, "digits": 0.2309, "punct": 0.5142, "ws_runs": 0.1415, "newlines": 3.1389, "non_ascii": 1.8830,
     "words": 1.1830, "digit_runs": 1.3637, "case_breaks": 1.3540, "dense_runs": 0.1716}


def estimate(text):
    return sum(W[f] * v for f, v in zip(FEATURES, features(text)))


def gaps(run_dir, con):
    """(texts of the tool results in the gap, exact tokens added) per gap."""
    recs = [json.loads(l) for l in (run_dir / "trials.jsonl").read_text().splitlines() if l.strip()]
    out = []
    for r in recs:
        sid = r.get("session_id")
        if not sid:
            continue
        name = f"{r['task']}__{r['condition']}__{r['trial']}.jsonl"
        tpath = run_dir / "transcripts" / name
        if not tpath.exists():
            continue
        calls = con.execute("SELECT input_tokens, output_tokens, reasoning_tokens FROM assistant_usage_events "
                            "WHERE session_id = ? AND parent_tool_call_id IS NULL ORDER BY rowid", (sid,)).fetchall()
        # Tool results grouped by the model call that follows them.
        groups, cur, k, names = [], [], 0, {}
        for line in tpath.read_text().splitlines():
            try:
                e = json.loads(line)
            except Exception:
                continue
            t, d = e.get("type"), e.get("data") or {}
            if t == "model.call_start":
                if k > 0:
                    groups.append(cur)
                cur, k = [], k + 1
            elif t == "tool.execution_start":
                names[d.get("toolCallId")] = d.get("toolName")
            elif t == "tool.execution_complete":
                res = d.get("result") or {}
                content = res.get("content") if isinstance(res, dict) else res
                text = content if isinstance(content, str) else json.dumps(content)
                cur.append((names.get(d.get("toolCallId"), "?"), text))
        if k != len(calls):
            continue  # transcript and usage log disagree (retries); skip the session
        for i, items in enumerate(groups):
            if not items or any(name not in READ_TOOLS for name, _ in items):
                continue
            (a_in, a_out, a_reason), (b_in, _, _) = calls[i], calls[i + 1]
            added = b_in - a_in - a_out + (a_reason or 0)
            if added > 0:
                out.append({"texts": [t for _, t in items], "added": added, "task": r["task"],
                            "condition": r["condition"], "tools": [n for n, _ in items]})
    return out


def theil_sen(x, y, min_dx=50):
    """Robust line fit (median of pairwise slopes): outlier gaps (harness
    injections) barely move it."""
    order = np.argsort(x)
    x, y = x[order], y[order]
    slopes = [(y[j] - y[i]) / (x[j] - x[i]) for i in range(len(x)) for j in range(i + 1, len(x)) if x[j] - x[i] > min_dx]
    m = float(np.median(slopes)) if slopes else float("nan")
    return m, float(np.median(y - m * x))


def robust(G, label):
    single = [g for g in G if len(g["texts"]) == 1]
    if len(single) < 5:
        return None
    x = np.array([estimate(g["texts"][0]) for g in single])
    y = np.array([g["added"] for g in single], float)
    m, b = theil_sen(x, y)
    big = x >= 300
    r = (y[big] - b) / x[big]
    out = {"n": len(single), "n_big": int(big.sum()), "slope": m, "overhead": b,
           "p50": float(np.median(r)) if len(r) else None, "p90": float(np.percentile(r, 90)) if len(r) else None}
    if len(r):
        print(f"  {label:<34} n={len(single):>3} (>=300 est. tokens: {int(big.sum()):>2})  slope {m:.3f}  "
              f"overhead {b:5.1f}  real/est p50 {out['p50']:.3f}  p90 {out['p90']:.3f}")
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("runs", nargs="+", type=Path)
    ap.add_argument("--claude-counter", default=None)
    ap.add_argument("--json", default=None)
    args = ap.parse_args()
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    enc = tiktoken.get_encoding("o200k_base")
    claude = Claude(args.claude_counter)
    G = [g for run in args.runs for g in gaps(run, con)]
    joined = ["\n".join(g["texts"]) for g in G]
    legacy = claude.count(joined)
    rows = []
    for g, text, lc in zip(G, joined, legacy):
        rows.append({"n": len(g["texts"]), "added": g["added"], "bytes": len(text.encode()),
                     "est": estimate(text), "o200k": len(enc.encode(text, disallowed_special=())),
                     "legacy": lc, "fixed": len(text.encode()) / 2.6, "condition": g["condition"]})
    # added ≈ overhead·n_results + scale·content; fit both on o200k-free ground truth.
    X = np.array([[r["n"], r["est"]] for r in rows], float)
    y = np.array([r["added"] for r in rows], float)
    (overhead, scale), *_ = np.linalg.lstsq(X, y, rcond=None)
    content = y - overhead * X[:, 0]
    big = [i for i, r in enumerate(rows) if content[i] >= 150]
    print(f"{len(rows)} read-tool result gaps from {len(args.runs)} run(s); fitted framing overhead "
          f"{overhead:.1f} tokens/result; real/estimate scale {scale:.3f}")
    print(f"gaps with >= 150 content tokens: {len(big)}\n")
    report = {"gaps": len(rows), "overhead_per_result": overhead, "scale": scale, "ratios": {}, "robust": {}}
    print("robust fit (Theil-Sen, single-result gaps):")
    sr = [g for g in G if all(t.startswith("speedread-") for t in g["tools"])]
    native = [g for g in G if not any(t.startswith("speedread-") for t in g["tools"])]
    for label, sub_g in (("speedread output", sr), ("built-in tools (view/grep/glob)", native), ("all", G)):
        res = robust(sub_g, label)
        if res:
            report["robust"][label] = res
    print()
    for key in ("est", "fixed", "o200k", "legacy"):
        if any(rows[i][key] is None for i in big):
            continue
        rr = [content[i] / rows[i][key] for i in big]
        report["ratios"][key] = {"p50": pct(rr, 50), "p90": pct(rr, 90), "p99": pct(rr, 99), "max": max(rr),
                                 "under": sum(x > 1 for x in rr) / len(rr)}
        print(f"real / {key:<7} p50 {pct(rr, 50):.3f}  p90 {pct(rr, 90):.3f}  p99 {pct(rr, 99):.3f}  "
              f"max {max(rr):.3f}  estimate too low in {sum(x > 1 for x in rr) / len(rr):.0%}")
    bpt = [rows[i]["bytes"] / content[i] for i in big]
    print(f"\nreal bytes/token (claude-sonnet-5): p10 {pct(bpt, 10):.2f}  p50 {pct(bpt, 50):.2f}  p90 {pct(bpt, 90):.2f}")
    report["real_bytes_per_token"] = {"p10": pct(bpt, 10), "p50": pct(bpt, 50), "p90": pct(bpt, 90)}
    if args.json:
        Path(args.json).write_text(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
