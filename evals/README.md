# How speedread is evaluated

speedread makes a strong claim — that coding agents should read code through a
budgeted, symbol-aware, stateful interface instead of byte-oriented `Read`/`Grep`.
A claim like that needs evidence at three levels: does each tool return the right
information (and less of everything else)? does the token budget hold on hostile
content? and do real agents, doing real work with the same model, get the same or
better results for less?

The suites below follow Anthropic's
[*Demystifying evals for AI agents*](https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents):
explicit **tasks**, repeated **trials**, deterministic **graders** on outcomes, full
**transcripts** kept and read, **pass@k** and **pass^k** reported, a **balanced** task
set (including cases where speedread should *not* win), **isolated** trials, and
graders fixed when transcript review showed them wrong. Results are in
[RESULTS.md](RESULTS.md); raw data (every trial, transcript and diff) is under
[`results/`](results/).

| Suite | Question | Type | Script |
|---|---|---|---|
| 1. Tool scenarios | Does each call return *enough* information, in fewer tokens and calls than the standard tools? | deterministic, regression | [`tool_eval.py`](tool_eval.py) |
| 2. Budget contract | Does a `budget` hold on dense and adversarial content, for OpenAI and Claude tokenizers? | deterministic, adversarial | [`budget_eval.py`](budget_eval.py), [`fit_estimator.py`](fit_estimator.py) |
| 2b. Tokenizer calibration | How far are offline estimates from a production tokenizer (claude-sonnet-5)? | measured from real sessions | [`tokenizer_calibration.py`](tokenizer_calibration.py) |
| 3. Agent Q&A | Same model + harness answering code questions: accuracy, tokens, cost, time | real agent, capability | [`agent_eval.py`](agent_eval.py), [`agent_tasks.json`](agent_tasks.json) |
| 3b. Agent relationships | Same, for callers, callees and implementations: does `trace` get used, and does it pay? | real agent, capability | [`agent_eval.py`](agent_eval.py), [`relationship_tasks.json`](relationship_tasks.json) |
| 4. Agent coding | Same model + harness fixing real bugs: tests pass? at what cost? is the compression ever unsafe? does the agent even use the tool? | real agent, capability | [`coding_eval.py`](coding_eval.py), [`coding_tasks.json`](coding_tasks.json) |

## Suite 1 — tool scenarios (regression)

Seven kinds of everyday reading work on six real repositories (ripgrep, zod,
vscode, flask, gin, Alamofire): orient in a repo, understand a large file, read one
function, re-read after an edit, find usages and read the callers, follow a growing
log, and — the balanced case — read small files, where speedread must return every
line unchanged and should cost about the same as a plain read.

- **speedread** runs as a real MCP server over stdio (one session per repo, so etag
  diffs behave as in an agent session), default budgets.
- **Baselines** emulate the de-facto agent tools: Claude Code's `Read` format
  (`%6d→` line numbers, 2,000 lines per call), ripgrep content output for `Grep`,
  one `ls` per directory. For "read one function" there is also a best-case
  baseline that greps for the name and reads exactly the function ±10 lines.
- **Graders check information sufficiency, not just size**: every top-level
  definition visible (large files), the whole function present (symbol reads), the
  edited line shown (re-reads), all new log lines and no old ones, every caller file
  complete and totals exact (usages), every line of a small file present.
  A smaller answer that drops what the task needs fails.

## Suite 2 — the budget contract (adversarial)

speedread sizes responses with an estimate, not the client's tokenizer. The suite
measures real tokens (o200k_base and cl100k_base exactly, plus the legacy Claude
tokenizer as a proxy — Anthropic has not published the tokenizer of current models)
against the estimate on 599 files in 20 content classes (594 real, 5 synthetic), including hostile ones:
SVG path data, JSON, lockfiles, minified JS, real CJK docs/locales, emoji, base64,
hex dumps, numeric tables. It then checks the end-to-end contract — tokens of actual
`read` responses at budgets of 1k, 4k and 8k — and reports p50/p99/max and the share
over budget.

This suite found a real defect: with a fixed 2.6 bytes/token, 5–9% of reads went
over budget (up to 1.8× on SVG, base64, JSON). The fix is a content-aware estimator
([`src/tokens.rs`](../src/tokens.rs)) fitted by `fit_estimator.py`; the per-category
tables before and after are in RESULTS.md.

