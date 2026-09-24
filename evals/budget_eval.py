#!/usr/bin/env python3
"""Adversarial check of speedread's token-budget estimator (Suite 3 in evals/README.md).

speedread sizes every response with a fixed bytes-per-token estimate (2.6 by
default) instead of the client's real tokenizer. This measures how far real
tokenizers deviate from that estimate, per content type — including hostile
content (CJK, emoji, minified JS, base64, hex, numbers, lockfiles) — and then
checks the end-to-end contract: tokens actually returned by `read` at a given
`budget`, across budgets and files.

  estimate ratio  = real tokens / (bytes / 2.6)      (> 1: estimator undercounts)
  budget ratio    = real tokens of a read / budget  (> 1: response over budget)

Tokenizers: o200k_base and cl100k_base (tiktoken, exact for OpenAI models) and
the legacy Claude tokenizer (@anthropic-ai/tokenizer; a proxy — Anthropic has
not published the tokenizer of current models; the count_tokens API is exact).

Usage:
  python3 evals/budget_eval.py <bench-dir> --speedread target/release/speedread
          [--claude-counter tok/claude_count.js] [--json out.json]
"""

import argparse
import base64
import json
import math
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import tiktoken

BPT = 2.6
BUDGETS = (1000, 4000, 8000)
MAX_BYTES = 2_000_000

# category -> (repo-relative globs, files to sample)
CORPUS = {
    "Rust": (["ripgrep/crates/**/*.rs"], 60),
    "Go": (["gin/**/*.go"], 60),
    "Python": (["flask/src/**/*.py", "flask/tests/**/*.py"], 60),
    "TypeScript": (["zod/packages/zod/src/**/*.ts", "vscode/src/vs/editor/**/*.ts"], 60),
    "JavaScript": (["vscode/build/**/*.js", "vscode/extensions/**/*.js"], 40),
    "Swift": (["Alamofire/Source/**/*.swift", "swift-argument-parser/Sources/**/*.swift"], 60),
    "Markdown": (["*/docs/**/*.md", "*/*.md", "vscode/extensions/**/*.md"], 50),
    "JSON": (["vscode/extensions/**/*.json"], 50),
    "YAML": (["**/*.yml", "**/*.yaml"], 40),
    "TOML": (["**/*.toml"], 30),
    "Lockfiles": (["ripgrep/Cargo.lock", "ripgrep/fuzz/Cargo.lock", "vscode/**/package-lock.json",
                   "zod/pnpm-lock.yaml"], 12),
    "Generated (protobuf, d.ts)": (["gin/testdata/**/*.pb.go", "vscode/src/vscode-dts/*.d.ts"], 30),
    "Minified JS": (["Alamofire/docs/js/*.min.js"], 4),
    "SVG": (["vscode/**/*.svg"], 40),
    "CJK (real: docs, locales, i18n)": (["zod/packages/docs-v3/README_ZH.md", "zod/packages/zod/src/v4/locales/ja.ts",
                                          "zod/packages/zod/src/v4/locales/zh-CN.ts", "zod/packages/zod/src/v4/locales/ko.ts",
                                          "zod/packages/zod/src/v4/locales/zh-TW.ts", "vscode/build/win32/i18n/*.isl"], 12),
}


def synthetic(dirpath: Path, rng: random.Random):
    """Hostile content that real repos contain in places: emoji-heavy text,
    base64 blobs, hex dumps, numeric tables, Unicode math/symbols."""
    out = {}
    emoji = "😀🚀🔥✨🎉🐛✅❌⚠️📦🧪🔒💡🌍🍎🦀🐍🧵🧠"
    lines = []
    for i in range(1500):
        words = [rng.choice(["fix", "add", "bump", "ship", "test", "docs"]) for _ in range(4)]
        lines.append(f"- {rng.choice(emoji)}{rng.choice(emoji)} {' '.join(words)} #{i} {rng.choice(emoji)}")
    out["emoji.md"] = "\n".join(lines) + "\n"
    blob = base64.b64encode(rng.randbytes(90_000)).decode()
    out["base64.txt"] = "\n".join(blob[i:i + 76] for i in range(0, len(blob), 76)) + "\n"
    hexlines = []
    for i in range(3000):
        b = rng.randbytes(16)
        hexlines.append(f"{i * 16:08x}  " + " ".join(f"{x:02x}" for x in b))
    out["hexdump.txt"] = "\n".join(hexlines) + "\n"
    rows = ["id,lat,lon,value,ts"]
    for i in range(4000):
        rows.append(f"{i},{rng.uniform(-90, 90):.6f},{rng.uniform(-180, 180):.6f},{rng.random() * 1e6:.3f},{1700000000 + i * 37}")
    out["numbers.csv"] = "\n".join(rows) + "\n"
    sym = "∀∃∈∉⊂⊆∪∩∧∨¬→↔≤≥≠≈∞∑∏√∫∂∇αβγδεζηθλμπσφψω"
    out["unicode_math.txt"] = "\n".join(
        " ".join("".join(rng.choice(sym) for _ in range(rng.randint(2, 6))) for _ in range(10)) for _ in range(1500)) + "\n"
    for name, text in out.items():
        (dirpath / name).write_text(text)
    return {"Synthetic: " + n.split(".")[0]: [dirpath / n] for n in out}


def pct(xs, p):
    xs = sorted(xs)
    if not xs:
        return float("nan")
    k = max(0, min(len(xs) - 1, math.ceil(p / 100 * len(xs)) - 1))
    return xs[k]


