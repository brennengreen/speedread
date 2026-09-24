#!/usr/bin/env python3
"""Agent-scenario benchmark: tokens returned to the model and tool calls
needed for common code-reading tasks, speedread vs. the tool patterns agents
use today.

Baselines emulate the de-facto standard tools:
  * Read  — `cat -n`-style `%6d→line` numbering, 2,000 lines per call,
            long lines cut at 2,000 chars (Claude Code / Copilot / Gemini CLI
            all page at 2,000 lines).
  * Grep  — ripgrep `path:line:text` content output.
  * ls    — one call per directory explored.

speedread outputs come from the real MCP server over stdio (one session per
repo, so etag diffs work exactly as in an agent session).

Usage: python3 bench/agent_scenarios.py <bench_dir> [--speedread PATH] [--rg PATH]
Requires: pip install tiktoken.
"""

import argparse
import json
import math
import os
import re
import shutil
import subprocess
import tempfile
import warnings

warnings.filterwarnings("ignore")
import tiktoken  # noqa: E402

ENC = tiktoken.get_encoding("o200k_base")


def tok(s: str) -> int:
    return len(ENC.encode(s, disallowed_special=()))


# ---------------------------------------------------------------- baselines


def file_lines(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = f.read().split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def base_read(path, offset=1, limit=2000):
    lines = file_lines(path)
    chunk = lines[offset - 1 : offset - 1 + limit]
    return "\n".join(f"{i:>6}→{l[:2000]}" for i, l in enumerate(chunk, start=offset))


def base_read_all(path):
    """Whole file via 2,000-line pages → list of outputs (one per call)."""
    n = len(file_lines(path))
    return [base_read(path, 1 + 2000 * k) for k in range(max(1, math.ceil(n / 2000)))]


def base_grep(rg, root, pattern, word=False):
    args = [rg, "-n", "--no-heading", "--color=never"]
    if word:
        args.append("-w")
    args += ["--sort=path", "-e", pattern, "."]
    return subprocess.run(args, cwd=root, capture_output=True, text=True).stdout


def base_ls(path):
    return subprocess.run(["ls", "-F", path], capture_output=True, text=True).stdout


# ---------------------------------------------------------------- speedread


class Speedread:
    def __init__(self, binary, root):
        self.p = subprocess.Popen(
            [binary, "--root", root, "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.n = 0
        self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                "clientInfo": {"name": "bench", "version": "1"}})
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")

    def rpc(self, method, params):
        self.n += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            r = json.loads(self.p.stdout.readline())
            if r.get("id") == self.n:
                return r

    def call(self, tool, **args):
        r = self.rpc("tools/call", {"name": tool, "arguments": args})
        return "".join(c.get("text", "") for c in r["result"]["content"])

    def close(self):
        self.p.stdin.close()
        self.p.wait()


# ---------------------------------------------------------------- scenarios

LARGE_FILES = [
    ("ripgrep", "crates/core/flags/defs.rs"),
    ("zod", "packages/zod/src/v3/types.ts"),
    ("vscode", "src/vs/editor/common/model/textModel.ts"),
    ("flask", "src/flask/app.py"),
    ("gin", "context.go"),
    ("Alamofire", "Source/Core/Session.swift"),
]

# (repo, file, symbol, name to grep for)
FUNCTIONS = [
    ("flask", "src/flask/app.py", "Flask.url_for", "url_for"),
    ("gin", "context.go", "Context.AbortWithStatusJSON", "AbortWithStatusJSON"),
    ("ripgrep", "crates/ignore/src/walk.rs", "WalkParallel.run", "run"),
    ("zod", "packages/zod/src/v3/types.ts", "ZodString._parse", "_parse"),
    ("vscode", "src/vs/editor/common/model/textModel.ts", "TextModel.setValue", "setValue"),
]

USAGES = [
    ("flask", "url_for"),
    ("gin", "AbortWithStatus"),
    ("ripgrep", "build_parallel"),
    ("zod", "safeParse"),
]


def rows_to_md(rows):
    out = ["| Scenario | Baseline calls | Baseline tokens | speedread calls | speedread tokens | Tokens saved |",
           "|---|---:|---:|---:|---:|---:|"]
    for r in rows:
        saved = 1 - r["sr_tok"] / r["base_tok"] if r["base_tok"] else 0
        out.append(f"| {r['name']} | {r['base_calls']} | {r['base_tok']:,} | {r['sr_calls']} | {r['sr_tok']:,} | {saved:.0%} |")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bench_dir")
    ap.add_argument("--speedread", default=shutil.which("speedread") or "target/release/speedread")
    ap.add_argument("--rg", default=shutil.which("rg") or "rg")
    ap.add_argument("--json", help="write raw results here")
    a = ap.parse_args()
    B = os.path.abspath(a.bench_dir)
    rows = []

    def add(name, base_outputs, sr_outputs):
        rows.append({
            "name": name,
            "base_calls": len(base_outputs),
            "base_tok": sum(tok(o) for o in base_outputs),
            "sr_calls": len(sr_outputs),
            "sr_tok": sum(tok(o) for o in sr_outputs),
        })
        r = rows[-1]
        print(f"  {name:62} base {r['base_calls']:>2} calls {r['base_tok']:>7,} tok | "
              f"speedread {r['sr_calls']} calls {r['sr_tok']:>6,} tok", flush=True)

    servers = {}

    def sr(repo):
        if repo not in servers:
            servers[repo] = Speedread(a.speedread, os.path.join(B, repo))
        return servers[repo]

    print("1. Orient in a repository (ls root + each top-level dir, vs one map)")
    for repo in ["flask", "gin", "ripgrep", "zod", "vscode"]:
        root = os.path.join(B, repo)
        tops = sorted(d for d in os.listdir(root)
                      if os.path.isdir(os.path.join(root, d)) and not d.startswith("."))
        base = [base_ls(root)] + [base_ls(os.path.join(root, d)) for d in tops]
        add(f"orient: {repo} ({len(tops)} top-level dirs)", base, [sr(repo).call("map")])

    print("2. Understand a large file (page through it, vs one read)")
    for repo, f in LARGE_FILES:
        n = len(file_lines(os.path.join(B, repo, f)))
        add(f"large file: {os.path.basename(f)} ({n:,} lines)",
            base_read_all(os.path.join(B, repo, f)), [sr(repo).call("read", targets=[f])])

    print("3. Read one function in a known file (read file / grep file + window, vs read path#Symbol)")
    for repo, f, sym, name in FUNCTIONS:
        root = os.path.join(B, repo)
        path = os.path.join(root, f)
        out = sr(repo).call("read", targets=[f"{f}#{sym}"])
        m = re.search(r":(\d+)-(\d+) @", out)
        a0, b0 = int(m.group(1)), int(m.group(2))
        grep = subprocess.run([a.rg, "-n", "-w", "-e", name, path], capture_output=True, text=True).stdout
        add(f"function {sym}: read whole file", base_read_all(path), [out])
        add(f"function {sym}: grep file + exact window (best case)",
            [grep, base_read(path, max(1, a0 - 10), (b0 - a0 + 1) + 20)], [out])

    print("4. Re-read a file after a one-line edit (full re-read, vs path@etag)")
    for repo, f in LARGE_FILES[:4]:
        root = os.path.join(B, repo)
        path = os.path.join(root, f)
        first = sr(repo).call("read", targets=[f])
        etag = re.search(r"@([0-9a-f]{8})", first).group(1)
        original = open(path, encoding="utf-8").read()
        lines = original.split("\n")
        mid = len(lines) // 2
        indent = lines[mid - 1][: len(lines[mid - 1]) - len(lines[mid - 1].lstrip())]
        lines.insert(mid, indent + "// edited by agent")
        try:
            open(path, "w", encoding="utf-8").write("\n".join(lines))
            add(f"re-read after edit: {os.path.basename(f)}", base_read_all(path),
                [sr(repo).call("read", targets=[f"{f}@{etag}"])])
        finally:
            open(path, "w", encoding="utf-8").write(original)

    print("5. Find usages + read the calling code (grep + top-3 files, vs search output=symbols)")
    for repo, ident in USAGES:
        root = os.path.join(B, repo)
        grep = base_grep(a.rg, root, ident, word=True)
        counts = {}
        for line in grep.splitlines():
            p = line.split(":", 1)[0]
            counts[p] = counts.get(p, 0) + 1
        top = sorted(counts, key=lambda p: (-counts[p], p))[:3]
        base = [grep] + [o for p in top for o in base_read_all(os.path.join(root, p))]
        add(f"usages of {ident} ({repo}, {len(counts)} files)", base,
            [sr(repo).call("search", pattern=ident, word=True, output="symbols")])

    print("6. Follow a growing log (tail -n 200, vs log@etag)")
    tmp = tempfile.mkdtemp()
    log = os.path.join(tmp, "build.log")
    with open(log, "w") as fh:
        for i in range(5000):
            fh.write(f"[{i:05d}] compiling crate_{i % 97} v0.{i % 13}.{i % 7} (step {i})\n")
    s = Speedread(a.speedread, tmp)
    first = s.call("read", targets=["build.log"])
    etag = re.search(r"@([0-9a-f]{8})", first).group(1)
    with open(log, "a") as fh:
        for i in range(30):
            fh.write(f"[new {i:02d}] warning: unused variable `x{i}` in src/lib.rs:{100 + i}\n")
    tail = "\n".join(file_lines(log)[-200:])
    add("follow log: 30 lines appended to 5,000", [tail], [s.call("read", targets=[f"build.log@{etag}"])])
    s.close()
    shutil.rmtree(tmp)

    for srv in servers.values():
        srv.close()
    tb = sum(r["base_tok"] for r in rows)
    ts = sum(r["sr_tok"] for r in rows)
    cb = sum(r["base_calls"] for r in rows)
    cs = sum(r["sr_calls"] for r in rows)
    print()
    print(rows_to_md(rows))
    print(f"\nTotal: baseline {cb} calls / {tb:,} tokens; speedread {cs} calls / {ts:,} tokens "
          f"({1 - ts / tb:.0%} fewer tokens, {1 - cs / cb:.0%} fewer calls). Tokens: o200k_base.")
    if a.json:
        json.dump(rows, open(a.json, "w"), indent=1)


if __name__ == "__main__":
    main()
