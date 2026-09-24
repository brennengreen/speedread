#!/usr/bin/env python3
"""Real-agent coding evaluation of speedread (Suite 4 in evals/README.md).

SWE-style tasks (evals/coding_tasks.json): a regression is injected into a real
repository, the agent receives a symptom-only bug report and must find and fix
it. Outcome grader: the repository's own full test suite passes and no test
file was modified. Same harness (GitHub Copilot CLI, non-interactive), same
model, four conditions:

  baseline   built-in tools only (view / grep / glob / bash / edit)
  available  + speedread MCP server, no guidance (unprompted adoption)
  preferred  + speedread, and one sentence asking to prefer it for reading
  exclusive  + speedread, built-in view/grep/glob removed (edit and bash stay)

Sub-agents (`task`) are disabled in every condition so all tokens are
accounted for in one session. Each trial runs in a fresh copy of the repo
(injected bug, single-commit git history, no remote). Besides pass rate,
tokens, cost, model time and turns, transcripts are analysed for: adoption and
share of reads done with speedread, repeated source bytes (lines shown to the
model more than once), whether the decisive (buggy) line was ever shown, and
whether it was first hidden inside a collapsed skeleton (false-negative risk
of compression), plus the read-before-edit overhead Claude Code would add.

Usage:
  python3 evals/coding_eval.py --bench <repos> --venvs <venvs> [--trials 2]
          [--conditions baseline,available,preferred,exclusive] [--workers 3]
  python3 evals/coding_eval.py --bench <repos> --venvs <venvs> --verify
"""

import argparse
import concurrent.futures as cf
import json
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from agent_eval import COMMON, session_usage, tok  # noqa: E402

CONDITIONS = ("baseline", "available", "preferred", "exclusive")
PREFER = ("\n\nUse the speedread MCP tools (read, search, trace, map) to read, search and navigate code; "
          "use the built-in tools only to edit files and run commands.")
SR_TOOLS = {"speedread-read", "speedread-search", "speedread-map", "speedread-trace"}
NATIVE_READ = {"view", "grep", "glob", "rg"}
EDIT_TOOLS = {"edit", "create", "str_replace_editor", "write", "apply_patch", "str_replace"}
BASH_READ = re.compile(r"(^|[;&|(]\s*|\s)(cat|head|tail|sed\s+-n|less|more|grep|rg|ag|find|ls|tree|nl|awk|wc)\b")
BASH_RUN = re.compile(r"\bgo\s+(test|build|vet|run)\b|pytest|\bpython3?\s+-c\b|\.venv/bin/python\s+-c\b")
NO_SUBAGENTS = ["task", "web_fetch", "web_search"]
LINE_PREFIX = re.compile(r"^\s*\d+[\t.:|→]\s?")
GIT_ENV = {"GIT_AUTHOR_NAME": "eval", "GIT_AUTHOR_EMAIL": "eval@example.invalid",
           "GIT_COMMITTER_NAME": "eval", "GIT_COMMITTER_EMAIL": "eval@example.invalid"}


def condition_flags(cond, mcp_config):
    sr = ["--additional-mcp-config", f"@{mcp_config}", "--allow-all-mcp-server-instructions"]
    # `--excluded-tools` is variadic: keep it last.
    if cond == "baseline":
        return ["--excluded-tools", *NO_SUBAGENTS]
    if cond in ("available", "preferred"):
        return sr + ["--excluded-tools", *NO_SUBAGENTS]
    if cond == "exclusive":
        return sr + ["--excluded-tools", *NO_SUBAGENTS, "view", "grep", "glob", "rg"]
    raise ValueError(cond)


# ---------------------------------------------------------------- workspaces

def make_workspace(task, bench, venvs, root, name, fixed=False):
    ws = root / name
    shutil.copytree(bench / task["repo"], ws, ignore=shutil.ignore_patterns(".git"), symlinks=True)
    for inj in task["inject"]:
        p = ws / inj["file"]
        text = p.read_text()
        assert text.count(inj["find"]) == 1, f"{task['id']}: injection site not unique in {inj['file']}"
        if not fixed:
            p.write_text(text.replace(inj["find"], inj["replace"]))
    if task.get("venv"):
        (ws / ".venv").symlink_to(Path(venvs) / task["venv"])
    env = {**os.environ, **GIT_ENV}
    subprocess.run(["git", "init", "-q"], cwd=ws, check=True)
    (ws / ".git" / "info" / "exclude").write_text(".venv\n__pycache__/\n.pytest_cache/\n")
    subprocess.run(["git", "add", "-A"], cwd=ws, check=True, env=env)
    subprocess.run(["git", "commit", "-qm", "initial"], cwd=ws, check=True, env=env)
    return ws


