# Research: why agents read files badly, and what the fastest reader looks like

This document summarizes the research behind speedread's design: how today's coding agents read files, where their tokens and turns go, which techniques have evidence behind them, what macOS offers for speed, and what MCP clients allow. Numbers marked **(measured)** come from our own experiments on an Apple M4 (see [bench/RESULTS.md](../bench/RESULTS.md)). Everything else is cited. Research was conducted in September 2026.

## 1. Findings in brief

1. **Input tokens are the cost.** About 99% of agent tokens are accumulated input (tool results), not generated output ([AgentDiet](https://arxiv.org/abs/2509.23586)). Manus reports roughly a 100:1 input:output ratio in production loops ([Manus](https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus)).
2. **More context makes models worse, not just slower.** Accuracy degrades with input length well below the context limit, across 18 models ([Chroma, Context Rot](https://research.trychroma.com/context-rot)). Retrieval is U-shaped by position ([Lost in the Middle](https://arxiv.org/abs/2307.03172)). Anthropic frames context as a finite "attention budget" and recommends just-in-time retrieval ([Effective context engineering](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents)).
3. **Much of the input is waste.** AgentDiet removes redundant and expired trajectory content, cutting input tokens by 39.9–59.7% and cost by 21–36% with no loss in task performance ([AgentDiet](https://arxiv.org/abs/2509.23586)).
4. **Structure-first narrowing works.** Agentless (file → class/function skeleton → lines) reached 32% on SWE-bench Lite without an agent loop ([Agentless](https://arxiv.org/abs/2407.01489)). AutoCodeRover's AST search APIs solved 19% at $0.43/task ([AutoCodeRover](https://arxiv.org/abs/2404.05427)). LocAgent's code graph reached 92.7% file localization at ~86% lower cost ([LocAgent](https://arxiv.org/abs/2503.09089)). Aider's tree-sitter repo map fits a PageRank-ranked map into 1,024 tokens by default ([aider repomap](https://github.com/Aider-AI/aider/blob/main/aider/repomap.py)).
5. **Interface design changes outcomes.** SWE-agent's agent-computer interface (windowed viewer, summarized search, lint-gated edits) beat a raw shell by more than 10 points ([SWE-agent](https://arxiv.org/abs/2405.15793)). Anthropic reports state-of-the-art SWE-bench gains from refining tool descriptions alone ([Writing tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents)).
6. **Availability ≠ adoption.** With a strictly better structural navigation tool available, 58% of trials never called it ([CodeCompass](https://arxiv.org/abs/2602.20048)). Tool descriptions and server instructions must say when to use the tool.
7. **Nobody combines the pieces.** No surveyed MCP server offers budgeted + batched + symbol-aware + diff-on-reread reading. Diff-on-reread was not found anywhere (§5).

## 2. How agents read files today

| Agent | Read tool behavior | Source |
|---|---|---|
| Claude Code | `Read`: first 2,000 lines by default, lines cut at 2,000 chars, `cat -n`-style numbering (`%6d→`). `Grep` wraps ripgrep (default mode: file names). MCP output warns above 10,000 tokens and is capped at 25,000 (`MAX_MCP_OUTPUT_TOKENS`); larger results are saved to a file. | [system prompt](https://gist.github.com/mitchellgoffpc/ac429b7b3e7106c5e65fa9dea70284d9), [docs](https://code.claude.com/docs/en/mcp) |
| Gemini CLI | `read_file`: 2,000 lines, 2,000 chars/line, 20 MB max. The description tells the model that hitting limits is "token-inefficient". | [constants.ts](https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/utils/constants.ts) |
| VS Code / Copilot | `MAX_LINES_PER_READ = 2000`, offset/limit paging. | [readFileTool.tsx](https://raw.githubusercontent.com/microsoft/vscode-copilot-chat/main/src/extension/tools/node/readFileTool.tsx) |
| Copilot CLI | MCP results over 30 KB are saved to a file with a pointer; before v1.0.9, results were silently cut to 10 KB. | [copilot-cli#1732](https://github.com/github/copilot-cli/issues/1732) |
| Codex CLI | Shell-based (`sed -n`, `rg`). Head+tail truncation with a token budget (4 bytes/token estimate) and a "…N tokens truncated…" marker. | [truncate.rs](https://github.com/openai/codex/blob/main/codex-rs/utils/string/src/truncate.rs) |
| SWE-agent | 100-line windowed viewer, `search_file`/`find_file` returning names and counts, linter-gated edits. | [paper](https://arxiv.org/abs/2405.15793) |
| OpenHands | `str_replace_editor view` (`cat -n`), 16,000-char response cap. | [config.py](https://github.com/All-Hands-AI/openhands-aci/blob/main/openhands_aci/editor/config.py) |
| Aider | Repo map: tree-sitter tags → reference graph → personalized PageRank, 1,024-token default. | [repomap.py](https://github.com/Aider-AI/aider/blob/main/aider/repomap.py) |
| Cursor | Semantic search next to grep: +12.5% average QA accuracy, and removing it raised dissatisfied follow-ups by 2.2 points. | [Cursor blog](https://cursor.com/blog/semsearch) |
| Cline / Roo | `list_code_definition_names` (tree-sitter, names only). | [cline#4366](https://github.com/cline/cline/issues/4366) |
| Serena (MCP) | LSP-backed `get_symbols_overview`, `find_symbol`. Symbol-aware, but not batched or budgeted. | [serena](https://github.com/oraios/serena) |

Three vendors converged on **2,000-line blind paging**. An 8,161-line file costs 5 calls and ~105k tokens **(measured)**, and nothing tells the model what's in page 3 before it reads it.

## 3. The cost of formatting

**(measured)** Same content, different line-number prefixes, on 17 source files (25,438 lines):

| Format | tokens/line, o200k | tokens/line, Claude legacy |
|---|---:|---:|
| `%6d→` | 4.64 | 3.45 |
| `%6d\t` | 3.82 | 3.10 |
| `N\t` | 1.82 | 2.89 |

On o200k, the common padded formats add ~50% to the token cost of code. Unpadded `N<TAB>` is the cheapest *unambiguous* format. A single space is marginally cheaper, but it makes indentation ambiguous for exact-match edits.

Tool definitions are paid on every request. speedread's `tools/list` is 4.5 KB (~1,090 o200k tokens) with hand-written schemas, versus 6.3 KB with schemars-derived ones: `"default": null`, nullable unions, and `$defs` that repeat descriptions.

## 4. Techniques with evidence

| Technique | Evidence | In speedread |
|---|---|---|
| Structure first (skeleton/outline before bodies) | Agentless, AutoCodeRover, LocAgent, aider | 4-level ladder: full → skeleton → compact → budget-fitted outline |
| Symbol-level retrieval | LocAgent (92.7% localization, 86% cheaper), Serena | `path#Class.method`, `#name` workspace lookup, `path:LINE` → enclosing symbol |
| Batching | Claude Code's system prompt instructs batching reads | One `read` call takes many targets sharing one budget |
| Token budgets, not line counts | aider (binary search to 1,024 tokens), Codex (token budget) | Every tool is budgeted; items degrade largest-first |
| Head + tail for logs | Codex CLI | Plain-text and streamed files truncate as head + tail with the exact omitted range |
| Diff on re-read | Implied by AgentDiet ("expired" content) and Manus (filesystem as memory); **no existing implementation found** | `path@etag`: unchanged / appended lines / unified diff |
| Actionable errors | Anthropic tool guidance, MCP `isError` guidance | "Did you mean", closest symbols, exact continuation targets |
| Deterministic output | Manus: KV-cache hit rate is "the single most important metric" | Sorted walks, stable formats, no timings in tool output |

## 5. Existing MCP servers

| Server | Batched | Budgeted | Symbol-aware | Diff on re-read |
|---|---|---|---|---|
| [Official filesystem](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem) (`read_text_file` head/tail, `read_multiple_files`) | ✓ | line counts only | — | — |
| [Serena](https://github.com/oraios/serena) (LSP) | — | — | ✓ | — |
| [ast-grep MCP](https://github.com/ast-grep/ast-grep-mcp) | — | — | structural search | — |
| [repomix](https://github.com/yamadashy/repomix) | whole repo | — | signature compression | — |
| Desktop Commander (offset/length chunks), code-index-mcp (symbol index) | partial | — | partial | — |
| **speedread** | ✓ | ✓ tokens | ✓ 18 languages + docs/data | ✓ |

## 6. macOS: what makes file access fast

- **`getattrlistbulk(2)`** returns attributes for many directory entries per syscall. A published Rust `du` built on it ran 6.39× faster than `du -sh` (409k files, 91% of `du`'s time in syscalls) ([healeycodes](https://healeycodes.com/maybe-the-fastest-disk-usage-program-on-macos)). No mainstream walker uses it: ripgrep's `ignore`, `walkdir` and `jwalk` don't. **(measured)** Single-threaded on vscode's tree it costs 45.3 ms with size, mtime, flags and inode, vs 39.8 ms for bare `readdir` and 80.0 ms for `readdir` + `lstat`. With `FSOPT_PACK_INVAL_ATTRS`, the file-attribute group is **not** packed for directories (the record is 8 bytes shorter), so parsers must consult the returned attribute set.
- **Performance cores.** **(measured)** Filesystem-heavy parallel work slows down once threads spill onto efficiency cores. ripgrep's search of vscode takes 104 ms at 4 threads and 284 ms at its default 10 on an M4, as kernel time grows from 339 ms to 2,230 ms. The P-core count is `hw.perflevel0.logicalcpu`. QoS matters for CLI tools too: a default-QoS build starved other processes in one documented case ([jmmv](https://jmmv.dev/2019/03/macos-threads-qos-and-bazel.html)).
- **mmap.** ripgrep disables mmap on macOS ([ripgrep#36](https://github.com/BurntSushi/ripgrep/issues/36)), though a 2025 benchmark showed mmap 4.8× faster for a single 4 GB file ([ripgrep#3246](https://github.com/BurntSushi/ripgrep/pull/3246)). For a long-lived server, cached mappings risk SIGBUS when editors truncate files in place. speedread reads into owned buffers and streams large files through 1 MB `read(2)` chunks.
- **iCloud dataless files.** `stat`/`open` on a dataless file can block while it downloads. `setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_PROCESS, OFF)` makes access fail fast with `EDEADLK` instead ([TN3150](https://developer.apple.com/documentation/technotes/tn3150-getting-ready-for-data-less-files.md)). Firmlinks aren't reported as symlinks, which causes duplicate traversal ([walkdir#169](https://github.com/BurntSushi/walkdir/issues/169)). Skip `SF_FIRMLINK`.
- **Limits.** launchd gives GUI-launched processes a 256-descriptor soft limit **(measured: `launchctl limit maxfiles`)**. APFS `readdir` order is hash order, not sorted ([APFS FAQ](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/FAQ/FAQ.html)). APFS timestamps have nanosecond resolution ([swiftforensics](https://www.swiftforensics.com/2017/09/apfs-timestamps.html)). Cache keys should use (dev, inode, mtime_ns, size), which is Watchman's tuple ([Watchman](https://facebook.github.io/watchman/docs/cmd/query)).
- **SIMD on Apple Silicon.** `simdutf8` validates at 106 GB/s (ASCII) vs 28.7 GB/s for std on M1 ([simdutf8](https://github.com/rusticstuff/simdutf8)). XXH3-64 runs at ~35 GiB/s on M1 Max ([twox-hash comparison](https://github.com/shepmaster/twox-hash/blob/master/comparison/README.md)). `memchr` has a NEON backend.
- **Sorting paths.** **(measured)** `Path::cmp` re-parses components on every comparison. Sorting 19k walk results that way made the parallel walker slower than ripgrep; bytewise comparison fixed it.
- **Apple SDK headers.** **(measured)** `NS_ASSUME_NONNULL_BEGIN` derails tree-sitter's Objective-C error recovery for the rest of a header (0 symbols from a trivial header). Blanking a list of Apple annotation macros with same-length spaces restores full outlines: 121 symbols from `NSURLSession.h`.

## 7. MCP in 2026: protocol and client constraints

- The current spec is **2026-07-28**: a stateless core with `server/discover` (which carries `instructions`), multi-round-trip requests, and deprecation of Roots, Sampling and Logging (SEP-2577) ([changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)). Clients lag the spec. Claude Code still answers `roots/list` with its launch directory plus `--add-dir` directories, and sets `CLAUDE_PROJECT_DIR` for servers ([docs](https://code.claude.com/docs/en/mcp)). The official Rust SDK `rmcp` 3.4 supports both legacy `initialize` and the stateless protocol.
- **Output limits** (§2): Claude Code warns at 10k tokens and caps at 25k (tools can raise the cap via `_meta["anthropic/maxResultSizeChars"]`). Copilot CLI spills results over 30 KB to a file. speedread therefore caps budgets at 10,000 tokens (~26 KB) by default.
- Claude Code **prefers `structuredContent` over text** when both are present ([claude-code#55677](https://github.com/anthropics/claude-code/issues/55677)), so speedread returns text only. Codex reads server `instructions`; its guidance is to keep the first 512 characters self-contained ([OpenAI](https://learn.chatgpt.com/docs/extend/mcp)).
- Claude Code's `Edit`/`Write` require a prior built-in `Read`, and MCP reads don't satisfy it ([claude-code#32214](https://github.com/anthropics/claude-code/issues/32214)).
- Tool-count and schema cost: Cursor has a reported ~40-tool soft cap. Each tool definition costs ~50–200 tokens per request ([OpenAI function calling](https://platform.openai.com/docs/guides/function-calling)). Fewer, consolidated tools are better ([Anthropic](https://www.anthropic.com/engineering/writing-tools-for-agents)).

## 8. Tokenizers

Anthropic states Claude 4.7+ models use a tokenizer producing ~30% more tokens than earlier models for the same text ([Claude docs](https://platform.claude.com/docs/en/about-claude/glossary)). **(measured)** Line-numbered code runs ~3.3 bytes/token on the legacy Claude tokenizer and ~3.9 on o200k. speedread budgets at 2.6 bytes/token (3.6 for Codex clients), keeping estimates at or above real counts. GitHub's `bpe` crate is ~4× faster than tiktoken single-threaded on M1 ([GitHub blog](https://github.blog/ai-and-ml/llms/so-many-tokens-so-little-time-introducing-a-faster-more-flexible-byte-pair-tokenizer/)), but exact o200k counts wouldn't help Claude users, so speedread uses a calibrated estimate.

## 9. Design principles derived

1. **Minimize tokens × turns, not bytes read.** Disk I/O takes microseconds; a model turn takes seconds.
2. **Never truncate blindly.** Degrade to a view that still covers the whole target, or cut with an exact continuation target.
3. **Address code by meaning:** symbols, enclosing symbols, sections and keys, not guessed line windows.
4. **Batch by default.** One call, many targets, one budget.
5. **Don't resend what the agent has.** Etags, diffs, appended tails.
6. **Make results pay for the next step.** Search hits carry symbol ranges, and errors carry suggestions.
7. **Cheap to define.** Three tools and compact schemas, with instructions that say when to use them.
8. **Deterministic output** for prompt-cache stability.
9. **Engineer for the platform.** On macOS: bulk attribute listing, P-core threading, QoS, iCloud and launchd safety.
