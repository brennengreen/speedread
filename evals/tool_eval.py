#!/usr/bin/env python3
"""Suite 1 — tool-level regression eval (deterministic; see evals/README.md).

For common code-reading tasks, measures the tokens returned to the model and
the tool calls needed, speedread vs. the tool patterns agents use today — and
grades every output for *information sufficiency*, so savings never come from
silently dropping what the task needs.

Baselines emulate the de-facto standard tools:
  * Read  — `cat -n`-style `%6d→line` numbering, 2,000 lines per call,
            long lines cut at 2,000 chars (Claude Code / Copilot / Gemini CLI
            all page at 2,000 lines).
  * Grep  — ripgrep `path:line:text` content output.
  * ls    — one call per directory explored.

speedread outputs come from the real MCP server over stdio (one session per
repo, so etag diffs work exactly as in an agent session).

Usage: python3 evals/tool_eval.py <bench_dir> [--speedread PATH] [--rg PATH] [--json OUT]
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


SMALL_FILES = [
    ("gin", "go.mod"),
    ("flask", "src/flask/__init__.py"),
    ("ripgrep", "Cargo.toml"),
    ("zod", "package.json"),
    ("Alamofire", "Package.swift"),
]

# Independent (regex) extraction of top-level definitions, to grade outlines
# without trusting speedread's own parser.
TOP_DEFS = {
    ".rs": [r"^(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|fn|mod|type|const|static|union)\s+([A-Za-z_]\w*)",
            r"^impl(?:<[^>]*>)?\s+(?:[\w:]+(?:<[^>]*>)?\s+for\s+)?([A-Za-z_]\w*)"],
    ".py": [r"^(?:async\s+)?(?:class|def)\s+([A-Za-z_]\w*)"],
    ".ts": [r"^(?:export\s+)?(?:default\s+)?(?:declare\s+)?(?:abstract\s+)?(?:class|function|interface|enum|namespace)\s+([A-Za-z_$][\w$]*)",
            r"^(?:export\s+)?type\s+([A-Za-z_$][\w$]*)\s*(?:<[^=]*>)?\s*="],
    ".go": [r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)", r"^type\s+([A-Za-z_]\w*)"],
    ".swift": [r"^(?:@\w+(?:\([^)]*\))?\s+)*(?:(?:public|open|internal|fileprivate|private|final)\s+)*(?:class|struct|enum|protocol|extension|actor|func|typealias)\s+([A-Za-z_]\w*)"],
}


def top_level_names(path):
    pats = TOP_DEFS.get(os.path.splitext(path)[1], [])
    names = set()
    for line in file_lines(path):
        for p in pats:
            m = re.match(p, line)
            if m:
                names.add(m.group(1))
    return names


def mentions(out, name):
    return re.search(r"(?<![\w$])" + re.escape(name) + r"(?![\w$])", out) is not None


def coverage(found, total):
    return found / total if total else 1.0


def rows_to_md(rows):
    out = ["| Scenario | Graded | Baseline calls | Baseline tokens | speedread calls | speedread tokens | Tokens saved |",
           "|---|:---:|---:|---:|---:|---:|---:|"]
    for r in rows:
        saved = 1 - r["sr_tok"] / r["base_tok"] if r["base_tok"] else 0
        g = "✓" if r["sr_pass"] else "✗"
        if r.get("coverage") is not None and r["coverage"] < 1:
            g += f" {r['coverage']:.0%}"
        out.append(f"| {r['name']} | {g} | {r['base_calls']} | {r['base_tok']:,} | {r['sr_calls']} | "
                   f"{r['sr_tok']:,} | {saved:+.0%} |".replace("+", ""))
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

    def add(name, suite, base_outputs, sr_outputs, sr_pass, base_pass=True, cov=None, note=""):
        rows.append({
            "name": name, "suite": suite,
            "base_calls": len(base_outputs), "base_tok": sum(tok(o) for o in base_outputs),
            "sr_calls": len(sr_outputs), "sr_tok": sum(tok(o) for o in sr_outputs),
            "sr_pass": bool(sr_pass), "base_pass": bool(base_pass), "coverage": cov, "note": note,
        })
        r = rows[-1]
        print(f"  {'PASS' if r['sr_pass'] else 'FAIL'} {name:60} base {r['base_calls']:>2} calls {r['base_tok']:>7,} tok | "
              f"speedread {r['sr_calls']} calls {r['sr_tok']:>6,} tok {note}", flush=True)

    servers = {}

    def sr(repo):
        if repo not in servers:
            servers[repo] = Speedread(a.speedread, os.path.join(B, repo))
        return servers[repo]

    print("1. Orient in a repository (ls root + each top-level dir, vs one map) — grader: every top-level entry named")
    for repo in ["flask", "gin", "ripgrep", "zod", "vscode"]:
        root = os.path.join(B, repo)
        entries = sorted(d for d in os.listdir(root) if not d.startswith("."))
        tops = [d for d in entries if os.path.isdir(os.path.join(root, d))]
        base = [base_ls(root)] + [base_ls(os.path.join(root, d)) for d in tops]
        out = sr(repo).call("map")
        found = sum(mentions(out, e) for e in entries)
        add(f"orient: {repo} ({len(tops)} top-level dirs)", "orient", base, [out],
            found == len(entries), cov=coverage(found, len(entries)))

    print("2. Understand a large file (page through it, vs one read) — grader: every top-level definition visible")
    for repo, f in LARGE_FILES:
        path = os.path.join(B, repo, f)
        n = len(file_lines(path))
        out = sr(repo).call("read", targets=[f])
        names = top_level_names(path)
        found = sum(mentions(out, x) for x in names)
        view = re.search(r"\[(skeleton|outline)\]", out.splitlines()[0])
        add(f"large file: {os.path.basename(f)} ({n:,} lines)", "large-file", base_read_all(path), [out],
            coverage(found, len(names)) >= 0.98, cov=coverage(found, len(names)),
            note=f"[{view.group(1) if view else 'full'}; {found}/{len(names)} top-level defs]")

    print("3. Read one function in a known file — grader: the complete function text is present")
    for repo, f, sym, name in FUNCTIONS:
        root = os.path.join(B, repo)
        path = os.path.join(root, f)
        out = sr(repo).call("read", targets=[f"{f}#{sym}"])
        m = re.search(r":(\d+)-(\d+) @", out)
        a0, b0 = int(m.group(1)), int(m.group(2))
        body = [l for l in file_lines(path)[a0 - 1 : b0] if l.strip()]
        complete = all(l in out for l in body) and name in out.splitlines()[0]
        grep = subprocess.run([a.rg, "-n", "-w", "-e", name, path], capture_output=True, text=True).stdout
        add(f"function {sym}: read whole file", "function", base_read_all(path), [out], complete)
        add(f"function {sym}: grep file + exact window (best case)", "function",
            [grep, base_read(path, max(1, a0 - 10), (b0 - a0 + 1) + 20)], [out], complete)

    print("4. Re-read a file after a one-line edit (full re-read, vs path@etag) — grader: the edit is shown")
    for repo, f in LARGE_FILES[:4]:
        root = os.path.join(B, repo)
        path = os.path.join(root, f)
        first = sr(repo).call("read", targets=[f])
        etag = re.search(r"@([0-9a-f]{16})", first).group(1)
        original = open(path, encoding="utf-8").read()
        lines = original.split("\n")
        mid = len(lines) // 2
        indent = lines[mid - 1][: len(lines[mid - 1]) - len(lines[mid - 1].lstrip())]
        marker = f"// edited by agent at line {mid + 1}"
        lines.insert(mid, indent + marker)
        try:
            open(path, "w", encoding="utf-8").write("\n".join(lines))
            out = sr(repo).call("read", targets=[f"{f}@{etag}"])
            add(f"re-read after edit: {os.path.basename(f)}", "reread", base_read_all(path), [out],
                ("+" + indent + marker) in out)
        finally:
            open(path, "w", encoding="utf-8").write(original)

    print("5. Find usages + read the calling code (grep + top-3 files, vs search output=symbols) — grader: shown files complete, totals exact")
    for repo, ident in USAGES:
        root = os.path.join(B, repo)
        grep = base_grep(a.rg, root, ident, word=True)
        hits = {}
        for line in grep.splitlines():
            p, ln, text = line.split(":", 2)
            hits.setdefault(p[2:] if p.startswith("./") else p, []).append((int(ln), text))
        top = sorted(hits, key=lambda p: (-len(hits[p]), p))[:3]
        base = [grep] + [o for p in top for o in base_read_all(os.path.join(root, p))]
        out = sr(repo).call("search", pattern=ident, word=True, output="symbols")
        sections = dict(re.findall(r"^==> (\S+) @\S+ \([^)]*\)\n((?:(?!==> ).*\n?)*)", out, re.M))
        complete = True
        present = 0
        for p, section in sections.items():
            for ln, _ in hits.get(p, []):
                if re.search(rf"^{ln}\t", section, re.M):
                    present += 1
                elif "more in this file" not in section and "more matches in this file" not in section:
                    complete = False
        total = sum(len(v) for v in hits.values())
        m = re.search(r"in ([\d,]+) files? for", out)
        totals_ok = m is not None and int(m.group(1).replace(",", "")) == len(hits)
        add(f"usages of {ident} ({repo}, {len(hits)} files)", "usages", base, [out], complete and totals_ok,
            cov=coverage(present, total), note=f"[{len(sections)}/{len(hits)} files shown in full]")

    print("6. Follow a growing log (tail -n 200, vs log@etag) — grader: all new lines, no old ones")
    tmp = tempfile.mkdtemp()
    log = os.path.join(tmp, "build.log")
    with open(log, "w") as fh:
        for i in range(5000):
            fh.write(f"[{i:05d}] compiling crate_{i % 97} v0.{i % 13}.{i % 7} (step {i})\n")
    s = Speedread(a.speedread, tmp)
    first = s.call("read", targets=["build.log"])
    etag = re.search(r"@([0-9a-f]{16})", first).group(1)
    new = [f"[new {i:02d}] warning: unused variable `x{i}` in src/lib.rs:{100 + i}" for i in range(30)]
    with open(log, "a") as fh:
        fh.write("\n".join(new) + "\n")
    tail = "\n".join(file_lines(log)[-200:])
    out = s.call("read", targets=[f"build.log@{etag}"])
    add("follow log: 30 lines appended to 5,000", "log", [tail], [out],
        all(l in out for l in new) and "[04999]" not in out)
    s.close()
    shutil.rmtree(tmp)

    print("7. Balanced: small files must come back complete and unmodified — grader: every line present")
    for repo, f in SMALL_FILES:
        path = os.path.join(B, repo, f)
        out = sr(repo).call("read", targets=[f])
        lines = file_lines(path)
        ok = all(f"{i}\t{l}" in out for i, l in enumerate(lines, 1) if l.strip()) and "[skeleton]" not in out
        add(f"small file: {repo}/{f} ({len(lines)} lines)", "small-file", base_read_all(path), [out], ok)

    for srv in servers.values():
        srv.close()
    tb = sum(r["base_tok"] for r in rows)
    ts = sum(r["sr_tok"] for r in rows)
    cb = sum(r["base_calls"] for r in rows)
    cs = sum(r["sr_calls"] for r in rows)
    passed = sum(r["sr_pass"] for r in rows)
    print()
    print(rows_to_md(rows))
    print(f"\nGraded: {passed}/{len(rows)} scenarios pass. Total: baseline {cb} calls / {tb:,} tokens; "
          f"speedread {cs} calls / {ts:,} tokens ({1 - ts / tb:.0%} fewer tokens, {1 - cs / cb:.0%} fewer calls). "
          f"Tokens: o200k_base.")
    if a.json:
        json.dump(rows, open(a.json, "w"), indent=1)
    raise SystemExit(0 if passed == len(rows) else 1)


if __name__ == "__main__":
    main()
