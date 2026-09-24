#!/usr/bin/env python3
"""Real-agent A/B evaluation of speedread (Suite 2 in evals/README.md).

The same agent harness (GitHub Copilot CLI, non-interactive) and model answer
the same read-only code questions under different conditions:

  baseline   the harness's built-in file tools (view / grep / glob / bash)
  speedread  speedread's MCP tools as the file reader (built-in view/grep/glob
             excluded; bash still available)
  dropin     speedread merely added next to the built-in tools (measures
             unprompted adoption)

Each trial is an isolated, fresh session with custom instructions and built-in
MCP servers disabled. Transcripts (JSONL) are saved; answers are graded with
deterministic regex graders (partial credit); token usage and cost come from
the harness's local usage log (exact per-model-call accounting).

Usage:
  python3 evals/agent_eval.py --bench <dir-with-repos> [--trials 3] [--workers 3]
          [--model claude-sonnet-5] [--conditions baseline,speedread,dropin]
"""

import argparse
import concurrent.futures as cf
import json
import os
import random
import re
import sqlite3
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
DB = Path.home() / ".copilot" / "session-store.db"

COMMON = [
    "--allow-all-tools",
    "--no-custom-instructions",
    "--disable-builtin-mcps",
    "--no-ask-user",
    "--no-auto-update",
    "--output-format",
    "json",
]
SPEEDREAD_TOOLS = ("speedread-read", "speedread-search", "speedread-map")

try:  # optional: tokens of tool results entering the context
    import tiktoken

    _ENC = tiktoken.get_encoding("o200k_base")

    def tok(s):
        return len(_ENC.encode(s, disallowed_special=()))
except Exception:  # pragma: no cover
    def tok(s):
        return len(s) // 4


def condition_flags(cond, mcp_config):
    sr = ["--additional-mcp-config", f"@{mcp_config}", "--allow-all-mcp-server-instructions"]
    if cond == "baseline":
        return []
    if cond == "speedread":
        # Variadic flag: keep it last.
        return sr + ["--excluded-tools", "view", "grep", "glob", "rg"]
    if cond == "dropin":
        return sr
    raise ValueError(cond)


def parse_transcript(text):
    answer, calls, model_calls, session_id, usage = "", [], 0, None, {}
    pending = {}
    for line in text.splitlines():
        try:
            e = json.loads(line)
        except Exception:
            continue
        t, d = e.get("type"), e.get("data") or {}
        if t == "model.call_start":
            model_calls += 1
        elif t == "tool.execution_start":
            pending[d.get("toolCallId")] = {"tool": d.get("toolName"), "args": d.get("arguments")}
        elif t == "tool.execution_complete":
            c = pending.pop(d.get("toolCallId"), {"tool": "?", "args": None})
            res = d.get("result") or {}
            content = res.get("content") if isinstance(res, dict) else res
            content = content if isinstance(content, str) else json.dumps(content)
            c.update(ok=bool(d.get("success")), result_chars=len(content), result_tokens=tok(content))
            calls.append(c)
        elif t == "assistant.message" and d.get("content"):
            answer = d["content"]
        elif t == "result":
            session_id = e.get("sessionId")
            usage = e.get("usage") or {}
    return answer, calls, model_calls, session_id, usage


def session_usage(session_id, tries=20):
    q = """SELECT COUNT(*), SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens),
                  SUM(cache_write_tokens), SUM(reasoning_tokens), SUM(total_nano_aiu), SUM(duration_ms)
           FROM assistant_usage_events WHERE session_id = ?"""
    for _ in range(tries):
        try:
            con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True, timeout=5)
            row = con.execute(q, (session_id,)).fetchone()
            con.close()
            if row and row[0]:
                keys = ["model_calls", "input_tokens", "output_tokens", "cache_read_tokens",
                        "cache_write_tokens", "reasoning_tokens", "nano_aiu", "model_ms"]
                return dict(zip(keys, [v or 0 for v in row]))
        except sqlite3.Error:
            pass
        time.sleep(0.5)
    return {}


