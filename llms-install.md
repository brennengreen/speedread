# Installing speedread (instructions for AI agents)

These steps are for an AI agent that is installing speedread for its user. speedread is an MCP server (stdio) and CLI. It's built for macOS on Apple Silicon, and builds from source on Linux.

## 1. Install the binary

Use the first option that works:

1. Prebuilt, macOS on Apple Silicon:
   ```sh
   mkdir -p ~/.local/bin
   curl -fsSL https://github.com/brennengreen/speedread/releases/latest/download/speedread-aarch64-apple-darwin.tar.gz | tar -xz -C ~/.local/bin
   ```
   Make sure `~/.local/bin` is on `PATH`.
2. Homebrew: `brew install brennengreen/tap/speedread`. Use the full name: `brew install speedread` installs an unrelated program.
3. From source, macOS or Linux (Rust 1.90+): `cargo install --locked --git https://github.com/brennengreen/speedread`

Check: `speedread --version` prints `speedread 0.1.0` or later.

## 2. Register the MCP server

The server command is `speedread mcp`. It needs no API keys or environment variables.

| Client | How |
|---|---|
| GitHub Copilot CLI | `copilot mcp add speedread -- speedread mcp` |
| Claude Code | `claude mcp add --scope user speedread -- speedread mcp` |
| Cline, Cursor, Gemini CLI | add `{"mcpServers": {"speedread": {"command": "speedread", "args": ["mcp"]}}}` to the client's MCP settings |
| VS Code | `.vscode/mcp.json`: `{"servers": {"speedread": {"type": "stdio", "command": "speedread", "args": ["mcp"]}}}` |
| Codex CLI | `~/.codex/config.toml`: `[mcp_servers.speedread]` with `command = "speedread"` and `args = ["mcp"]` |
| Claude Desktop | open `speedread-aarch64-apple-darwin.mcpb` from the latest release, or use the absolute binary path with `"args": ["mcp", "--root", "/path/to/project"]` |

GUI apps may not inherit the shell's `PATH`; use the absolute path from `which speedread`.

## 3. Make it the reader

speedread saves tokens only when the agent uses it instead of the built-in read and search tools. In the evals, installing it without that made runs more expensive. Either remove the built-in readers (Copilot CLI: `copilot --excluded-tools view grep glob`; Claude Code: `claude --disallowedTools Grep Glob`, keeping `Read` because `Edit` requires it), or add this to the project's `AGENTS.md`, `CLAUDE.md`, `.clinerules` or `.github/copilot-instructions.md`:

```markdown
## Reading code
Use the speedread MCP tools to read, search and navigate code; use built-in tools only to edit and run commands.
- `read` everything you need (files, `path:A-B`, `path#Name`, `#Name`) in ONE call; expand skeletons with `path#Name`.
- `search` finds text (hits show their enclosing function); `trace` follows callers, callees and implementations.
- After editing, `read path@etag` to see only what changed.
```

## 4. Verify

Call the `map` tool on the workspace: it returns a budgeted tree of the repository. Reads outside the workspace roots are refused unless the server was started with `--unrestricted`.
