//! The `search` tool and workspace-wide definition lookup.
//!
//! ripgrep's matcher and searcher run on the macOS walker's threads. Hits are
//! grouped under their innermost enclosing symbol, so an agent can go
//! straight from a hit to `read path#symbol` — or ask for `output=symbols`
//! and get those bodies in the same call.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use parking_lot::Mutex;
use rayon::prelude::*;

use crate::engine::Engine;
use crate::outline::{Kind, Outline, split_segments};
use crate::render::{self, push_num};
use crate::source::Source;
use crate::util::{plural, thousands};
use crate::walk::{self, WalkOpts};
use crate::workspace::PathError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    /// Matching lines grouped by file and enclosing symbol.
    #[default]
    Matches,
    /// Full source of every symbol containing a match.
    Symbols,
    /// Paths with match counts only.
    Files,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Case {
    /// Case-insensitive unless the pattern has an uppercase letter.
    #[default]
    Smart,
    Sensitive,
    Insensitive,
}

/// A loaded file and its outline, prepared for rendering.
type Prepared = (Option<Arc<Source>>, Option<Arc<Outline>>);
/// A definition hit: source, outline, symbol index.
pub type DefHit = (Arc<Source>, Arc<Outline>, usize);

pub struct SearchRequest {
    pub pattern: String,
    pub paths: Vec<String>,
    pub globs: Vec<String>,
    pub literal: bool,
    pub word: bool,
    pub case: Case,
    pub context: usize,
    pub output: Output,
    pub budget: Option<usize>,
    pub max_matches: usize,
}

#[derive(Clone)]
struct Hit {
    line: u32,
    text: Vec<u8>,
    is_match: bool,
}

struct FileHits {
    path: PathBuf,
    hits: Vec<Hit>,
    matches: usize,
}

const MAX_HITS_PER_FILE: usize = 400;
/// Larger files are shown without enclosing-symbol grouping.
const MAX_GROUP_BYTES: usize = 1 << 20;

struct Collect {
    hits: Vec<Hit>,
    matches: usize,
}

impl Sink for Collect {
    type Error = io::Error;

    fn matched(&mut self, _s: &Searcher, m: &SinkMatch<'_>) -> Result<bool, io::Error> {
        self.matches += 1;
        if self.hits.len() < MAX_HITS_PER_FILE {
            let line = m.line_number().unwrap_or(0) as u32;
            // Multi-line matches are not enabled; take the first line.
            let bytes = m.bytes();
            let end = memchr::memchr(b'\n', bytes).unwrap_or(bytes.len());
            self.hits.push(Hit {
                line,
                text: trim_cr(&bytes[..end]).to_vec(),
                is_match: true,
            });
        }
        Ok(true)
    }

    fn context(&mut self, _s: &Searcher, c: &SinkContext<'_>) -> Result<bool, io::Error> {
        if self.hits.len() < MAX_HITS_PER_FILE {
            let line = c.line_number().unwrap_or(0) as u32;
            let bytes = c.bytes();
            let end = memchr::memchr(b'\n', bytes).unwrap_or(bytes.len());
            self.hits.push(Hit {
                line,
                text: trim_cr(&bytes[..end]).to_vec(),
                is_match: false,
            });
        }
        Ok(true)
    }
}

fn trim_cr(b: &[u8]) -> &[u8] {
    b.strip_suffix(b"\r").unwrap_or(b)
}

thread_local! {
    static SEARCHER: RefCell<Option<(usize, Searcher)>> = const { RefCell::new(None) };
}

fn with_searcher<R>(context: usize, f: impl FnOnce(&mut Searcher) -> R) -> R {
    SEARCHER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_none_or(|(c, _)| *c != context) {
            let s = SearcherBuilder::new()
                .line_number(true)
                .before_context(context)
                .after_context(context)
                .binary_detection(BinaryDetection::quit(b'\x00'))
                .build();
            *slot = Some((context, s));
        }
        f(&mut slot.as_mut().unwrap().1)
    })
}

