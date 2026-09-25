# Roadmap

Work that would make speedread more useful or better evidenced, written so you can tell whether you can help. The README's [Limitations](README.md#limitations-and-roadmap) section is the short version.

Sizes: **S** is an afternoon, **M** a few days, **L** a design discussion first. If you want to take something on, comment on its issue (or open one) so work isn't duplicated.

| Item | Size | First contribution? | Needs |
|---|:-:|:-:|---|
| [Verify a client setup](#verify-a-client-setup) | S | yes | the client |
| [Linux: gate macOS-only walker helpers](#linux-gate-macos-only-walker-helpers) | S | yes | Rust basics |
| [Linux: run the tests, report or fix](#linux-run-the-tests-report-or-fix) | S–M | yes | a Linux machine, some Rust |
| [Outline fixes for a language](#outline-fixes-for-a-language) | S | yes | the language, reading tree-sitter trees |
| [Claude Code: does a ranged `Read` unlock `Edit`?](#claude-code-does-a-ranged-read-unlock-edit) | S | yes | Claude Code |
| [Linux in CI, and more release binaries](#linux-in-ci-and-more-release-binaries) | M | — | GitHub Actions, Rust cross-builds |
| [Evals in another harness](#evals-in-another-harness) | M–L | — | Python, the harness's logs, API budget |
| [More tasks and trials](#more-tasks-and-trials) | M | — | the eval suites, API budget |
| [Leaner definitions and unprompted adoption](#leaner-definitions-and-unprompted-adoption) | M–L | — | evals; see the model-facing rule in CONTRIBUTING |
| [Exact relationships: an optional LSP/SCIP layer](#exact-relationships-an-optional-lspscip-layer) | L | — | LSP or SCIP, async Rust, `src/trace.rs` |
| [Calls inside closures](#calls-inside-closures) | M | — | tree-sitter, `src/trace.rs` |
| [Windows](#windows) | L | — | Rust on Windows |

## Verify a client setup

The README gives a configuration for VS Code, Cursor, Codex CLI, Gemini CLI, Zed and Claude Desktop. Only GitHub Copilot CLI has been measured, and client configuration formats change.

- **Done when:** the client's row in README → Configuration is confirmed (or fixed) for a named client version, and "Make it the reader" says how to make speedread the reader there: a way to exclude or deny the built-in read and search tools, or the instructions file the client honors.
- **Start:** follow the row, ask the agent a question about a repository, and check which tools it called.
- One client per pull request.

## Linux: gate macOS-only walker helpers

On Linux, `cargo clippy --all-targets -- -D warnings`, the check CI enforces on macOS, fails with five `dead_code` errors in `src/walk.rs`: `IgnoreNode`, `is_ignored`, `load_matcher`, `global_node` and `initial_stack`. Only the macOS walker (`mod fast`) uses them, but unlike it they aren't behind `#[cfg(target_os = "macos")]`. Nothing else fails. This blocks running the same checks on Linux as on macOS, not using speedread there.

- **Done when:** those items are compiled only on macOS (a `#[cfg(target_os = "macos")]` on each, or moving them into `mod fast`), and clippy passes on both macOS and Linux.
- **Start:** the five warnings also appear in any Linux build, including the log of the [Linux (experimental)](.github/workflows/linux.yml) workflow.

## Linux: run the tests, report or fix

On Linux, macOS-specific code is compiled out and the portable walker (the `ignore` crate) is used. Everything compiles and links for x86_64 Linux (checked by cross-compiling from macOS), but the tests have never run on Linux.

- **Done when:** `cargo test --all-targets` passes on Linux, or each failure has an issue with its output.
- **Start:** run the [Linux (experimental)](.github/workflows/linux.yml) workflow on your fork (Actions → Run workflow), or `cargo test` on a Linux machine. Failures in tests that assume macOS behavior are usually small fixes; failures in the product are bugs worth reporting on their own.

## Outline fixes for a language

Skeletons, `path#Symbol` and `trace` all depend on the outline. If speedread misses or misnames a definition in your language (a decorator, an extension function, a template, a macro), that is a contained fix.

- **Done when:** a fixture in `tests/fixtures/` shows the construct, its snapshot is right, and nothing else changed.
- **Start:** `speedread debug <file> --sexp` shows what tree-sitter parsed. See "Adding or improving a language" in [CONTRIBUTING.md](CONTRIBUTING.md).

## Claude Code: does a ranged `Read` unlock `Edit`?

Claude Code requires a native `Read` of a file before `Edit` or `Write`, and MCP reads don't count. The evals charge a full default `Read` of every edited file, as an upper bound, which turns the bug-fix suite's −3%/−1% input tokens into +10%/+19%. If a `Read` of just the edit site satisfies the check, the real cost is much smaller.

- **Done when:** a short, reproducible test shows whether a ranged `Read` (with `offset`/`limit`) satisfies the check in a named Claude Code version, and the README's caveat is updated with the result.
- **Start:** ask Claude Code to edit a line it has only seen through speedread, with and without a ranged `Read` first.

## Linux in CI, and more release binaries

- **Done when:** a Linux job runs on every push and pull request, and releases attach Linux (x86_64 and arm64) and Intel macOS binaries next to the Apple Silicon one.
- **Start:** after the Linux tests pass, move the experimental job into `.github/workflows/ci.yml`, then extend the `release` job. For Intel macOS, the test suite already passes as an x86_64 build under Rosetta 2 (`rustup target add x86_64-apple-darwin`, then `cargo test --target x86_64-apple-darwin` on an Apple Silicon Mac), so building that target on the existing macOS runner is a likely path.

## Evals in another harness

Every agent result so far comes from one model in one harness: claude-sonnet-5 in GitHub Copilot CLI. This is the largest gap in the evidence.

- **Done when:** `evals/agent_eval.py` (and ideally `coding_eval.py`) can drive another harness, such as Claude Code, Codex CLI or Gemini CLI, with exact per-call token counts, and a run is committed with its raw data.
- **Needs:** a way to get exact input tokens per model call from that harness. The Copilot CLI adapter reads them from the session store; see how `agent_eval.py` collects usage.
- **Also useful on its own:** running the existing suites with another model in Copilot CLI.

## More tasks and trials

The relationship and bug-fix suites have 8 and 16 trials per arm, and most bug-fix differences are within noise.

- **Done when:** new tasks, each with a deterministic grader, are added to `evals/relationship_tasks.json` or `evals/coding_tasks.json`, bug-fix tasks pass `coding_eval.py --verify`, and results are committed with their transcripts.
- Tasks where speedread should *not* help are as valuable as ones where it should.

## Leaner definitions and unprompted adoption

speedread's tool definitions and instructions add about 2.2k tokens to every model call (net +0.9k when they replace view, grep and glob), and without guidance agents didn't use it at all.

- **Candidates:** shorter descriptions; A/B tests of tool names and descriptions for unprompted adoption; a single high-level `context` tool that picks map, search, trace or read itself.
- **Done when:** a change reduces definition tokens or raises unprompted adoption, with before/after eval numbers.
- These strings are model-facing; read the rule in [CONTRIBUTING.md](CONTRIBUTING.md) first.

## Exact relationships: an optional LSP/SCIP layer

`trace` is syntactic: tree-sitter plus name resolution by receiver, class and package, with no type inference. `x.f()` on an unknown receiver matches every `f` (marked `?`).

- **Goal:** exact references, overrides and call hierarchies from a language server or a SCIP index when one is available, behind the same `trace` interface and output format, with the syntactic path as the fallback.
- **Start with an issue:** which languages first, how servers are discovered and started, and how results stay within budget.

## Calls inside closures

Calls inside closures and lambdas are attributed to the enclosing named function, and functions passed as values aren't treated as calls. The eval graders accept either reading, but a user may want to tell them apart.

- **Done when:** `trace` can report, or mark, calls that happen inside a closure, with fixtures covering at least Go and JavaScript.

## Windows

Windows isn't supported: the code uses Unix-only APIs.

- **Scope:** byte-path handling via `std::os::unix::ffi::OsStrExt` (`src/search.rs`, `src/walk.rs`), `std::os::unix::fs::MetadataExt` for cache keys and mtimes (`src/source.rs`, `src/walk.rs`), `file://` URI decoding (`src/workspace.rs`), and path-policy semantics on Windows (drive letters, case, junctions).
- **Start with an issue** describing the approach before writing code.

## Upstream

Some limits live in other projects. Fixing them there helps everyone who uses those tools:

- **tree-sitter-objc:** a bare `NS_ASSUME_NONNULL_BEGIN` line makes the rest of a header parse as errors; in a six-line header, tree-sitter finds neither the `@interface` nor its method. speedread blanks a list of Apple macros before parsing (`APPLE_MACROS` in `src/outline.rs`); a grammar fix would make that unnecessary. Related: [tree-sitter-grammars/tree-sitter-objc#21](https://github.com/tree-sitter-grammars/tree-sitter-objc/issues/21).
