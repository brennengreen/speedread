//! MCP server: four tools (`read`, `search`, `map`, `trace`) over stdio.

use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeRequestParams, InitializeResult,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::{NotificationContext, Peer, RequestContext};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router,
};
use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer};

use crate::engine::Engine;
use crate::read::{Mode, ReadRequest};
use crate::search::{Case, Output, SearchRequest};

pub const INSTRUCTIONS: &str = "speedread: fast, token-budgeted code reading. Prefer its tools over shell cat/sed/head/grep/find/ls and over one-file-at-a-time reads. Batch every file, range and symbol you need into ONE `read` call (`path`, `path:A-B`, `path#Symbol`, `#Symbol`, globs); oversized files come back as skeletons you expand by symbol instead of being cut off. `search` hits show their enclosing function/class; `trace` follows callers, callees, references and implementations; `map` orients you in a repo. After editing a file, read `path@etag` (etag from the `==>` header) to see only what changed.";

const READ_DESC: &str = "Read files token-efficiently. Use instead of cat/head/sed or one-file-at-a-time reads: put everything you need in ONE call (targets share the budget).
Targets:
- `src/app.ts` whole file. Too big for the budget → returns a skeleton (signatures, types, docs; bodies collapsed as `A-B ⋯`), never a blind cut
- `src/app.ts:120-180` lines; `src/app.ts:120` (or `file:line:col`) the function/class enclosing that line
- `src/app.ts#handleRequest`, `src/app.ts#Server.start` a symbol's full source incl. docs/decorators; `#handleRequest` alone finds its definition anywhere
- `README.md#Install` a section, `package.json#scripts` a key, `src/**/*.test.ts` a glob
- `src/app.ts@9f86d081884c7d65` only what changed since the version whose @etag appeared in a header (after an edit), or lines appended to a log
Output: `==> path @etag (N lines)` then `line<TAB>text`.";

const SEARCH_DESC: &str = "Search file contents (ripgrep engine, .gitignore-aware; regex by default, literal=true for exact text). Hits are grouped by file and under their enclosing function/class with its line range, e.g. `[20-80] fn parse_header(...)`, so your next step can be read `path#parse_header` instead of guessing ranges. output=symbols returns the full source of every enclosing symbol in this same call; output=files lists paths with counts.";

const MAP_DESC: &str = "Directory overview (.gitignore-aware): files with line counts, subdirectories expanded breadth-first until the budget is used, the rest summarized with file counts. symbols=true adds each file's top-level definitions (a compact repo map). Use first in an unfamiliar codebase instead of ls/find/tree.";

const TRACE_DESC: &str = "Follow code relationships instead of searching and opening files one by one: callers, callees, references or implementations of a symbol, grouped by enclosing function with line ranges.
- direction=callers (default): call sites; depth 2-3 builds the call tree upward
- callees: each call in the body, resolved to its definition
- refs: all uses incl. imports and type mentions
- impls: subclasses / trait, protocol and interface implementations (Go: structural); a method target lists each override
Syntactic (tree-sitter), no type inference: same-named definitions are told apart by receiver, class and package; `?` marks unresolved receivers.";

fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(String),
        Many(Vec<String>),
    }
    Ok(match V::deserialize(d)? {
        V::One(s) => {
            // Tolerate a JSON-encoded array passed as a string.
            if s.trim_start().starts_with('[')
                && let Ok(v) = serde_json::from_str::<Vec<String>>(&s)
            {
                v
            } else {
                vec![s]
            }
        }
        V::Many(v) => v,
    })
}

fn opt_one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        One(String),
        Many(Vec<String>),
        Null(()),
    }
    Ok(match Option::<V>::deserialize(d)? {
        Some(V::One(s)) if s.is_empty() => None,
        Some(V::One(s)) => Some(vec![s]),
        Some(V::Many(v)) => Some(v),
        Some(V::Null(())) | None => None,
    })
}

