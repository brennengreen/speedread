#!/usr/bin/env python3
"""Build the speedread demo page and README media from recorded eval data.

Everything shown is generated from files in evals/results/ (and, for the race,
from the saved transcripts plus the harness's per-call usage log, cached in
demo/race-data.json so the build is reproducible without the log):

  docs/assets/race.svg          animated replay of a real transcript pair
  docs/assets/chart-agent.svg   real-agent A/B (Suite 3)
  docs/assets/chart-tools.svg   tool scenarios (Suite 1)
  docs/assets/chart-budget.svg  budget contract (Suite 2)
  docs/assets/chart-coding.svg  SWE-style coding tasks, four conditions (Suite 4)
  docs/assets/chart-relations.svg  relationship questions (Suite 3b)
  demo/index.html               self-contained report

Usage: python3 demo/build.py
"""

import base64
import html
import json
import re
import sqlite3
import statistics as st
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RES = ROOT / "evals" / "results"
ASSETS = ROOT / "docs" / "assets"
DEMO = ROOT / "demo"
AGENT = RES / "agent-claude-sonnet-5"
CODING = RES / "coding-claude-sonnet-5"
RELATIONS = RES / "relationships-claude-sonnet-5"
# All three trials of each condition were identical for this task, so the
# replay is representative rather than a best case.
RACE_TASK, RACE_TRIAL = "sap-configuration", 0

BG, CARD, BORDER = "#0b1020", "#111831", "#223057"
TEXT, MUTED, DIM = "#e7eaf3", "#98a1b8", "#5d6784"
BASE, A, B = "#64748b", "#6366f1", "#22d3ee"
GOOD, WARN, BAD = "#34d399", "#fbbf24", "#f87171"
SANS = "-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif"
MONO = "ui-monospace,SFMono-Regular,Menlo,Consolas,monospace"

# Suite 2 with the old fixed 2.6 bytes/token estimate: the run of
# evals/budget_eval.py on 2026-09-24 before src/tokens.rs (same corpus, seed
# and script; "after" numbers come from evals/results/budget_eval.json).
# (share of read responses over budget, worst of o200k / legacy Claude;
#  worst o200k ratio of real tokens to budget)
BUDGET_BEFORE = {
    "SVG": (0.44, 1.55), "JSON": (0.39, 1.41), "Lockfiles": (0.11, 0.96), "Go": (0.06, 0.94),
    "CJK (real: docs, locales, i18n)": (0.03, 0.88), "Synthetic: emoji": (1.0, 1.00),
    "Synthetic: base64": (1.0, 1.74), "Synthetic: hexdump": (1.0, 1.73), "Synthetic: numbers": (1.0, 1.24),
    "Synthetic: unicode_math": (1.0, 1.31),
}
BUDGET_BEFORE_POOLED = {"over": 0.095, "max": 1.81, "n": 525}

CITATIONS = [
    ("Anthropic: Demystifying evals for AI agents", "https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents"),
    ("Anthropic: Code execution with MCP", "https://www.anthropic.com/engineering/code-execution-with-mcp"),
    ("Anthropic: Effective context engineering for AI agents", "https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents"),
    ("Anthropic: Writing effective tools for agents", "https://www.anthropic.com/engineering/writing-tools-for-agents"),
    ("AgentDiet: trajectory reduction, 40–60% fewer input tokens without loss", "https://arxiv.org/abs/2509.23586"),
    ("Chroma: Context Rot, accuracy falls with input length", "https://research.trychroma.com/context-rot"),
    ("Lost in the Middle", "https://arxiv.org/abs/2307.03172"),
    ("Agentless: file → skeleton → lines localization", "https://arxiv.org/abs/2407.01489"),
    ("SWE-agent: agent-computer interfaces", "https://arxiv.org/abs/2405.15793"),
    ("AutoCodeRover: AST search APIs", "https://arxiv.org/abs/2404.05427"),
    ("LocAgent: code graphs for localization", "https://arxiv.org/abs/2503.09089"),
    ("CodeCompass: 58% of trials never called a better navigation tool", "https://arxiv.org/abs/2602.20048"),
    ("Manus: context engineering lessons (~100:1 input:output)", "https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus"),
    ("Aider: tree-sitter repo map", "https://github.com/Aider-AI/aider/blob/main/aider/repomap.py"),
    ("Apple TN3150: dataless files", "https://developer.apple.com/documentation/technotes/tn3150-getting-ready-for-data-less-files.md"),
    ("getattrlistbulk on macOS (healeycodes)", "https://healeycodes.com/maybe-the-fastest-disk-usage-program-on-macos"),
]


def esc(s):
    return html.escape(str(s), quote=True)


def mean(xs):
    xs = [x for x in xs if x is not None]
    return st.mean(xs) if xs else None


def median(xs):
    xs = [x for x in xs if x is not None]
    return st.median(xs) if xs else None


def change(new, old):
    return (new / old - 1) * 100


def signed(x, digits=0):
    """-50 -> '−50%' (typographic minus), 12 -> '+12%'."""
    return f"{'−' if x < 0 else '+'}{abs(x):.{digits}f}%"


def load_jsonl(p):
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()]


# ---------------------------------------------------------------- data

def agent_data():
    s = json.loads((AGENT / "summary.json").read_text())
    recs = load_jsonl(AGENT / "trials.jsonl")
    m = {}
    for c in ("baseline", "speedread", "dropin"):
        rs = [r for r in recs if r["condition"] == c]
        m[c] = {"n": len(rs), "input": mean(r["input_tokens"] for r in rs), "cost": mean(r["cost_aiu"] for r in rs),
                "calls": mean(r["model_calls"] for r in rs), "tools": mean(r["tool_calls"] for r in rs),
                "api_med": median(r["api_ms"] for r in rs) / 1000, "session_med": median(r["session_ms"] for r in rs) / 1000,
                "pass": mean(r["pass"] for r in rs), "adoption": mean(r["used_speedread"] for r in rs)}
    for c in ("baseline", "speedread"):
        m[c]["pass_k"] = s["conditions"][c]["pass^3"]
    return s, m