def protected(path, patterns):
    return any(path.endswith(p) if not p.endswith("/") else (path.startswith(p) or f"/{p}" in f"/{path}")
               for p in patterns)


def grade(task, ws, timeout=900):
    diff_names = subprocess.run(["git", "diff", "--name-only", "HEAD"], cwd=ws, capture_output=True, text=True).stdout.split()
    new_files = subprocess.run(["git", "ls-files", "--others", "--exclude-standard"], cwd=ws,
                               capture_output=True, text=True).stdout.split()
    touched = [f for f in diff_names + new_files if protected(f, task["protect"])]
    t0 = time.time()
    try:
        res = subprocess.run(task["test"], shell=True, cwd=ws, capture_output=True, text=True, timeout=timeout)
        ok, tail = res.returncode == 0, (res.stdout + res.stderr)[-1500:]
    except subprocess.TimeoutExpired:
        ok, tail = False, "test timeout"
    diff = subprocess.run(["git", "diff", "HEAD"], cwd=ws, capture_output=True, text=True).stdout
    return {"pass": ok and not touched, "tests_ok": ok, "touched_tests": touched, "changed_files": diff_names,
            "new_files": new_files, "diff": diff[:12000], "test_tail": tail, "test_s": round(time.time() - t0, 1)}


def verify(tasks, args):
    root = Path(tempfile.mkdtemp(prefix="speedread-verify-"))
    ok = True
    for t in tasks:
        bad = grade(t, make_workspace(t, args.bench, args.venvs, root, t["id"] + "-bug"))
        good = grade(t, make_workspace(t, args.bench, args.venvs, root, t["id"] + "-fix", fixed=True))
        status = "OK " if (not bad["tests_ok"] and good["pass"]) else "BAD"
        ok &= status == "OK "
        print(f"{status} {t['id']:<24} injected: tests {'pass' if bad['tests_ok'] else 'FAIL'} ({bad['test_s']}s)   "
              f"reference fix: tests {'pass' if good['tests_ok'] else 'FAIL'} ({good['test_s']}s)", flush=True)
        if status == "BAD":
            print(bad["test_tail"][-600:], good["test_tail"][-600:], sep="\n---\n")
    shutil.rmtree(root, ignore_errors=True)
    return 0 if ok else 1


# ---------------------------------------------------------------- transcripts

def parse_events(text):
    events, pending = [], {}
    answer, sid, usage = "", None, {}
    for line in text.splitlines():
        try:
            e = json.loads(line)
        except Exception:
            continue
        t, d = e.get("type"), e.get("data") or {}
        if t == "model.call_start":
            events.append({"kind": "model"})
        elif t == "tool.execution_start":
            pending[d.get("toolCallId")] = {"kind": "tool", "tool": d.get("toolName"), "args": d.get("arguments") or {}}
        elif t == "tool.execution_complete":
            c = pending.pop(d.get("toolCallId"), {"kind": "tool", "tool": "?", "args": {}})
            res = d.get("result") or {}
            content = res.get("content") if isinstance(res, dict) else res
            c["result"] = content if isinstance(content, str) else json.dumps(content)
            c["ok"] = bool(d.get("success"))
            events.append(c)
        elif t == "assistant.message" and d.get("content"):
            answer = d["content"]
        elif t == "result":
            sid, usage = e.get("sessionId"), e.get("usage") or {}
    return events, answer, sid, usage


def bash_cmd(t):
    a = t.get("args") or {}
    return a.get("command") or a.get("cmd") or ""


def edit_path(t):
    a = t.get("args") or {}
    return a.get("path") or a.get("file_path") or a.get("filePath") or ""