/// Extensions that are never worth opening for a text search.
fn skip_ext(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "icns"
            | "heic"
            | "tiff"
            | "bmp"
            | "pdf"
            | "zip"
            | "gz"
            | "tgz"
            | "xz"
            | "bz2"
            | "zst"
            | "7z"
            | "rar"
            | "dmg"
            | "pkg"
            | "a"
            | "o"
            | "so"
            | "dylib"
            | "rlib"
            | "wasm"
            | "class"
            | "jar"
            | "mp3"
            | "mp4"
            | "mov"
            | "wav"
            | "aiff"
            | "ttf"
            | "otf"
            | "woff"
            | "woff2"
            | "sqlite"
            | "db"
            | "car"
            | "nib"
            | "mlmodel"
            | "pyc"
            | "exe"
            | "dll"
            | "bin"
            | "psd"
            | "sketch"
    )
}

fn build_matcher(req: &SearchRequest) -> Result<RegexMatcher, String> {
    let mut b = RegexMatcherBuilder::new();
    b.line_terminator(Some(b'\n'))
        .fixed_strings(req.literal)
        .word(req.word);
    match req.case {
        Case::Smart => {
            b.case_smart(true);
        }
        Case::Insensitive => {
            b.case_insensitive(true);
        }
        Case::Sensitive => {}
    }
    b.build(&req.pattern).map_err(|err| {
        let msg = err.to_string();
        let first = msg.lines().last().unwrap_or(&msg).trim().to_string();
        format!(
            "Invalid regex /{}/: {first}. Pass literal=true to search for the exact text.\n",
            req.pattern
        )
    })
}

struct Scan {
    files: Vec<FileHits>,
    searched: usize,
    stopped: bool,
}

fn scan(
    e: &Engine,
    roots: &[PathBuf],
    globs: &[String],
    matcher: &RegexMatcher,
    context: usize,
    max_matches: usize,
) -> Scan {
    let out = Mutex::new(Vec::new());
    let total = AtomicUsize::new(0);
    let searched = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let visit = |path: &Path| -> bool {
        if total.load(Ordering::Relaxed) >= max_matches {
            stopped.store(true, Ordering::Relaxed);
            return false;
        }
        searched.fetch_add(1, Ordering::Relaxed);
        let mut sink = Collect {
            hits: Vec::new(),
            matches: 0,
        };
        let res = with_searcher(context, |s| s.search_path(matcher, path, &mut sink));
        if res.is_ok() && sink.matches > 0 {
            total.fetch_add(sink.matches, Ordering::Relaxed);
            out.lock().push(FileHits {
                path: path.to_path_buf(),
                hits: sink.hits,
                matches: sink.matches,
            });
        }
        true
    };
    for root in roots {
        if root.is_file() {
            visit(root);
            continue;
        }
        let opts = WalkOpts {
            globs: globs.to_vec(),
            ..Default::default()
        };
        let _ = walk::walk(root, &opts, |entry| {
            if entry.dataless
                || skip_ext(&entry.path)
                || entry.size > crate::source::MAX_IN_MEMORY * 4
            {
                return true;
            }
            visit(&entry.path)
        });
    }
    let _ = e;
    let mut files = out.into_inner();
    files.sort_unstable_by(|a, b| {
        use std::os::unix::ffi::OsStrExt;
        a.path
            .as_os_str()
            .as_bytes()
            .cmp(b.path.as_os_str().as_bytes())
    });
    Scan {
        files,
        searched: searched.load(Ordering::Relaxed),
        stopped: stopped.load(Ordering::Relaxed),
    }
}

fn resolve_roots(e: &Engine, paths: &[String]) -> Result<Vec<PathBuf>, String> {
    if paths.is_empty() {
        return Ok(vec![e.ws.primary().to_path_buf()]);
    }
    let mut roots = Vec::new();
    for p in paths {
        match e.ws.resolve(p) {
            Ok(r) => roots.push(r),
            Err(PathError::NotFound(_)) => {
                let files = e.file_list();
                let sugg = crate::workspace::suggest(p, &files, 3);
                let mut t = format!("Path not found: {p}");
                if !sugg.is_empty() {
                    t.push_str(&format!(". Did you mean: {}", sugg.join(", ")));
                }
                t.push('\n');
                return Err(t);
            }
            Err(PathError::Outside(r)) => {
                return Err(format!("Path outside the workspace: {}\n", r.display()));
            }
        }
    }
    Ok(roots)
}