def tool_data():
    rows = json.loads((RES / "tool_eval.json").read_text())
    order = [("large-file", "Understand a large file"), ("function", "Read one function"),
             ("function-best", "Read one function (vs best-case grep + window)"),
             ("reread", "Re-read a file after an edit"), ("usages", "Find usages + read the callers"),
             ("log", "Follow a growing log"), ("small-file", "Small files (control)"), ("orient", "Orient in a repo")]
    agg = {k: [0, 0, 0, 0, 0, 0] for k, _ in order}
    for x in rows:
        a = agg["function-best" if "best case" in x["name"] else x["suite"]]
        a[0] += x["base_tok"]; a[1] += x["sr_tok"]; a[2] += x["base_calls"]; a[3] += x["sr_calls"]
        a[4] += 1; a[5] += bool(x["sr_pass"])
    tot = {"base": sum(x["base_tok"] for x in rows), "sr": sum(x["sr_tok"] for x in rows),
           "base_calls": sum(x["base_calls"] for x in rows), "sr_calls": sum(x["sr_calls"] for x in rows),
           "n": len(rows), "pass": sum(bool(x["sr_pass"]) for x in rows)}
    return [(label, *agg[k]) for k, label in order], tot


def budget_data():
    rep = json.loads((RES / "budget_eval.json").read_text())
    after = {}
    for cat, v in rep["categories"].items():
        s = v["summary"]
        after[cat] = (max(s.get(f"budget_{k}", {}).get("over", 0) for k in ("o200k", "claude")),
                      max(s.get(f"budget_{k}", {}).get("max", 0) for k in ("o200k", "claude")))
    pooled = rep["pooled_reads (everything)"]
    return after, {"over": max(v["over"] for v in pooled.values()), "max": max(v["max"] for v in pooled.values()),
                   "n": max(v["n"] for v in pooled.values())}


def relation_data():
    if not (RELATIONS / "trials.jsonl").exists():
        return None
    recs = load_jsonl(RELATIONS / "trials.jsonl")
    tasks = json.loads((ROOT / "evals" / "relationship_tasks.json").read_text())["tasks"]
    rows = []
    for t in tasks:
        b = [r for r in recs if r["task"] == t["id"] and r["condition"] == "baseline"]
        s_ = [r for r in recs if r["task"] == t["id"] and r["condition"] == "speedread"]
        if b and s_:
            rows.append({"id": t["id"], "category": t["category"],
                         "b_in": mean(r["input_tokens"] for r in b), "s_in": mean(r["input_tokens"] for r in s_),
                         "b_tools": mean(r["tool_calls"] for r in b), "s_tools": mean(r["tool_calls"] for r in s_),
                         "b_pass": mean(r["pass"] for r in b), "s_pass": mean(r["pass"] for r in s_)})
    by = {c: [r for r in recs if r["condition"] == c] for c in ("baseline", "speedread")}
    tot = {c: {"input": mean(r["input_tokens"] for r in v), "cost": mean(r["cost_aiu"] for r in v),
               "calls": mean(r["model_calls"] for r in v), "tools": mean(r["tool_calls"] for r in v),
               "pass": mean(r["pass"] for r in v), "n": len(v),
               "trace": sum("speedread-trace" in r["tools"] for r in v)} for c, v in by.items()}
    return {"rows": rows, "tot": tot}


def optional_json(p):
    return json.loads(p.read_text()) if p.exists() else None


# ---------------------------------------------------------------- race data

def _ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def race_data():
    cache = DEMO / "race-data.json"
    db = Path.home() / ".copilot" / "session-store.db"
    if cache.exists() and not db.exists():
        return json.loads(cache.read_text())
    recs = load_jsonl(AGENT / "trials.jsonl")
    task = next(t for t in json.loads((ROOT / "evals" / "agent_tasks.json").read_text())["tasks"] if t["id"] == RACE_TASK)
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    out = {"task": RACE_TASK, "prompt": task["prompt"], "repo": task["repo"], "sides": {}}
    for cond in ("baseline", "speedread"):
        r = next(x for x in recs if x["task"] == RACE_TASK and x["condition"] == cond and x["trial"] == RACE_TRIAL)
        usage = con.execute("SELECT input_tokens, total_nano_aiu FROM assistant_usage_events WHERE session_id = ? "
                            "AND parent_tool_call_id IS NULL ORDER BY rowid", (r["session_id"],)).fetchall()
        events, t0, k = [], None, 0
        for line in (AGENT / "transcripts" / f"{RACE_TASK}__{cond}__{RACE_TRIAL}.jsonl").read_text().splitlines():
            e = json.loads(line)
            t, d, ts = e.get("type"), e.get("data") or {}, e.get("timestamp")
            if not ts:
                continue
            sec = _ts(ts)
            if t == "model.call_start":
                t0 = sec if t0 is None else t0
                events.append({"t": sec - t0, "kind": "model", "k": k + 1, "input": usage[k][0], "aiu": usage[k][1] / 1e9})
                k += 1
            elif t0 is None:
                continue
            elif t == "tool.execution_start":
                events.append({"t": sec - t0, "kind": "tool", "tool": d.get("toolName"), "args": d.get("arguments") or {}})
            elif t == "tool.execution_complete":
                res = d.get("result") or {}
                c = res.get("content") if isinstance(res, dict) else res
                events.append({"t": sec - t0, "kind": "result", "text": c if isinstance(c, str) else json.dumps(c)})
            elif t == "assistant.message" and d.get("content") and not d.get("toolRequests"):
                events.append({"t": sec - t0, "kind": "answer", "text": d["content"]})
        same = [(x["input_tokens"], x["model_calls"], x["tool_calls"]) for x in recs
                if x["task"] == RACE_TASK and x["condition"] == cond]
        out["sides"][cond] = {"events": events, "pass": r["pass"], "input": r["input_tokens"], "cost": r["cost_aiu"],
                              "api_s": r["api_ms"] / 1000, "calls": r["model_calls"], "tools": r["tool_calls"],
                              "trials": len(same),
                              # same round trips and tool calls, tokens within 1%
                              "identical": len({(b, c) for _, b, c in same}) == 1
                              and max(a for a, _, _ in same) <= 1.01 * min(a for a, _, _ in same)}
    DEMO.mkdir(exist_ok=True)
    cache.write_text(json.dumps(out, indent=1))
    return out


def clip(s, n):
    s = s.replace("\t", "  ").rstrip()
    return s if len(s) <= n else s[: n - 1] + "…"


def tool_line(ev):
    a, tool = ev["args"], ev["tool"]

    def short(p):
        if isinstance(p, list):
            return ",".join(short(x) for x in p)
        return p.split("/")[-1] if isinstance(p, str) else ""

    if tool == "grep":
        flags = " ".join(k for k in ("-n", "-i") if a.get(k))
        return "grep", f'"{a.get("pattern")}" {short(a.get("paths") or a.get("path") or "")} {flags}'.strip()
    if tool == "bash":
        cmd = re.sub(r'"?<bench>/[^"]*/([^/"]+)"?', r"\1", a.get("command", ""))
        return "bash", cmd
    if tool == "view":
        return "view", f'{short(a.get("path", ""))} {a.get("view_range") or ""}'.strip()
    if tool.startswith("speedread-"):
        name = tool.split("-", 1)[1]
        if name == "search":
            return "search", f'"{a.get("pattern")}" {short(a.get("paths") or "")}'.strip()
        if name == "read":
            return "read", ", ".join(a.get("targets") or [])
        return name, json.dumps(a)
    return tool, json.dumps(a)


