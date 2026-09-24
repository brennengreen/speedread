//! `trace`: how symbols connect — callers, callees, references and
//! implementations.
//!
//! Resolution is syntactic. A word search finds candidate lines; tree-sitter
//! classifies each occurrence (call, reference or import — comments and
//! strings are dropped); outlines attribute it to its enclosing function.
//! Definitions that share a name are told apart where syntax allows
//! (`Type::f`, `self.f`, Go packages, the enclosing class, the defining file)
//! and sites that stay ambiguous are marked `?`. There is no type inference:
//! `x.f()` on an unknown receiver matches every `f`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use rayon::prelude::*;
use tree_sitter::{Node, Tree};

use crate::engine::Engine;
use crate::lang::LangId;
use crate::outline::{Kind, Outline, Symbol};
use crate::source::Source;
use crate::util::plural;
use crate::walk::{self, WalkOpts};
use crate::workspace::PathError;

pub const DEFAULT_TRACE_BUDGET: usize = 4000;
/// Occurrence lines scanned per trace level.
const MAX_SITES: usize = 20_000;
/// Symbols expanded per level when depth > 1.
const MAX_EXPAND: usize = 24;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Callers,
    Callees,
    Refs,
    Impls,
}

impl Direction {
    fn noun(self) -> &'static str {
        match self {
            Direction::Callers => "callers",
            Direction::Callees => "callees",
            Direction::Refs => "references",
            Direction::Impls => "implementations",
        }
    }
}

pub struct TraceRequest {
    pub target: String,
    pub direction: Direction,
    pub depth: usize,
    pub budget: Option<usize>,
}

#[derive(Clone)]
struct Def {
    src: Arc<Source>,
    o: Arc<Outline>,
    i: usize,
}

impl Def {
    fn sym(&self) -> &Symbol {
        &self.o.symbols[self.i]
    }
    fn key(&self) -> (PathBuf, u32) {
        (self.src.path.clone(), self.sym().start)
    }
    fn info(&self) -> Info {
        let s = self.sym();
        Info {
            name: s.name.clone(),
            path: self.src.path.clone(),
            dir: self
                .src
                .path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            owner: owner_type(&self.o, self.i),
            package: package_of(&self.src),
            lang: self.src.lang,
            kind: s.kind,
        }
    }
}

/// What attribution needs to know about a definition.
#[derive(Clone)]
struct Info {
    name: String,
    path: PathBuf,
    dir: PathBuf,
    owner: Option<String>,
    package: Option<String>,
    lang: Option<LangId>,
    kind: Kind,
}

impl Info {
    fn is_method(&self) -> bool {
        self.owner.is_some()
    }
}

fn is_callable(k: Kind) -> bool {
    matches!(
        k,
        Kind::Function | Kind::Method | Kind::Constructor | Kind::Test | Kind::Macro
    )
}

fn last_segment(s: &str) -> &str {
    let s = s.rsplit("::").next().unwrap_or(s);
    s.rsplit('.').next().unwrap_or(s)
}

/// `io::Error` → `Error`, `&'a mut S` → `S`, `Box<dyn E>` → `Box`.
fn type_base_name(s: &str) -> String {
    let s = s.split('<').next().unwrap_or(s).trim();
    let s = s.rsplit(char::is_whitespace).next().unwrap_or(s);
    last_segment(s.trim_start_matches(['&', '*'])).to_string()
}

/// Enclosing type of symbol `i`: Go receiver / C++ scope, else the nearest
/// type-like ancestor (class, struct, impl, extension…).
fn owner_type(o: &Outline, i: usize) -> Option<String> {
    let s = &o.symbols[i];
    if let Some(sc) = &s.scope {
        return Some(type_base_name(sc));
    }
    let mut p = s.parent;
    while let Some(j) = p {
        let t = &o.symbols[j as usize];
        if t.kind.is_type_like() {
            return Some(type_base_name(&t.name));
        }
        p = t.parent;
    }
    None
}

/// Type enclosing a line (the type itself when the line is in its body).
fn type_at(o: &Outline, line0: u32) -> Option<String> {
    let i = o.innermost_at(line0)?;
    if o.symbols[i].kind.is_type_like() {
        return Some(type_base_name(&o.symbols[i].name));
    }
    owner_type(o, i)
}

/// Go package name (its directory), used to attribute `pkg.Func()` calls.
fn package_of(src: &Source) -> Option<String> {
    if src.lang != Some(LangId::Go) {
        return None;
    }
    let text = &src.data[..src.data.len().min(8192)];
    for line in text.split(|&b| b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"package ") {
            let name = String::from_utf8_lossy(rest).trim().to_string();
            return Some(name);
        }
    }
    None
}

fn enclosing_callable(o: &Outline, line0: u32) -> Option<usize> {
    let i = o.innermost_at(line0)?;
    let mut cur = Some(i);
    while let Some(j) = cur {
        let k = o.symbols[j].kind;
        if is_callable(k) || k == Kind::Property {
            return Some(j);
        }
        cur = o.symbols[j].parent.map(|p| p as usize);
    }
    Some(i)
}

pub(crate) fn is_test_path(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.split('/').any(|seg| {
        matches!(
            seg,
            "test" | "tests" | "__tests__" | "spec" | "specs" | "testing" | "testdata"
        ) || seg.ends_with("tests")
    }) || name.contains("_test.")
        || name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.contains("_spec.")
        || name.ends_with("tests.swift")
        || name.ends_with("test.java")
        || name.ends_with("test.kt")
}

// ---------------------------------------------------------------------------
// Target resolution

fn load_outline(e: &Engine, p: &str) -> Result<(Arc<Source>, Arc<Outline>), String> {
    let path = match e.ws.resolve(p) {
        Ok(path) => path,
        Err(PathError::NotFound(_)) => {
            let sugg = crate::workspace::suggest(p, &e.file_list(), 3);
            return Err(if sugg.is_empty() {
                format!("Not found: {p}")
            } else {
                format!("Not found: {p}. Did you mean: {}", sugg.join(", "))
            });
        }
        Err(PathError::Outside(r)) => {
            return Err(format!("Outside the workspace: {}", r.display()));
        }
    };
    let src = e.load_text(&path).map_err(|x| format!("{p}: {x}"))?;
    let o = e
        .outline(&src)
        .ok_or_else(|| format!("{p}: no symbols (unsupported language)"))?;
    Ok((src, o))
}

/// The traced definition plus other definitions matching the query.
fn resolve_target(e: &Engine, raw: &str) -> Result<(Def, Vec<Def>), String> {
    let raw = raw.trim().trim_matches('`');
    let raw = raw.strip_suffix("()").unwrap_or(raw);
    if let Some(h) = raw.rfind('#') {
        let (p, name) = (&raw[..h], &raw[h + 1..]);
        if name.is_empty() {
            return Err("Empty symbol name after #".into());
        }
        if p.is_empty() {
            return lookup(e, name);
        }
        let (src, o) = load_outline(e, p)?;
        let found = o.find(name);
        if found.is_empty() {
            let sugg = o.suggest(name, 5);
            return Err(if sugg.is_empty() {
                format!("No symbol `{name}` in {p}")
            } else {
                format!(
                    "No symbol `{name}` in {p}. Did you mean: {}",
                    sugg.join(", ")
                )
            });
        }
        let defs: Vec<Def> = found
            .iter()
            .map(|&i| Def {
                src: src.clone(),
                o: o.clone(),
                i,
            })
            .collect();
        return Ok((defs[0].clone(), defs[1..].to_vec()));
    }
    // path:LINE[:COL] → the function enclosing that line.
    let parts: Vec<&str> = raw.rsplitn(3, ':').collect();
    let line_spec = match parts.as_slice() {
        [col, line, p] if col.parse::<u32>().is_ok() && line.parse::<u32>().is_ok() => {
            Some((*p, line.parse::<u32>().unwrap_or(1)))
        }
        [line, ..] if line.parse::<u32>().is_ok() => Some((
            &raw[..raw.len() - line.len() - 1],
            line.parse().unwrap_or(1),
        )),
        _ => None,
    };
    if let Some((p, line)) = line_spec
        && !p.is_empty()
    {
        let (src, o) = load_outline(e, p)?;
        let i = enclosing_callable(&o, line.saturating_sub(1))
            .ok_or_else(|| format!("No symbol encloses {p}:{line}"))?;
        return Ok((Def { src, o, i }, Vec::new()));
    }
    lookup(e, raw)
}