def numbered_read_tokens(path, max_lines=2000):
    try:
        lines = Path(path).read_text("utf-8", "replace").splitlines()[:max_lines]
    except OSError:
        return 0
    return tok("".join(f"{i + 1:>6}\t{l}\n" for i, l in enumerate(lines)))


def analyze(task, events, ws):
    tools = [e for e in events if e["kind"] == "tool"]
    def is_read(t):
        if t["tool"] in SR_TOOLS or t["tool"] in NATIVE_READ:
            return True
        cmd = bash_cmd(t) or ""
        return t["tool"] == "bash" and bool(BASH_READ.search(cmd)) and not BASH_RUN.search(cmd)
    reads = [t for t in tools if is_read(t)]
    sr = [t for t in tools if t["tool"] in SR_TOOLS]
    edits = [t for t in tools if t["tool"] in EDIT_TOOLS]
    # Repeated source bytes: normalised result lines already shown earlier.
    seen, shown, repeated = set(), 0, 0
    for t in reads:
        for line in (t.get("result") or "").splitlines():
            norm = LINE_PREFIX.sub("", line).strip()
            if len(norm) < 8:
                continue
            b = len(norm.encode())
            shown += b
            if norm in seen:
                repeated += b
            else:
                seen.add(norm)
    # Decisive line: when was it first shown, and was it hidden in a skeleton first?
    dec = task["decisive"]
    first_seen, collapsed_first = None, False
    for i, t in enumerate(tools):
        r = t.get("result") or ""
        if first_seen is None and dec["snippet"] in r and t["tool"] not in EDIT_TOOLS:
            first_seen = i
        if first_seen is None and t["tool"] in SR_TOOLS and dec["file"] in r and ("[skeleton]" in r or "[outline]" in r
                                                                                  or " ⋯" in r):
            collapsed_first = True
    first_edit = next((i for i, t in enumerate(tools) if t["tool"] in EDIT_TOOLS), None)
    # Claude Code requires its own Read of a file before Edit; MCP reads don't count.
    viewed = {(t.get("args") or {}).get("path", "") for t in tools if t["tool"] == "view"}
    edited = sorted({edit_path(t) for t in edits if edit_path(t)})
    need = [p for p in edited if not any(p.endswith(v) or v.endswith(p) for v in viewed if v)]
    cc_tokens = sum(numbered_read_tokens(p if os.path.isabs(p) else ws / p) for p in need)
    model_after_edit = 0
    if first_edit is not None:
        k = [i for i, e in enumerate(events) if e is tools[first_edit]][0]
        model_after_edit = sum(1 for e in events[k:] if e["kind"] == "model")
    return {
        "tool_calls": len(tools), "tools": [t["tool"] for t in tools],
        "read_calls": len(reads), "speedread_calls": len(sr), "edit_calls": len(edits),
        "native_read_calls": sum(1 for t in reads if t["tool"] in NATIVE_READ),
        "bash_read_calls": sum(1 for t in reads if t["tool"] == "bash"),
        "used_speedread": bool(sr), "speedread_share": (len(sr) / len(reads)) if reads else None,
        "trace_calls": sum(1 for t in sr if t["tool"] == "speedread-trace"),
        "read_result_tokens": sum(tok(t.get("result") or "") for t in reads),
        "tool_result_tokens": sum(tok(t.get("result") or "") for t in tools),
        "shown_source_bytes": shown, "repeated_source_bytes": repeated,
        "decisive_seen": first_seen is not None,
        "decisive_seen_before_edit": first_seen is not None and (first_edit is None or first_seen < first_edit),
        "decisive_collapsed_first": collapsed_first,
        "edited_files": edited, "cc_mandatory_reads": len(need), "cc_read_tokens": cc_tokens,
        "cc_read_tokens_resent": cc_tokens * model_after_edit,
    }


# ---------------------------------------------------------------- trials