# ---------------------------------------------------------------- SVG

def svg_open(w, h, style=""):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img" '
            f'font-family="{SANS}" xml:space="preserve">\n<defs><linearGradient id="g" x1="0" x2="1"><stop offset="0" stop-color="{A}"/>'
            f'<stop offset="1" stop-color="{B}"/></linearGradient></defs>\n<style>text{{white-space:pre}}{style}</style>\n'
            f'<rect width="{w}" height="{h}" rx="18" fill="{BG}"/>\n')


def keyframes(name, t_on, t_off, T, fade=0.25):
    """Opacity 0 until t_on, 1 until t_off, then 0 (seconds of a T-second loop)."""
    p = lambda t: max(0.0, min(100.0, t / T * 100))
    stops = [(0.0, 0)]
    a = p(t_on)
    if a > 0:
        stops.append((a, 0))
    for s, o in ((p(t_on + fade), 1), (p(t_off), 1), (p(t_off + fade), 0)):
        s = max(s, stops[-1][0] + 0.01)
        stops.append((min(s, 100.0), o))
    if stops[-1][0] < 100:
        stops.append((100.0, 0))
    body = "".join(f"{s:.2f}%{{opacity:{o}}}" for s, o in stops)
    return f"@keyframes {name}{{{body}}}.{name}{{opacity:0;animation:{name} {T}s linear infinite}}"


def race_svg(data):
    W, H, T = 1280, 700, 16.0
    END = T - 1.1
    sides = data["sides"]
    b, s = sides["baseline"], sides["speedread"]
    done = {c: next(e["t"] for e in v["events"] if e["kind"] == "answer") for c, v in sides.items()}
    styles, body, n = [], [], [0]

    def uid():
        n[0] += 1
        return f"e{n[0]}"

    note = (f"all {b['trials']} trials of each condition were identical" if b["identical"] and s["identical"]
            else f"trial {RACE_TRIAL + 1} of {b['trials']}")
    body.append(f'<text x="40" y="50" font-size="27" font-weight="700" fill="{TEXT}">Same question. Same model. Same harness.</text>')
    body.append(f'<text x="40" y="78" font-size="14.5" fill="{MUTED}">claude-sonnet-5 via GitHub Copilot CLI · '
                f'{esc(data["repo"])} · real transcripts replayed at recorded speed ({note})</text>')
    q = re.sub(r"\s*Answer in one line\.?$", "", data["prompt"]).replace(
        "'Sources/ArgumentParser/Parsable Types/ParsableCommand.swift'", "ParsableCommand.swift")
    body.append(f'<text x="40" y="112" font-size="15.5" font-style="italic" fill="{TEXT}">“{esc(clip(q, 175))}”</text>')
    PW, PY, PH, LH = 585, 134, 450, 20.5
    for cond, x, title, sub in (("baseline", 40, "built-in tools", "view · grep · glob · bash"),
                                ("speedread", 655, "speedread", "read · search · trace · map")):
        side = sides[cond]
        body.append(f'<rect x="{x}" y="{PY}" width="{PW}" height="{PH}" rx="12" fill="{CARD}" stroke="{BORDER}"/>')
        for i, c in enumerate(("#ff5f57", "#febc2e", "#28c840")):
            body.append(f'<circle cx="{x + 22 + i * 18}" cy="{PY + 20}" r="5.5" fill="{c}" opacity=".85"/>')
        body.append(f'<text x="{x + 86}" y="{PY + 25}" font-size="15" font-weight="700" fill="{B if cond == "speedread" else MUTED}">'
                    f'{title} <tspan font-size="13" font-weight="400" fill="{DIM}">  {sub}</tspan></text>')
        body.append(f'<line x1="{x}" y1="{PY + 40}" x2="{x + PW}" y2="{PY + 40}" stroke="{BORDER}"/>')
        lines = []
        for ev in side["events"]:
            if ev["kind"] == "model":
                lines.append((ev["t"], "model", f'● model call {ev["k"]}', f'{ev["input"]:,} tokens in'))
            elif ev["kind"] == "tool":
                name, arg = tool_line(ev)
                lines.append((ev["t"], "tool", name, clip(arg, 62)))
            elif ev["kind"] == "result":
                rl = ev["text"].strip("\n").splitlines()
                names_only = bool(rl) and all(":" not in l and "/" in l for l in rl)
                keep = 5 if cond == "speedread" else 3
                for j, l in enumerate(rl[:keep]):
                    lines.append((ev["t"], "dim" if names_only else "result", ("→ " if j == 0 else "  ") + clip(l, 70), ""))
                if names_only:
                    lines.append((ev["t"], "hint", "  a file name: no lines, no line numbers", ""))
                if len(rl) > keep:
                    lines.append((ev["t"], "dim", f"  … {len(rl) - keep} more lines", ""))
            elif ev["kind"] == "answer":
                lines.append((ev["t"], "answer", "✓ " + clip(ev["text"].replace("`", ""), 60), ""))
        y = PY + 66
        for t, kind, a, bb in lines:
            name = uid()
            styles.append(keyframes(name, t, END, T))
            if kind == "model":
                body.append(f'<g class="{name}"><text x="{x + 20}" y="{y}" font-family="{MONO}" font-size="13.5" '
                            f'font-weight="700" fill="{A}">{esc(a)}</text><text x="{x + PW - 20}" y="{y}" text-anchor="end" '
                            f'font-family="{MONO}" font-size="13" fill="{MUTED}">{esc(bb)}</text></g>')
            elif kind == "tool":
                body.append(f'<text class="{name}" x="{x + 20}" y="{y}" font-family="{MONO}" font-size="13.5" fill="{TEXT}">'
                            f'<tspan font-weight="700" fill="{B if cond == "speedread" else WARN}">{esc(a)}</tspan> {esc(bb)}</text>')
            elif kind == "answer":
                body.append(f'<text class="{name}" x="{x + 20}" y="{y}" font-family="{MONO}" font-size="13.5" '
                            f'font-weight="700" fill="{GOOD}">{esc(a)}</text>')
            else:
                fill = {"result": "#c9cfdd", "dim": DIM, "hint": BAD}[kind]
                it = ' font-style="italic"' if kind == "hint" else ""
                body.append(f'<text class="{name}" x="{x + 20}" y="{y}" font-family="{MONO}" font-size="12.5" '
                            f'fill="{fill}"{it}>{esc(a)}</text>')
            y += LH + (3 if kind == "model" else 0)
        # Progress bar (scaled to the slower side) and live counters.
        by, dur = PY + PH - 46, done[cond]
        name = uid()
        p = lambda t: min(100.0, t / T * 100)
        styles.append(f"@keyframes {name}{{0%{{transform:scaleX(0)}}{p(dur):.2f}%{{transform:scaleX(1)}}{p(END):.2f}%"
                      f"{{transform:scaleX(1)}}{p(END + 0.3):.2f}%{{transform:scaleX(0)}}100%{{transform:scaleX(0)}}}}"
                      f".{name}{{transform-box:fill-box;transform-origin:left;animation:{name} {T}s linear infinite}}")
        body.append(f'<rect x="{x + 20}" y="{by}" width="{PW - 40}" height="7" rx="3.5" fill="#1a2445"/>')
        body.append(f'<rect class="{name}" x="{x + 20}" y="{by}" width="{(PW - 40) * dur / max(done.values()):.1f}" '
                    f'height="7" rx="3.5" fill="{"url(#g)" if cond == "speedread" else BASE}"/>')
        models = [e for e in side["events"] if e["kind"] == "model"]
        tool_ts = [e["t"] for e in side["events"] if e["kind"] == "tool"]
        states = []
        for i, mdl in enumerate(models):
            t_next = models[i + 1]["t"] if i + 1 < len(models) else dur
            cum_in = sum(x["input"] for x in models[: i + 1])
            cum_aiu = sum(x["aiu"] for x in models[: i + 1])
            n_tools = sum(1 for t in tool_ts if t <= mdl["t"])
            states.append((mdl["t"], t_next - 0.2, f"{i + 1} model call{'s' if i else ''} · {n_tools} tool call"
                                                   f"{'' if n_tools == 1 else 's'} · {cum_in:,} tokens · {cum_aiu:.2f} AIU", False))
        states.append((dur, END, f"done in {dur:.1f} s · {len(models)} model calls · {len(tool_ts)} tool call"
                                 f"{'' if len(tool_ts) == 1 else 's'} · {side['input']:,} tokens · {side['cost']:.2f} AIU", True))
        for t_on, t_off, label, final in states:
            name = uid()
            styles.append(keyframes(name, t_on, t_off, T, fade=0.2))
            color = (GOOD if cond == "speedread" else TEXT) if final else MUTED
            body.append(f'<text class="{name}" x="{x + 20}" y="{by + 30}" font-size="14" fill="{color}" '
                        f'font-weight="{700 if final else 400}">{esc(label)}</text>')
    name = uid()
    styles.append(keyframes(name, max(done.values()) + 0.5, END, T, fade=0.35))
    yb = PY + PH + 18
    body.append(f'<g class="{name}"><rect x="40" y="{yb}" width="{W - 80}" height="58" rx="12" fill="url(#g)" opacity=".16"/>'
                f'<rect x="40" y="{yb}" width="{W - 80}" height="58" rx="12" fill="none" stroke="url(#g)"/>'
                f'<text x="{W / 2}" y="{yb + 36}" text-anchor="middle" font-size="19" font-weight="700" fill="{TEXT}">'
                f'speedread: {s["calls"]} model calls instead of {b["calls"]} · {signed(change(s["input"], b["input"]))} input tokens · '
                f'{signed(change(s["cost"], b["cost"]))} cost · {b["api_s"] / s["api_s"]:.1f}× less model time</text></g>')
    return svg_open(W, H, "".join(styles)) + "\n".join(body) + "\n</svg>\n"