fn push_hit_line(out: &mut String, h: &Hit, matcher: &RegexMatcher) {
    push_num(out, h.line as usize);
    out.push('\t');
    if h.text.len() > 300 {
        // Show a window around the first match.
        let pos = matcher
            .find(&h.text)
            .ok()
            .flatten()
            .map(|m| m.start())
            .unwrap_or(0);
        let start = pos.saturating_sub(100);
        let end = (start + 300).min(h.text.len());
        if start > 0 {
            out.push('…');
        }
        render::push_text(out, &h.text[start..end], 300);
        if end < h.text.len() {
            out.push('…');
        }
    } else {
        render::push_text(out, &h.text, 300);
    }
    out.push('\n');
}

fn symbol_header(out: &mut String, o: &Outline, i: usize) {
    let s = &o.symbols[i];
    out.push('[');
    push_num(out, s.start as usize + 1);
    out.push('-');
    push_num(out, s.end as usize + 1);
    out.push_str("] ");
    out.push_str(&s.label);
    if let Some(p) = s.parent {
        out.push_str(" (in ");
        out.push_str(&o.qualified_name(p as usize));
        out.push(')');
    }
    out.push('\n');
}

/// Innermost symbol for a line, preferring function-level granularity.
fn owner(o: &Outline, line0: u32) -> Option<usize> {
    o.innermost_at(line0)
}

fn render_file(
    e: &Engine,
    f: &FileHits,
    src: Option<&Arc<Source>>,
    o: Option<&Arc<Outline>>,
    output: Output,
    matcher: &RegexMatcher,
    numbers_budget: usize,
) -> String {
    let mut out = String::new();
    out.push_str("==> ");
    out.push_str(&e.ws.display(&f.path));
    if let Some(s) = src {
        out.push_str(&format!(" @{:08x}", s.etag()));
    }
    if output == Output::Files {
        out.push_str(&format!(" ({})\n", f.matches));
        return out;
    }
    out.push_str(&format!(" ({})\n", plural(f.matches, "match")));
    let mut hits = f.hits.clone();
    hits.sort_by_key(|h| (h.line, !h.is_match));
    hits.dedup_by_key(|h| h.line);
    let mut shown_syms: Vec<usize> = Vec::new();
    let mut cur: Option<Option<usize>> = None;
    let mut shown_matches = 0usize;
    for h in &hits {
        if out.len() > numbers_budget {
            let rest = f.matches.saturating_sub(shown_matches);
            if rest > 0 {
                out.push_str(&format!(
                    "⋯ {} more in this file (search with path={})\n",
                    plural(rest, "match"),
                    e.ws.display(&f.path)
                ));
            }
            break;
        }
        let sym = o.and_then(|o| owner(o, h.line.saturating_sub(1)));
        if output == Output::Symbols
            && let (Some(i), Some(o), Some(src)) = (sym, o, src)
        {
            let s = &o.symbols[i];
            if shown_syms.contains(&i) {
                continue;
            }
            if s.end - s.start <= 200 {
                shown_syms.push(i);
                symbol_header(&mut out, o, i);
                render::push_lines(
                    &mut out,
                    src,
                    s.start as usize,
                    s.end as usize,
                    true,
                    render::MAX_LINE,
                    usize::MAX,
                );
                shown_matches += hits
                    .iter()
                    .filter(|x| x.is_match && x.line > s.start && x.line <= s.end + 1)
                    .count();
                cur = Some(Some(i));
                continue;
            }
        }
        if cur != Some(sym) {
            if let (Some(i), Some(o)) = (sym, o) {
                symbol_header(&mut out, o, i);
            }
            cur = Some(sym);
        }
        push_hit_line(&mut out, h, matcher);
        if h.is_match {
            shown_matches += 1;
        }
    }
    if f.hits.len() >= MAX_HITS_PER_FILE
        && f.matches > shown_matches
        && !out.contains("more in this file")
    {
        out.push_str(&format!(
            "⋯ {} more matches in this file\n",
            f.matches - shown_matches
        ));
    }
    out
}