fn lookup(e: &Engine, name: &str) -> Result<(Def, Vec<Def>), String> {
    let (hits, _) = crate::search::find_definitions(e, name, 60);
    let mut defs: Vec<Def> = hits
        .into_iter()
        .map(|(src, o, i)| Def { src, o, i })
        .collect();
    if defs.is_empty() {
        return Err(format!(
            "No definition of `{name}` found in the workspace (trace needs a definition; try search)"
        ));
    }
    let first = defs.remove(0);
    Ok((first, defs))
}

/// All definitions whose name is exactly one of `names`, found with one
/// workspace scan. Keys are the names.
fn defs_named(e: &Engine, names: &[String]) -> HashMap<String, Vec<Def>> {
    let mut out: HashMap<String, Vec<Def>> = HashMap::new();
    if names.is_empty() {
        return out;
    }
    let Some(matcher) = word_matcher(names) else {
        return out;
    };
    let want: HashSet<&str> = names.iter().map(String::as_str).collect();
    let candidates = parking_lot::Mutex::new(Vec::new());
    let root = e.ws.primary();
    let _ = walk::walk(&root, &WalkOpts::default(), |entry| {
        if entry.dataless
            || entry.size > crate::source::MAX_PARSE as u64
            || crate::lang::detect(&entry.path, b"").is_none()
        {
            return true;
        }
        if file_matches(&matcher, &entry.path) {
            candidates.lock().push(entry.path);
        }
        candidates.lock().len() < 5000
    });
    let mut candidates = candidates.into_inner();
    crate::search::prefilter_definitions(&mut candidates, names);
    let found: Vec<Def> = candidates
        .par_iter()
        .filter_map(|p| {
            let src = e.load_text(p).ok()?;
            let o = e.outline(&src)?;
            let defs: Vec<Def> = o
                .symbols
                .iter()
                .enumerate()
                .filter(|(_, s)| want.contains(s.name.as_str()))
                .map(|(i, _)| Def {
                    src: src.clone(),
                    o: o.clone(),
                    i,
                })
                .collect();
            Some(defs)
        })
        .flatten()
        .collect();
    for d in found {
        out.entry(d.sym().name.clone()).or_default().push(d);
    }
    for v in out.values_mut() {
        v.sort_by_key(|a| a.key());
    }
    out
}

fn file_matches(m: &RegexMatcher, path: &Path) -> bool {
    let mut found = false;
    // `sinks::Bytes` requires line numbers.
    let mut searcher = grep_searcher::SearcherBuilder::new()
        .line_number(true)
        .build();
    let _ = searcher.search_path(
        m,
        path,
        grep_searcher::sinks::Bytes(|_, _| {
            found = true;
            Ok(false)
        }),
    );
    found
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn word_pattern(name: &str) -> String {
    let w = |c: char| c.is_alphanumeric() || c == '_';
    let mut esc = String::new();
    for c in name.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            esc.push('\\');
        }
        esc.push(c);
    }
    format!(
        "{}{}{}",
        if name.starts_with(w) { r"\b" } else { "" },
        esc,
        if name.ends_with(w) { r"\b" } else { "" }
    )
}

fn word_matcher(names: &[String]) -> Option<RegexMatcher> {
    let pat = names
        .iter()
        .map(|n| word_pattern(n))
        .collect::<Vec<_>>()
        .join("|");
    RegexMatcherBuilder::new()
        .line_terminator(Some(b'\n'))
        .build(&pat)
        .ok()
}