fn lenient_usize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<usize>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        N(f64),
        S(String),
    }
    Ok(match Option::<V>::deserialize(d)? {
        Some(V::N(n)) if n >= 0.0 => Some(n as usize),
        Some(V::S(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

fn lenient_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum V {
        B(bool),
        S(String),
    }
    Ok(match Option::<V>::deserialize(d)? {
        Some(V::B(b)) => Some(b),
        Some(V::S(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        None => None,
    })
}

#[derive(Debug, Deserialize)]
pub struct ReadArgs {
    #[serde(
        deserialize_with = "one_or_many",
        alias = "paths",
        alias = "path",
        alias = "files",
        alias = "target"
    )]
    pub targets: Vec<String>,
    #[serde(default)]
    pub mode: Option<Mode>,
    #[serde(default, deserialize_with = "lenient_usize")]
    pub budget: Option<usize>,
    #[serde(default, deserialize_with = "lenient_bool")]
    pub line_numbers: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SearchArgs {
    #[serde(alias = "query", alias = "regex")]
    pub pattern: String,
    #[serde(default, deserialize_with = "opt_one_or_many", alias = "path")]
    pub paths: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "opt_one_or_many",
        alias = "glob",
        alias = "include"
    )]
    pub globs: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lenient_bool")]
    pub literal: Option<bool>,
    #[serde(default, deserialize_with = "lenient_bool")]
    pub word: Option<bool>,
    #[serde(default)]
    pub case: Option<Case>,
    #[serde(default, deserialize_with = "lenient_usize")]
    pub context: Option<usize>,
    #[serde(default)]
    pub output: Option<Output>,
    #[serde(default, deserialize_with = "lenient_usize")]
    pub budget: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct MapArgs {
    #[serde(default, alias = "dir", alias = "directory")]
    pub path: Option<String>,
    #[serde(default, deserialize_with = "lenient_bool")]
    pub symbols: Option<bool>,
    #[serde(default, deserialize_with = "lenient_usize")]
    pub depth: Option<usize>,
    #[serde(default, deserialize_with = "opt_one_or_many", alias = "glob")]
    pub globs: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lenient_usize")]
    pub budget: Option<usize>,
}

fn lenient_direction<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<crate::trace::Direction>, D::Error> {
    use crate::trace::Direction;
    let s = Option::<String>::deserialize(d)?;
    Ok(
        s.and_then(|s| match s.trim().to_ascii_lowercase().as_str() {
            "callers" | "caller" | "incoming" | "up" => Some(Direction::Callers),
            "callees" | "callee" | "calls" | "outgoing" | "down" => Some(Direction::Callees),
            "refs" | "references" | "usages" | "uses" => Some(Direction::Refs),
            "impls" | "implementations" | "implementers" | "subclasses" | "overrides" => {
                Some(Direction::Impls)
            }
            _ => None,
        }),
    )
}

#[derive(Debug, Deserialize)]
pub struct TraceArgs {
    #[serde(alias = "symbol", alias = "name", alias = "targets")]
    pub target: String,
    #[serde(default, deserialize_with = "lenient_direction")]
    pub direction: Option<crate::trace::Direction>,
    #[serde(default, deserialize_with = "lenient_usize")]
    pub depth: Option<usize>,
    #[serde(default, deserialize_with = "lenient_usize")]
    pub budget: Option<usize>,
}

impl JsonSchema for TraceArgs {
    fn schema_name() -> Cow<'static, str> {
        "TraceArgs".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "description": "`#Name`, `Type.method`, `path#Name` or `path:LINE`."},
                "direction": {"type": "string", "enum": ["callers", "callees", "refs", "impls"], "description": "Default callers."},
                "depth": {"type": "integer", "description": "Levels to follow, 1-3 (default 1)."},
                "budget": {"type": "integer", "description": "Max response tokens (default 4000)."}
            },
            "required": ["target"]
        })
    }
}

impl JsonSchema for ReadArgs {
    fn schema_name() -> Cow<'static, str> {
        "ReadArgs".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "targets": {"type": "array", "items": {"type": "string"}, "description": "Everything to read, in one call (syntax above)."},
                "mode": {"type": "string", "enum": ["auto", "full", "skeleton", "outline"], "description": "auto (default): full if it fits, else skeleton, else outline. full: never collapse (paginates). skeleton|outline: structure only."},
                "budget": {"type": "integer", "description": "Max response tokens (default 8000)."},
                "line_numbers": {"type": "boolean", "description": "Default true."}
            },
            "required": ["targets"]
        })
    }
}

impl JsonSchema for SearchArgs {
    fn schema_name() -> Cow<'static, str> {
        "SearchArgs".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex (Rust syntax), or exact text with literal=true."},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Files/dirs to search (default: workspace root)."},
                "globs": {"type": "array", "items": {"type": "string"}, "description": "Include/exclude globs, e.g. [\"*.ts\", \"!**/*.test.ts\"]."},
                "literal": {"type": "boolean", "description": "Exact text, not regex."},
                "word": {"type": "boolean", "description": "Whole words only."},
                "case": {"type": "string", "enum": ["smart", "sensitive", "insensitive"], "description": "smart (default): insensitive unless the pattern has uppercase."},
                "context": {"type": "integer", "description": "Context lines per hit (default 0)."},
                "output": {"type": "string", "enum": ["matches", "symbols", "files"], "description": "matches (default) | symbols: full source of each enclosing symbol | files: paths + counts."},
                "budget": {"type": "integer", "description": "Max response tokens (default 8000)."}
            },
            "required": ["pattern"]
        })
    }
}

impl JsonSchema for MapArgs {
    fn schema_name() -> Cow<'static, str> {
        "MapArgs".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory (default: workspace root); a file returns its outline."},
                "symbols": {"type": "boolean", "description": "Add each file's top-level definitions."},
                "depth": {"type": "integer", "description": "Max depth to expand (default: as far as the budget allows)."},
                "globs": {"type": "array", "items": {"type": "string"}, "description": "Include/exclude globs, e.g. [\"*.swift\"]."},
                "budget": {"type": "integer", "description": "Max response tokens (default 3000)."}
            }
        })
    }
}

