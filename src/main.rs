use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};

use speedread::engine::{Config, Engine};
use speedread::read::{Mode, ReadRequest};
use speedread::search::{Case, Output, SearchRequest};
use speedread::workspace::Workspace;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(
    name = "speedread",
    version,
    about = "The fastest, most token-efficient file reader for AI coding agents (MCP server + CLI), built for macOS."
)]
struct Cli {
    /// Workspace root (repeatable). Default: $CLAUDE_PROJECT_DIR, else the current directory.
    #[arg(long = "root", global = true, value_name = "DIR")]
    roots: Vec<PathBuf>,
    /// Allow reading anywhere, not just inside the roots.
    #[arg(long, global = true)]
    unrestricted: bool,
    /// Don't allow read-only access to dependency caches (cargo registry, SwiftPM checkouts, SDKs, Go modules).
    #[arg(long, global = true)]
    no_deps: bool,
    /// Default token budget per call (map defaults to 3000).
    #[arg(long, global = true, default_value_t = 8000)]
    budget: usize,
    #[arg(skip)]
    budget_set: bool,
    /// Upper limit for budgets requested by agents (~26 KB keeps results inline in every client).
    #[arg(long, global = true, default_value_t = 10000)]
    max_budget: usize,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Auto,
    Full,
    Skeleton,
    Outline,
}