pub fn search(e: &Engine, req: &SearchRequest) -> String {
    let budget = e.budget(req.budget);
    let max_bytes = e.bytes_for(budget);
    let matcher = match build_matcher(req) {
        Ok(m) => m,
        Err(msg) => return msg,
    };
    let roots = match resolve_roots(e, &req.paths) {
        Ok(r) => r,
        Err(msg) => return msg,
    };
    let context = if req.output == Output::Files {
        0
    } else {
        req.context.min(10)
    };
    let sc = scan(
        e,
        &roots,
        &req.globs,
        &matcher,
        context,
        req.max_matches.max(1),
    );
    let total: usize = sc.files.iter().map(|f| f.matches).sum();
    let scope = roots
        .iter()
        .map(|r| e.ws.display(r))
        .collect::<Vec<_>>()
        .join(", ");
    if sc.files.is_empty() {
        let mut t = format!(
            "No matches for /{}/ in {scope} ({} searched; .gitignored files skipped).",
            req.pattern,
            plural(sc.searched, "file")
        );
        if req.case == Case::Sensitive
            || (req.case == Case::Smart && req.pattern.chars().any(|c| c.is_uppercase()))
        {
            t.push_str(" Try case=insensitive.");
        }
        if !req.literal
            && req
                .pattern
                .contains(['(', '[', '.', '*', '+', '?', '|', '\\', '{'])
        {
            t.push_str(" Pattern is a regex; pass literal=true for exact text.");
        }
        t.push('\n');
        return t;
    }
    let mut out = format!(
        "{} in {} for /{}/{}\n",
        plural(total, "match"),
        plural(sc.files.len(), "file"),
        req.pattern,
        if sc.stopped {
            format!(
                " (stopped at {}; narrow with path/glob)",
                thousands(req.max_matches)
            )
        } else {
            String::new()
        }
    );
    // Load sources/outlines lazily in small parallel chunks: only files that
    // can still fit in the budget are ever parsed.
    let needs_outline = req.output != Output::Files;
    let prepare = |fs: &[FileHits]| -> Vec<Prepared> {
        fs.par_iter()
            .map(|f| {
                let src = e.load_text(&f.path).ok();
                // Grouping hits is worth a parse for source-sized files only.
                let o = if needs_outline {
                    src.as_ref()
                        .filter(|s| s.data.len() <= MAX_GROUP_BYTES)
                        .and_then(|s| e.outline(s))
                } else {
                    None
                };
                (src, o)
            })
            .collect()
    };
    let per_file_cap = (max_bytes / 2).max(2000);
    let mut rendered_files = 0;
    let mut prepared: Vec<Prepared> = Vec::new();
    let chunk = 2 * crate::macos::worker_threads();
    'files: for (k, f) in sc.files.iter().enumerate() {
        if k >= prepared.len() {
            let end = (k + chunk).min(sc.files.len());
            prepared.extend(prepare(&sc.files[k..end]));
        }
        let (src, o) = prepared
            .get(k)
            .map(|(s, o)| (s.as_ref(), o.as_ref()))
            .unwrap_or((None, None));
        let block = render_file(e, f, src, o, req.output, &matcher, per_file_cap);
        if out.len() + block.len() > max_bytes && rendered_files > 0 {
            break 'files;
        }
        let (cut, was_cut) = render::cut_lines(&block, max_bytes.saturating_sub(out.len()));
        out.push_str(cut);
        if was_cut {
            out.push_str("⋯ truncated\n");
        }
        if let Some(s) = src {
            e.remember(s);
        }
        rendered_files += 1;
    }
    if rendered_files < sc.files.len() {
        let rest = &sc.files[rendered_files..];
        let mut t = format!("… {} more files: ", rest.len());
        for (i, f) in rest.iter().enumerate() {
            let item = format!(
                "{}{} ({})",
                if i > 0 { ", " } else { "" },
                e.ws.display(&f.path),
                f.matches
            );
            if out.len() + t.len() + item.len() > max_bytes + 400 {
                t.push_str(", …");
                break;
            }
            t.push_str(&item);
        }
        t.push_str(". Narrow with path/glob or raise budget.\n");
        out.push_str(&t);
    }
    out
}