class Claude:
    def __init__(self, counter):
        self.counter = counter

    def count(self, texts):
        if not self.counter:
            return [None] * len(texts)
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as fh:
            json.dump(texts, fh)
            name = fh.name
        try:
            out = subprocess.run(["node", self.counter, name], capture_output=True, text=True, check=True,
                                 cwd=Path(self.counter).parent).stdout
            return json.loads(out)
        finally:
            os.unlink(name)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bench")
    ap.add_argument("--speedread", default="speedread")
    ap.add_argument("--claude-counter", default=None, help="node script: counts tokens with @anthropic-ai/tokenizer")
    ap.add_argument("--json", default=None)
    ap.add_argument("--seed", type=int, default=11)
    args = ap.parse_args()
    bench = Path(args.bench).resolve()
    rng = random.Random(args.seed)
    enc = {"o200k": tiktoken.get_encoding("o200k_base"), "cl100k": tiktoken.get_encoding("cl100k_base")}
    claude = Claude(args.claude_counter)

    groups = {}
    for cat, (globs, n) in CORPUS.items():
        files = sorted({p for g in globs for p in bench.glob(g)
                        if p.is_file() and 64 < p.stat().st_size <= MAX_BYTES and "node_modules" not in p.parts})
        rng.shuffle(files)
        groups[cat] = files[:n]
    tmp = Path(tempfile.mkdtemp(prefix="speedread-budget-"))
    groups.update(synthetic(tmp, rng))

    def toks(text):
        return {k: len(e.encode(text, disallowed_special=())) for k, e in enc.items()}

    report = {"bytes_per_token_estimate": BPT, "budgets": BUDGETS, "categories": {}}
    for cat, files in groups.items():
        texts, sizes = [], []
        for p in files:
            data = p.read_bytes()
            texts.append(data.decode("utf-8", "replace"))
            sizes.append(len(data))
        if not texts:
            continue
        cl = claude.count(texts)
        est_rows = []
        for t, b, c in zip(texts, sizes, cl):
            est = b / BPT
            r = toks(t)
            row = {"bytes": b, **{f"ratio_{k}": v / est for k, v in r.items()},
                   "bpt_o200k": b / max(1, r["o200k"])}
            if c is not None:
                row["ratio_claude"] = c / est
            est_rows.append(row)
        # End to end: the largest files (so budgets bind), each at every budget.
        big = sorted(zip(files, sizes), key=lambda x: -x[1])[:12]
        e2e_rows, outs = [], []
        for p, _ in big:
            root = tmp if tmp in p.parents else bench
            rel = str(p.relative_to(root))
            for bud in BUDGETS:
                res = subprocess.run([args.speedread, "--root", str(root), "--budget", str(bud), "read", rel],
                                     capture_output=True, text=True)
                outs.append(res.stdout)
                e2e_rows.append({"file": rel, "budget": bud, "bytes": len(res.stdout.encode()), **toks(res.stdout)})
        for row, c in zip(e2e_rows, claude.count(outs)):
            if c is not None:
                row["claude"] = c
        tok_keys = ["o200k", "cl100k"] + (["claude"] if args.claude_counter else [])
        summ = {"files": len(texts), "bytes": sum(sizes),
                "median_bpt_o200k": pct([r["bpt_o200k"] for r in est_rows], 50)}
        for k in tok_keys:
            rs = [r[f"ratio_{k}"] for r in est_rows if f"ratio_{k}" in r]
            summ[f"est_{k}"] = {"p50": pct(rs, 50), "p90": pct(rs, 90), "p99": pct(rs, 99), "max": max(rs),
                                "over": sum(x > 1 for x in rs) / len(rs)}
            br = [r[k] / r["budget"] for r in e2e_rows if k in r]
            if br:
                summ[f"budget_{k}"] = {"p50": pct(br, 50), "p99": pct(br, 99), "max": max(br),
                                       "over": sum(x > 1 for x in br) / len(br), "n": len(br)}
        report["categories"][cat] = {"summary": summ, "estimate": est_rows, "reads": e2e_rows}
        s = summ
        print(f"{cat:<34} n={s['files']:>3}  bytes/tok(o200k) {s['median_bpt_o200k']:.2f}  "
              f"est-ratio o200k p50 {s['est_o200k']['p50']:.2f} p99 {s['est_o200k']['p99']:.2f}  "
              f"reads over budget (o200k) {s.get('budget_o200k', {}).get('over', 0):.0%} "
              f"max {s.get('budget_o200k', {}).get('max', 0):.2f}"
              + (f"  | claude p99 {s['est_claude']['p99']:.2f} reads-over {s.get('budget_claude', {}).get('over', 0):.0%}"
                 if 'est_claude' in s else ""), flush=True)
    # Pooled: all real-repo reads (synthetic excluded) and everything.
    for label, pred in (("all real files", lambda c: not c.startswith("Synthetic")), ("everything", lambda c: True)):
        pooled = {}
        for k in ("o200k", "cl100k", "claude"):
            br = [r[k] / r["budget"] for c, v in report["categories"].items() if pred(c) for r in v["reads"] if k in r]
            if br:
                pooled[k] = {"p50": pct(br, 50), "p99": pct(br, 99), "max": max(br),
                             "over": sum(x > 1 for x in br) / len(br), "n": len(br)}
        report[f"pooled_reads ({label})"] = pooled
        print(f"pooled reads ({label}): " + "; ".join(
            f"{k}: p50 {v['p50']:.2f} p99 {v['p99']:.2f} max {v['max']:.2f} over {v['over']:.1%} (n={v['n']})"
            for k, v in pooled.items()))
    if args.json:
        Path(args.json).write_text(json.dumps(report, indent=1))
    shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()