def bar_chart(title, subtitle, rows, foot, label_w=380, W=1280, legend=("built-in tools", "speedread"), absolute=None):
    """rows: (label, base_value, sr_value, base_text, sr_text, delta_text, good: True | False | None).
    Bars are scaled per row, or against `absolute` (e.g. 1.0 for rates)."""
    RH, top = 62, 124
    H = top + RH * len(rows) + (60 if foot else 28)
    x0 = 40 + label_w
    bw = W - x0 - 350
    style = ("@keyframes grow{from{transform:scaleX(0)}to{transform:scaleX(1)}}"
             ".bar{transform-box:fill-box;transform-origin:left;animation:grow 1.1s cubic-bezier(.2,.8,.2,1) both}")
    out = [svg_open(W, H, style),
           f'<text x="40" y="50" font-size="25" font-weight="700" fill="{TEXT}">{esc(title)}</text>',
           f'<text x="40" y="80" font-size="14.5" fill="{MUTED}">{esc(subtitle)}</text>',
           f'<rect x="{x0}" y="98" width="11" height="11" rx="2" fill="{BASE}"/>'
           f'<text x="{x0 + 17}" y="108" font-size="13" fill="{MUTED}">{esc(legend[0])}</text>'
           f'<rect x="{x0 + 210}" y="98" width="11" height="11" rx="2" fill="url(#g)"/>'
           f'<text x="{x0 + 227}" y="108" font-size="13" fill="{MUTED}">{esc(legend[1])}</text>']
    for i, (label, bv, sv, bt, stt, dt, good) in enumerate(rows):
        y = top + i * RH
        m = absolute or max(bv, sv) or 1
        wb, ws = max(2, bw * bv / m), max(2, bw * sv / m)
        d = i * 0.07
        out.append(f'<text x="40" y="{y + 32}" font-size="16" fill="{TEXT}">{esc(label)}</text>')
        out.append(f'<rect class="bar" style="animation-delay:{d:.2f}s" x="{x0}" y="{y + 8}" width="{wb:.1f}" height="18" rx="4" fill="{BASE}"/>')
        out.append(f'<text x="{x0 + wb + 8}" y="{y + 22}" font-size="13" fill="{MUTED}">{esc(bt)}</text>')
        out.append(f'<rect class="bar" style="animation-delay:{d + 0.15:.2f}s" x="{x0}" y="{y + 31}" width="{ws:.1f}" height="18" rx="4" fill="url(#g)"/>')
        out.append(f'<text x="{x0 + ws + 8}" y="{y + 45}" font-size="13" fill="{TEXT}">{esc(stt)}</text>')
        color = GOOD if good else (WARN if good is None else BAD)
        size = 22 if len(dt) <= 6 else 18
        out.append(f'<text x="{W - 40}" y="{y + 37}" text-anchor="end" font-size="{size}" font-weight="700" fill="{color}">{esc(dt)}</text>')
    if foot:
        out.append(f'<text x="40" y="{H - 26}" font-size="13" fill="{DIM}">{esc(foot)}</text>')
    return "\n".join(out) + "\n</svg>\n"