/// Byte columns where `name` occurs as a whole word in `line`.
fn word_positions(line: &[u8], name: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    if name.is_empty() {
        return out;
    }
    let first_word = is_word(name[0]);
    let last_word = is_word(name[name.len() - 1]);
    for pos in memchr::memmem::find_iter(line, name) {
        let before_ok = !first_word || pos == 0 || !is_word(line[pos - 1]);
        let end = pos + name.len();
        let after_ok = !last_word || end >= line.len() || !is_word(line[end]);
        if before_ok && after_ok {
            out.push(pos);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Occurrence classification

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OccKind {
    Call,
    Ref,
    Import,
}

const CALL_KINDS: &[&str] = &[
    "call_expression",
    "call",
    "method_invocation",
    "invocation_expression",
    "macro_invocation",
    "new_expression",
    "object_creation_expression",
    "message_expression",
    "function_call",
    "function_call_expression",
    "member_call_expression",
    "scoped_call_expression",
    "nullsafe_member_call_expression",
    "command",
    "method_call",
    "constructor_invocation",
    "explicit_constructor_invocation",
];

fn is_string_kind(k: &str) -> bool {
    k.contains("string")
        || matches!(
            k,
            "heredoc_body" | "char_literal" | "character_literal" | "rune_literal" | "regex"
        )
}

/// `Some(kind)` for code occurrences, `None` inside comments and strings.
fn classify(tree: &Tree, data: &[u8], start: usize, end: usize) -> Option<OccKind> {
    let node = tree.root_node().descendant_for_byte_range(start, end)?;
    let mut import = false;
    let mut in_code = false;
    let mut cur = Some(node);
    let mut depth = 0;
    while let Some(n) = cur {
        let k = n.kind();
        if !in_code {
            if k.contains("comment") {
                return None;
            }
            if k.contains("substitution") || k.contains("interpolat") {
                in_code = true;
            } else if is_string_kind(k) {
                return None;
            }
        }
        if k.contains("import") || k.contains("use_declaration") || k == "using_directive" {
            import = true;
        }
        depth += 1;
        if depth > 48 {
            break;
        }
        cur = n.parent();
    }
    if import {
        return Some(OccKind::Import);
    }
    let mut cur = node.parent();
    for _ in 0..4 {
        let Some(n) = cur else { break };
        if CALL_KINDS.contains(&n.kind()) {
            if callee_ident(n, data) == Some((start, end)) {
                return Some(OccKind::Call);
            }
            break;
        }
        cur = n.parent();
    }
    Some(OccKind::Ref)
}

/// Byte range of the called name: the last identifier of the callee.
fn callee_ident(call: Node<'_>, data: &[u8]) -> Option<(usize, usize)> {
    let callee = ["function", "method", "name", "macro", "constructor", "type"]
        .iter()
        .find_map(|f| call.child_by_field_name(f))
        .or_else(|| call.named_child(0))?;
    last_ident(data, callee.start_byte(), callee.end_byte())
}

fn last_ident(data: &[u8], s: usize, mut e: usize) -> Option<(usize, usize)> {
    let id = |b: u8| is_word(b) || b == b'$';
    for _ in 0..8 {
        while e > s && data[e - 1].is_ascii_whitespace() {
            e -= 1;
        }
        if e > s && matches!(data[e - 1], b'!' | b'?' | b':') {
            e -= 1;
            continue;
        }
        if e > s && data[e - 1] == b'>' {
            let mut depth = 0i32;
            let mut k = e;
            while k > s {
                k -= 1;
                match data[k] {
                    b'>' => depth += 1,
                    b'<' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if depth != 0 {
                return None;
            }
            e = k;
            if e >= s + 2 && &data[e - 2..e] == b"::" {
                e -= 2;
            }
            continue;
        }
        break;
    }
    let mut b = e;
    while b > s && id(data[b - 1]) {
        b -= 1;
    }
    (b < e).then_some((b, e))
}

/// Call syntax right after a name: `f(`, `f::<T>(`, `f<T>(`, `f!(`.
fn textual_call(line: &[u8], mut k: usize) -> bool {
    let skip_ws = |k: &mut usize| {
        while *k < line.len() && (line[*k] == b' ' || line[*k] == b'\t') {
            *k += 1;
        }
    };
    if line[k..].starts_with(b"::<") {
        k += 2;
    }
    if line.get(k) == Some(&b'<') {
        let mut depth = 0;
        let limit = (k + 120).min(line.len());
        let mut j = k;
        while j < limit {
            match line[j] {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                b';' | b'{' | b'}' => return false,
                _ => {}
            }
            j += 1;
        }
        if depth != 0 || j >= limit {
            return false;
        }
        k = j + 1;
    }
    if line.get(k) == Some(&b'!') {
        k += 1;
    }
    skip_ws(&mut k);
    line.get(k) == Some(&b'(')
}

/// Receiver before a name: `None` (unqualified), `Some("")` (an expression
/// such as `f().x`), or `Some(ident)` for `ident.name` / `ident::name` /
/// `ident->name`.
fn qualifier_before(line: &[u8], col: usize) -> Option<String> {
    let mut k = col;
    if k >= 2 && matches!(&line[k - 2..k], b"::" | b"->" | b"?.") {
        k -= 2;
    } else if k >= 1 && line[k - 1] == b'.' {
        k -= 1;
    } else {
        return None;
    }
    if k > 0 && line[k - 1] == b'>' {
        let mut depth = 0;
        while k > 0 {
            k -= 1;
            match line[k] {
                b'>' => depth += 1,
                b'<' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let end = k;
    let mut b = k;
    while b > 0 && (is_word(line[b - 1]) || line[b - 1] == b'$') {
        b -= 1;
    }
    Some(
        String::from_utf8_lossy(&line[b..end])
            .trim_start_matches('$')
            .to_string(),
    )
}

struct Occ {
    name: usize,
    line0: u32,
    col: usize,
    kind: OccKind,
    qualifier: Option<String>,
}

struct FileScan {
    src: Arc<Source>,
    o: Option<Arc<Outline>>,
    occs: Vec<Occ>,
}

/// Shared path components (a cheap "how close" measure).
fn proximity(a: &Path, b: &Path) -> usize {
    a.components()
        .zip(b.components())
        .take_while(|(x, y)| x == y)
        .count()
}

struct Scanned {
    files: Vec<FileScan>,
    stopped: bool,
    /// Matching files/lines left unexamined because of the budget.
    rest_files: usize,
    rest_lines: usize,
}

/// Line predicate applied to textual matches before parsing.
type LineFilter = dyn Fn(&[u8]) -> bool + Sync;

/// Occurrences of any of `names` (definitions, comments and strings
/// excluded). Candidate files are ordered non-test first, then by closeness
/// to `near`; only the first `max_files` are parsed and classified, so the
/// cost follows the output budget rather than the repository size.
fn scan_names(
    e: &Engine,
    names: &[String],
    near: &Path,
    max_files: usize,
    keep_line: Option<&LineFilter>,
) -> Scanned {
    let Some(matcher) = word_matcher(names) else {
        return Scanned {
            files: Vec::new(),
            stopped: false,
            rest_files: 0,
            rest_lines: 0,
        };
    };
    let roots = vec![e.ws.primary()];
    let sc = crate::search::scan(e, &roots, &[], &matcher, 0, MAX_SITES);
    let mut hits = sc.files;
    if let Some(keep) = keep_line {
        hits.retain(|f| f.hits.iter().any(|h| h.is_match && keep(&h.text)));
    }
    let key = |p: &Path| {
        let disp = p.to_string_lossy();
        (is_test_path(&disp), std::cmp::Reverse(proximity(p, near)))
    };
    hits.sort_by(|a, b| {
        key(&a.path)
            .cmp(&key(&b.path))
            .then_with(|| a.path.cmp(&b.path))
    });
    let n = hits.len().min(max_files.max(1));
    let files: Vec<FileScan> = hits[..n]
        .par_iter()
        .filter_map(|f| classify_file(e, f, names))
        .collect();
    Scanned {
        files,
        stopped: sc.stopped,
        rest_files: hits.len() - n,
        rest_lines: hits[n..].iter().map(|f| f.matches).sum(),
    }
}

fn classify_file(e: &Engine, f: &crate::search::FileHits, names: &[String]) -> Option<FileScan> {
    let src = e.load_text(&f.path).ok()?;
    if src.binary {
        return None;
    }
    let tree = src
        .lang
        .filter(|_| src.data.len() <= crate::source::MAX_PARSE)
        .and_then(|l| crate::outline::parse_tree(l, &src.data));
    let o = e.outline(&src);
    let mut occs = Vec::new();
    for h in f.hits.iter().filter(|h| h.is_match) {
        let line0 = h.line.saturating_sub(1);
        let text = src.line(line0 as usize);
        let base = src.offset_of_line(line0 as usize);
        for (ni, n) in names.iter().enumerate() {
            for col in word_positions(text, n.as_bytes()) {
                if let Some(o) = &o
                    && o.symbols.iter().any(|s| s.def == line0 && s.name == *n)
                {
                    continue;
                }
                let (start, end) = (base + col, base + col + n.len());
                let kind = match &tree {
                    Some(t) => match classify(t, &src.data, start, end) {
                        Some(OccKind::Ref) if textual_call(text, col + n.len()) => OccKind::Call,
                        Some(k) => k,
                        None => continue,
                    },
                    None if textual_call(text, col + n.len()) => OccKind::Call,
                    None => OccKind::Ref,
                };
                occs.push(Occ {
                    name: ni,
                    line0,
                    col,
                    kind,
                    qualifier: qualifier_before(text, col),
                });
            }
        }
    }
    (!occs.is_empty()).then_some(FileScan { src, o, occs })
}

// ---------------------------------------------------------------------------
// Attribution of a site to one of several same-named definitions

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attr {
    Target,
    Other,
    Unknown,
}

fn implicit_self(lang: Option<LangId>) -> bool {
    matches!(
        lang,
        Some(
            LangId::Java
                | LangId::Cpp
                | LangId::CSharp
                | LangId::Swift
                | LangId::Kotlin
                | LangId::Scala
                | LangId::Ruby
                | LangId::ObjC
        )
    )
}

fn attribute(occ: &Occ, f: &FileScan, t: &Info, others: &[Info]) -> Attr {
    let go = t.lang == Some(LangId::Go) && f.src.lang == Some(LangId::Go);
    let here = f.o.as_deref().and_then(|o| type_at(o, occ.line0));
    let owner_is = |ow: &Option<String>| -> Attr {
        match ow {
            Some(w) if t.owner.as_deref() == Some(w.as_str()) => Attr::Target,
            Some(w)
                if others
                    .iter()
                    .any(|o| o.owner.as_deref() == Some(w.as_str())) =>
            {
                Attr::Other
            }
            _ => Attr::Unknown,
        }
    };
    let file_dir = f.src.path.parent().unwrap_or(Path::new(""));
    let attr = match occ.qualifier.as_deref() {
        Some("self" | "this" | "Self" | "cls" | "static" | "$this") => owner_is(&here),
        Some("super") => Attr::Unknown,
        Some(q) if !q.is_empty() => {
            if t.owner.as_deref() == Some(q) || (go && t.package.as_deref() == Some(q)) {
                Attr::Target
            } else if others
                .iter()
                .any(|o| o.owner.as_deref() == Some(q) || (go && o.package.as_deref() == Some(q)))
            {
                Attr::Other
            } else if go && !t.is_method() {
                // `x.F()` is a method call or another package's F.
                Attr::Other
            } else {
                Attr::Unknown
            }
        }
        Some(_) => {
            if !t.is_method() && (go || t.lang == Some(LangId::Rust)) {
                Attr::Other
            } else {
                Attr::Unknown
            }
        }
        None => {
            if go {
                if !t.is_method() && file_dir == t.dir {
                    Attr::Target
                } else {
                    Attr::Other
                }
            } else if t.is_method() {
                if implicit_self(t.lang) {
                    owner_is(&here)
                } else if matches!(
                    t.lang,
                    Some(
                        LangId::Python
                            | LangId::Rust
                            | LangId::JavaScript
                            | LangId::TypeScript
                            | LangId::Tsx
                            | LangId::Php
                            | LangId::Lua
                    )
                ) && !matches!(t.kind, Kind::Class | Kind::Struct | Kind::Enum)
                {
                    // Methods need an explicit receiver in these languages.
                    Attr::Other
                } else {
                    Attr::Unknown
                }
            } else if f.src.path == t.path {
                Attr::Target
            } else if others
                .iter()
                .any(|o| o.path == f.src.path && !o.is_method())
            {
                Attr::Other
            } else {
                Attr::Unknown
            }
        }
    };
    if others.is_empty() && attr == Attr::Unknown {
        Attr::Target
    } else {
        attr
    }
}

// ---------------------------------------------------------------------------
// Callers and references

struct Site {
    line0: u32,
    kind: OccKind,
    attr: Attr,
}

/// Sites inside one enclosing symbol (or at a file's top level).
struct Group {
    src: Arc<Source>,
    o: Option<Arc<Outline>>,
    sym: Option<usize>,
    sites: Vec<Site>,
    children: Vec<Group>,
}

impl Group {
    fn key(&self) -> (PathBuf, Option<u32>) {
        (
            self.src.path.clone(),
            self.sym
                .and_then(|i| self.o.as_ref().map(|o| o.symbols[i].start)),
        )
    }
    fn def(&self) -> Option<Def> {
        let (o, i) = (self.o.clone()?, self.sym?);
        is_callable(o.symbols[i].kind).then(|| Def {
            src: self.src.clone(),
            o,
            i,
        })
    }
    fn uncertain(&self) -> bool {
        self.sites.iter().all(|s| s.attr == Attr::Unknown)
    }
}

struct SiteStats {
    excluded: usize,
    stopped: bool,
    rest_files: usize,
    rest_lines: usize,
}

/// Group sites of each target (callers or all references) by enclosing
/// symbol. `targets[k]` gets `result[k]`.
fn sites_for(
    e: &Engine,
    targets: &[(Info, Vec<Info>)],
    refs: bool,
    near: &Path,
    max_files: usize,
    stats: &mut SiteStats,
) -> Vec<Vec<Group>> {
    let names: Vec<String> = {
        let mut v: Vec<String> = targets.iter().map(|t| t.0.name.clone()).collect();
        v.sort();
        v.dedup();
        v
    };
    let sc = scan_names(e, &names, near, max_files, None);
    stats.stopped |= sc.stopped;
    stats.rest_files += sc.rest_files;
    stats.rest_lines += sc.rest_lines;
    let files = sc.files;
    let mut out: Vec<Vec<Group>> = (0..targets.len()).map(|_| Vec::new()).collect();
    for f in &files {
        for (k, (t, others)) in targets.iter().enumerate() {
            let mut by_sym: Vec<Group> = Vec::new();
            let code = f
                .src
                .lang
                .is_some_and(|l| !l.is_structured_data() && l.grammar().is_some());
            for occ in f.occs.iter().filter(|o| names[o.name] == t.name) {
                if !refs && (occ.kind != OccKind::Call || !code) {
                    continue;
                }
                let attr = attribute(occ, f, t, others);
                if attr == Attr::Other {
                    stats.excluded += 1;
                    continue;
                }
                let sym = if occ.kind == OccKind::Import {
                    None
                } else {
                    f.o.as_deref()
                        .and_then(|o| enclosing_callable(o, occ.line0))
                };
                // A symbol's own body is not a caller of itself unless it recurses.
                let site = Site {
                    line0: occ.line0,
                    kind: occ.kind,
                    attr,
                };
                match by_sym.iter_mut().find(|g| g.sym == sym) {
                    Some(g) => {
                        if g.sites.last().is_none_or(|s| s.line0 != occ.line0) {
                            g.sites.push(site);
                        }
                    }
                    None => by_sym.push(Group {
                        src: f.src.clone(),
                        o: f.o.clone(),
                        sym,
                        sites: vec![site],
                        children: Vec::new(),
                    }),
                }
            }
            out[k].extend(by_sym);
        }
    }
    for groups in &mut out {
        groups.sort_by(|a, b| {
            let k = |g: &Group| {
                (
                    is_test_path(&g.src.path.to_string_lossy()),
                    std::cmp::Reverse(proximity(&g.src.path, near)),
                    g.src.path.clone(),
                    g.uncertain(),
                    g.key().1,
                )
            };
            k(a).cmp(&k(b))
        });
    }
    out
}

fn with_others(d: &Def, all: &HashMap<String, Vec<Def>>) -> (Info, Vec<Info>) {
    let me = d.key();
    let others = all
        .get(&d.sym().name)
        .map(|v| v.iter().filter(|x| x.key() != me).map(Def::info).collect())
        .unwrap_or_default();
    (d.info(), others)
}

/// Expand callers breadth-first to `depth` levels.
fn caller_tree(
    e: &Engine,
    root: &Def,
    root_others: Vec<Info>,
    depth: usize,
    refs: bool,
    max_files: usize,
    stats: &mut SiteStats,
) -> Vec<Group> {
    let near = root.src.path.clone();
    let mut top = sites_for(
        e,
        &[(root.info(), root_others)],
        refs,
        &near,
        max_files,
        stats,
    )
    .pop()
    .unwrap_or_default();
    let mut visited: HashSet<(PathBuf, u32)> = HashSet::new();
    visited.insert(root.key());
    // Level by level: paths into the tree of groups to expand next.
    let mut frontier: Vec<Vec<usize>> = (0..top.len()).map(|i| vec![i]).collect();
    for _level in 1..depth.max(1) {
        let mut defs: Vec<(Vec<usize>, Def)> = Vec::new();
        for path in &frontier {
            let g = group_at(&top, path);
            if let Some(d) = g.def()
                && visited.insert(d.key())
            {
                defs.push((path.clone(), d));
            }
            if defs.len() >= MAX_EXPAND {
                break;
            }
        }
        if defs.is_empty() {
            break;
        }
        let names: Vec<String> = defs.iter().map(|(_, d)| d.sym().name.clone()).collect();
        let all = defs_named(e, &names);
        let targets: Vec<(Info, Vec<Info>)> =
            defs.iter().map(|(_, d)| with_others(d, &all)).collect();
        let results = sites_for(e, &targets, false, &near, max_files, stats);
        let mut next = Vec::new();
        for ((path, d), mut kids) in defs.into_iter().zip(results) {
            // Recursion shows up as the symbol calling itself; drop it.
            kids.retain(|g| g.def().is_none_or(|k| k.key() != d.key()));
            let g = group_at_mut(&mut top, &path);
            for (j, _) in kids.iter().enumerate() {
                let mut p = path.clone();
                p.push(j);
                next.push(p);
            }
            g.children = kids;
        }
        frontier = next;
    }
    top
}

fn group_at<'a>(groups: &'a [Group], path: &[usize]) -> &'a Group {
    let mut g = &groups[path[0]];
    for &i in &path[1..] {
        g = &g.children[i];
    }
    g
}

fn group_at_mut<'a>(groups: &'a mut [Group], path: &[usize]) -> &'a mut Group {
    let mut g = &mut groups[path[0]];
    for &i in &path[1..] {
        g = &mut g.children[i];
    }
    g
}

// ---------------------------------------------------------------------------
// Callees

struct Callee {
    line0: u32,
    name: String,
    qualifier: Option<String>,
    count: usize,
}

/// Calls made inside a definition's body, in order of first appearance.
fn calls_in(d: &Def) -> Vec<Callee> {
    let src = &d.src;
    let Some(lang) = src.lang else {
        return Vec::new();
    };
    let Some(tree) = crate::outline::parse_tree(lang, &src.data) else {
        return Vec::new();
    };
    let s = d.sym();
    let (a, b) = (
        src.offset_of_line(s.def as usize),
        src.offset_of_line(s.end as usize + 1),
    );
    let mut out: Vec<Callee> = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if n.end_byte() <= a || n.start_byte() >= b {
            continue;
        }
        if CALL_KINDS.contains(&n.kind())
            && n.start_byte() >= a
            && let Some((cs, ce)) = callee_ident(n, &src.data)
        {
            let name = String::from_utf8_lossy(&src.data[cs..ce]).into_owned();
            let line0 = n.start_position().row as u32;
            let line_start = src.offset_of_line(line0 as usize);
            let q = if cs >= line_start {
                qualifier_before(src.line(line0 as usize), cs - line_start)
            } else {
                None
            };
            let is_def_name = line0 == s.def && name == s.name;
            if !name.is_empty() && !is_def_name {
                match out.iter_mut().find(|c| c.name == name && c.qualifier == q) {
                    Some(c) => c.count += 1,
                    None => out.push(Callee {
                        line0,
                        name,
                        qualifier: q,
                        count: 1,
                    }),
                }
            }
        }
        let mut cursor = n.walk();
        let kids: Vec<Node<'_>> = n.children(&mut cursor).collect();
        stack.extend(kids.into_iter().rev());
    }
    out.sort_by_key(|c| c.line0);
    out
}

/// Pick the definitions a call most likely refers to.
fn resolve_callee<'a>(c: &Callee, caller: &Def, cands: &'a [Def]) -> Vec<&'a Def> {
    let callable: Vec<&Def> = cands
        .iter()
        .filter(|d| {
            let k = d.sym().kind;
            is_callable(k) || k.is_type_like() || k == Kind::Property || k == Kind::Const
        })
        .collect();
    if callable.len() <= 1 {
        return callable;
    }
    let me = caller.info();
    let score = |d: &Def| -> i32 {
        let info = d.info();
        let mut s = 0;
        match c.qualifier.as_deref() {
            Some("self" | "this" | "Self" | "cls" | "$this") => {
                if info.owner.is_some() && info.owner == me.owner {
                    s += 8;
                }
            }
            Some(q) if !q.is_empty() => {
                if info.owner.as_deref() == Some(q) || info.package.as_deref() == Some(q) {
                    s += 8;
                }
            }
            Some(_) => {}
            None => {
                if info.owner.is_none() {
                    s += 2;
                }
                if implicit_self(me.lang) && info.owner.is_some() && info.owner == me.owner {
                    s += 6;
                }
            }
        }
        if info.path == me.path {
            s += 3;
        } else if info.dir == me.dir {
            s += 2;
        }
        if info.lang == me.lang {
            s += 1;
        }
        if d.sym().collapse.is_some() || d.sym().end > d.sym().def {
            s += 1;
        }
        s
    };
    let best = callable.iter().map(|d| score(d)).max().unwrap_or(0);
    callable.into_iter().filter(|d| score(d) == best).collect()
}

struct CalleeNode {
    callee: Callee,
    defs: Vec<Def>,
    children: Vec<CalleeNode>,
}

fn callee_tree(e: &Engine, root: &Def, depth: usize) -> (Vec<CalleeNode>, Vec<String>) {
    let mut visited: HashSet<(PathBuf, u32)> = HashSet::new();
    visited.insert(root.key());
    let mut external: Vec<String> = Vec::new();
    let mut level: Vec<(Vec<usize>, Def)> = vec![(Vec::new(), root.clone())];
    let mut top: Vec<CalleeNode> = Vec::new();
    for lvl in 0..depth.max(1) {
        let calls: Vec<(Vec<usize>, Def, Vec<Callee>)> = level
            .iter()
            .map(|(p, d)| (p.clone(), d.clone(), calls_in(d)))
            .collect();
        let mut names: Vec<String> = calls
            .iter()
            .flat_map(|(_, _, cs)| cs.iter().map(|c| c.name.clone()))
            .collect();
        names.sort();
        names.dedup();
        let all = defs_named(e, &names);
        let mut next = Vec::new();
        for (path, caller, cs) in calls {
            let mut nodes = Vec::new();
            for c in cs {
                let defs: Vec<Def> = all
                    .get(&c.name)
                    .map(|v| {
                        resolve_callee(&c, &caller, v)
                            .into_iter()
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                if defs.is_empty() && lvl == 0 {
                    let q = match c.qualifier.as_deref() {
                        Some(q) if !q.is_empty() => format!("{q}.{}", c.name),
                        _ => c.name.clone(),
                    };
                    if !external.contains(&q) {
                        external.push(q);
                    }
                    continue;
                }
                if defs.is_empty() {
                    continue;
                }
                nodes.push(CalleeNode {
                    callee: c,
                    defs,
                    children: Vec::new(),
                });
            }
            for (j, n) in nodes.iter().enumerate() {
                if n.defs.len() == 1
                    && is_callable(n.defs[0].sym().kind)
                    && visited.insert(n.defs[0].key())
                    && next.len() < MAX_EXPAND
                {
                    let mut p = path.clone();
                    p.push(j);
                    next.push((p, n.defs[0].clone()));
                }
            }
            if path.is_empty() {
                top = nodes;
            } else {
                let mut cur = &mut top[path[0]];
                for &i in &path[1..] {
                    cur = &mut cur.children[i];
                }
                cur.children = nodes;
            }
        }
        level = next;
        if level.is_empty() {
            break;
        }
    }
    (top, external)
}

// ---------------------------------------------------------------------------
// Implementations

struct ImplHit {
    ty: Def,
    /// The implementing method when a method was traced.
    method: Option<Def>,
    children: Vec<ImplHit>,
}

/// Type declarations whose heritage clause can name a base type.
const TYPE_DECLS: &[&str] = &[
    "class_definition",
    "class_declaration",
    "class_specifier",
    "struct_specifier",
    "struct_declaration",
    "interface_declaration",
    "protocol_declaration",
    "extension_declaration",
    "impl_item",
    "trait_item",
    "object_declaration",
    "object_definition",
    "trait_definition",
    "enum_declaration",
    "enum_definition",
    "record_declaration",
    "abstract_class_declaration",
    "class_interface",
    "class",
];

fn body_start(n: Node<'_>) -> usize {
    if let Some(b) = n.child_by_field_name("body") {
        return b.start_byte();
    }
    let mut cursor = n.walk();
    for c in n.children(&mut cursor) {
        let k = c.kind();
        if k.contains("body") || k.contains("declaration_list") || k == "block" {
            return c.start_byte();
        }
    }
    n.end_byte()
}

/// The type declaration whose heritage clause (`class X(Base)`, `extends`,
/// `implements`, `: Base`, `impl Base for X`) contains the byte range.
fn heritage_owner(tree: &Tree, s: usize, e: usize) -> Option<Node<'_>> {
    let node = tree.root_node().descendant_for_byte_range(s, e)?;
    let mut cur = node.parent();
    for _ in 0..6 {
        let n = cur?;
        if TYPE_DECLS.contains(&n.kind()) {
            if n.kind() == "impl_item" {
                let tr = n.child_by_field_name("trait")?;
                return (tr.start_byte() <= s && e <= tr.end_byte()).then_some(n);
            }
            let name_end = n
                .child_by_field_name("name")
                .map(|x| x.end_byte())
                .unwrap_or(n.start_byte());
            return (s >= name_end && e <= body_start(n)).then_some(n);
        }
        cur = n.parent();
    }
    None
}

fn node_text(src: &Source, n: Node<'_>) -> String {
    String::from_utf8_lossy(&src.data[n.start_byte()..n.end_byte()]).into_owned()
}

fn first_line_label(src: &Source, n: Node<'_>) -> String {
    let row = n.start_position().row;
    let line = String::from_utf8_lossy(src.line(row)).trim().to_string();
    let line = line.trim_end_matches('{').trim_end().to_string();
    crate::outline::clip(&line, 160)
}

/// A symbol for a type the outline does not list (e.g. a class defined
/// inside a test function), with its direct methods.
fn synth_type(src: &Arc<Source>, t: Node<'_>) -> Def {
    let name_node = if t.kind() == "impl_item" {
        t.child_by_field_name("type")
    } else {
        t.child_by_field_name("name")
    };
    let name = name_node
        .map(|n| type_base_name(&node_text(src, n)))
        .unwrap_or_else(|| "?".into());
    let start = t.start_position().row as u32;
    let mut symbols = vec![Symbol {
        kind: if t.kind() == "impl_item" {
            Kind::Impl
        } else {
            Kind::Class
        },
        name,
        scope: None,
        label: first_line_label(src, t),
        start,
        def: start,
        end: crate::outline::end_row(t) as u32,
        collapse: None,
        parent: None,
        depth: 0,
    }];
    let body = t.child_by_field_name("body").or_else(|| {
        let mut cursor = t.walk();
        t.children(&mut cursor)
            .find(|c| c.kind().contains("body") || c.kind().contains("declaration_list"))
    });
    if let Some(body) = body {
        let mut cursor = body.walk();
        for c in body.named_children(&mut cursor) {
            let f = if c.kind() == "decorated_definition" {
                c.child_by_field_name("definition").unwrap_or(c)
            } else {
                c
            };
            let k = f.kind();
            if !(k.contains("function") || k.contains("method")) {
                continue;
            }
            let Some(nm) = f.child_by_field_name("name") else {
                continue;
            };
            let row = f.start_position().row as u32;
            symbols.push(Symbol {
                kind: Kind::Method,
                name: node_text(src, nm),
                scope: None,
                label: first_line_label(src, f),
                start: c.start_position().row as u32,
                def: row,
                end: crate::outline::end_row(f) as u32,
                collapse: None,
                parent: Some(0),
                depth: 1,
            });
        }
    }
    Def {
        src: src.clone(),
        o: Arc::new(Outline {
            symbols,
            ..Default::default()
        }),
        i: 0,
    }
}

const HERITAGE_WORDS: &[&[u8]] = &[
    b"class",
    b"interface",
    b"struct",
    b"impl",
    b"extends",
    b"implements",
    b"extension",
    b"protocol",
    b"object",
    b"trait",
    b"enum",
    b"record",
    b"actor",
];

fn impl_candidates(
    e: &Engine,
    base: &str,
    exclude: &(PathBuf, u32),
    max_files: usize,
    rest: &mut usize,
) -> Vec<Def> {
    let names = vec![base.to_string()];
    let keep = |line: &[u8]| {
        HERITAGE_WORDS
            .iter()
            .any(|w| !word_positions(line, w).is_empty())
    };
    let sc = scan_names(e, &names, &exclude.0, max_files, Some(&keep));
    *rest += sc.rest_files;
    let files = sc.files;
    let mut out = Vec::new();
    let mut seen: HashSet<(PathBuf, u32)> = HashSet::new();
    for f in files {
        let Some(lang) = f.src.lang else { continue };
        let Some(tree) = crate::outline::parse_tree(lang, &f.src.data) else {
            continue;
        };
        for occ in f.occs.iter().filter(|o| o.kind != OccKind::Import) {
            let s = f.src.offset_of_line(occ.line0 as usize) + occ.col;
            let Some(t) = heritage_owner(&tree, s, s + base.len()) else {
                continue;
            };
            let row = t.start_position().row as u32;
            if !seen.insert((f.src.path.clone(), row)) {
                continue;
            }
            let listed = f.o.as_ref().and_then(|o| {
                o.symbols
                    .iter()
                    .position(|sy| sy.kind.is_type_like() && sy.def == row)
                    .map(|i| (o.clone(), i))
            });
            let def = match listed {
                Some((o, i)) => Def {
                    src: f.src.clone(),
                    o,
                    i,
                },
                None => synth_type(&f.src, t),
            };
            if def.key() != *exclude {
                out.push(def);
            }
        }
    }
    out
}

/// Go interfaces are satisfied structurally: find types whose method set
/// covers the interface's methods.
fn go_implementers(e: &Engine, iface: &Def) -> Vec<Def> {
    let o = &iface.o;
    let methods: Vec<String> = o
        .symbols
        .iter()
        .filter(|s| s.parent == Some(iface.i as u32) && s.kind == Kind::Method)
        .map(|s| s.name.clone())
        .collect();
    if methods.is_empty() {
        return Vec::new();
    }
    let defs = defs_named(e, &methods);
    // receiver type (dir, name) → method names it defines
    let mut sets: HashMap<(PathBuf, String), HashSet<String>> = HashMap::new();
    for (name, ds) in &defs {
        for d in ds {
            if d.src.lang != Some(LangId::Go) {
                continue;
            }
            if let Some(recv) = &d.sym().scope {
                let dir = d
                    .src
                    .path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default();
                sets.entry((dir, type_base_name(recv)))
                    .or_default()
                    .insert(name.clone());
            }
        }
    }
    let mut types: Vec<(PathBuf, String)> = sets
        .into_iter()
        .filter(|(_, have)| methods.iter().all(|m| have.contains(m)))
        .map(|(k, _)| k)
        .collect();
    types.sort();
    let type_names: Vec<String> = types.iter().map(|(_, n)| n.clone()).collect();
    let tdefs = defs_named(e, &type_names);
    let mut out = Vec::new();
    for (dir, name) in types {
        if let Some(d) = tdefs.get(&name).and_then(|v| {
            v.iter().find(|d| {
                d.sym().kind.is_type_like()
                    && d.src
                        .path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default()
                        == dir
            })
        }) {
            out.push(d.clone());
        }
    }
    out
}

fn method_in(e: &Engine, ty: &Def, name: &str) -> Option<Def> {
    let o = &ty.o;
    if let Some(i) = o
        .symbols
        .iter()
        .position(|s| s.parent == Some(ty.i as u32) && s.name == name)
    {
        return Some(Def {
            src: ty.src.clone(),
            o: o.clone(),
            i,
        });
    }
    // Go methods live outside the type; match on receiver within the package.
    if ty.src.lang == Some(LangId::Go) {
        let tname = type_base_name(&ty.sym().name);
        let dir = ty.src.path.parent()?.to_path_buf();
        let defs = defs_named(e, &[name.to_string()]);
        return defs
            .get(name)?
            .iter()
            .find(|d| {
                d.src.path.parent() == Some(dir.as_path())
                    && d.sym().scope.as_deref().map(type_base_name).as_deref()
                        == Some(tname.as_str())
            })
            .cloned();
    }
    None
}

fn impl_tree(
    e: &Engine,
    target: &Def,
    depth: usize,
    max_files: usize,
    rest: &mut usize,
) -> (Def, Vec<ImplHit>) {
    // Tracing `Iface.method` means implementations of Iface, then the method.
    let (base, method) = if is_callable(target.sym().kind) {
        match target.sym().parent {
            Some(p) => (
                Def {
                    src: target.src.clone(),
                    o: target.o.clone(),
                    i: p as usize,
                },
                Some(target.sym().name.clone()),
            ),
            None => (target.clone(), None),
        }
    } else {
        (target.clone(), None)
    };
    let mut visited: HashSet<(PathBuf, u32)> = HashSet::new();
    visited.insert(base.key());
    let find = |d: &Def, visited: &mut HashSet<(PathBuf, u32)>, rest: &mut usize| -> Vec<ImplHit> {
        let name = type_base_name(&d.sym().name);
        let mut tys = impl_candidates(e, &name, &d.key(), max_files, rest);
        if d.src.lang == Some(LangId::Go) && d.sym().kind == Kind::Interface {
            tys.extend(go_implementers(e, d));
        }
        let mut hits = Vec::new();
        for ty in tys {
            if !visited.insert(ty.key()) {
                continue;
            }
            let m = method.as_deref().and_then(|m| method_in(e, &ty, m));
            hits.push(ImplHit {
                ty,
                method: m,
                children: Vec::new(),
            });
        }
        let near = d.src.path.clone();
        hits.sort_by(|a, b| {
            let k = |h: &ImplHit| {
                (
                    is_test_path(&h.ty.src.path.to_string_lossy()),
                    std::cmp::Reverse(proximity(&h.ty.src.path, &near)),
                    h.ty.key(),
                )
            };
            k(a).cmp(&k(b))
        });
        hits
    };
    let mut top = find(&base, &mut visited, rest);
    if depth > 1 {
        for h in top.iter_mut().take(MAX_EXPAND) {
            h.children = find(&h.ty, &mut visited, rest);
            if depth > 2 {
                for c in h.children.iter_mut() {
                    c.children = find(&c.ty, &mut visited, rest);
                }
            }
        }
    }
    (base, top)
}

// ---------------------------------------------------------------------------
// Rendering

struct Out<'a> {
    e: &'a Engine,
    text: String,
    max: usize,
    omitted: usize,
    omitted_tests: usize,
    omitted_files: Vec<String>,
}

impl Out<'_> {
    fn full(&self) -> bool {
        self.text.len() >= self.max
    }

    fn omit(&mut self, path: &Path) {
        self.omitted += 1;
        let disp = self.e.ws.display(path);
        if is_test_path(&disp) {
            self.omitted_tests += 1;
        }
        if !self.omitted_files.contains(&disp) {
            self.omitted_files.push(disp);
        }
    }

    fn push_block(&mut self, block: &str, path: &Path) -> bool {
        if self.text.len() + block.len() > self.max && !self.text.is_empty() {
            self.omit(path);
            return false;
        }
        self.text.push_str(block);
        true
    }
}

