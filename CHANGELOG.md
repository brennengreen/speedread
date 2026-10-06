# Changelog

Notable changes by release. Before 1.0, minor versions may change tool arguments and output formats.

## Unreleased

- Luau: outlines, `path#Symbol`, function-grouped `search` hits and `trace` for `.luau` files, and for `.lua` files that use Luau syntax (recognized by a `--!strict` directive, type aliases or annotations, or because only Luau parses them). Covers type aliases, attributes such as `@native`, and functions in table constructors.
- Lua and Luau: `function M.f()` is named `f` in scope `M`, and `function C:m()` is a method `m` of `C`. `#m`, `#C.m` and `#C:m` all find it, and `trace` resolves `obj:m()` calls. Calls through the standard libraries and Roblox globals (`table.insert`, `task.spawn`, `Instance.new`) are no longer matched to workspace functions of the same name.

## 0.1.0 (2026-09-26)

First public release. Release notes: [docs/releases/v0.1.0.md](docs/releases/v0.1.0.md).

- MCP server (stdio) and CLI with four primitives: `map`, `search`, `trace` and `read`.
- `read`: batched targets under one token budget: files, ranges, `path:LINE` (the enclosing symbol), `path#Symbol`, `#Symbol`, Markdown sections, JSON/YAML/TOML keys and globs. Oversized files degrade to skeletons and outlines; `path@etag` returns only what changed, with symbol-labelled diffs.
- `search`: hits grouped under their enclosing function or class; `output=symbols` and `output=files`.
- `trace`: callers (depth 1–3), callees, references and implementations, syntactic and receiver-aware.
- `map`: budgeted, importance-weighted repository tree, with top-level symbols on request.
- Content-aware token estimator, and tokenizer profiles (`SPEEDREAD_TOKENIZER=claude|openai|legacy`).
- macOS: `getattrlistbulk` walker, performance-core thread pools, iCloud dataless files fail fast.
- Evals: tool scenarios, budget contract, tokenizer calibration, and real-agent code questions, relationship questions and bug fixes, with raw data.
- Distribution: a prebuilt macOS arm64 binary, a Claude Desktop bundle (`.mcpb`), and publishing to the MCP Registry as `io.github.brennengreen/speedread`.
