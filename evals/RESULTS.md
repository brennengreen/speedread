# Results

Machine: Apple M4 (4 performance + 6 efficiency cores), 16 GB RAM, macOS 15.7.9, APFS internal SSD. Method and definitions: [README.md](README.md). Raw data: [`results/`](results/). All agent trials use claude-sonnet-5 via GitHub Copilot CLI 1.0.89.

| Suite | What it shows | Headline |
|---|---|---|
| 1. Tool scenarios | each call returns enough, in fewer tokens and calls | 35/35 graded pass · 104 calls → 35 calls · **−93% tokens** |
| 2. Budget contract | budgets hold on hostile content | reads over budget **9.5% → 0%** (525 reads, 3 tokenizers) |
| 2b. Tokenizer calibration | offline estimate vs real claude-sonnet-5 counts | real = **1.22×** estimate (median) → Claude profile ×1.4 |
| 3. Real agent, code questions | same model and harness, answers graded | **−35% input tokens, −47% model time, pass^3 90% → 100%** |
| 4. Real agent, coding tasks | same model and harness, graded by test suites | baseline arm done (16/16 pass); speedread arms pending |

## Suite 1 — tool scenarios

`tool_eval.py`, speedread as a real MCP server (one session per repository), default budgets; o200k tokens of what the model sees. Baselines: Claude Code-style `Read` (`%6d→`, 2,000-line pages), ripgrep content output for `Grep`, `ls` per directory, and for single functions a best-case grep + exact window. "Graded" = the information-sufficiency grader passed (percentages: share of callers shown in full within the budget, with exact totals and continuation targets for the rest).

Graded: 35/35 scenarios pass. Total: baseline 104 calls / 954,734 tokens; speedread 35 calls / 67,079 tokens (93% fewer tokens, 66% fewer calls). Tokens: o200k_base.

| Scenario | Graded | Baseline calls | Baseline tokens | speedread calls | speedread tokens | Tokens saved |
|---|:---:|---:|---:|---:|---:|---:|
| orient: flask (4 top-level dirs) | ✓ | 5 | 304 | 1 | 1,801 | -492% |
| orient: gin (8 top-level dirs) | ✓ | 9 | 437 | 1 | 920 | -111% |
| orient: ripgrep (8 top-level dirs) | ✓ | 9 | 232 | 1 | 2,060 | -788% |
| orient: zod (5 top-level dirs) | ✓ | 6 | 247 | 1 | 2,412 | -877% |
| orient: vscode (8 top-level dirs) | ✓ | 9 | 1,009 | 1 | 2,745 | -172% |
| large file: defs.rs (8,161 lines) | ✓ 99% | 5 | 105,147 | 1 | 6,179 | 94% |
| large file: types.ts (5,138 lines) | ✓ | 3 | 67,281 | 1 | 5,792 | 91% |
| large file: textModel.ts (2,745 lines) | ✓ | 2 | 39,756 | 1 | 4,959 | 88% |
| large file: app.py (1,628 lines) | ✓ | 1 | 21,264 | 1 | 3,367 | 84% |
| large file: context.go (1,544 lines) | ✓ | 1 | 19,362 | 1 | 2,801 | 86% |
| large file: Session.swift (1,441 lines) | ✓ | 1 | 22,184 | 1 | 2,046 | 91% |
| function Flask.url_for: read whole file | ✓ | 1 | 21,264 | 1 | 1,392 | 93% |
| function Flask.url_for: grep file + exact window (best case) | ✓ | 2 | 2,082 | 1 | 1,392 | 33% |
| function Context.AbortWithStatusJSON: read whole file | ✓ | 1 | 19,362 | 1 | 119 | 99% |
| function Context.AbortWithStatusJSON: grep file + exact window (best case) | ✓ | 2 | 388 | 1 | 119 | 69% |
| function WalkParallel.run: read whole file | ✓ | 2 | 34,449 | 1 | 146 | 100% |
| function WalkParallel.run: grep file + exact window (best case) | ✓ | 2 | 581 | 1 | 146 | 75% |
| function ZodString._parse: read whole file | ✓ | 3 | 67,281 | 1 | 2,690 | 96% |
| function ZodString._parse: grep file + exact window (best case) | ✓ | 2 | 4,863 | 1 | 2,690 | 45% |
| function TextModel.setValue: read whole file | ✓ | 2 | 39,756 | 1 | 136 | 100% |
| function TextModel.setValue: grep file + exact window (best case) | ✓ | 2 | 448 | 1 | 136 | 70% |
| re-read after edit: defs.rs | ✓ | 5 | 105,164 | 1 | 94 | 100% |
| re-read after edit: types.ts | ✓ | 3 | 67,296 | 1 | 111 | 100% |
| re-read after edit: textModel.ts | ✓ | 2 | 39,772 | 1 | 159 | 100% |
| re-read after edit: app.py | ✓ | 1 | 21,279 | 1 | 124 | 99% |
| usages of url_for (flask, 47 files) | ✓ 62% | 4 | 41,840 | 1 | 4,168 | 90% |
| usages of AbortWithStatus (gin, 8 files) | ✓ | 4 | 25,999 | 1 | 4,526 | 83% |
| usages of build_parallel (ripgrep, 3 files) | ✓ | 5 | 41,579 | 1 | 2,763 | 93% |
| usages of safeParse (zod, 188 files) | ✓ 8% | 4 | 134,994 | 1 | 6,065 | 96% |
| follow log: 30 lines appended to 5,000 | ✓ | 1 | 3,969 | 1 | 703 | 82% |
| small file: gin/go.mod (41 lines) | ✓ | 1 | 728 | 1 | 657 | 10% |
| small file: flask/src/flask/__init__.py (39 lines) | ✓ | 1 | 579 | 1 | 484 | 16% |
| small file: ripgrep/Cargo.toml (126 lines) | ✓ | 1 | 1,589 | 1 | 1,288 | 19% |
| small file: zod/package.json (97 lines) | ✓ | 1 | 1,514 | 1 | 1,243 | 18% |
| small file: Alamofire/Package.swift (52 lines) | ✓ | 1 | 735 | 1 | 646 | 12% |