fn sym_header(o: &Outline, i: usize) -> String {
    let s = &o.symbols[i];
    let mut t = format!("[{}-{}] {}", s.start + 1, s.end + 1, s.label);
    if let Some(p) = s.parent {
        t.push_str(&format!(" (in {})", o.qualified_name(p as usize)));
    }
    t
}

fn site_line(src: &Source, s: &Site, indent: &str) -> String {
    let mut t = String::new();
    t.push_str(indent);
    t.push_str(&(s.line0 + 1).to_string());
    t.push('\t');
    let line = src.line(s.line0 as usize);
    crate::render::push_text(&mut t, trim_ascii_start(line), 200);
    t.push('\n');
    t
}

fn trim_ascii_start(b: &[u8]) -> &[u8] {
    let k = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    &b[k..]
}

fn render_group(out: &mut Out<'_>, g: &Group, depth: usize, parent: Option<&Path>) {
    let pad = "  ".repeat(depth * 2 + 1);
    let mut block = String::new();
    block.push_str(&pad);
    if depth > 0 {
        block.push_str("← ");
        if parent != Some(g.src.path.as_path()) {
            block.push_str(&out.e.ws.display(&g.src.path));
            block.push(' ');
        }
    }
    if g.uncertain() {
        block.push_str("? ");
    }
    match (g.sym, &g.o) {
        (Some(i), Some(o)) => block.push_str(&sym_header(o, i)),
        _ if g.sites.iter().all(|s| s.kind == OccKind::Import) => block.push_str("(imports)"),
        _ => block.push_str("(top level)"),
    }
    block.push('\n');
    let site_pad = format!("{pad}  ");
    for s in g.sites.iter().take(6) {
        block.push_str(&site_line(&g.src, s, &site_pad));
    }
    if g.sites.len() > 6 {
        block.push_str(&format!("{site_pad}… {} more\n", g.sites.len() - 6));
    }
    if !out.push_block(&block, &g.src.path) {
        return;
    }
    for c in &g.children {
        render_group(out, c, depth + 1, Some(&g.src.path));
    }
}