#[derive(Clone, Copy, ValueEnum)]
enum DirectionArg {
    Callers,
    Callees,
    Refs,
    Impls,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputArg {
    Matches,
    Symbols,
    Files,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the MCP server on stdio (the default when stdin is not a terminal).
    Mcp,
    /// Read targets: path, path:A-B, path:LINE, path#Symbol, #Symbol, glob, path@etag.
    Read {
        #[arg(required = true)]
        targets: Vec<String>,
        #[arg(long, value_enum, default_value = "auto")]
        mode: ModeArg,
        /// Omit line numbers.
        #[arg(long)]
        no_numbers: bool,
    },
    /// Search file contents; hits are grouped by enclosing symbol.
    Search {
        pattern: String,
        paths: Vec<String>,
        #[arg(short = 'g', long = "glob")]
        globs: Vec<String>,
        #[arg(short = 'F', long)]
        literal: bool,
        #[arg(short = 'w', long)]
        word: bool,
        #[arg(short = 'i', long)]
        ignore_case: bool,
        #[arg(short = 's', long)]
        case_sensitive: bool,
        #[arg(short = 'C', long, default_value_t = 0)]
        context: usize,
        #[arg(long, value_enum, default_value = "matches")]
        output: OutputArg,
        /// Emit JSON Lines (unbudgeted) for scripts: one record per match/symbol/file.
        #[arg(long)]
        json: bool,
    },
    /// Directory overview with line counts (and top-level symbols with --symbols).
    Map {
        path: Option<String>,
        #[arg(long)]
        symbols: bool,
        #[arg(long)]
        depth: Option<usize>,
        #[arg(short = 'g', long = "glob")]
        globs: Vec<String>,
        /// Emit JSON Lines (unbudgeted): {path, bytes, lines} per file.
        #[arg(long)]
        json: bool,
    },
    /// Relationships of a symbol: callers, callees, references or implementations.
    Trace {
        /// `#Name`, `Type.method`, `path#Name` or `path:LINE`.
        target: String,
        #[arg(short = 'd', long, value_enum, default_value = "callers")]
        direction: DirectionArg,
        /// Levels to follow (1-3) for callers/callees/impls.
        #[arg(long, default_value_t = 1)]
        depth: usize,
    },
    /// List symbols (functions, types, headings, keys) in files, directories or globs.
    Symbols {
        targets: Vec<String>,
        /// Emit JSON Lines: {path, name, qualified, kind, start, end, def, depth, signature}.
        #[arg(long)]
        json: bool,
    },
    /// Walk a directory and report file count and time (benchmarking).
    #[command(hide = true)]
    BenchWalk { path: PathBuf },
    /// Print a file's parsed symbols (debugging).
    #[command(hide = true)]
    Debug {
        file: PathBuf,
        /// Also print the tree-sitter S-expression.
        #[arg(long)]
        sexp: bool,
        /// Print the size of each view (full, skeleton, compact, outline).
        #[arg(long)]
        views: bool,
    },
}

fn main() -> anyhow::Result<()> {
    speedread::macos::tune_process();
    speedread::macos::init_thread_pool();
    let mut cli = Cli::parse();
    cli.budget_set = std::env::args().any(|a| a == "--budget" || a.starts_with("--budget="));
    let mut roots = cli.roots.clone();
    if roots.is_empty()
        && let Some(dir) = std::env::var_os("CLAUDE_PROJECT_DIR")
    {
        roots.push(PathBuf::from(dir));
    }
    let ws = Workspace::new(roots, cli.unrestricted, !cli.no_deps)?;
    let cfg = Config {
        default_budget: cli.budget,
        max_budget: cli.max_budget.max(cli.budget),
        ..Config::default()
    };
    let engine = Arc::new(Engine::new(ws, cfg));
    // SPEEDREAD_TOKENIZER=claude|openai|legacy pins the budget calibration
    // (default: detect from the MCP client, else legacy).
    if let Some(t) = std::env::var("SPEEDREAD_TOKENIZER")
        .ok()
        .and_then(|v| speedread::engine::Tokenizer::parse(&v))
    {
        engine.set_tokenizer(t, true);
    }
    let budget = Some(cli.budget);
    let cli_budget = cli.budget_set.then_some(cli.budget);
    let cmd = match cli.cmd {
        Some(c) => c,
        None if !std::io::stdin().is_terminal() => Cmd::Mcp,
        None => {
            use clap::CommandFactory;
            Cli::command().print_help()?;
            return Ok(());
        }
    };
    match cmd {
        Cmd::Mcp => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .on_thread_start(speedread::macos::set_thread_qos)
                .enable_all()
                .build()?;
            rt.block_on(speedread::server::serve(engine))?;
        }
        Cmd::Read {
            targets,
            mode,
            no_numbers,
        } => {
            let mode = match mode {
                ModeArg::Auto => Mode::Auto,
                ModeArg::Full => Mode::Full,
                ModeArg::Skeleton => Mode::Skeleton,
                ModeArg::Outline => Mode::Outline,
            };
            let req = ReadRequest {
                targets,
                mode,
                budget,
                numbers: !no_numbers,
            };
            print!("{}", speedread::read::read(&engine, &req));
        }
        Cmd::Search {
            pattern,
            paths,
            globs,
            literal,
            word,
            ignore_case,
            case_sensitive,
            context,
            output,
            json,
        } => {
            let req = SearchRequest {
                pattern,
                paths,
                globs,
                literal,
                word,
                case: if ignore_case {
                    Case::Insensitive
                } else if case_sensitive {
                    Case::Sensitive
                } else {
                    Case::Smart
                },
                context,
                output: match output {
                    OutputArg::Matches => Output::Matches,
                    OutputArg::Symbols => Output::Symbols,
                    OutputArg::Files => Output::Files,
                },
                budget,
                max_matches: 5000,
            };
            if json {
                let max = if req.max_matches == 5000 {
                    1_000_000
                } else {
                    req.max_matches
                };
                let req = SearchRequest {
                    max_matches: max,
                    ..req
                };
                emit(|w| speedread::json::search(&engine, &req, w))?;
            } else {
                print!("{}", speedread::search::search(&engine, &req));
            }
        }
        Cmd::Map {
            path,
            symbols,
            depth,
            globs,
            json,
        } => {
            if json {
                return emit(|w| {
                    speedread::json::files(&engine, path.as_deref(), &globs, depth, w)
                });
            }
            let budget = cli_budget.or(Some(speedread::map::DEFAULT_MAP_BUDGET));
            let req = speedread::map::MapRequest {
                path,
                depth,
                symbols,
                globs,
                budget,
            };
            print!("{}", speedread::map::map(&engine, &req));
        }
        Cmd::Trace {
            target,
            direction,
            depth,
        } => {
            use speedread::trace::{Direction, TraceRequest};
            let req = TraceRequest {
                target,
                direction: match direction {
                    DirectionArg::Callers => Direction::Callers,
                    DirectionArg::Callees => Direction::Callees,
                    DirectionArg::Refs => Direction::Refs,
                    DirectionArg::Impls => Direction::Impls,
                },
                depth,
                budget: cli_budget,
            };
            print!("{}", speedread::trace::trace(&engine, &req));
        }
        Cmd::Symbols { targets, json } => {
            if json {
                return emit(|w| speedread::json::symbols(&engine, &targets, w));
            }
            let records =
                speedread::json::symbol_records(&engine, &targets).map_err(anyhow::Error::msg)?;
            let mut out = std::io::BufWriter::new(std::io::stdout().lock());
            use std::io::Write;
            for v in &records {
                let r = writeln!(
                    out,
                    "{}:{}-{}\t{}\t{}\t{}",
                    v["path"].as_str().unwrap_or(""),
                    v["start"],
                    v["end"],
                    v["kind"].as_str().unwrap_or(""),
                    v["qualified"].as_str().unwrap_or(""),
                    v["signature"].as_str().unwrap_or("")
                );
                if r.is_err() {
                    break;
                }
            }
            let _ = out.flush();
        }
        Cmd::BenchWalk { path } => {
            let t = std::time::Instant::now();
            let (files, _) = speedread::walk::list_files(&path, &Default::default(), usize::MAX)?;
            let bytes: u64 = files.iter().map(|f| f.size).sum();
            println!(
                "{} files, {} bytes, {:.2?}",
                files.len(),
                bytes,
                t.elapsed()
            );
        }
        Cmd::Debug { file, sexp, views } => debug(&engine, &file, sexp, views)?,
    }
    Ok(())
}

