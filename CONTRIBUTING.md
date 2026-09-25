# Contributing to speedread

Thanks for helping. speedread has one maintainer, so for anything larger than a fix, a short issue first saves everyone time.

Good ways to help, roughly from smallest to largest:

- **Report a wrong result**: an outline that misses a definition, a `trace` that misattributes a call, a budget that's exceeded. A public repository and commit, or a small file, makes it fixable.
- **Verify a client setup** and fix its row in the README.
- **Try it on Linux** and report what breaks.
- **Share eval results** from another harness or model, favorable or not ([template](https://github.com/brennengreen/speedread/issues/new?template=eval_report.yml)).
- **Pick up a [ROADMAP.md](ROADMAP.md) item.** Each one lists its scope and the skills it needs.

## Setup

You need Rust 1.90+ and a C compiler for the tree-sitter grammars (on macOS, Xcode's Command Line Tools).

```sh
git clone https://github.com/brennengreen/speedread && cd speedread
cargo build --profile fast        # quick local builds, no LTO: target/fast/speedread
cargo test
```

Before opening a pull request, run what CI runs:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
SPEEDREAD_WALKER=portable cargo test --all-targets   # on macOS, also exercises the portable walker
```

To try a build in an agent, point the client's MCP config at the absolute path of `target/fast/speedread` (or `target/release/speedread`).

Two debugging aids: `speedread debug <file> --sexp` prints a file's parsed symbols and its tree-sitter S-expression, and `speedread debug <file> --views` prints the size of each view (full, skeleton, compact, outline).

## Where things are

| File | What it does |
|---|---|
| `src/server.rs` | MCP server: the four tools, their argument schemas, and the model-facing instructions and descriptions |
| `src/main.rs` | CLI, mirroring the tools, plus `symbols` and JSON Lines output |
| `src/read.rs` | target parsing (`path:A-B`, `#Symbol`, globs, `@etag`) and the budget ladder |
| `src/render.rs` | line-number format, skeletons, comment and import folding |
| `src/outline.rs` | tree-sitter symbol extraction, per language |
| `src/structured.rs` | Markdown, JSON, YAML and TOML outlines |
| `src/search.rs`, `src/trace.rs`, `src/map.rs` | the other three primitives |
| `src/diff.rs` | symbol-labelled diffs for `path@etag` |
| `src/tokens.rs`, `src/engine.rs` | token estimator, tokenizer profiles, caches |
| `src/walk.rs`, `src/macos.rs` | the macOS `getattrlistbulk` walker, the portable walker, process tuning |
| `src/workspace.rs` | workspace roots and the path policy |
| `src/lang.rs` | language detection |
| `tests/` | MCP protocol tests (`mcp.rs`) and outline snapshots (`outlines.rs`, `fixtures/`) |
| `evals/` | the eval suites, their tasks and committed results |

## Model-facing text is product behavior

The server instructions and tool descriptions in `src/server.rs` (`INSTRUCTIONS`, `READ_DESC`, `SEARCH_DESC`, `MAP_DESC`, `TRACE_DESC`) and the `description` in `skills/speedread/SKILL.md` are read by the model. They decide whether an agent uses speedread at all, and in the evals adoption decided everything: installed without guidance, speedread was used in 0 of 26 trials. They are also paid for on every model call.

So changes to these strings:

- go in their own pull request;
- say what you expect to change;
- include before/after eval numbers, or ask for an eval run before merging;
- note their token cost.

## Adding or improving a language

1. Add the grammar crate to `Cargo.toml`.
2. In `src/lang.rs`, add a `LangId` variant with its `name()` and `grammar()`, and its extensions or file names in `detect()` (and `detect_shebang()` for scripts).
3. In `src/outline.rs`, map the grammar's definition nodes to symbols in `Extractor::classify`. `Extractor::lua` is a small example. Set `comment_prefix` if comments don't start with `//`.
4. In `src/render.rs`, teach `line_kind` the comment syntax if it isn't `//`, `/*` or one of the `#` languages, so compact skeletons fold comments.
5. In `src/trace.rs`, check how `attribute` resolves receivers for the new language; languages are grouped there by how method calls name their receiver.
6. Add `tests/fixtures/Sample.<ext>` with the constructs you mapped, run `UPDATE_SNAPSHOTS=1 cargo test --test outlines`, and review the generated `.outline` file line by line.
7. Add the language to the list in the README (How it works → Outlines).

To fix an outline in a language that's already supported, steps 3 and 6 are usually enough: add the construct that goes wrong to the fixture first, so the snapshot diff shows the fix.

## Adding or verifying a client

- Check the configuration against the client's current version, and add or fix its row in the README's Configuration table.
- Document how to make speedread the reader in that client, under "Make it the reader": a way to exclude or deny the built-in read and search tools, or an instructions file it honors.
- If the client only ever runs one model family, consider `Tokenizer::for_client` in `src/engine.rs`, which picks the budget calibration from the client's name.
- A client is "configured" once its setup is verified, and "measured" only after an eval runs in it. Please keep that distinction in the docs.

## Evals

[`evals/README.md`](evals/README.md) describes the suites and how to run them. The real-agent suites currently drive GitHub Copilot CLI and read exact per-call token usage from its session store; supporting other harnesses is on the [roadmap](ROADMAP.md).

If you run evals or cite a number:

- Commit or link the raw data (trials, transcripts, diffs) behind every number.
- Keep results that don't favor speedread. If a trial or task turns out to be invalid, move it to an `excluded/` folder with a note explaining why, as in [`evals/results/coding-claude-sonnet-5/excluded/`](evals/results/coding-claude-sonnet-5/excluded/), rather than deleting it.
- Report uncertainty: trials per arm, and intervals from `evals/stats.py` where they apply.
- Regenerate the README charts with `python3 demo/build.py`.

## Pull requests

- One topic per pull request, with tests for behavior changes.
- Update the README and `skills/speedread/SKILL.md` when flags, targets or output formats change.
- Any number you add to the docs should link to committed eval data.

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