fn render_groups(out: &mut Out<'_>, groups: &[Group]) {
    let mut last: Option<&Path> = None;
    for g in groups {
        if out.full() {
            out.omit(&g.src.path);
            continue;
        }
        if last != Some(g.src.path.as_path()) {
            let head = format!("{}\n", out.e.ws.display(&g.src.path));
            if !out.push_block(&head, &g.src.path) {
                continue;
            }
            last = Some(g.src.path.as_path());
        }
        render_group(out, g, 0, None);
    }
}

fn count_groups(gs: &[Group]) -> (usize, usize) {
    let sites = gs.iter().map(|g| g.sites.len()).sum();
    (sites, gs.len())
}

fn def_line(e: &Engine, d: &Def) -> String {
    let s = d.sym();
    format!(
        "{}:{}-{} {}",
        e.ws.display(&d.src.path),
        s.start + 1,
        s.end + 1,
        s.label
    )
}

fn render_callee(out: &mut Out<'_>, caller: &Def, n: &CalleeNode, depth: usize) {
    let pad = "  ".repeat(depth * 2 + 1);
    let mut block = String::new();
    let line = caller.src.line(n.callee.line0 as usize);
    block.push_str(&format!("{pad}{}\t", n.callee.line0 + 1));
    crate::render::push_text(&mut block, trim_ascii_start(line), 160);
    if n.callee.count > 1 {
        block.push_str(&format!("  (×{})", n.callee.count));
    }
    block.push('\n');
    let first = &n.defs[0];
    block.push_str(&format!("{pad}  → {}", def_line(out.e, first)));
    if n.defs.len() > 1 {
        let rest: Vec<String> = n.defs[1..]
            .iter()
            .take(3)
            .map(|d| format!("{}:{}", out.e.ws.display(&d.src.path), d.sym().start + 1))
            .collect();
        block.push_str(&format!(
            "  (? {} candidates; also {}{})",
            n.defs.len(),
            rest.join(", "),
            if n.defs.len() > 4 { ", …" } else { "" }
        ));
    }
    block.push('\n');
    if !out.push_block(&block, &first.src.path) {
        return;
    }
    for c in &n.children {
        render_callee(out, first, c, depth + 1);
    }
}