def agent_chart(m):
    b, s = m["baseline"], m["speedread"]
    d = lambda k: signed(change(s[k], b[k]))
    rows = [("Input tokens per task", b["input"], s["input"], f"{b['input']:,.0f}", f"{s['input']:,.0f}", d("input"), True),
            ("Model time per task (median)", b["api_med"], s["api_med"], f"{b['api_med']:.1f} s", f"{s['api_med']:.1f} s", d("api_med"), True),
            ("Model calls (round trips)", b["calls"], s["calls"], f"{b['calls']:.2f}", f"{s['calls']:.2f}", d("calls"), True),
            ("Tool calls", b["tools"], s["tools"], f"{b['tools']:.2f}", f"{s['tools']:.2f}", d("tools"), True),
            ("Cost per task (AI units)", b["cost"], s["cost"], f"{b['cost']:.2f}", f"{s['cost']:.2f}", d("cost"), True),
            ("Correct on every trial (pass^3)", b["pass_k"], s["pass_k"], f"{b['pass_k']:.0%}", f"{s['pass_k']:.0%}", f"{s['pass_k']:.0%}", True)]
    return bar_chart("Real agent, same model: speedread vs built-in tools",
                     f"10 code questions × 3 trials per condition · claude-sonnet-5 via GitHub Copilot CLI · exact tokens "
                     f"from the harness's usage log · pass@1 {b['pass']:.0%} → {s['pass']:.0%}",
                     rows, f"Merely installed next to the built-in tools, speedread was used in {m['dropin']['adoption']:.0%} "
                           f"of {m['dropin']['n']} trials. Adoption is part of the product: make it the reader (see README).")


def tools_chart(agg, tot):
    rows = []
    for label, bt, stt, bc, sc, _, _ in agg:
        d = change(stt, bt)
        good = d < 0
        rows.append((label, bt, stt, f"{bt:,} tokens · {bc} calls", f"{stt:,} tokens · {sc} call{'s' if sc != 1 else ''}",
                     signed(d) if good else f"{signed(change(sc, bc))} calls", True if good else None))
    return bar_chart(f"Tool-level scenarios: {tot['base']:,} → {tot['sr']:,} tokens ({signed(change(tot['sr'], tot['base']))})",
                     f"{tot['n']} scenarios on 6 real repos, each graded for information sufficiency ({tot['pass']}/{tot['n']} "
                     f"pass) · {tot['base_calls']} → {tot['sr_calls']} tool calls · o200k tokens the model sees",
                     rows, "Orient costs more tokens on purpose: one budgeted map replaces 5–9 ls calls. "
                           "Small files: identical content; only the header differs.", label_w=450)


def budget_chart(after, pooled, calib):
    rows = []
    names = {"Synthetic: unicode_math": "Unicode math symbols", "Synthetic: hexdump": "Hex dump", "Synthetic: base64": "Base64",
             "Synthetic: numbers": "Numeric CSV", "Synthetic: emoji": "Emoji-heavy text",
             "CJK (real: docs, locales, i18n)": "CJK docs & locales"}
    for cat, (over, mx) in BUDGET_BEFORE.items():
        a_over, a_max = after.get(cat, (0.0, 0.0))
        rows.append((names.get(cat, cat), over, a_over, f"{over:.0%} over · worst {mx:.2f}×",
                     f"{a_over:.0%} over · worst {a_max:.2f}×", f"{a_over:.0%}", True))
    rows.append((f"All {pooled['n']} reads", BUDGET_BEFORE_POOLED["over"], pooled["over"],
                 f"{BUDGET_BEFORE_POOLED['over']:.1%} over · worst {BUDGET_BEFORE_POOLED['max']:.2f}×",
                 f"{pooled['over']:.1%} over · worst {pooled['max']:.2f}×", f"{pooled['over']:.0%}", True))
    foot = "Offline tokenizers. Real claude-sonnet-5 counts run higher"
    if calib:
        r = calib["robust"]["all"]
        foot += f" ({r['p50']:.2f}× the estimate at the median); speedread's Claude profile scales budgets 1.4×."
    return bar_chart("Budget contract on hostile content: reads over budget",
                     "read responses at budgets of 1k, 4k and 8k · o200k, cl100k and legacy Claude tokenizers",
                     rows, foot, label_w=300, legend=("fixed 2.6 bytes/token", "content-aware estimator"), absolute=1.0)


COND_STYLE = {"baseline": (BASE, "built-in tools"), "available": ("#8b8fe8", "speedread available"),
              "preferred": (A, "speedread preferred"), "exclusive": ("url(#g)", "speedread exclusive")}