/// Stream JSON Lines to stdout; a closed pipe (`| head`) is not an error.
fn emit(
    f: impl FnOnce(&mut std::io::BufWriter<std::io::StdoutLock<'static>>) -> Result<usize, String>,
) -> anyhow::Result<()> {
    use std::io::Write;
    let mut w = std::io::BufWriter::with_capacity(1 << 16, std::io::stdout().lock());
    match f(&mut w) {
        Ok(_) => {}
        Err(e) if e.contains("Broken pipe") => return Ok(()),
        Err(e) => anyhow::bail!(e),
    }
    match w.flush() {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        r => Ok(r?),
    }
}

fn debug(engine: &Engine, file: &std::path::Path, sexp: bool, views: bool) -> anyhow::Result<()> {
    let src = speedread::source::Source::load(file)?;
    println!(
        "lang={:?} lines={} binary={} etag={:016x}",
        src.lang,
        src.line_count(),
        src.binary,
        src.etag()
    );
    if sexp
        && let Some(lang) = src.lang
        && let Some(g) = lang.grammar()
    {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&g)?;
        if let Some(t) = p.parse(&src.data, None) {
            println!("{}", t.root_node().to_sexp());
        }
    }
    if views {
        if let Some(o) = engine.outline(&src) {
            use speedread::render;
            let n = src.line_count().saturating_sub(1);
            let full = render::numbered_bytes(&src, 0, n, true);
            let sk = render::skeleton(&src, &o, 0, n, true).len();
            let cp = render::compact_skeleton(&src, &o, 0, n, true).len();
            let ol = render::outline_text(&o, 0, n, None).len();
            let top = render::outline_text(&o, 0, n, Some(0)).len();
            println!(
                "bytes: full={full} skeleton={sk} compact={cp} outline={ol} top={top} (≈tokens: {} / {} / {} / {} / {})",
                engine.tokens(full),
                engine.tokens(sk),
                engine.tokens(cp),
                engine.tokens(ol),
                engine.tokens(top)
            );
        }
        return Ok(());
    }
    let t = std::time::Instant::now();
    let o = engine.outline(&src);
    let dt = t.elapsed();
    match o {
        Some(o) => {
            for (i, s) in o.symbols.iter().enumerate() {
                println!(
                    "{}{:?} {} [{}-{}] def={} collapse={:?} q={} | {}",
                    "  ".repeat(s.depth as usize),
                    s.kind,
                    s.name,
                    s.start + 1,
                    s.end + 1,
                    s.def + 1,
                    s.collapse.map(|(a, b)| (a + 1, b + 1)),
                    o.qualified_name(i),
                    s.label
                );
            }
            eprintln!("{} symbols in {:.2?}", o.symbols.len(), dt);
        }
        None => println!("(no outline)"),
    }
    Ok(())
}