def grade(task, answer):
    hits = [bool(re.search(p, answer, re.I)) for p in task["graders"]]
    return sum(hits) / len(hits), all(hits), hits


def run_trial(job, args, out_dir, bench):
    task, cond, trial = job
    cwd = bench / task["repo"]
    cmd = ["copilot", "-p", task["prompt"], "--model", args.model, "--max-ai-credits", str(args.max_credits)]
    cmd += COMMON + condition_flags(cond, args.mcp_config)
    t0 = time.time()
    try:
        proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=args.timeout)
        out, err, code = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as ex:
        out, err, code = ex.stdout or "", "timeout", -1
        out = out.decode() if isinstance(out, bytes) else out
    wall = time.time() - t0
    scrubbed = out.replace(str(bench), "<bench>").replace(str(Path.home()), "~")
    tpath = out_dir / "transcripts" / f"{task['id']}__{cond}__{trial}.jsonl"
    tpath.write_text(scrubbed)
    answer, calls, model_calls, sid, usage = parse_transcript(out)
    score, passed, hits = grade(task, answer)
    u = session_usage(sid) if sid else {}
    rec = {
        "task": task["id"], "category": task["category"], "condition": cond, "trial": trial,
        "model": args.model, "exit_code": code, "wall_s": round(wall, 2),
        "session_ms": usage.get("sessionDurationMs"), "api_ms": usage.get("totalApiDurationMs"),
        "session_id": sid, "answer": answer.strip(), "score": score, "pass": passed, "grader_hits": hits,
        "tool_calls": len(calls), "tools": [c["tool"] for c in calls],
        "used_speedread": any(c["tool"] in SPEEDREAD_TOOLS for c in calls),
        "tool_result_tokens": sum(c.get("result_tokens", 0) for c in calls),
        "model_calls": u.get("model_calls") or model_calls,
        "input_tokens": u.get("input_tokens"), "output_tokens": u.get("output_tokens"),
        "cache_read_tokens": u.get("cache_read_tokens"), "cache_write_tokens": u.get("cache_write_tokens"),
        "reasoning_tokens": u.get("reasoning_tokens"),
        "cost_aiu": (u.get("nano_aiu") or 0) / 1e9 if u else None,
        "stderr_tail": err[-300:] if code else "",
    }
    return rec


def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.mean(xs) if xs else None