## Suite 2b — calibration against a production tokenizer

Anthropic has not published the tokenizer of current Claude models, so offline counts use the legacy tokenizer as a proxy. The harness, however, logs exact input and output tokens for every model call. The tokens a tool result added are therefore `input[k+1] − input[k] − output[k]`. This script recovers those counts from the saved transcripts of Suites 3 and 4 (read-only tools only, since bash and edit output can reach the model in a different rendering). It then fits a robust Theil–Sen line whose intercept absorbs the per-result framing, and compares real counts with speedread's estimate and with offline tokenizers. It found that real claude-sonnet-5 counts run 1.22× the estimate (1.36× for speedread's own output) and 1.35× the legacy tokenizer. That finding produced the Claude profile.

## Suite 3 — agent Q&A (real agent)

Ten read-only questions with version-specific answers (e.g. *which line of
`Engine.run` returns…*) over six repositories and five languages, graded by
regex graders with partial credit. Conditions: `baseline` (built-in view/grep/glob/
bash), `speedread` (speedread as the reader: built-in view/grep/glob excluded, bash
kept) and `dropin` (speedread merely added — measures unprompted adoption).

## Suite 3b — agent relationship questions (real agent)

Four questions whose answers are relationships: two-hop callers, resolved callees, Go interface implementations (structural) and Rust trait implementations. The prompts never mention speedread or `trace`, so the suite also measures whether an agent picks the new primitive on its own. It is balanced: two questions are answerable with one good grep. Conditions `baseline` and `speedread` match Suite 3, plus `--no-subagents` in both.

## Suite 4 — agent coding (real agent, SWE-style)

Eight bug-fix tasks in two real codebases (gin/Go, flask/Python), easy/medium/hard
balanced: a one-line regression is injected, and the agent gets a symptom-only bug
report — no file names — plus the command that runs the tests.

- **Outcome grader:** the repository's own full test suite passes *and* no test
  file was modified (`git diff`). Every task is checked by `--verify` to fail as
  injected and to pass with the reference fix, so an unsolvable or mis-graded task
  cannot hide in the results.
- **Conditions** (same model, same harness, same prompt):
  `baseline` built-in tools only · `available` + speedread, no guidance ·
  `preferred` + speedread and one sentence asking to use it for reading ·
  `exclusive` + speedread with built-in view/grep/glob removed (edit and bash stay).
- **Isolation:** fresh copy of the repository per trial with the bug baked into a
  single-commit history (no `git log` hints), fresh session, custom instructions and
  built-in MCP servers disabled, sub-agents disabled in every condition (so every
  token is accounted for in one session), trial order shuffled so conditions
  interleave over time.
- **Metrics:** pass@1, pass^k; input/output tokens and cost from the harness's own
  per-call usage log (exact); model-API time and session time; turns (model calls);
  tool calls; read-result tokens; **repeated source bytes** (normalised lines shown
  to the model more than once in a session); **adoption** (trials that used
  speedread at all) and **share of reads** done with speedread.
- **Is compression safe?** For every trial we check whether the *decisive* (buggy)
  line was ever shown to the model, and whether a speedread skeleton first showed
  the file with that line collapsed — then whether the agent expanded it, and
  whether the trial failed. This is the false-negative cost of skeletonisation.
- **Claude Code caveat, quantified:** Claude Code requires its own `Read` of a file
  before `Edit`/`Write`, and MCP reads don't count. For each speedread trial we
  compute the tokens those mandatory reads would add (a numbered read of every
  edited file not already viewed natively, up to 2,000 lines), once and as re-sent
  on every later turn.

## What transcript review changed

Reading transcripts, not just scores, found three problems that scores alone hid:

1. A broken grader (Suite 3): `ripgrep-walk-run` expected line 1425; the correct
   line is 1428. Fixed and all trials re-graded (`agent_eval.py --regrade`), which
   changed 7 verdicts.
