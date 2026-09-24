# speedread

**The fastest, most token-efficient way for AI coding agents to read code.** An MCP server (and CLI) built in Rust for macOS on Apple Silicon.

Agents burn most of their context and most of their turns reading files: whole files for one function, 2,000-line pages, the same file again after every edit, grep → read → grep loops. speedread replaces that with three tools that are budgeted, batched, symbol-aware and diff-aware:

- **93% fewer tokens and 70% fewer tool calls** than Read/Grep-style tools across 30 real-world scenarios on six popular repos ([methodology](bench/RESULTS.md)).
- **Faster than ripgrep on Apple Silicon** for walking and searching repos: a `getattrlistbulk(2)` walker plus threads sized to the performance cores (ripgrep's default thread count runs **2.7× slower** than optimal on an M4).
- **Outlines for 18 languages** (tree-sitter), plus Markdown, JSON, YAML and TOML. `README.md#Install`, `package.json#scripts` and `Session.swift#Session.request` are all addressable.

```
read   ["src/app.ts", "src/db.ts#Pool.connect", "#handleRequest", "logs/dev.log@1a2b3c4d"]
search {"pattern": "retryPolicy", "output": "symbols"}
map    {"symbols": true}
```

---

## Why

Research and measurements behind the design are in [docs/RESEARCH.md](docs/RESEARCH.md). In short:

- **~99% of agent tokens are input**, i.e. accumulated tool output ([AgentDiet, 2025](https://arxiv.org/abs/2509.23586)). Accuracy degrades as context grows, long before the window is full ([Chroma, "Context Rot"](https://research.trychroma.com/context-rot)).
- **Read tools page blindly.** Claude Code, Gemini CLI and VS Code/Copilot all read 2,000 lines per call. An 8,000-line file is five calls and ~105k tokens.
- **Line numbers are expensive.** The common `%6d→` prefix costs **4.6 tokens per line** (o200k), a ~50% surcharge on code. speedread's `N<TAB>` costs 1.8.
- **Re-reading is the norm.** After every edit, agents re-read the whole file to check it. No existing tool returns only the diff since the last read.
- **Agents skip tools they don't recognize.** In one study, 58% of runs never called a strictly better navigation tool that was available ([CodeCompass, 2026](https://arxiv.org/abs/2602.20048)). speedread's tool descriptions and server instructions say explicitly when to use it instead of shell reads.

## What you get

### `read`: batched, budgeted, symbol-aware

One call takes any mix of targets. They share one token budget (default 8,000).

| Target | Returns |
|---|---|
| `src/app.ts` | Whole file. If it doesn't fit, a **skeleton**: signatures, types and docs, with bodies collapsed as `A-B ⋯`. If that's still too big, an **outline**. Never cut off blindly. |
| `src/app.ts:120-180` | Those lines (paginates with an exact continuation target). |
| `src/app.ts:120`, `src/app.ts:120:5` | The function/class enclosing line 120 (paste compiler errors directly). |
| `src/app.ts#handleRequest`, `#Server.start`, `Foo::bar` | That symbol's full source, including doc comments, attributes and decorators. |
| `#handleRequest` | Finds the definition anywhere in the workspace (no path needed). |
| `README.md#Install`, `package.json#scripts`, `Cargo.toml#dependencies` | A Markdown section, JSON key, YAML key or TOML table. |
| `src/**/*.test.ts` | Glob (.gitignore-aware). Large sets degrade largest-first to fit the budget. |
| `src/app.ts@1a2b3c4d` | **Only what changed** since the version whose etag appeared in a header. A unified diff after an edit, just the appended lines for a growing log, or `unchanged`. |

Every file header carries an etag. Here is a real skeleton of flask's 1,628-line `app.py` (excerpt). It costs 3.4k tokens, against 21k for the file:

```
==> src/flask/app.py @5f738ada (1,628 lines) [skeleton]
110	class Flask(App):
111	    """The flask object implements a WSGI application and acts as the central
112-205	    ⋯
…
366	    def get_send_file_max_age(self, filename: str | None) -> int | None:
367	        """Used by :func:`send_file` to determine the ``max_age`` cache
368-391	        ⋯
392	
393	    def send_static_file(self, filename: str) -> Response:
394	        """The view function used to serve files from
395-412	        ⋯
```

After an edit, `read ["src/flask/app.py@5f738ada"]` returns only what changed:

```
==> app.py @b1436eb4 (was @5f738ada): 1 hunk, +1 -1, now 1,628 lines
@@ -626,5 +626,5 @@
         .. versionadded:: 0.11
         """
-        rv = {"app": self, "g": g}
+        rv = {"app": self, "g": g, "config": self.config}
         for processor in self.shell_context_processors:
             rv.update(processor())
```

### `search`: hits grouped by enclosing symbol

ripgrep's engine (regex or literal, .gitignore-aware). Every hit sits under the function or class that contains it, with its line range, so the next step is `read path#name` instead of guessing ranges. `output=symbols` returns the full source of each enclosing symbol in the same call. `output=files` returns paths with counts.

```
==> src/flask/helpers.py @c9cf99f2 (4 matches)
[200-251] def url_for(endpoint: str, *, _anchor: str | None = None, _method: str | None = None, …) -> str
200	def url_for(
212	    :meth:`current_app.url_for() <flask.Flask.url_for>`. See that method
244	    return current_app.url_for(
```

### `map`: a budgeted repo overview

A .gitignore-aware tree with line counts. Directories expand by importance (source before hidden, test or vendored trees; large before small) until the budget (default 3,000) is spent. The rest is summarized with file counts and extension mix. `symbols=true` adds each file's top-level definitions, giving a compact repo map.

## Benchmarks

Apple M4 (4 performance + 6 efficiency cores), 16 GB, macOS 15.7. Warm cache, medians of 30 runs ([details](bench/RESULTS.md)).

### Tokens and tool calls (o200k tokens returned to the model)

| Scenario | Baseline calls | Baseline tokens | speedread calls | speedread tokens | Saved |
|---|---:|---:|---:|---:|---:|
| Understand a large file (6 files, 1.4k–8.2k lines) | 13 | 274,994 | 6 | 25,429 | 91% |
| Read one function (baseline reads the file) | 9 | 182,112 | 5 | 4,464 | 98% |
| Read one function (baseline: best-case grep + exact window) | 10 | 8,362 | 5 | 4,464 | 47% |
| Re-read after a one-line edit (4 files) | 11 | 233,492 | 4 | 362 | 99.8% |
| Find usages + read calling code (4 symbols) | 17 | 244,412 | 4 | 17,240 | 93% |
| Follow a growing log | 1 | 3,969 | 1 | 692 | 83% |
| Orient in a repo (5 repos; baseline: `ls` per directory) | 38 | 2,222 | 5 | 10,278 | 87% fewer calls¹ |
| **All 30 scenarios** | **99** | **949,563** | **30** | **62,929** | **93% tokens, 70% calls** |

Baselines: `%6d→`-numbered reads in 2,000-line pages (the Claude Code / Gemini CLI / VS Code pattern), ripgrep content output, and `ls`. Repos: ripgrep, zod, vscode, flask, gin, Alamofire.
¹ `map` returns more tokens than shallow `ls` calls, but in one call and with line counts and nested structure.

### Speed

| Task | speedread | ripgrep defaults | ripgrep `-j4` |
|---|---:|---:|---:|
| Walk vscode (19,167 files), **with sizes and mtimes** | **26 ms** | 29 ms (names only) | 30 ms |
| Walk, single thread | **65 ms** | 93 ms | — |
| Search vscode for a literal, list files | **103 ms** | 294 ms | 109 ms |
| Search 21k-file cargo registry, list files | 144 ms | 364 ms | **139 ms** |
| Read a 742 KB, 21k-line `.d.ts` → skeleton (cold process) | 17 ms | | |
| `#createDecorator` definition lookup across vscode | 332 ms | | |
| Stream a 148 MB, 1.6M-line log (head + tail); append check | 72 ms; 53 ms | | |

## Install

Requires Rust 1.90+ (`brew install rust` or [rustup](https://rustup.rs)).

```sh
cargo install --git https://github.com/brennengreen/speedread
# or from a clone:
cargo install --path .
```

This installs `~/.cargo/bin/speedread`. It's a single native binary with no runtime dependencies.

## Configure your agent

speedread serves MCP over stdio: `speedread mcp`. Workspace roots come from, in order:
1. `--root <dir>` (repeatable)
2. `$CLAUDE_PROJECT_DIR` (set by Claude Code)
3. the client's MCP roots: VS Code/Cursor workspace folders, or Claude Code `--add-dir` directories (these are added to explicit roots)
4. the current directory

In most clients no flags are needed.

**Claude Code**
```sh
claude mcp add --scope user speedread -- speedread mcp
```

**GitHub Copilot CLI**
```sh
copilot mcp add speedread -- speedread mcp
```

**VS Code** (Copilot agent mode): `.vscode/mcp.json`
```json
{ "servers": { "speedread": { "type": "stdio", "command": "speedread", "args": ["mcp"] } } }
```

**Cursor**: `~/.cursor/mcp.json` or `.cursor/mcp.json`
```json
{ "mcpServers": { "speedread": { "command": "speedread", "args": ["mcp"] } } }
```

**OpenAI Codex CLI**: `~/.codex/config.toml`
```toml
[mcp_servers.speedread]
command = "speedread"
args = ["mcp"]
```

**Gemini CLI**: `~/.gemini/settings.json`
```json
{ "mcpServers": { "speedread": { "command": "speedread", "args": ["mcp"] } } }
```

**Claude Desktop**: `~/Library/Application Support/Claude/claude_desktop_config.json`. Claude Desktop doesn't run in a project directory, so pass `--root`:
```json
{ "mcpServers": { "speedread": { "command": "/Users/you/.cargo/bin/speedread", "args": ["mcp", "--root", "/Users/you/code/project"] } } }
```

**Zed**: `settings.json`
```json
{ "context_servers": { "speedread": { "source": "custom", "command": "speedread", "args": ["mcp"] } } }
```

GUI apps may not inherit your shell's `PATH`. If a client can't find `speedread`, use the absolute path (`which speedread`).

### Recommended: tell the agent to use it

Agents default to their built-in tools. Add this to `AGENTS.md`, `CLAUDE.md` or `.github/copilot-instructions.md`:

```markdown
## Reading code
Use the speedread MCP tools instead of cat/sed/head/grep/find or one-file-at-a-time reads:
- `read` with ALL the files, ranges (`path:A-B`), and symbols (`path#Name`, `#Name`) you need in ONE call.
  Big files come back as skeletons — expand the parts you need with `path#Name`.
- `search` to find code (hits show their enclosing function); `output: "symbols"` to get those functions directly.
- `map` first in an unfamiliar repo.
- After editing a file, `read` `path@etag` (etag from the `==>` header) to see only what changed.
```

> **Claude Code note:** Claude Code's built-in `Edit`/`Write` tools require the file to have been read with its built-in `Read` first; MCP reads don't count. Use speedread for exploration, search and verification, and let Claude Code do its `Read` right before editing a file.

## Tool reference

**`read`** `{ targets: string[], mode?: "auto"|"full"|"skeleton"|"outline", budget?: number, line_numbers?: boolean }`

- `auto` (default): full content if it fits, else skeleton, compact skeleton, then outline. When several targets compete for the budget, the largest exploratory ones degrade first. Explicitly requested symbols go later, and explicit line ranges never degrade.
- `full`: never collapses. Pages with `⋯ truncated at budget; continue with path:641-2000`.
- Output format: `==> path[:A-B] @etag (N lines[, CRLF|BOM|UTF-16|minified]) [skeleton|outline] [Symbol]`, then `line<TAB>text`. Text is byte-exact (safe for exact-match edits). Lines over 2,000 characters are cut with a marker.
- Typos get suggestions (`Did you mean: src/util.rs`). A unique filename match is resolved automatically and flagged in the header.
- Binary files are identified (PNG, Mach-O, SQLite, …) rather than dumped. iCloud placeholders are reported, never downloaded.

**`search`** `{ pattern, paths?, globs?, literal?, word?, case?: "smart"|"sensitive"|"insensitive", context?, output?: "matches"|"symbols"|"files", budget? }`

**`map`** `{ path?, symbols?, depth?, globs?, budget? }`

The same operations exist on the command line: `speedread read …`, `speedread search …`, `speedread map …`.

## How it works

- **Budget ladder.** Every target becomes an item with views: full → skeleton → compact skeleton (long comment blocks, docstrings and import runs collapsed to one line) → outline. A final outline that still doesn't fit keeps every top-level symbol and fills in members breadth-first, with `(+N members)` for the rest. Items degrade largest-first until the batch fits, and anything still too big is cut with an exact continuation target. The budget is in tokens, estimated at a conservative 2.6 bytes/token. Claude 4.7+ tokenizers emit ~30% more tokens than earlier ones. Codex clients get 3.6 (o200k). The maximum (10,000) keeps results inline in every client: Copilot CLI spills results over 30 KB to a file, and Claude Code warns above 10k tokens.
- **Outlines.** tree-sitter grammars for Rust, Python, JavaScript, TypeScript/TSX, Go, Java, C, C++, C#, Ruby, PHP, Bash, Swift, Kotlin, Scala, Lua and Objective-C, walked iteratively (no recursion, so minified or deeply nested code can't overflow the stack). Hand-written single-pass scanners handle Markdown, JSON/JSONC, YAML and TOML. Symbols carry doc-inclusive ranges, one-line signatures and collapsible body ranges. Test blocks (`describe`/`it`, RSpec) are symbols too. Apple SDK macros (`NS_ASSUME_NONNULL_BEGIN`, `API_AVAILABLE(…)`) are blanked before parsing Objective-C and C headers; otherwise tree-sitter's error recovery drops the rest of the file.
- **Etags and diffs.** An etag is the low 32 bits of xxh3 over the content. Content the agent has seen is kept in a 256 MB LRU keyed by etag. `path@etag` returns `unchanged`, the appended tail (prefix check), or an imara-diff histogram diff. It falls back to full content when that's smaller. Files over 64 MB are streamed with a sparse line index and never loaded whole; appends are verified by re-hashing the old prefix.
- **Caches.** Sources are validated by (size, mtime ns, inode, device) on every access (one `stat`). Outlines are cached by content hash. The long-lived MCP process keeps both warm.

### macOS-specific engineering

- **`getattrlistbulk(2)` walker.** It gets name, type, size, mtime and flags for a batch of entries in one syscall. That's 1.8× faster than readdir+lstat and as cheap as bare readdir. `.gitignore` files are only opened when the listing shows they exist. Results are batched per directory and sorted bytewise (`Path::cmp` re-parses components on every compare). It falls back to readdir on filesystems without bulk support.
- **Performance-core threading.** On M-series chips, filesystem-heavy parallel work *slows down* once threads spill onto efficiency cores. Kernel time explodes from lock contention: ripgrep searching vscode takes 104 ms at 4 threads and 284 ms at its default 10. speedread sizes its pool from `hw.perflevel0.logicalcpu` (override: `SPEEDREAD_THREADS`) and runs workers at `QOS_CLASS_USER_INITIATED`.
- **iCloud-safe.** `setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, OFF)`: a project in iCloud-synced Desktop/Documents never triggers a download or a hang. Dataless placeholders (`SF_DATALESS`) are skipped and reported. Firmlinks are not followed.
- **GUI-launch safe.** Raises the 256-descriptor soft limit that launchd gives GUI-spawned processes, and disables atime updates for reads.
- **No mmap for cached files.** A cached mapping of a file another process truncates in place (which editors and agents do constantly) raises SIGBUS. Files are read with one `read(2)` into owned buffers. NEON-accelerated `memchr` builds the line index, `simdutf8` validates UTF-8, and `xxh3` hashes.
- `mimalloc`, fat LTO, `codegen-units=1`.

## Security

Read-only by construction; there are no write tools. Paths are canonicalized (symlinks resolved) and must lie inside a root. Well-known dependency caches are also readable: `~/.cargo/registry`, `~/.rustup/toolchains`, `~/go/pkg/mod`, Xcode `DerivedData` (SwiftPM checkouts), and the macOS SDKs. `--no-deps` removes those, and `--unrestricted` lifts all restrictions. macOS privacy protections (Full Disk Access) still apply to the process.

## Limitations

- Outlines are syntactic (tree-sitter), not semantic. There's no type-aware cross-file reference resolution, so `#Name` finds definitions by name.
- Token counts are estimates tuned to stay at or above real counts for Claude and OpenAI tokenizers.
- Built and tuned for macOS on Apple Silicon. Other Unix platforms use the portable `ignore`-crate walker but aren't tested in CI. Windows is not supported.

## Development

```sh
cargo test                                  # unit, MCP protocol, and outline snapshot tests
UPDATE_SNAPSHOTS=1 cargo test --test outlines   # after an intentional outline change
speedread debug --sexp file.swift           # inspect symbols / syntax tree (hidden command)
python3 bench/agent_scenarios.py <bench-dir> # token/call benchmark (see bench/RESULTS.md)
```

Adding a language usually means adding a grammar crate, an extension in `src/lang.rs` and a `match` arm in `src/outline.rs`, plus a fixture in `tests/fixtures/`.

## License

MIT
