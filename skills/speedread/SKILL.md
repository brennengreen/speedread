---
name: speedread
description: Read, search and navigate code with a token budget. Use for any code reading - whole files, line ranges, symbols (functions, classes, sections, keys), text search grouped by enclosing function, call graphs (callers, callees, references, implementations), repo overviews, and re-reading a file after an edit (diff only). Prefer it over cat/head/sed/grep/find and over one-file-at-a-time reads.
---

# speedread

speedread returns the *smallest useful unit of code* for the question, sized to a
token budget: a symbol instead of a file, a skeleton instead of a truncated page, a
diff instead of a re-read. Four tools (MCP) or the same four commands (CLI).

## The loop

1. **Orient** — `map` (or `map symbols=true`) instead of `ls`/`find`/`tree`.
2. **Locate text** — `search "pattern"`: hits come grouped under their enclosing
   function/class with its line range, e.g. `[40-72] fn parse(...)`.
3. **Locate relationships** — `trace "#parse"` (callers), `direction=callees`,
   `refs`, `impls`, `depth=2` for a call tree.
4. **Read exact evidence** — ONE `read` call with every target you need:
   `["src/app.ts#Server.start", "src/util.ts:40-80", "#parse_header"]`.
5. **After editing** — `read "path@etag"` (etag from the `==> path @etag` header):
   returns `unchanged`, the appended lines, or a diff labelled with the changed
   symbols (`body changed, signature unchanged` / `signature changed` / `added`).

## Targets (read)

| Target | Returns |
|---|---|
| `path` | whole file; too big for the budget → skeleton (signatures, docs; bodies collapsed as `A-B ⋯`) |
| `path:120-180`, `path:120` | a range; a line returns the function/class enclosing it |
| `path#Name`, `path#Type.method`, `#Name` | a symbol's full source incl. docs; `#Name` searches the workspace |
| `README.md#Install`, `package.json#scripts` | a Markdown section, a JSON/YAML/TOML key |
| `src/**/*.test.ts` | a glob (shares the budget) |
| `path@etag` | only what changed since that version |

Rules of thumb:
- Batch. Five targets in one `read` cost one round-trip; five reads cost five
  (each round-trip re-sends the whole conversation).
- A skeleton is a map, not an answer: expand the body you need with `path#Name` or
  the `A-B` range shown on the collapsed line.
- `search output=symbols` returns the full source of every enclosing function in
  the same call — use it when you will read the callers anyway.
- `trace` is syntactic (tree-sitter + name resolution, no type inference). Sites it
  cannot attribute to one of several same-named definitions are marked `?`.
- Budgets are tokens (default 8000; map 3000; trace 4000; max 10000). Raise
  `budget` rather than paging.

## Scripts and code execution

The CLI mirrors the tools and adds JSON Lines for filtering in code instead of
in context:

```bash
speedread read 'src/app.ts#Server.start' src/util.ts:40-80
speedread search 'AbortWithStatus\(' --output symbols
speedread trace '#AbortWithStatus' --direction callers --depth 2
speedread symbols src --json | jq -r 'select(.kind=="function" and .end-.start>80) | "\(.path):\(.start) \(.qualified)"'
speedread search 'TODO' --json | jq -r .path | sort | uniq -c | sort -rn | head
speedread map --json | jq -s 'map(select(.lines != null)) | sort_by(-.lines) | .[:10]'
```

## Make it the file reader

Agents rarely pick up a new tool on their own (0% adoption in our drop-in trials).
Tell the harness: in Copilot CLI start with
`--excluded-tools view grep glob` (edit/bash stay), or add to your project
instructions: "Use the speedread tools to read, search and navigate code."
In Claude Code, Edit/Write still require one native Read of the file first.