2. A broken task (Suite 4): `flask-blueprint-prefix` passed its tests *with* the bug
   injected (werkzeug merges repeated slashes), so its symptom was false and its
   grader could not tell a fix from no fix. `--verify` now gates every task: it must
   fail as injected and pass with the reference fix. The task was replaced and its
   two trials excluded, but kept for audit. Another injected bug made `go test` hang
   (an infinite loop in route insertion) and was replaced before any trial ran;
   gin's grader now has a 300 s test timeout.
3. Tool adoption: with speedread merely installed, the agent ignored it in 10 of 10
   Q&A trials and 16 of 16 coding trials, despite server instructions, and those
   runs cost *more* than baseline, because tool definitions are sent on every call.
   One sentence of guidance gave 16 of 16. So "available" is not "used", and the
   recommended setup makes speedread *the* reader (see README).
4. An ambiguous grader (Suite 3b): both baseline trials of the two-hop callers
   task "failed" by excluding `BasicAuth`, whose path to `AbortWithStatus` runs
   through a closure invoked as a value. That reading is defensible, so the grader
   accepts both answers (`--regrade`, 2 verdicts changed).
5. A harness bug after the switch to 64-bit etags (Suite 1 parsed 8 hex digits);
   fixed in the harness, not by loosening the grader.

## Noise and limits

- The machine was shared with unrelated heavy workloads during some runs (load
  averages 30–70). Token counts, tool calls and costs are unaffected (exact, from
  the harness's usage log); wall-clock is not, so medians and model-API time are
  reported and session time is labelled as noisy.
- One harness (GitHub Copilot CLI) and one model family per run; the model and
  harness versions are recorded with each run. Results for other harnesses (Claude
  Code, Cursor, Codex) may differ — especially where the harness forces native
  reads (the Claude Code caveat above).
- The coding tasks are injected regressions in well-known repositories: realistic
  in shape, smaller than SWE-bench issues, and the model may know the original
  code. They measure navigation and reading cost for a fixed outcome, not
  frontier bug-fixing ability.
- Sample sizes are small: 30, 8 and 16 trials per arm in Suites 3, 3b and 4. Every
  headline carries a 95% bootstrap interval ([`stats.py`](stats.py)). Suite 4's
  token and time differences are within noise; Suite 3's are not.
- Suite 3 ran with speedread's first three tools, before `trace`, symbol diffs and
  the content-aware estimator existed. Suites 3b and 4 ran with the final server.

## Reproduce

Before you start:

- **Benchmark repositories.** The suites expect flask, gin, ripgrep, zod, vscode, Alamofire and swift-argument-parser cloned side by side in one directory (`<bench>`). The commits the tasks were written against are not yet recorded in this repository. Graders check version-specific facts such as line numbers, so on other checkouts some tasks can fail in every condition. For the bug-fix suite, `coding_eval.py --verify` checks each task against your checkouts.
- **Agent suites** need [GitHub Copilot CLI](https://github.com/github/copilot-cli) and `speedread` on your `PATH` (`evals/speedread-mcp.json` runs `speedread mcp`). The bug-fix suite also needs Go for gin's tests, and for flask a virtual environment with its test dependencies at `<venvs>/flask` (each task's `venv` field names the directory).
- **Suite 2** needs a Python environment with `tiktoken`, plus, for the legacy Claude tokenizer, a Node script that counts tokens with `@anthropic-ai/tokenizer` (`--claude-counter`).

```bash
python3 evals/tool_eval.py  <bench> --speedread target/release/speedread --rg rg --json evals/results/tool_eval.json
python3 evals/budget_eval.py <bench> --speedread target/release/speedread --claude-counter tok/claude_count.js
python3 evals/agent_eval.py  --bench <bench> --trials 3 --conditions baseline,speedread --dropin-trials 1
python3 evals/agent_eval.py  --bench <bench> --tasks evals/relationship_tasks.json --trials 2 --no-subagents
python3 evals/coding_eval.py --bench <bench> --venvs <venvs> --verify
python3 evals/coding_eval.py --bench <bench> --venvs <venvs> --trials 2
python3 evals/stats.py evals/results/<run>      # bootstrap intervals, per-task wins
```

Agent suites need [GitHub Copilot CLI](https://github.com/github/copilot-cli)
(`copilot -p … --output-format json`); token usage is read from its local session
store. `evals/speedread-mcp.json` is the MCP config passed to it.