`map` returns more tokens than shallow `ls` calls, but in one call and with line counts, nested structure and file-type summaries. Small files: the same content plus a header.

## Suite 2 — budget contract

`budget_eval.py`: 599 files in 20 content classes: 594 from the benchmark repositories in 15 classes, plus 5 synthetic hostile files, `read` at budgets of 1k, 4k and 8k tokens on the 12 largest files per class. "Estimate ratio" = real tokens / (bytes / 2.6); "reads" = real tokens of the response / budget.

Content-aware estimator (current):

| Class | Files | Bytes/token (o200k) | Estimate ratio o200k p50 / p99 | Estimate ratio Claude p50 / p99 | Reads over · worst (o200k) | Reads over · worst (Claude legacy) |
|---|---:|---:|---:|---:|---:|---:|
| Rust | 60 | 4.09 | 0.63 / 0.89 | 0.71 / 1.01 | 0% · 0.82× | 0% · 0.94× |
| Go | 60 | 3.77 | 0.68 / 1.00 | 0.86 / 1.10 | 0% · 0.74× | 0% · 0.83× |
| Python | 57 | 4.31 | 0.60 / 0.81 | 0.68 / 0.93 | 0% · 0.68× | 0% · 0.88× |
| TypeScript | 60 | 4.06 | 0.64 / 0.91 | 0.76 / 1.05 | 0% · 0.68× | 0% · 0.77× |
| JavaScript | 40 | 4.48 | 0.58 / 0.98 | 0.69 / 1.11 | 0% · 0.70× | 0% · 0.83× |
| Swift | 60 | 4.61 | 0.56 / 0.65 | 0.59 / 0.72 | 0% · 0.72× | 0% · 0.79× |
| Markdown | 50 | 4.37 | 0.59 / 1.21 | 0.64 / 1.60 | 0% · 0.77× | 0% · 0.95× |
| JSON | 50 | 3.05 | 0.85 / 1.23 | 0.91 / 1.29 | 0% · 0.85× | 0% · 0.99× |
| YAML | 40 | 4.04 | 0.64 / 0.81 | 0.63 / 0.90 | 0% · 0.77× | 0% · 0.88× |
| TOML | 21 | 3.31 | 0.79 / 0.93 | 0.92 / 1.06 | 0% · 0.68× | 0% · 0.84× |
| Lockfiles | 12 | 2.55 | 1.01 / 1.06 | 1.06 / 1.15 | 0% · 0.66× | 0% · 0.83× |
| Generated (protobuf, d.ts) | 30 | 5.21 | 0.49 / 0.63 | 0.56 / 0.65 | 0% · 0.68× | 0% · 0.81× |
| Minified JS | 2 | 2.87 | 0.79 / 0.91 | 1.05 / 1.16 | 0% · 0.71× | 0% · 0.91× |
| SVG | 40 | 1.67 | 1.55 / 1.99 | 1.34 / 1.55 | 0% · 0.87× | 0% · 0.94× |
| CJK (real: docs, locales, i18n) | 12 | 3.78 | 0.67 / 0.81 | 0.83 / 1.01 | 0% · 0.68× | 0% · 0.84× |
| Synthetic: emoji | 1 | 2.67 | 0.97 / 0.97 | 1.10 / 1.10 | 0% · 0.85× | 0% · 0.95× |
| Synthetic: base64 | 1 | 1.45 | 1.79 / 1.79 | 1.85 / 1.85 | 0% · 0.81× | 0% · 0.84× |
| Synthetic: hexdump | 1 | 1.48 | 1.76 / 1.76 | 1.30 / 1.30 | 0% · 0.88× | 0% · 0.65× |
| Synthetic: numbers | 1 | 2.11 | 1.24 / 1.24 | 1.31 / 1.31 | 0% · 0.85× | 0% · 0.90× |
| Synthetic: unicode_math | 1 | 1.99 | 1.31 / 1.31 | 1.55 / 1.55 | 0% · 0.72× | 0% · 0.85× |