def coding_chart(coding):
    """Grouped bars: one per condition, per metric (scaled per row)."""
    C = coding["conditions"]
    conds = [c for c in ("baseline", "available", "preferred", "exclusive") if c in C]
    if len(conds) < 2:
        return None
    b = C["baseline"]
    rows = [("Pass rate (pass@1)", "pass@1", lambda v: f"{v:.0%}", False),
            ("Input tokens per task", "mean_input_tokens", lambda v: f"{v:,.0f}", True),
            ("Model time per task (median)", "median_api_ms", lambda v: f"{v / 1000:.1f} s", True),
            ("Model calls (round trips)", "mean_model_calls", lambda v: f"{v:.1f}", True),
            ("Cost per task (AI units)", "mean_cost_aiu", lambda v: f"{v:.2f}", True),
            ("Reads done with speedread", "speedread_share", lambda v: f"{v:.0%}", False)]
    W, top, BH, GAP = 1280, 128, 15, 5
    RH = len(conds) * (BH + GAP) + 26
    H = top + RH * len(rows) + 64
    x0, bw = 420, 1280 - 420 - 330
    style = ("@keyframes grow{from{transform:scaleX(0)}to{transform:scaleX(1)}}"
             ".bar{transform-box:fill-box;transform-origin:left;animation:grow 1.1s cubic-bezier(.2,.8,.2,1) both}")
    out = [svg_open(W, H, style),
           f'<text x="40" y="50" font-size="25" font-weight="700" fill="{TEXT}">Real bug fixes, same model: four ways to give an agent speedread</text>',
           f'<text x="40" y="80" font-size="14.5" fill="{MUTED}">{coding["meta"]["tasks"]} injected regressions in gin (Go) and flask (Python) × '
           f'{coding["meta"]["trials"]} trials per condition · pass = full test suite, tests untouched · claude-sonnet-5 via GitHub Copilot CLI</text>']
    lx = 40
    for c in conds:
        color, label = COND_STYLE[c]
        out.append(f'<rect x="{lx}" y="98" width="11" height="11" rx="2" fill="{color}"/>'
                   f'<text x="{lx + 17}" y="108" font-size="13" fill="{MUTED}">{esc(label)}</text>')
        lx += 30 + len(label) * 7.4
    for i, (label, key, fmt, lower_better) in enumerate(rows):
        y = top + i * RH
        vals = [C[c].get(key) or 0 for c in conds]
        m = max(vals) or 1
        out.append(f'<text x="40" y="{y + RH / 2}" font-size="16" fill="{TEXT}">{esc(label)}</text>')
        for j, (c, v) in enumerate(zip(conds, vals)):
            yy = y + j * (BH + GAP)
            w = max(2, bw * v / m)
            color = COND_STYLE[c][0]
            out.append(f'<rect class="bar" style="animation-delay:{i * 0.06 + j * 0.05:.2f}s" x="{x0}" y="{yy}" '
                       f'width="{w:.1f}" height="{BH}" rx="4" fill="{color}"/>')
            txt = fmt(v)
            if lower_better and c != "baseline" and b.get(key):
                d = change(v, b[key])
                txt += f"  ({signed(d)})"
            out.append(f'<text x="{x0 + w + 8}" y="{yy + BH - 3}" font-size="12.5" '
                       f'fill="{TEXT if c != "baseline" else MUTED}">{esc(txt)}</text>')
    used = [c for c in conds if c != "baseline" and C[c].get("adoption")]
    n_used = sum(round(C[c]["adoption"] * C[c]["trials"]) for c in used)
    hidden = sum(C[c]["decisive_collapsed_first"] for c in used)
    foot = (f"The buggy line was first hidden inside a skeleton or outline in {hidden} of the {n_used} trials that used speedread. "
            f"Unused (available), it was chosen in {C['available']['adoption']:.0%} of trials." if "available" in C else "")
    out.append(f'<text x="40" y="{H - 30}" font-size="13" fill="{DIM}">{esc(foot)}</text>')
    return "\n".join(out) + "\n</svg>\n"


def relation_chart(rel):
    b, s = rel["tot"]["baseline"], rel["tot"]["speedread"]
    rows = []
    for r in rel["rows"]:
        rows.append((r["category"][0].upper() + r["category"][1:], r["b_in"], r["s_in"],
                     f"{r['b_in']:,.0f} tokens · {r['b_tools']:.1f} tool calls",
                     f"{r['s_in']:,.0f} tokens · {r['s_tools']:.1f} tool calls",
                     signed(change(r["s_in"], r["b_in"])), r["s_in"] < r["b_in"]))
    rows.append(("All relationship questions", b["input"], s["input"], f"{b['input']:,.0f} tokens · {b['tools']:.1f} tool calls",
                 f"{s['input']:,.0f} tokens · {s['tools']:.1f} tool calls", signed(change(s["input"], b["input"])), True))
    return bar_chart("Relationship questions: where trace earns its keep",
                     f"callers two hops out, resolved callees, interface and trait implementations · {b['n']} trials per condition · "
                     f"both pass {b['pass']:.0%} · claude-sonnet-5 via GitHub Copilot CLI",
                     rows, f"Unprompted, the agent chose trace in {s['trace']} of {s['n']} speedread trials. "
                           "Two of the four are answerable with one good grep (the controls); there the gap is small.",
                     label_w=430)


# ---------------------------------------------------------------- HTML

def data_uri(p, mime):
    return f"data:{mime};base64," + base64.b64encode(p.read_bytes()).decode()


