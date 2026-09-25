# Changelog

Notable changes by release. Before 1.0, minor versions may change tool arguments and output formats.

## 0.1.0 (unreleased)

First public release. Release notes: [docs/releases/v0.1.0.md](docs/releases/v0.1.0.md).

- MCP server (stdio) and CLI with four primitives: `map`, `search`, `trace` and `read`.
- `read`: batched targets under one token budget: files, ranges, `path:LINE` (the enclosing symbol), `path#Symbol`, `#Symbol`, Markdown sections, JSON/YAML/TOML keys and globs. Oversized files degrade to skeletons and outlines; `path@etag` returns only what changed, with symbol-labelled diffs.
- `search`: hits grouped under their enclosing function or class; `output=symbols` and `output=files`.
- `trace`: callers (depth 1–3), callees, references and implementations, syntactic and receiver-aware.
- `map`: budgeted, importance-weighted repository tree, with top-level symbols on request.
- Content-aware token estimator, and tokenizer profiles (`SPEEDREAD_TOKENIZER=claude|openai|legacy`).
- macOS: `getattrlistbulk` walker, performance-core thread pools, iCloud dataless files fail fast.
- Evals: tool scenarios, budget contract, tokenizer calibration, and real-agent code questions, relationship questions and bug fixes, with raw data.