/// Find definitions named `query` (optionally qualified) anywhere in the
/// workspace: a fast word search narrows candidate files, then outlines are
/// parsed in parallel. Returns up to `limit` hits and the total count.
pub fn find_definitions(e: &Engine, query: &str, limit: usize) -> (Vec<DefHit>, usize) {
    let segs = split_segments(query.trim_end_matches("()"));
    let Some(last) = segs.last() else {
        return (Vec::new(), 0);
    };
    let last = last.split(':').next().unwrap_or(last);
    if last.is_empty() {
        return (Vec::new(), 0);
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    // `\b` only where the name itself starts/ends with a word character
    // (Ruby `valid?`, `save!`, ObjC selectors).
    let pat = format!(
        "{}{}{}",
        if last.starts_with(is_word) { r"\b" } else { "" },
        regex_escape(last),
        if last.ends_with(is_word) { r"\b" } else { "" }
    );
    let Ok(matcher) = RegexMatcherBuilder::new()
        .line_terminator(Some(b'\n'))
        .build(&pat)
    else {
        return (Vec::new(), 0);
    };
    let candidates = Mutex::new(Vec::new());
    for root in e.ws.roots().iter().take(1) {
        let _ = walk::walk(root, &WalkOpts::default(), |entry| {
            if entry.dataless || entry.size > crate::source::MAX_PARSE as u64 {
                return true;
            }
            if crate::lang::detect(&entry.path, b"").is_none() {
                return true;
            }
            let mut found = false;
            let _ = with_searcher(0, |s| {
                s.search_path(
                    &matcher,
                    &entry.path,
                    grep_searcher::sinks::Bytes(|_, _| {
                        found = true;
                        Ok(false)
                    }),
                )
            });
            if found {
                candidates.lock().push(entry.path);
            }
            candidates.lock().len() < 5000
        });
    }
    let mut candidates = candidates.into_inner();
    candidates.sort_unstable_by(|a, b| {
        use std::os::unix::ffi::OsStrExt;
        a.as_os_str().as_bytes().cmp(b.as_os_str().as_bytes())
    });
    let mut hits: Vec<(Arc<Source>, Arc<Outline>, usize, i32)> = candidates
        .par_iter()
        .filter_map(|p| {
            let src = e.load_text(p).ok()?;
            let o = e.outline(&src)?;
            let found = o.find(query);
            if found.is_empty() {
                return None;
            }
            Some(
                found
                    .into_iter()
                    .map(|i| {
                        let s = &o.symbols[i];
                        let exact = s.name == last;
                        let rank = match s.kind {
                            Kind::Class
                            | Kind::Struct
                            | Kind::Interface
                            | Kind::Trait
                            | Kind::Enum
                            | Kind::Type => 0,
                            Kind::Function | Kind::Method | Kind::Constructor => 1,
                            Kind::Impl | Kind::Module => 2,
                            _ => 3,
                        } + if exact { 0 } else { 4 };
                        (src.clone(), o.clone(), i, rank)
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect();
    // Prefer definitions over declarations (e.g. C++ prototypes, Rust trait
    // signatures) by ranking bodies first, then by kind, then path.
    hits.sort_by(|a, b| {
        let body = |h: &(Arc<Source>, Arc<Outline>, usize, i32)| {
            h.1.symbols[h.2].collapse.is_none() as i32
        };
        (a.3, body(a), &a.0.path, a.1.symbols[a.2].start).cmp(&(
            b.3,
            body(b),
            &b.0.path,
            b.1.symbols[b.2].start,
        ))
    });
    let total = hits.len();
    let out = hits
        .into_iter()
        .take(limit)
        .map(|(s, o, i, _)| (s, o, i))
        .collect();
    (out, total)
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