def median(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None


def summarize(recs, tasks):
    conds = sorted({r["condition"] for r in recs}, key=["baseline", "speedread", "dropin"].index)
    out = {"conditions": {}, "tasks": {}}
    for c in conds:
        rs = [r for r in recs if r["condition"] == c]
        by_task = {}
        for r in rs:
            by_task.setdefault(r["task"], []).append(r)
        ks = [len(v) for v in by_task.values()]
        k = min(ks) if ks else 0
        out["conditions"][c] = {
            "trials": len(rs),
            "pass@1": mean([r["pass"] for r in rs]),
            "mean_score": mean([r["score"] for r in rs]),
            f"pass@{k}": mean([any(x["pass"] for x in v) for v in by_task.values()]),
            f"pass^{k}": mean([all(x["pass"] for x in v) for v in by_task.values()]),
            "k": k,
            **{f"mean_{m}": mean([r[m] for r in rs]) for m in (
                "input_tokens", "output_tokens", "cache_read_tokens", "cost_aiu", "session_ms", "api_ms",
                "wall_s", "model_calls", "tool_calls", "tool_result_tokens")},
            **{f"median_{m}": median([r[m] for r in rs]) for m in ("input_tokens", "cost_aiu", "session_ms")},
            "adoption": mean([r["used_speedread"] for r in rs]),
        }
    for t in tasks:
        row = {"category": t["category"]}
        for c in conds:
            rs = [r for r in recs if r["condition"] == c and r["task"] == t["id"]]
            if not rs:
                continue
            row[c] = {m: mean([r[m] for r in rs]) for m in (
                "pass", "score", "input_tokens", "cost_aiu", "session_ms", "model_calls", "tool_calls",
                "tool_result_tokens", "used_speedread")}
        out["tasks"][t["id"]] = row
    return out


def pct(new, old):
    if not old:
        return "–"
    d = (1 - new / old) * 100
    return f"{d:.0f}% lower" if d >= 0 else f"{-d:.0f}% higher"


def markdown(summary, meta):
    C = summary["conditions"]
    b, s = C.get("baseline"), C.get("speedread")
    lines = [f"# Agent A/B eval — {meta['model']} via {meta['harness']}", "",
             f"{meta['tasks']} tasks × {meta['trials']} trials per condition, run {meta['date']}.", ""]
    hdr = "| Metric | " + " | ".join(C) + (" | speedread vs baseline |" if b and s else " |")
    lines += [hdr, "|---|" + "---:|" * (len(C) + (1 if b and s else 0))]
    rows = [("Pass rate (pass@1)", "pass@1", "{:.0%}"), ("Mean input tokens / task", "mean_input_tokens", "{:,.0f}"),
            ("Mean output tokens / task", "mean_output_tokens", "{:,.0f}"), ("Mean cost / task (AI units)", "mean_cost_aiu", "{:.2f}"),
            ("Mean session time (s)", "mean_session_ms", "{:.1f}"), ("Mean model calls", "mean_model_calls", "{:.2f}"),
            ("Mean tool calls", "mean_tool_calls", "{:.2f}"), ("Tool-result tokens / task", "mean_tool_result_tokens", "{:,.0f}"),
            ("Used speedread", "adoption", "{:.0%}")]
    for label, key, fmt in rows:
        vals = []
        for c in C:
            v = C[c].get(key)
            if key == "mean_session_ms" and v is not None:
                v = v / 1000
            vals.append(fmt.format(v) if v is not None else "–")
        line = f"| {label} | " + " | ".join(vals) + " |"
        if b and s and key not in ("pass@1", "adoption") and b.get(key):
            line += f" {pct(s[key], b[key])} |"
        elif b and s:
            line += " |"
        lines.append(line)
    lines += ["", "Consistency: " + "; ".join(
        f"{c} pass@{C[c]['k']} {C[c]['pass@' + str(C[c]['k'])]:.0%}, "
        f"pass^{C[c]['k']} {C[c]['pass^' + str(C[c]['k'])]:.0%}" for c in C), ""]
    lines += ["## Per task (means)", "", "| Task | Category | baseline cost | speedread cost | baseline s | speedread s | baseline pass | speedread pass |",
              "|---|---|---:|---:|---:|---:|---:|---:|"]
    for tid, row in summary["tasks"].items():
        bb, ss = row.get("baseline", {}), row.get("speedread", {})
        f = lambda d, m, fmt: fmt.format(d[m]) if d.get(m) is not None else "–"
        lines.append(f"| {tid} | {row['category']} | {f(bb,'cost_aiu','{:.2f}')} | {f(ss,'cost_aiu','{:.2f}')} | "
                     f"{(bb.get('session_ms') or 0)/1000:.1f} | {(ss.get('session_ms') or 0)/1000:.1f} | "
                     f"{f(bb,'pass','{:.0%}')} | {f(ss,'pass','{:.0%}')} |")
    return "\n".join(lines) + "\n"


def regrade(run_dir, args):
    suite = json.loads(Path(args.tasks).read_text())
    by_id = {t["id"]: t for t in suite["tasks"]}
    recs = [json.loads(l) for l in (run_dir / "trials.jsonl").read_text().splitlines() if l.strip()]
    changed = 0
    for r in recs:
        score, passed, hits = grade(by_id[r["task"]], r["answer"])
        changed += passed != r["pass"]
        r.update(score=score, grader_hits=hits, **{"pass": passed})
    (run_dir / "trials.jsonl").write_text("".join(json.dumps(r) + "\n" for r in recs))
    meta = json.loads((run_dir / "summary.json").read_text()).get("meta", {}) if (run_dir / "summary.json").exists() else {
        "model": recs[0]["model"], "harness": "GitHub Copilot CLI", "tasks": len(by_id), "trials": args.trials,
        "date": time.strftime("%Y-%m-%d")}
    tasks = [t for t in suite["tasks"] if any(r["task"] == t["id"] for r in recs)]
    summary = summarize(recs, tasks)
    (run_dir / "summary.json").write_text(json.dumps({"meta": meta, **summary}, indent=1))
    (run_dir / "summary.md").write_text(markdown(summary, meta))
    print(f"re-graded {len(recs)} trials ({changed} verdicts changed)")
    print(markdown(summary, meta))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bench", required=True, help="directory containing the cloned repos")
    ap.add_argument("--tasks", default=str(HERE / "agent_tasks.json"))
    ap.add_argument("--model", default="claude-sonnet-5")
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--conditions", default="baseline,speedread")
    ap.add_argument("--dropin-trials", type=int, default=0, help="extra trials for the drop-in condition")
    ap.add_argument("--max-credits", type=int, default=60, help="AI-credit cap per trial")
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--mcp-config", default=str(HERE / "speedread-mcp.json"))
    ap.add_argument("--out", default=None)
    ap.add_argument("--only", default=None, help="comma-separated task ids")
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--regrade", default=None, help="re-grade an existing run dir with the current graders (no new trials)")
    args = ap.parse_args()
    if args.regrade:
        return regrade(Path(args.regrade), args)

    bench = Path(args.bench).resolve()
    suite = json.loads(Path(args.tasks).read_text())
    tasks = [t for t in suite["tasks"] if not args.only or t["id"] in args.only.split(",")]
    conds = args.conditions.split(",")
    run_id = time.strftime("%Y%m%d-%H%M%S")
    out_dir = Path(args.out) if args.out else HERE / "results" / f"agent-{run_id}"
    (out_dir / "transcripts").mkdir(parents=True, exist_ok=True)

    jobs = [(t, c, i) for t in tasks for c in conds for i in range(args.trials)]
    jobs += [(t, "dropin", i) for t in tasks for i in range(args.dropin_trials)]
    random.Random(args.seed).shuffle(jobs)  # interleave conditions to spread drift/noise
    print(f"{len(jobs)} trials, {args.workers} workers, model {args.model} → {out_dir}", flush=True)

    lock = threading.Lock()
    recs = []
    with open(out_dir / "trials.jsonl", "w") as fh, cf.ThreadPoolExecutor(args.workers) as pool:
        futs = {pool.submit(run_trial, j, args, out_dir, bench): j for j in jobs}
        for n, fut in enumerate(cf.as_completed(futs), 1):
            r = fut.result()
            with lock:
                recs.append(r)
                fh.write(json.dumps(r) + "\n")
                fh.flush()
            print(f"[{n:>3}/{len(jobs)}] {r['task']:<28} {r['condition']:<9} pass={int(r['pass'])} "
                  f"cost={r['cost_aiu'] or 0:5.2f} in={r['input_tokens'] or 0:>7,} tools={r['tool_calls']} "
                  f"t={r['wall_s']:5.1f}s {'SR' if r['used_speedread'] else ''}", flush=True)

    summary = summarize(recs, tasks)
    meta = {"model": args.model, "harness": "GitHub Copilot CLI " + subprocess.run(
        ["copilot", "--version"], capture_output=True, text=True).stdout.strip().split("CLI ")[-1].rstrip("."),
            "tasks": len(tasks), "trials": args.trials, "date": time.strftime("%Y-%m-%d")}
    (out_dir / "summary.json").write_text(json.dumps({"meta": meta, **summary}, indent=1))
    (out_dir / "summary.md").write_text(markdown(summary, meta))
    print(markdown(summary, meta))


if __name__ == "__main__":
    main()