def html_page(race, race_svg_text, charts, summary, m, tot, pooled, calib, coding, rel):
    b, s = m["baseline"], m["speedread"]
    logo = data_uri(ASSETS / "logo-256.png", "image/png") if (ASSETS / "logo-256.png").exists() else ""
    task_rows = "".join(
        f"<tr><td><code>{esc(tid)}</code></td><td>{esc(row['category'])}</td><td>{row['baseline']['input_tokens']:,.0f}</td>"
        f"<td>{row['speedread']['input_tokens']:,.0f}</td><td class=g>{signed(change(row['speedread']['input_tokens'], row['baseline']['input_tokens']))}</td>"
        f"<td>{row['baseline']['cost_aiu']:.2f}</td><td>{row['speedread']['cost_aiu']:.2f}</td>"
        f"<td>{row['baseline']['model_calls']:.1f} → {row['speedread']['model_calls']:.1f}</td>"
        f"<td>{row['baseline']['pass']:.0%} → {row['speedread']['pass']:.0%}</td></tr>"
        for tid, row in summary["tasks"].items())
    panes = []
    for cond in ("baseline", "speedread"):
        parts = []
        for ev in race["sides"][cond]["events"]:
            if ev["kind"] == "model":
                parts.append(f'<div class="turn">model call {ev["k"]} · {ev["input"]:,} tokens in</div>')
            elif ev["kind"] == "tool":
                name, arg = tool_line(ev)
                parts.append(f'<div class="call"><b>{esc(name)}</b> {esc(arg)}</div>')
            elif ev["kind"] == "result":
                parts.append(f"<pre>{esc(ev['text'].strip())}</pre>")
            else:
                parts.append(f'<div class="ans">✓ {esc(ev["text"])}</div>')
        panes.append("".join(parts))
    calib_html = ""
    if calib:
        r, rb = calib["ratios"], calib["robust"]
        calib_html = (
            f"<h3>Calibrated against a production tokenizer</h3><p>Offline tokenizers are proxies. The harness logs exact input "
            f"tokens per model call, so the tokens each tool result added can be recovered from consecutive calls "
            f"({calib['gaps']} read-tool results; Theil–Sen fit, framing overhead ≈{rb['all']['overhead']:.0f} tokens). On "
            f"<b>claude-sonnet-5</b>, real counts are <b>{rb['all']['p50']:.2f}×</b> speedread's estimate at the median "
            f"(p90 {rb['all']['p90']:.2f}×; {rb['speedread output']['p50']:.2f}× for speedread's own output), "
            f"{r['legacy']['p50']:.2f}× the legacy Claude tokenizer and {r['o200k']['p50']:.2f}× o200k — consistent with "
            f"Anthropic's note that Claude 4.7+ tokenizers produce ~30% more tokens. The content-aware estimator fixes "
            f"relative density; the <b>Claude profile</b> (automatic for Claude clients, or <code>SPEEDREAD_TOKENIZER=claude</code>) "
            f"scales budgets 1.4× so a response lands at ≈{rb['all']['p50'] / 1.4:.0%} of its budget at the median.</p>")
    coding_html = "<p>Pending.</p>"
    if coding:
        C = coding["conditions"]
        cells = "".join(
            f"<tr><td>{esc(c)}</td><td>{v['trials']}</td><td>{v['pass@1']:.0%}</td><td>{v['mean_input_tokens']:,.0f}</td>"
            f"<td>{v['mean_model_calls']:.1f}</td><td>{v['mean_tool_calls']:.1f}</td><td>{v['mean_read_result_tokens']:,.0f}</td>"
            f"<td>{v['mean_cost_aiu']:.2f}</td></tr>" for c, v in C.items())
        missing = [c for c in ("available", "preferred", "exclusive") if c not in C]
        bl = C.get("baseline")
        insight = ""
        if bl:
            insight = (f"<p>The baseline already shows where agent cost lives: read and search results averaged "
                       f"<b>{bl['mean_read_result_tokens']:,.0f} tokens</b> per task, about "
                       f"{bl['mean_read_result_tokens'] / bl['mean_input_tokens']:.0%} of {bl['mean_input_tokens']:,.0f} input tokens. "
                       f"The rest is the conversation being re-sent on each of {bl['mean_model_calls']:.1f} round trips, "
                       f"so a better reader can only save tokens by saving turns. With guidance, speedread took fewer turns on "
                       f"most tasks and less model time, and tokens stayed within noise. Merely installed, it was never used, "
                       f"and it made every task more expensive.</p>")
        safety = ""
        sr_conds = [c for c in ("available", "preferred", "exclusive") if c in C and C[c].get("adoption")]
        if sr_conds:
            parts = [f"{c}: {C[c]['decisive_collapsed_first']} of {round(C[c]['adoption'] * C[c]['trials'])} trials "
                     f"(never expanded {C[c]['collapsed_never_expanded']}, failed {C[c]['collapsed_then_failed']})" for c in sr_conds]
            safety = ("<p><b>Did compression hide the bug?</b> Among trials that used speedread, those where the file with the bug "
                      "was first shown as a skeleton or outline, before the buggy line itself: " + "; ".join(parts) +
                      ". Agents searched first, then read exact ranges.</p>")
        cc = ""
        ex = C.get("exclusive")
        if ex and ex.get("mean_cc_adjusted_input") is not None:
            cells_cc = "; ".join(f"{c} {C[c]['mean_cc_adjusted_input']:,.0f} ({signed(change(C[c]['mean_cc_adjusted_input'], bl['mean_cc_adjusted_input']))})"
                                 for c in ("preferred", "exclusive") if c in C)
            cc = (f"<p><b>Claude Code caveat, quantified.</b> Claude Code requires a native Read of a file before editing it, and MCP "
                  f"reads don't count. Adding a full Read of every edited file the agent hadn't viewed natively, re-sent on every later "
                  f"call (an upper bound), gives input tokens per task of: baseline {bl['mean_cc_adjusted_input']:,.0f}; {cells_cc}. "
                  f"On edit-heavy work in Claude Code, expect speedread to save turns and time rather than tokens.</p>")
        coding_html = (charts.get("coding", "") + f"<table><tr><th>Condition</th><th>Trials</th><th>Pass</th><th>Input tokens</th><th>Model calls</th>"
                       f"<th>Tool calls</th><th>Read-result tokens</th><th>AIU</th></tr>{cells}</table>{insight}{safety}{cc}"
                       + (f"<p class=muted>Not run yet: {', '.join(missing)}. Command in evals/RESULTS.md.</p>" if missing else ""))
    hero = [(signed(change(s['input'], b['input'])), "input tokens · code questions", f"real agent, {s['n']} trials per arm"),
            (signed(change(s['api_med'], b['api_med'])), "model time · code questions", "median, same model and harness")]
    if rel:
        rb_, rs_ = rel["tot"]["baseline"], rel["tot"]["speedread"]
        hero.append((signed(change(rs_['input'], rb_['input'])), "input tokens · relationship questions",
                     f"trace chosen in {rs_['trace']} of {rs_['n']} trials"))
    if coding and "exclusive" in coding["conditions"]:
        C = coding["conditions"]
        # Only trials that actually used speedread can have been misled by it.
        n_sr = sum(round(C[c]["adoption"] * C[c]["trials"]) for c in ("available", "preferred", "exclusive") if c in C)
        hidden = sum(C[c]["decisive_collapsed_first"] for c in ("available", "preferred", "exclusive") if c in C)
        allp = sum(round(v["pass@1"] * v["trials"]) for v in C.values())
        hero.append((f"{allp}/{sum(v['trials'] for v in C.values())}", "bug fixes pass the full test suite",
                     f"compression hid the bug in {hidden} of {n_sr}"))
        hero.append((f"{C['available']['adoption']:.0%}", "adoption when merely installed",
                     f"{C['preferred']['adoption']:.0%} with one sentence of guidance"))
    hero.append((f"{pooled['over']:.0%}", "reads over budget on hostile content", f"was 9.5% · {pooled['n']} reads"))
    hero_html = "".join(f'<div class="stat"><div class="big">{esc(v)}</div><div>{esc(l)}</div><div class="muted">{esc(n)}</div></div>'
                        for v, l, n in hero)
    cites = "".join(f'<li><a href="{esc(u)}">{esc(t)}</a></li>' for t, u in CITATIONS)
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>speedread: reading code for agents</title>
<style>
*{{box-sizing:border-box}}body{{margin:0;background:{BG};color:{TEXT};font:16px/1.6 {SANS}}}
main{{max-width:1320px;margin:0 auto;padding:40px 24px 80px}}a{{color:{B}}}
header{{display:flex;gap:28px;align-items:center;margin:10px 0 30px}}header img{{width:116px;height:116px;border-radius:26px}}
h1{{font-size:50px;line-height:1.06;margin:0;letter-spacing:-.02em}}.grad{{background:linear-gradient(90deg,{A},{B});-webkit-background-clip:text;background-clip:text;color:transparent}}
h2{{font-size:30px;margin:64px 0 10px;letter-spacing:-.01em}}h3{{font-size:21px;margin:30px 0 6px}}.lead{{font-size:20px;color:{MUTED};margin:8px 0 0;max-width:920px}}
.stats{{display:grid;grid-template-columns:repeat(auto-fit,minmax(195px,1fr));gap:14px;margin:26px 0}}
.stat{{background:{CARD};border:1px solid {BORDER};border-radius:16px;padding:18px 20px}}
.big{{font-size:44px;font-weight:800;line-height:1.1;background:linear-gradient(90deg,{A},{B});-webkit-background-clip:text;background-clip:text;color:transparent}}
.muted{{color:{MUTED};font-size:14px}}svg{{max-width:100%;height:auto;display:block;margin:14px 0}}
table{{border-collapse:collapse;width:100%;background:{CARD};border-radius:12px;overflow:hidden;font-size:14.5px;margin:12px 0}}
th,td{{padding:9px 12px;border-bottom:1px solid {BORDER};text-align:right}}th:first-child,td:first-child,th:nth-child(2),td:nth-child(2){{text-align:left}}
th{{color:{MUTED};font-weight:600}}.g{{color:{GOOD};font-weight:700}}
.pair{{display:grid;grid-template-columns:1fr 1fr;gap:16px}}.pane{{background:{CARD};border:1px solid {BORDER};border-radius:14px;padding:14px 16px;overflow:auto}}
.pane h3{{margin:0 0 8px}}pre{{background:#0a0f20;border:1px solid {BORDER};border-radius:8px;padding:10px;font:12.5px/1.45 {MONO};white-space:pre-wrap;word-break:break-word;color:#c9cfdd}}
.call{{font:13.5px {MONO}}}.call b{{color:{WARN}}}.sr .call b{{color:{B}}}.turn{{font:12.5px {MONO};color:{A};margin-top:10px}}.ans{{color:{GOOD};font-weight:700;margin-top:8px}}
code{{font-family:{MONO};font-size:.92em}}.cols{{display:grid;grid-template-columns:repeat(auto-fit,minmax(270px,1fr));gap:14px}}
.cols div{{background:{CARD};border:1px solid {BORDER};border-radius:14px;padding:16px 18px}}
@media(max-width:800px){{.pair{{grid-template-columns:1fr}}h1{{font-size:36px}}header{{flex-direction:column;align-items:flex-start}}}}
</style></head><body><main>
<header>{f'<img src="{logo}" alt="speedread logo">' if logo else ''}<div><h1><span class="grad">speedread</span><br>Read is the wrong abstraction for coding agents.</h1>
<p class="lead">Agent file reading should be adaptive, stateful, symbol-aware and token-budgeted instead of byte-oriented. ripgrep returns matches; speedread returns the minimum useful unit of code.</p></div></header>
<div class="stats">{hero_html}</div>
<h2>Watch one real task</h2>
<p class="muted">Both sides are recorded eval transcripts, replayed at recorded speed. The built-in grep answers with a file name, so the agent has to ask again. speedread's search answers with the matching lines under their enclosing declaration, with line ranges.</p>
{race_svg_text}
<div class="pair"><div class="pane"><h3>built-in tools</h3>{panes[0]}</div><div class="pane sr"><h3>speedread</h3>{panes[1]}</div></div>
<h2>Real agent A/B</h2>{charts['agent']}
<table><tr><th>Task</th><th>Category</th><th>Tokens, built-in</th><th>Tokens, speedread</th><th>Δ</th><th>AIU, built-in</th><th>AIU, speedread</th><th>Model calls</th><th>Pass</th></tr>{task_rows}</table>
<p>Where the savings come from: every model call re-sends the system prompt, tool definitions and conversation (~21k tokens here), so an answer in one call instead of three saves two full round trips. Tool results were about the same size in both conditions.</p>
<h2>Tool-level scenarios</h2>{charts['tools']}
<h2>The budget contract</h2>{charts['budget']}{calib_html}
<h2>Relationship questions</h2>{charts.get('relations', '<p>Pending.</p>')}
<h2>Coding tasks (SWE-style)</h2><p>Injected regressions in gin (Go) and flask (Python); symptom-only bug reports; graded by each repository's full test suite with tests unmodified; every task verified to fail as injected and pass with the reference fix. Conditions: built-in tools · speedread available · speedread preferred · speedread exclusive.</p>{coding_html}
<h2>How it works</h2><div class="cols">
<div><b>map</b>: locate structure<br><span class="muted">budgeted, importance-weighted tree with line counts; symbols on request</span></div>
<div><b>search</b>: locate text<br><span class="muted">ripgrep engine; hits grouped under their enclosing function or class, with its line range</span></div>
<div><b>trace</b>: locate relationships<br><span class="muted">callers, callees, references, implementations; syntactic and receiver-aware</span></div>
<div><b>read</b>: exact evidence<br><span class="muted">batched targets and symbols; skeletons instead of blind cuts; <code>path@etag</code> symbol-aware diffs</span></div></div>
<h2>Method</h2><p>Suites follow Anthropic's <i>Demystifying evals for AI agents</i>: explicit tasks, repeated trials, deterministic outcome graders, pass@k and pass^k, balanced tasks (with controls where speedread should not win), isolated trials, and transcripts read. Reading them found a wrong grader, an invalid task and the adoption problem. The tools follow <i>Code execution with MCP</i>: four tools (~1.4k tokens of definitions), filtering before results reach the model, and a CLI with JSON Lines for agents that script. Details: <a href="../evals/README.md">evals/README.md</a>.</p>
<h2>Research</h2><ul>{cites}</ul>
<p class="muted">Generated by demo/build.py from evals/results/. Apple M4, macOS 15.7.9.</p>
</main></body></html>
"""


def main():
    ASSETS.mkdir(parents=True, exist_ok=True)
    DEMO.mkdir(exist_ok=True)
    summary, m = agent_data()
    agg, tot = tool_data()
    after, pooled = budget_data()
    calib = optional_json(RES / "tokenizer_calibration.json")
    coding = optional_json(CODING / "summary.json")
    race = race_data()
    charts = {"agent": agent_chart(m), "tools": tools_chart(agg, tot), "budget": budget_chart(after, pooled, calib)}
    if coding and (cc := coding_chart(coding)):
        charts["coding"] = cc
    rel = relation_data()
    if rel:
        charts["relations"] = relation_chart(rel)
    race_text = race_svg(race)
    (ASSETS / "race.svg").write_text(race_text)
    for k, v in charts.items():
        (ASSETS / f"chart-{k}.svg").write_text(v)
    (DEMO / "index.html").write_text(html_page(race, race_text, charts, summary, m, tot, pooled, calib, coding, rel))
    if "coding" not in charts:
        (ASSETS / "chart-coding.svg").unlink(missing_ok=True)
    for p in sorted(ASSETS.glob("*.svg")) + [DEMO / "index.html", DEMO / "race-data.json"]:
        print(f"{p.relative_to(ROOT)}  {p.stat().st_size / 1024:.0f} KB")


if __name__ == "__main__":
    main()