Pooled reads (all 525): o200k p50 0.37, p99 0.85, worst 0.88×; cl100k worst 0.92×; legacy Claude p50 0.44, p99 0.95, worst 0.99× — **0 over budget** for every tokenizer.

With the previous fixed 2.6 bytes/token (same script, corpus and seed): 7.4% (o200k), 8.2% (cl100k) and 9.5% (legacy Claude) of the 525 reads were over budget, worst 1.81×. Over-budget classes: SVG (44%, worst 1.55×), JSON (39%, 1.41×), lockfiles (11%), Go (6%), CJK (3%), and 100% of emoji, base64 (1.74×), hex (1.73×), numeric CSV and Unicode-symbol reads.

`fit_estimator.py` fits the estimator ([`src/tokens.rs`](../src/tokens.rs)) on 1,842 line-numbered and raw windows: undercount rate **22.4% → 0.5%**, p99 ratio 1.80 → 1.00, worst 1.85 → 1.07, with the same average headroom (18.7% → 18.1% of the budget unused).

### Suite 2b — calibration against claude-sonnet-5

`tokenizer_calibration.py` recovers exact token counts of 162 read-tool results from the harness's per-call usage log (input[k+1] − input[k] − output[k]; bash and edit results are excluded because the model may see a different rendering than the transcript records). Robust (Theil–Sen) fits over single-result gaps:

| Content | Results | Real / estimate, median | p90 | Framing overhead |
|---|---:|---:|---:|---:|
| speedread output | 30 | 1.36× | 1.48× | 21 tokens |
| built-in view/grep/glob output | 129 | 1.21× | 1.40× | 27 tokens |
| all | 159 | 1.22× | 1.46× | 27 tokens |

Against offline tokenizers (medians of real / counted): legacy Claude 1.35×, o200k 1.54×. Real bytes/token for tool output on claude-sonnet-5: p50 2.63. So the budget contract above holds for offline tokenizers; for current Claude models speedread applies a 1.4× Claude profile (automatic for clients whose name contains "claude", or `SPEEDREAD_TOKENIZER=claude`), putting a median response at ≈87% of its budget. The end-to-end suites below ran with the default (legacy) profile.

## Suite 3 — real agent, code questions

`agent_eval.py` · `agent_tasks.json`: 10 read-only questions with version-specific answers (6 repositories, 5 languages), regex graders with partial credit, 3 trials per condition, trials shuffled across conditions. Conditions: `baseline` (built-in view/grep/glob/bash), `speedread` (speedread as the reader: built-in view/grep/glob excluded, bash kept), `dropin` (speedread only added). Run 2026-09-23 with speedread's first three tools (`read`, `search`, `map`; 32-bit etags; fixed 2.6 bytes/token) — `trace`, symbol diffs and the estimator came after.

10 tasks × 3 trials per condition, run 2026-09-23.

| Metric | baseline | speedread | dropin | speedread vs baseline |
|---|---:|---:|---:|---:|
| Pass rate (pass@1) | 97% | 100% | 90% | |
| Mean input tokens / task | 75,511 | 48,984 | 99,290 | 35% lower |
| Mean output tokens / task | 386 | 177 | 509 | 54% lower |
| Mean cost / task (AI units) | 3.90 | 3.07 | 5.17 | 21% lower |
| Mean session time (s) | 12.1 | 14.5 | 14.3 | 19% higher |
| Mean model calls | 3.37 | 2.17 | 4.10 | 36% lower |
| Mean tool calls | 2.37 | 1.20 | 3.20 | 49% lower |
| Tool-result tokens / task | 198 | 220 | 209 | 11% higher |
| Used speedread | 0% | 90% | 0% | |