def run_trial(job, args, root, out_dir):
    task, cond, trial = job
    name = f"{task['id']}__{cond}__{trial}"
    ws = make_workspace(task, args.bench, args.venvs, root, name)
    prompt = task["prompt"] + (PREFER if cond == "preferred" else "")
    cmd = ["copilot", "-p", prompt, "--model", args.model, "--max-ai-credits", str(args.max_credits)]
    # The shared venv and Go caches live outside the workspace.
    cmd += COMMON + ["--allow-all-paths"] + condition_flags(cond, args.mcp_config)
    t0 = time.time()
    try:
        proc = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, timeout=args.timeout)
        out, err, code = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as ex:
        out = ex.stdout.decode() if isinstance(ex.stdout, bytes) else (ex.stdout or "")
        err, code = "timeout", -1
    wall = time.time() - t0
    real = os.path.realpath(ws)
    tmp = re.compile(r"(/private)?/var/folders/[^/\"' ]+/[^/\"' ]+/T/")
    scrub = lambda t: tmp.sub("<tmp>/", t.replace(real, "<ws>").replace(str(ws), "<ws>").replace(str(Path.home()), "~"))
    (out_dir / "transcripts" / f"{name}.jsonl").write_text(scrub(out))
    events, answer, sid, usage = parse_events(out)
    g = grade(task, ws)
    a = analyze(task, events, ws)
    u = session_usage(sid) if sid else {}
    rec = {
        "task": task["id"], "difficulty": task["difficulty"], "condition": cond, "trial": trial, "model": args.model,
        "exit_code": code, "wall_s": round(wall, 1), "session_ms": usage.get("sessionDurationMs"),
        "api_ms": usage.get("totalApiDurationMs"), "session_id": sid, "answer": (answer or "").strip()[-600:],
        **{k: g[k] for k in ("pass", "tests_ok", "touched_tests", "changed_files", "test_s")},
        **a,
        "edited_files": [scrub(f) for f in a["edited_files"]],
        "model_calls": u.get("model_calls") or sum(1 for e in events if e["kind"] == "model"),
        "input_tokens": u.get("input_tokens"), "output_tokens": u.get("output_tokens"),
        "cache_read_tokens": u.get("cache_read_tokens"), "reasoning_tokens": u.get("reasoning_tokens"),
        "cost_aiu": (u.get("nano_aiu") or 0) / 1e9 if u else None,
        "stderr_tail": err[-300:] if code else "",
    }
    (out_dir / "diffs").mkdir(exist_ok=True)
    (out_dir / "diffs" / f"{name}.diff").write_text(g["diff"])
    if not args.keep:
        shutil.rmtree(ws, ignore_errors=True)
    return rec


# ---------------------------------------------------------------- summary

def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.mean(xs) if xs else None