fn render_impl(out: &mut Out<'_>, h: &ImplHit, depth: usize, parent: Option<&Path>) {
    let pad = "  ".repeat(depth + 1);
    let mut block = String::new();
    block.push_str(&pad);
    if depth > 0 {
        block.push_str("← ");
    }
    let path = &h.ty.src.path;
    if parent != Some(path.as_path()) {
        block.push_str(&out.e.ws.display(path));
        block.push(' ');
    }
    block.push_str(&sym_header(&h.ty.o, h.ty.i));
    block.push('\n');
    if let Some(m) = &h.method {
        block.push_str(&format!(
            "{pad}  {}\n",
            sym_header(&m.o, m.i).replace(&format!(" (in {})", m.o.qualified_name(h.ty.i)), "")
        ));
    }
    if !out.push_block(&block, path) {
        return;
    }
    for c in &h.children {
        render_impl(out, c, depth + 1, Some(path));
    }
}

fn finish(out: &mut Out<'_>, hint: &str) {
    if out.omitted > 0 {
        let files = out
            .omitted_files
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let tests = if out.omitted_tests > 0 {
            format!(", {} in tests", out.omitted_tests)
        } else {
            String::new()
        };
        out.text.push_str(&format!(
            "… {} more{tests} in: {files}{}. {hint}\n",
            out.omitted,
            if out.omitted_files.len() > 8 {
                ", …"
            } else {
                ""
            }
        ));
    }
}