Consistency: baseline pass@3 100%, pass^3 90%; speedread pass@3 100%, pass^3 100%; dropin pass@1 90%, pass^1 90%

### Per task (means)

| Task | Category | baseline cost | speedread cost | baseline s | speedread s | baseline pass | speedread pass |
|---|---|---:|---:|---:|---:|---:|---:|
| flask-shell-context | control: small symbol | 3.56 | 2.87 | 10.6 | 7.2 | 100% | 100% |
| gin-go-version | control: tiny file | 2.55 | 2.53 | 5.7 | 6.5 | 100% | 100% |
| ripgrep-fixed-strings | large file (8,161 lines) | 3.38 | 2.78 | 10.0 | 6.1 | 100% | 100% |
| ripgrep-walk-run | large file (2,740 lines) | 3.74 | 2.62 | 27.0 | 28.2 | 100% | 100% |
| zod-ip-kinds | large file (5,138 lines) | 4.90 | 3.07 | 14.1 | 43.7 | 67% | 100% |
| flask-dispatch-lines | large file, several symbols | 3.32 | 2.65 | 9.4 | 6.2 | 100% | 100% |
| gin-abort-callers | cross-file search | 2.78 | 2.71 | 6.4 | 6.4 | 100% | 100% |
| vscode-setvalue | huge repo (19k files) | 7.55 | 6.11 | 16.8 | 13.5 | 100% | 100% |
| alamofire-start-immediately | Swift, large file (1,441 lines) | 3.15 | 2.69 | 8.9 | 20.6 | 100% | 100% |
| sap-configuration | Swift, protocol requirement | 4.04 | 2.63 | 12.5 | 6.2 | 100% | 100% |

Notes:
- Tokens, cost and model (API) time come from the harness's own usage log and are exact. **Session time is noisy**: the machine ran release builds and other evals concurrently, and a few trials waited 30–120 s on CLI startup or tool execution, so the *mean* session time is higher for speedread (14.5 s vs 12.1 s) while the **median is 6.6 s vs 9.9 s** and model time is 3.5 s vs 6.7 s (median).
- Adoption as the reader: 27/30 trials. The 3 exceptions are `gin-go-version` (a 41-line `go.mod`), where the agent used `cat`.
- Transcript review found a wrong grader (`ripgrep-walk-run` expected line 1425; correct is 1428); all trials were re-graded (`--regrade`), changing 7 verdicts.

## Suite 4 — real agent, coding tasks

`coding_eval.py` · `coding_tasks.json`: 8 injected regressions (gin: 4, flask: 4; easy 3, medium 3, hard 2), symptom-only bug reports, pass = the full test suite passes with no test file modified. `--verify` confirms every task fails as injected and passes with the reference fix. Sub-agents and web tools are disabled in all conditions.

8 SWE-style bug-fix tasks × 2 trials per condition, run 2026-09-24. Pass = the repository's full test suite passes with tests unmodified.

| Metric | baseline |
|---|---:|
| Pass rate (pass@1) | 100% |
| Mean input tokens / task | 166,147 |
| Median input tokens / task | 124,460 |
| Mean output tokens / task | 1,714 |
| Mean cost / task (AI units) | 7.84 |
| Median model (API) time, s | 20.5 |
| Median session time, s | 33.8 |
| Mean model calls (turns) | 7.8 |
| Mean tool calls | 6.9 |
| Mean read/search calls | 4.5 |
| Read-result tokens / task | 1,604 |
| Source bytes shown / task | 5,150 |
| Repeated source bytes / task | 594 |
| Used speedread (adoption) | 0% |
| Share of reads via speedread | 0% |
| Decisive line shown | 100% |

Consistency: baseline pass@2 100%, pass^2 100%

### Per task (means)

| Task | Difficulty | baseline pass / tokens |
|---|---|---:|
| gin-client-ip | medium | 100% / 131,080 |
| gin-catchall-param | hard | 100% / 483,034 |
| gin-json-charset | easy | 100% / 109,792 |
| gin-is-aborted | easy | 100% / 84,010 |
| flask-prefixed-env | medium | 100% / 94,683 |
| flask-blueprint-url-prefix | medium | 100% / 137,438 |
| flask-response-tuple | hard | 100% / 144,914 |
| flask-json-decimal | easy | 100% / 144,224 |