/// Client workspace roots (MCP `roots/list`) are fetched once, lazily, before
/// the first tool call, so VS Code/Cursor folders and Claude Code
/// `--add-dir` directories work without configuration.
#[derive(Default)]
struct RootsState {
    done: std::sync::atomic::AtomicBool,
    attempts: std::sync::atomic::AtomicU8,
    lock: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct Server {
    engine: Arc<Engine>,
    roots: Arc<RootsState>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Server>,
}

async fn blocking<F>(f: F) -> Result<CallToolResult, McpError>
where
    F: FnOnce() -> String + Send + 'static,
{
    let text = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| McpError::internal_error(format!("speedread worker failed: {e}"), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
}

#[tool_router]
impl Server {
    pub fn new(engine: Arc<Engine>) -> Self {
        Server {
            engine,
            roots: Arc::new(RootsState::default()),
            tool_router: Self::tool_router(),
        }
    }

    // Roots are deprecated by the 2026-07-28 spec (SEP-2577) but still what
    // Claude Code, VS Code and Cursor send.
    #[allow(deprecated)]
    async fn ensure_roots(&self, peer: &Peer<RoleServer>) {
        use std::sync::atomic::Ordering;
        if self.roots.done.load(Ordering::Acquire) {
            return;
        }
        let _guard = self.roots.lock.lock().await;
        if self.roots.done.load(Ordering::Acquire) {
            return;
        }
        let supported = peer
            .peer_info()
            .is_some_and(|info| info.capabilities.roots.is_some());
        if supported {
            let fetch = peer.list_roots();
            match tokio::time::timeout(std::time::Duration::from_secs(2), fetch).await {
                Ok(Ok(res)) => {
                    let paths = res
                        .roots
                        .iter()
                        .filter_map(|r| crate::workspace::file_uri_to_path(&r.uri))
                        .collect();
                    self.engine.ws.set_client_roots(paths);
                }
                // Timeout or error: retry on a later call (a few times).
                _ if self.roots.attempts.fetch_add(1, Ordering::AcqRel) < 2 => return,
                _ => {}
            }
        }
        self.roots.done.store(true, Ordering::Release);
    }

    #[tool(
        name = "read",
        description = READ_DESC,
        annotations(title = "Read files", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn read(
        &self,
        Parameters(args): Parameters<ReadArgs>,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_roots(&peer).await;
        let engine = self.engine.clone();
        let req = ReadRequest {
            targets: args.targets,
            mode: args.mode.unwrap_or_default(),
            budget: args.budget,
            numbers: args.line_numbers.unwrap_or(true),
        };
        blocking(move || crate::read::read(&engine, &req)).await
    }

    #[tool(
        name = "search",
        description = SEARCH_DESC,
        annotations(title = "Search code", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_roots(&peer).await;
        let engine = self.engine.clone();
        let req = SearchRequest {
            pattern: args.pattern,
            paths: args.paths.unwrap_or_default(),
            globs: args.globs.unwrap_or_default(),
            literal: args.literal.unwrap_or(false),
            word: args.word.unwrap_or(false),
            case: args.case.unwrap_or_default(),
            context: args.context.unwrap_or(0),
            output: args.output.unwrap_or_default(),
            budget: args.budget,
            max_matches: 5000,
        };
        blocking(move || crate::search::search(&engine, &req)).await
    }

    #[tool(
        name = "map",
        description = MAP_DESC,
        annotations(title = "Map directory", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn map(
        &self,
        Parameters(args): Parameters<MapArgs>,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_roots(&peer).await;
        let engine = self.engine.clone();
        let req = crate::map::MapRequest {
            path: args.path,
            depth: args.depth,
            symbols: args.symbols.unwrap_or(false),
            globs: args.globs.unwrap_or_default(),
            budget: args.budget,
        };
        blocking(move || crate::map::map(&engine, &req)).await
    }

    #[tool(
        name = "trace",
        description = TRACE_DESC,
        annotations(title = "Trace relationships", read_only_hint = true, destructive_hint = false, idempotent_hint = true, open_world_hint = false)
    )]
    async fn trace(
        &self,
        Parameters(args): Parameters<TraceArgs>,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_roots(&peer).await;
        let engine = self.engine.clone();
        let req = crate::trace::TraceRequest {
            target: args.target,
            direction: args.direction.unwrap_or_default(),
            depth: args.depth.unwrap_or(1),
            budget: args.budget,
        };
        blocking(move || crate::trace::trace(&engine, &req)).await
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("speedread", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // Clients that only run one model family get budgets calibrated to its
        // tokenizer (Claude 4.7+ counts ~1.4× more tokens than the estimate).
        if let Some(t) = crate::engine::Tokenizer::for_client(&request.client_info.name) {
            self.engine.set_tokenizer(t, false);
        }
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn on_roots_list_changed(&self, _context: NotificationContext<RoleServer>) {
        self.roots
            .attempts
            .store(0, std::sync::atomic::Ordering::Release);
        self.roots
            .done
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

pub async fn serve(engine: Arc<Engine>) -> anyhow::Result<()> {
    let service = Server::new(engine).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}