// ---------------------------------------------------------------------------

pub fn trace(e: &Engine, req: &TraceRequest) -> String {
    let budget = e.budget(Some(
        req.budget
            .unwrap_or(DEFAULT_TRACE_BUDGET.min(e.cfg.default_budget)),
    ));
    let max = e.bytes_for(budget);
    let depth = req.depth.clamp(1, 3);
    // Files parsed per level: enough to fill the budget, not the whole repo.
    let max_files = (max / 90).clamp(24, 800);
    let (target, alts) = match resolve_target(e, &req.target) {
        Ok(t) => t,
        Err(msg) => return format!("{msg}\n"),
    };
    let category = |k: Kind| -> u8 {
        if is_callable(k) {
            0
        } else if k.is_type_like() {
            1
        } else {
            2
        }
    };
    let alts: Vec<Def> = alts
        .into_iter()
        .filter(|d| category(d.sym().kind) == category(target.sym().kind))
        .collect();
    let s = target.sym();
    let mut out = Out {
        e,
        text: String::new(),
        max,
        omitted: 0,
        omitted_tests: 0,
        omitted_files: Vec::new(),
    };
    out.text.push_str(&format!(
        "==> {} of {} ({}:{}-{})\n{}\n",
        req.direction.noun(),
        target.o.qualified_name(target.i),
        e.ws.display(&target.src.path),
        s.start + 1,
        s.end + 1,
        s.label
    ));
    let alt_note = |alts: &[Def]| -> String {
        if alts.is_empty() {
            return String::new();
        }
        let list: Vec<String> = alts
            .iter()
            .take(4)
            .map(|d| {
                format!(
                    "{}:{} {}",
                    e.ws.display(&d.src.path),
                    d.sym().start + 1,
                    d.o.qualified_name(d.i)
                )
            })
            .collect();
        format!(
            "(also matched: {}{}; pass path#Name or Type.name to pick one)\n",
            list.join(", "),
            if alts.len() > 4 { ", …" } else { "" }
        )
    };
    match req.direction {
        Direction::Callers | Direction::Refs => {
            let refs = req.direction == Direction::Refs;
            let all = defs_named(e, std::slice::from_ref(&s.name));
            let (_, others) = with_others(&target, &all);
            let mut stats = SiteStats {
                excluded: 0,
                stopped: false,
                rest_files: 0,
                rest_lines: 0,
            };
            let groups = caller_tree(
                e,
                &target,
                others.clone(),
                if refs { 1 } else { depth },
                refs,
                max_files,
                &mut stats,
            );
            let (sites, n) = count_groups(&groups);
            let what = if refs {
                let imports = groups
                    .iter()
                    .flat_map(|g| &g.sites)
                    .filter(|x| x.kind == OccKind::Import)
                    .count();
                let calls = groups
                    .iter()
                    .flat_map(|g| &g.sites)
                    .filter(|x| x.kind == OccKind::Call)
                    .count();
                format!(
                    "{} ({}, {}, {} other) in {}",
                    plural(sites, "reference"),
                    plural(calls, "call"),
                    plural(imports, "import"),
                    sites - calls - imports,
                    plural(n, "scope")
                )
            } else {
                format!(
                    "{} in {}",
                    plural(sites, "call site"),
                    plural(n, "function")
                )
            };
            out.text.push_str(&what);
            out.text
                .push_str(" · syntactic: comments, strings and the definition excluded");
            if !others.is_empty() {
                out.text.push_str(&format!(
                    "; {} other definition{} named `{}`",
                    others.len(),
                    if others.len() == 1 { "" } else { "s" },
                    s.name
                ));
                if stats.excluded > 0 {
                    out.text.push_str(&format!(
                        " ({} attributed to them left out)",
                        plural(stats.excluded, "site")
                    ));
                }
                out.text.push_str("; ? = receiver unknown");
            }
            if stats.rest_files > 0 {
                out.text.push_str(&format!(
                    "; +{} textual matches in {} farther away not examined (raise budget)",
                    stats.rest_lines,
                    plural(stats.rest_files, "file")
                ));
            }
            if stats.stopped {
                out.text
                    .push_str(&format!("; stopped after {MAX_SITES} lines"));
            }
            out.text.push('\n');
            out.text.push_str(&alt_note(&alts));
            if groups.is_empty() {
                out.text.push_str(if refs {
                    "No references found outside the definition.\n"
                } else {
                    "No call sites found (it may be unused, an entry point, or called dynamically/via an interface; try direction=refs).\n"
                });
                return out.text;
            }
            render_groups(&mut out, &groups);
            finish(&mut out, "Raise budget or trace a narrower target.");
        }
        Direction::Callees => {
            let (nodes, external) = callee_tree(e, &target, depth);
            let resolved = nodes.len();
            out.text.push_str(&format!(
                "{} resolved in the workspace{} · syntactic: best match by receiver, file and package\n",
                plural(resolved, "call"),
                if external.is_empty() {
                    String::new()
                } else {
                    format!(", {} not", external.len())
                }
            ));
            out.text.push_str(&alt_note(&alts));
            for n in &nodes {
                if out.full() {
                    out.omit(&n.defs[0].src.path);
                    continue;
                }
                render_callee(&mut out, &target, n, 0);
            }
            if !external.is_empty() {
                let mut line = String::from("not in workspace (stdlib/dependencies): ");
                for (k, x) in external.iter().enumerate() {
                    if line.len() > 400 {
                        line.push_str(&format!("… +{}", external.len() - k));
                        break;
                    }
                    if k > 0 {
                        line.push_str(", ");
                    }
                    line.push_str(x);
                }
                line.push('\n');
                if out.text.len() + line.len() <= out.max + 400 {
                    out.text.push_str(&line);
                }
            }
            finish(&mut out, "Raise budget or lower depth.");
        }
        Direction::Impls => {
            let mut rest = 0;
            let (base, hits) = impl_tree(e, &target, depth, max_files, &mut rest);
            let go_iface = base.src.lang == Some(LangId::Go) && base.sym().kind == Kind::Interface;
            out.text.push_str(&format!(
                "{} of {} · syntactic: {}\n",
                plural(hits.len(), "implementation"),
                base.o.qualified_name(base.i),
                if go_iface {
                    "types whose method sets cover the interface"
                } else {
                    "inheritance, conformance and impl clauses"
                }
            ));
            if rest > 0 {
                out.text.push_str(&format!(
                    "(+{} more candidate files farther away not examined; raise budget)\n",
                    rest
                ));
            }
            out.text.push_str(&alt_note(&alts));
            if hits.is_empty() {
                out.text.push_str("None found.\n");
                return out.text;
            }
            for h in &hits {
                if out.full() {
                    out.omit(&h.ty.src.path);
                    continue;
                }
                render_impl(&mut out, h, 0, None);
            }
            finish(&mut out, "Raise budget.");
        }
    }
    e.enforce_budget(&mut out.text, budget);
    out.text
}
