# Benchmarks

Machine: Apple M4 (4 performance + 6 efficiency cores), 16 GB RAM, macOS 15.7.9, APFS internal SSD. Warm page cache. speedread 0.1.0 release build.

## 1. Tokens and tool calls for agent tasks

`bench/agent_scenarios.py` measures, for common code-reading tasks, the text returned to the model (o200k_base tokens) and the number of tool calls.

**speedread** runs as a real MCP server over stdio, one session per repository, so etag diffs behave exactly as they do in an agent session. All reads use default budgets.

**Baselines** emulate the de-facto standard agent tools:

| Baseline tool | Emulation |
|---|---|
| Read | `%6d→`-numbered lines, 2,000 lines per call, lines cut at 2,000 chars. This is the Claude Code format; Gemini CLI and VS Code/Copilot also page at 2,000 lines. |
| Grep | ripgrep `path:line:text` content output (`rg -n -w`). |
| ls | `ls -F`, one call per directory. |

Scenarios:
1. **Orient**: `ls` of the root and each top-level directory, vs one `map`.
2. **Understand a large file**: page through the whole file, vs one `read` (auto view).
3. **Read one function in a known file.** The typical baseline reads the whole file. The best-case baseline greps the file for the name, then reads exactly the function ±10 lines, which assumes perfect knowledge of the range. speedread does one `read path#Class.method`.
4. **Re-read after a one-line edit**: full re-read, vs `read path@etag`.
5. **Find usages + read calling code**: `grep` the repo, then read the 3 files with the most hits, vs one `search output=symbols`.
6. **Follow a growing log**: `tail -n 200` after 30 new lines, vs `read log@etag`.

Repositories (shallow clones): ripgrep@3fce3b5, zod@9dc926c, vscode@a41ff3b2, flask@d73fa1c, gin@3b08cd7, Alamofire@bda9ed5.

| Scenario | Baseline calls | Baseline tokens | speedread calls | speedread tokens | Tokens saved |
|---|---:|---:|---:|---:|---:|
| orient: flask (4 top-level dirs) | 5 | 299 | 1 | 1,801 | -502% |
| orient: gin (8 top-level dirs) | 9 | 435 | 1 | 671 | -54% |
| orient: ripgrep (8 top-level dirs) | 9 | 232 | 1 | 2,056 | -786% |
| orient: zod (5 top-level dirs) | 6 | 247 | 1 | 2,388 | -867% |
| orient: vscode (8 top-level dirs) | 9 | 1,009 | 1 | 3,362 | -233% |
| large file: defs.rs (8,161 lines) | 5 | 105,147 | 1 | 6,321 | 94% |
| large file: types.ts (5,138 lines) | 3 | 67,281 | 1 | 5,917 | 91% |
| large file: textModel.ts (2,745 lines) | 2 | 39,756 | 1 | 4,989 | 87% |
| large file: app.py (1,628 lines) | 1 | 21,264 | 1 | 3,364 | 84% |
| large file: context.go (1,544 lines) | 1 | 19,362 | 1 | 2,797 | 86% |
| large file: Session.swift (1,441 lines) | 1 | 22,184 | 1 | 2,041 | 91% |
| function Flask.url_for: read whole file | 1 | 21,264 | 1 | 1,389 | 93% |
| function Flask.url_for: grep file + exact window (best case) | 2 | 2,082 | 1 | 1,389 | 33% |
| function Context.AbortWithStatusJSON: read whole file | 1 | 19,362 | 1 | 115 | 99% |
| function Context.AbortWithStatusJSON: grep file + exact window (best case) | 2 | 388 | 1 | 115 | 70% |
| function WalkParallel.run: read whole file | 2 | 34,449 | 1 | 143 | 100% |
| function WalkParallel.run: grep file + exact window (best case) | 2 | 581 | 1 | 143 | 75% |
| function ZodString._parse: read whole file | 3 | 67,281 | 1 | 2,686 | 96% |
| function ZodString._parse: grep file + exact window (best case) | 2 | 4,863 | 1 | 2,686 | 45% |
| function TextModel.setValue: read whole file | 2 | 39,756 | 1 | 131 | 100% |
| function TextModel.setValue: grep file + exact window (best case) | 2 | 448 | 1 | 131 | 71% |
| re-read after edit: defs.rs | 5 | 105,159 | 1 | 79 | 100% |
| re-read after edit: types.ts | 3 | 67,291 | 1 | 74 | 100% |
| re-read after edit: textModel.ts | 2 | 39,767 | 1 | 117 | 100% |
| re-read after edit: app.py | 1 | 21,275 | 1 | 92 | 100% |
| usages of url_for (flask, 47 files) | 4 | 41,840 | 1 | 4,008 | 90% |
| usages of AbortWithStatus (gin, 8 files) | 4 | 25,999 | 1 | 4,490 | 83% |
| usages of build_parallel (ripgrep, 3 files) | 5 | 41,579 | 1 | 2,750 | 93% |
| usages of safeParse (zod, 188 files) | 4 | 134,994 | 1 | 5,992 | 96% |
| follow log: 30 lines appended to 5,000 | 1 | 3,969 | 1 | 692 | 83% |

`map` returns more tokens than shallow `ls` calls, but in one call and with line counts, nested structure and file-type summaries. The default `map` budget is 3,000 tokens.

Reproduce:
```sh
pip install tiktoken
python3 bench/agent_scenarios.py <dir-with-clones> --speedread target/release/speedread --rg $(which rg)
```

Why calls matter as much as tokens: every tool call is another model turn. It adds latency (typically seconds) and re-reads the whole accumulated context (cached or not). Cutting 99 calls to 30 removes 69 round trips.

## 2. Speed

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