- **Excluded:** `flask-blueprint-prefix` failed verification after its baseline trials ran — werkzeug merges repeated slashes, so the tests pass with the bug injected. It was replaced by `flask-blueprint-url-prefix`; the two invalid trials are kept in [`results/coding-claude-sonnet-5/excluded/`](results/coding-claude-sonnet-5/excluded/) and excluded from every number.
- **Pending: the speedread arms** (`available`, `preferred`, `exclusive`). They were not run in this session because executing the speedread binary was not permitted by the session's tool-approval policy at the time. To run them and merge with the baseline:

  ```sh
  python3 evals/coding_eval.py --bench <repos> --venvs <venvs> --conditions available,preferred,exclusive \
      --trials 2 --out evals/results/coding-claude-sonnet-5
  python3 evals/coding_eval.py --bench <repos> --venvs <venvs> --summarize evals/results/coding-claude-sonnet-5
  python3 demo/build.py
  ```

## Speed

`hyperfine 1.20 -N --warmup 3`, medians of 30 runs, each a fresh process (the MCP server keeps caches warm between calls, so real sessions are faster still).

| Task | speedread | ripgrep 15.2 (defaults) | ripgrep 15.2 `-j4` |
|---|---:|---:|---:|
| Walk vscode, 19,167 files (speedread also collects size + mtime) | **26.0 ms** | 28.7 ms (`--files --hidden`) | 30.3 ms |
| Walk cargo registry, 21,387 files | **24.5 ms** | 26.9 ms | — |
| Search vscode for `createDecorator`, list files | **103 ms** | 294 ms | 109 ms |
| Search vscode, matching lines (+ enclosing-symbol grouping) | 166 ms | 288 ms | **116 ms** |
| Search cargo registry for `impl Drop for`, list files | 144 ms | 364 ms | **139 ms** |
| Search cargo registry, matching lines (+ grouping) | **263 ms** | 377 ms | — |

| speedread operation | Time |
|---|---:|
| `read` a 742 KB, 21,239-line TypeScript `.d.ts` (parse + skeleton), cold process | 17 ms |
| `read` an 8,161-line Rust file (parse + ladder), cold process | 16 ms |
| `read #createDecorator` (definition lookup across 19k files) | 332 ms |
| `map` of vscode (19k files) | 84 ms |
| `read` a 148 MB, 1.6M-line log (index + head/tail) | 72 ms |
| `read log@etag` on that log after an append (prefix re-hash) | 53 ms |
| Process startup (`--version`) | 1.9 ms |

### Thread scaling on Apple Silicon (why speedread uses P-core count)

ripgrep searching vscode for a literal, by thread count:

| Threads | Wall time | Kernel time |
|---:|---:|---:|
| 2 | 169 ms | 279 ms |
| **4** (= P-cores) | **104 ms** | 339 ms |
| 6 | 155 ms | 808 ms |
| 8 | 258 ms | 1,798 ms |
| 10 (ripgrep default on M4) | 284 ms | 2,230 ms |

Once threads spill onto efficiency cores, kernel lock contention in the filesystem path dominates. speedread sizes its pool from `hw.perflevel0.logicalcpu`.

### Directory listing primitives (single thread, C, vscode tree)

| Method | Time |
|---|---:|
| `readdir` (names + `d_type` only) | 39.8 ms |
| `readdir` + `lstat` per entry | 80.0 ms |
| `getattrlistbulk` name + type | 43.1 ms |
| `getattrlistbulk` + flags, mtime, inode, size | 45.3 ms |

`getattrlistbulk` gets full metadata for the price of a bare `readdir`.

### Line-number formats (token cost per line)

Measured on 17 source files (867 KB, 25,438 lines of Rust, Python, Go, C and JavaScript):

| Format | o200k | cl100k | Claude (legacy tokenizer) |
|---|---:|---:|---:|
| `%6d→` (Claude Code Read) | 4.64 | 4.61 | 3.45 |
| `%6d\t` (`cat -n`) | 3.82 | 3.78 | 3.10 |
| `N:` | 2.48 | 2.44 | 3.18 |
| `N\t` (speedread) | **1.82** | **1.78** | **2.89** |

On the same content, speedread's format is 22% fewer tokens than `%6d→` (o200k).