def median(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


METRICS = ("input_tokens", "output_tokens", "cost_aiu", "api_ms", "session_ms", "model_calls", "tool_calls",
           "read_calls", "edit_calls", "read_result_tokens", "shown_source_bytes", "repeated_source_bytes",
           "cc_read_tokens", "cc_read_tokens_resent", "trace_calls")


def summarize(recs, tasks):
    order = [c for c in CONDITIONS if any(r["condition"] == c for r in recs)]
    out = {"conditions": {}, "tasks": {}}
    for c in order:
        rs = [r for r in recs if r["condition"] == c]
        by_task = {}
        for r in rs:
            by_task.setdefault(r["task"], []).append(r)
        k = min(len(v) for v in by_task.values())
        out["conditions"][c] = {
            "trials": len(rs), "k": k,
            "pass@1": mean([r["pass"] for r in rs]),
            f"pass@{k}": mean([any(x["pass"] for x in v) for v in by_task.values()]),
            f"pass^{k}": mean([all(x["pass"] for x in v[:k]) for v in by_task.values()]),
            **{f"mean_{m}": mean([r.get(m) for r in rs]) for m in METRICS},
            **{f"median_{m}": median([r.get(m) for r in rs]) for m in ("input_tokens", "cost_aiu", "api_ms", "session_ms")},
            "adoption": mean([r["used_speedread"] for r in rs]),
            "speedread_share": mean([r["speedread_share"] for r in rs]),
            "decisive_seen": mean([r["decisive_seen"] for r in rs]),
            "decisive_collapsed_first": sum(r["decisive_collapsed_first"] for r in rs),
            "collapsed_then_failed": sum(r["decisive_collapsed_first"] and not r["pass"] for r in rs),
            "collapsed_never_expanded": sum(r["decisive_collapsed_first"] and not r["decisive_seen"] for r in rs),
            "touched_tests": sum(bool(r["touched_tests"]) for r in rs),
        }
    for t in tasks:
        row = {"difficulty": t["difficulty"]}
        for c in order:
            rs = [r for r in recs if r["condition"] == c and r["task"] == t["id"]]
            if rs:
                row[c] = {m: mean([r.get(m) for r in rs]) for m in ("pass", "input_tokens", "cost_aiu", "api_ms",
                                                                   "model_calls", "tool_calls", "used_speedread")}
        out["tasks"][t["id"]] = row
    return out


def pct(new, old):
    if not old or new is None:
        return "–"
    d = (1 - new / old) * 100
    return f"−{d:.0f}%" if d >= 0 else f"+{-d:.0f}%"


def markdown(s, meta):
    C = s["conditions"]
    b = C.get("baseline")
    lines = [f"# Coding eval — {meta['model']} via {meta['harness']}", "",
             f"{meta['tasks']} SWE-style bug-fix tasks × {meta['trials']} trials per condition, run {meta['date']}. "
             "Pass = the repository's full test suite passes with tests unmodified.", ""]
    hdr = "| Metric | " + " | ".join(C) + " |"
    lines += [hdr, "|---|" + "---:|" * len(C)]
    rows = [("Pass rate (pass@1)", "pass@1", "{:.0%}", False), ("Mean input tokens / task", "mean_input_tokens", "{:,.0f}", True),
            ("Median input tokens / task", "median_input_tokens", "{:,.0f}", True),
            ("Mean output tokens / task", "mean_output_tokens", "{:,.0f}", True), ("Mean cost / task (AI units)", "mean_cost_aiu", "{:.2f}", True),
            ("Median model (API) time, s", "median_api_ms", "{:.1f}", True), ("Median session time, s", "median_session_ms", "{:.1f}", True),
            ("Mean model calls (turns)", "mean_model_calls", "{:.1f}", True), ("Mean tool calls", "mean_tool_calls", "{:.1f}", True),
            ("Mean read/search calls", "mean_read_calls", "{:.1f}", True), ("Read-result tokens / task", "mean_read_result_tokens", "{:,.0f}", True),
            ("Source bytes shown / task", "mean_shown_source_bytes", "{:,.0f}", True),
            ("Repeated source bytes / task", "mean_repeated_source_bytes", "{:,.0f}", True),
            ("Used speedread (adoption)", "adoption", "{:.0%}", False), ("Share of reads via speedread", "speedread_share", "{:.0%}", False),
            ("Decisive line shown", "decisive_seen", "{:.0%}", False)]
    for label, key, fmt, delta in rows:
        vals = []
        for c, v in C.items():
            x = v.get(key)
            if x is not None and key.endswith("_ms"):
                x = x / 1000
            cell = fmt.format(x) if x is not None else "–"
            if delta and b and c != "baseline" and v.get(key) is not None and b.get(key):
                cell += f" ({pct(v[key], b[key])})"
            vals.append(cell)
        lines.append(f"| {label} | " + " | ".join(vals) + " |")
    lines += ["", "Consistency: " + "; ".join(
        f"{c} pass@{v['k']} {v['pass@' + str(v['k'])]:.0%}, pass^{v['k']} {v['pass^' + str(v['k'])]:.0%}" for c, v in C.items()), ""]
    lines += ["Compression safety (speedread conditions): " + "; ".join(
        f"{c}: decisive line first hidden in a skeleton in {v['decisive_collapsed_first']} trials "
        f"(never expanded {v['collapsed_never_expanded']}, failed {v['collapsed_then_failed']})"
        for c, v in C.items() if c != "baseline"), ""]
    lines += ["## Per task (means)", "", "| Task | Difficulty | " + " | ".join(f"{c} pass / tokens" for c in C) + " |",
              "|---|---|" + "---:|" * len(C)]
    for tid, row in s["tasks"].items():
        cells = []
        for c in C:
            d = row.get(c)
            cells.append(f"{d['pass']:.0%} / {d['input_tokens']:,.0f}" if d and d.get("input_tokens") is not None else "–")
        lines.append(f"| {tid} | {row['difficulty']} | " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench", required=True, type=Path)
    ap.add_argument("--venvs", required=True, type=Path)
    ap.add_argument("--tasks", default=str(HERE / "coding_tasks.json"))
    ap.add_argument("--model", default="claude-sonnet-5")
    ap.add_argument("--trials", type=int, default=2)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--conditions", default=",".join(CONDITIONS))
    ap.add_argument("--max-credits", type=int, default=90)
    ap.add_argument("--timeout", type=int, default=1200)
    ap.add_argument("--mcp-config", default=str(HERE / "speedread-mcp.json"))
    ap.add_argument("--out", default=None)
    ap.add_argument("--only", default=None)
    ap.add_argument("--seed", type=int, default=13)
    ap.add_argument("--keep", action="store_true", help="keep trial workspaces")
    ap.add_argument("--verify", action="store_true", help="check every task fails as injected and passes when fixed")
    ap.add_argument("--summarize", default=None, help="rebuild summary for an existing run dir")
    args = ap.parse_args()
    args.bench, args.venvs = args.bench.resolve(), args.venvs.resolve()
    suite = json.loads(Path(args.tasks).read_text())
    tasks = [t for t in suite["tasks"] if not args.only or t["id"] in args.only.split(",")]
    if args.verify:
        return verify(tasks, args)
    if args.summarize:
        d = Path(args.summarize)
        recs = [json.loads(l) for l in (d / "trials.jsonl").read_text().splitlines() if l.strip()]
        meta = json.loads((d / "summary.json").read_text())["meta"]
        present = [t for t in tasks if any(r["task"] == t["id"] for r in recs)]
        counts = {}
        for r in recs:
            counts[(r["task"], r["condition"])] = counts.get((r["task"], r["condition"]), 0) + 1
        meta.update(tasks=len(present), trials=min(counts.values()), date=time.strftime("%Y-%m-%d"))
        s = summarize(recs, present)
        (d / "summary.json").write_text(json.dumps({"meta": meta, **s}, indent=1))
        (d / "summary.md").write_text(markdown(s, meta))
        print(markdown(s, meta))
        return 0
    conds = args.conditions.split(",")
    out_dir = Path(args.out) if args.out else HERE / "results" / f"coding-{time.strftime('%Y%m%d-%H%M%S')}"
    (out_dir / "transcripts").mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="speedread-coding-"))
    jobs = [(t, c, i) for t in tasks for c in conds for i in range(args.trials)]
    random.Random(args.seed).shuffle(jobs)
    print(f"{len(jobs)} trials, {args.workers} workers, {args.model} → {out_dir}", flush=True)
    recs, lock = [], threading.Lock()
    mode = "a" if (out_dir / "trials.jsonl").exists() else "w"
    with open(out_dir / "trials.jsonl", mode) as fh, cf.ThreadPoolExecutor(args.workers) as pool:
        futs = [pool.submit(run_trial, j, args, root, out_dir) for j in jobs]
        for n, fut in enumerate(cf.as_completed(futs), 1):
            try:
                r = fut.result()
            except Exception as ex:  # keep the run going; record nothing for this trial
                print(f"[{n:>3}/{len(jobs)}] trial error: {ex!r}", flush=True)
                continue
            with lock:
                recs.append(r)
                fh.write(json.dumps(r) + "\n")
                fh.flush()
            print(f"[{n:>3}/{len(jobs)}] {r['task']:<24} {r['condition']:<9} pass={int(r['pass'])} "
                  f"cost={r['cost_aiu'] or 0:5.2f} in={r['input_tokens'] or 0:>8,} turns={r['model_calls']} "
                  f"tools={r['tool_calls']} sr={r['speedread_calls']} t={r['wall_s']:6.1f}s", flush=True)
    all_recs = [json.loads(l) for l in (out_dir / "trials.jsonl").read_text().splitlines() if l.strip()]
    s = summarize(all_recs, tasks)
    version = subprocess.run(["copilot", "--version"], capture_output=True, text=True).stdout.strip().splitlines()[0]
    meta = {"model": args.model, "harness": "GitHub Copilot CLI " + version.split("CLI ")[-1].rstrip("."),
            "tasks": len(tasks), "trials": args.trials, "date": time.strftime("%Y-%m-%d")}
    (out_dir / "summary.json").write_text(json.dumps({"meta": meta, **s}, indent=1))
    (out_dir / "summary.md").write_text(markdown(s, meta))
    print(markdown(s, meta))
    shutil.rmtree(root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
