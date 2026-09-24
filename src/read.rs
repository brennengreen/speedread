//! The `read` tool: batched, budgeted, symbol-aware reading.
//!
//! Every target becomes an item with a ladder of views (full → skeleton →
//! outline). Items are degraded largest-first — exploratory whole-file reads
//! before explicitly requested symbols, never explicit line ranges — until
//! the batch fits the token budget; anything still too large is cut with an
//! exact continuation target, never silently.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use crate::engine::{Engine, Loaded};
use crate::outline::Outline;
use crate::render::{self, MAX_LINE, MAX_LINE_FULL, numbered_bytes, push_lines};
use crate::source::{BigFile, Source, describe_binary};
use crate::util::{human_bytes, is_etag, parse_etag, thousands};
use crate::walk::{self, WalkOpts};
use crate::workspace::{self, PathError, Workspace};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Full content if it fits the budget, else skeleton, else outline.
    #[default]
    Auto,
    /// Never collapse; paginate with exact continuation targets.
    Full,
    /// Signatures, docs and structure; function bodies collapsed.
    Skeleton,
    /// Symbols with line ranges only.
    Outline,
}

pub struct ReadRequest {
    pub targets: Vec<String>,
    pub mode: Mode,
    pub budget: Option<usize>,
    pub numbers: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Sel {
    Whole,
    /// 1-based inclusive; `None` end = EOF.
    Lines(usize, Option<usize>),
    /// 1-based line whose enclosing symbol should be shown.
    Around(usize),
    Symbol(String),
}

#[derive(Debug)]
struct Target {
    path: Option<String>,
    sel: Sel,
    since: Option<u64>,
}

fn parse_line_spec(spec: &str) -> Option<Sel> {
    let spec = spec.trim();
    let num = |s: &str| -> Option<usize> {
        let s = s.trim().trim_start_matches(['L', 'l']);
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    if let Some((a, b)) = spec.split_once('-') {
        let a = num(a)?;
        if b.trim().is_empty() {
            return Some(Sel::Lines(a.max(1), None));
        }
        let b = num(b)?;
        return Some(Sel::Lines(a.max(1).min(b.max(1)), Some(b.max(a).max(1))));
    }
    num(spec).map(|n| Sel::Around(n.max(1)))
}

fn parse_target(raw: &str, exists: &dyn Fn(&str) -> bool) -> Target {
    let raw = Workspace::clean(raw);
    if let Some(sym) = raw.strip_prefix('#')
        && !sym.is_empty()
        && !exists(&raw)
    {
        return Target {
            path: None,
            sel: Sel::Symbol(sym.to_string()),
            since: None,
        };
    }
    let mut body: &str = &raw;
    let mut since = None;
    if let Some(at) = body.rfind('@') {
        let tag = &body[at + 1..];
        if is_etag(tag) && !exists(body) {
            since = parse_etag(tag);
            body = &body[..at];
        }
    }
    let whole = |p: &str| Target {
        path: Some(p.to_string()),
        sel: Sel::Whole,
        since,
    };
    if body.is_empty() || exists(body) {
        return whole(body);
    }
    if let Some(h) = body.rfind('#') {
        let (p, frag) = (&body[..h], &body[h + 1..]);
        if !p.is_empty() && !frag.is_empty() {
            if frag.starts_with(['L', 'l'])
                && frag[1..].starts_with(|c: char| c.is_ascii_digit())
                && let Some(sel) = parse_line_spec(frag)
            {
                return Target {
                    path: Some(p.to_string()),
                    sel,
                    since,
                };
            }
            return Target {
                path: Some(p.to_string()),
                sel: Sel::Symbol(frag.to_string()),
                since,
            };
        }
    }
    if let Some((p, spec)) = body.rsplit_once(':') {
        // `file:line:col`, as printed by compilers and linters.
        if let Some((p2, line)) = p.rsplit_once(':')
            && !p2.is_empty()
            && !line.is_empty()
            && line.bytes().all(|b| b.is_ascii_digit())
            && !spec.is_empty()
            && spec.bytes().all(|b| b.is_ascii_digit())
        {
            return Target {
                path: Some(p2.to_string()),
                sel: Sel::Around(line.parse::<usize>().unwrap_or(1).max(1)),
                since,
            };
        }
        if !p.is_empty()
            && let Some(sel) = parse_line_spec(spec)
        {
            return Target {
                path: Some(p.to_string()),
                sel,
                since,
            };
        }
    }
    whole(body)
}

fn is_glob(p: &str) -> bool {
    p.contains(['*', '?', '[', '{'])
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Full,
    Skeleton,
    /// Skeleton with long comment blocks and import runs collapsed too.
    Compact,
    Outline,
}

struct Doc {
    src: Arc<Source>,
    disp: String,
    a: usize,
    b: usize,
    whole: bool,
    title: Option<String>,
    note: Option<String>,
    header_override: Option<String>,
    levels: Vec<Level>,
    level: usize,
    outline: Option<Arc<Outline>>,
    skeleton: Option<String>,
    compact: Option<String>,
    outline_text: Option<String>,
    head_tail: bool,
    full_mode: bool,
}

struct BigDoc {
    big: Arc<BigFile>,
    header: String,
    disp: String,
    a: u64,
    b: u64,
}

enum Body {
    Text(String),
    Doc(Box<Doc>),
    Big(BigDoc),
}

struct Item {
    body: Body,
    /// 0 = exploratory (degrade first), 1 = requested symbol, 2 = exact.
    tier: u8,
    limit: Option<usize>,
}

#[derive(Default)]
struct Flags {
    skeleton: bool,
    outline: bool,
    truncated: bool,
}

const HEADER_EST: usize = 90;

impl Doc {
    fn level(&self) -> Level {
        self.levels[self.level]
    }

    fn render_view(&mut self, numbers: bool) {
        match self.level() {
            Level::Skeleton if self.skeleton.is_none() => {
                if let Some(o) = &self.outline {
                    self.skeleton = Some(render::skeleton(&self.src, o, self.a, self.b, numbers));
                }
            }
            Level::Compact if self.compact.is_none() => {
                if let Some(o) = &self.outline {
                    self.compact = Some(render::compact_skeleton(
                        &self.src, o, self.a, self.b, numbers,
                    ));
                }
            }
            Level::Outline if self.outline_text.is_none() => {
                if let Some(o) = &self.outline {
                    self.outline_text = Some(render::outline_text(o, self.a, self.b, None));
                }
            }
            _ => {}
        }
    }

    fn cost_bytes(&mut self, numbers: bool) -> usize {
        self.render_view(numbers);
        HEADER_EST
            + match self.level() {
                Level::Full => numbered_bytes(&self.src, self.a, self.b, numbers),
                Level::Skeleton => self.skeleton.as_ref().map_or(0, |s| s.len()),
                Level::Compact => self.compact.as_ref().map_or(0, |s| s.len()),
                Level::Outline => self.outline_text.as_ref().map_or(0, |s| s.len()),
            }
    }

    fn header(&self) -> String {
        if let Some(h) = &self.header_override {
            return h.clone();
        }
        let src = &self.src;
        let mut h = String::with_capacity(96);
        h.push_str("==> ");
        h.push_str(&self.disp);
        if !self.whole {
            h.push_str(&format!(":{}-{}", self.a + 1, self.b + 1));
        }
        h.push_str(&format!(
            " @{:016x} ({}",
            src.etag(),
            crate::util::plural(src.line_count(), "line")
        ));
        if src.line_count() > 0 && src.data.len() / src.line_count() > 1000 {
            h.push_str(", minified");
        }
        if src.crlf {
            h.push_str(", CRLF");
        }
        if src.bom {
            h.push_str(", BOM");
        }
        if src.utf16 {
            h.push_str(", UTF-16");
        }
        if src.lossy {
            h.push_str(", invalid UTF-8");
        }
        h.push(')');
        match self.level() {
            Level::Skeleton | Level::Compact => h.push_str(" [skeleton]"),
            Level::Outline => h.push_str(" [outline]"),
            Level::Full => {}
        }
        if let Some(t) = &self.title {
            h.push(' ');
            h.push_str(t);
        }
        if let Some(n) = &self.note {
            h.push_str(" (");
            h.push_str(n);
            h.push(')');
        }
        h
    }
}

impl Item {
    fn text(s: String, tier: u8) -> Item {
        Item {
            body: Body::Text(s),
            tier,
            limit: None,
        }
    }

    fn cost(&mut self, e: &Engine, numbers: bool) -> usize {
        match &mut self.body {
            Body::Text(s) => e.estimate(s.as_bytes()),
            Body::Doc(d) => {
                let bpt = e.bpt_for(&d.src, numbers);
                (d.cost_bytes(numbers) as f32 / bpt).ceil() as usize
            }
            Body::Big(b) => {
                let span = b.b.saturating_sub(b.a) + 1;
                let avg = (b.big.len / b.big.lines.max(1)).max(1) + 8;
                e.tokens(HEADER_EST + (span.min(1_000_000) * avg).min(64 << 20) as usize)
            }
        }
    }

    /// Short name for summaries.
    fn label(&self) -> String {
        match &self.body {
            Body::Doc(d) if d.whole => d.disp.clone(),
            Body::Doc(d) => format!("{}:{}-{}", d.disp, d.a + 1, d.b + 1),
            Body::Big(b) => b.disp.clone(),
            Body::Text(t) => {
                let first = t.lines().next().unwrap_or("").trim_start_matches("==> ");
                crate::outline::clip(first, 80)
            }
        }
    }

    fn can_degrade(&self) -> bool {
        matches!(&self.body, Body::Doc(d) if d.level + 1 < d.levels.len())
    }

    fn degrade(&mut self, numbers: bool) {
        if let Body::Doc(d) = &mut self.body {
            let before = d.cost_bytes(numbers);
            d.level += 1;
            // A view that barely saves anything isn't worth the lost detail.
            while matches!(d.level(), Level::Skeleton | Level::Compact)
                && d.level + 1 < d.levels.len()
            {
                if d.cost_bytes(numbers) * 10 >= before * 9 {
                    d.level += 1;
                } else {
                    break;
                }
            }
        }
    }
}

fn ensure_outline(e: &Engine, d: &mut Doc) {
    if d.outline.is_none() {
        d.outline = e.outline(&d.src);
    }
}

fn whole_levels(mode: Mode, has_outline: bool) -> Vec<Level> {
    match (mode, has_outline) {
        (Mode::Auto, true) => vec![Level::Full, Level::Skeleton, Level::Compact, Level::Outline],
        (Mode::Skeleton, true) => vec![Level::Skeleton, Level::Compact, Level::Outline],
        (Mode::Outline, true) => vec![Level::Outline],
        _ => vec![Level::Full],
    }
}

fn is_texty_log(src: &Source) -> bool {
    src.lang.is_none()
}

struct Ctx<'a> {
    e: &'a Engine,
    mode: Mode,
}

impl Ctx<'_> {
    fn doc(&self, src: Arc<Source>, disp: String, a: usize, b: usize, whole: bool) -> Doc {
        Doc {
            head_tail: whole && is_texty_log(&src),
            src,
            disp,
            a,
            b,
            whole,
            title: None,
            note: None,
            header_override: None,
            levels: vec![Level::Full],
            level: 0,
            outline: None,
            skeleton: None,
            compact: None,
            outline_text: None,
            full_mode: self.mode == Mode::Full,
        }
    }

    fn whole_item(&self, src: Arc<Source>, disp: String, note: Option<String>, tier: u8) -> Item {
        let n = src.line_count();
        if n == 0 {
            return Item::text(format!("==> {disp} @{:016x} (empty file)\n", src.etag()), 2);
        }
        let mut d = self.doc(src, disp, 0, n - 1, true);
        d.note = note;
        if self.mode != Mode::Full {
            ensure_outline(self.e, &mut d);
        }
        let has = d.outline.as_ref().is_some_and(|o| !o.is_empty());
        d.levels = whole_levels(self.mode, has);
        if self.mode == Mode::Outline && !has {
            d.note = Some(match d.note.take() {
                Some(n) => format!("{n}; no outline for this file type"),
                None => "no outline for this file type".to_string(),
            });
        }
        Item {
            body: Body::Doc(Box::new(d)),
            tier,
            limit: None,
        }
    }

    fn symbol_item(
        &self,
        src: Arc<Source>,
        outline: Arc<Outline>,
        idx: usize,
        disp: String,
        tier: u8,
    ) -> Item {
        let s = &outline.symbols[idx];
        let (a, b) = (
            s.start as usize,
            (s.end as usize).min(src.line_count().saturating_sub(1)),
        );
        let mut d = self.doc(src, disp, a, b, false);
        d.title = Some(outline.qualified_name(idx));
        let nested = outline
            .symbols
            .iter()
            .any(|c| c.start as usize >= a && c.end as usize <= b && c.collapse.is_some());
        d.levels = match self.mode {
            Mode::Full => vec![Level::Full],
            Mode::Skeleton if nested => vec![Level::Skeleton, Level::Compact, Level::Outline],
            Mode::Outline => vec![Level::Outline],
            _ if nested => vec![Level::Full, Level::Skeleton, Level::Compact, Level::Outline],
            _ => vec![Level::Full],
        };
        d.outline = Some(outline);
        Item {
            body: Body::Doc(Box::new(d)),
            tier,
            limit: None,
        }
    }

    fn lines_item(&self, src: Arc<Source>, disp: String, a1: usize, b1: Option<usize>) -> Item {
        let n = src.line_count();
        if a1 > n {
            return Item::text(
                format!(
                    "==> {disp}:{a1}: past the end of the file ({} lines)\n",
                    thousands(n)
                ),
                2,
            );
        }
        let b = b1.unwrap_or(n).min(n);
        let d = self.doc(src, disp, a1 - 1, b - 1, false);
        Item {
            body: Body::Doc(Box::new(d)),
            tier: 2,
            limit: None,
        }
    }

    fn around_item(&self, src: Arc<Source>, disp: String, line1: usize) -> Item {
        let n = src.line_count();
        if line1 > n {
            return self.lines_item(src, disp, line1, None);
        }
        if let Some(o) = self.e.outline(&src) {
            let mut cur = o.innermost_at((line1 - 1) as u32);
            while let Some(i) = cur {
                let s = &o.symbols[i];
                if s.end - s.start <= 400 {
                    return self.symbol_item(src, o.clone(), i, disp, 2);
                }
                cur = s.parent.map(|p| p as usize);
            }
        }
        let a = line1.saturating_sub(40).max(1);
        let b = (line1 + 40).min(n);
        self.lines_item(src, disp, a, Some(b))
    }

    fn since_item(&self, src: Arc<Source>, disp: String, old: u64, sel: &Sel) -> Vec<Item> {
        let e = self.e;
        let new = src.etag();
        let n = src.line_count();
        if new == old {
            e.remember(&src);
            return vec![Item::text(
                format!(
                    "==> {disp} @{new:016x} unchanged ({} lines)\n",
                    thousands(n)
                ),
                2,
            )];
        }
        let Some(prev) = e.snapshot(old) else {
            return self.select(
                src,
                disp,
                sel,
                Some(format!("no snapshot of @{old:016x}; full content")),
            );
        };
        if src.data.len() > prev.data.len() && src.data.starts_with(&prev.data) {
            let first = if prev.ends_with_newline() {
                prev.line_count()
            } else {
                prev.line_count().saturating_sub(1)
            };
            let first = first.min(n.saturating_sub(1));
            let added = n - first;
            let mut d = self.doc(src, disp.clone(), first, n - 1, false);
            d.header_override = Some(format!(
                "==> {disp} @{new:016x} (was @{old:016x}): +{} appended, now {} lines",
                crate::util::plural(added, "line"),
                thousands(n)
            ));
            d.head_tail = true;
            return vec![Item {
                body: Body::Doc(Box::new(d)),
                tier: 2,
                limit: None,
            }];
        }
        let diff = crate::diff::unified(&prev.data, &src.data, 2);
        let full_cost = numbered_bytes(&src, 0, n.saturating_sub(1), true);
        let (old_o, new_o) = (e.outline(&prev), e.outline(&src));
        let changes = match (&old_o, &new_o) {
            (Some(a), Some(b)) => crate::diff::symbol_changes(a, b, &diff),
            _ => Vec::new(),
        };
        // mode=outline: which symbols changed and how, without the hunks.
        let summary_only = self.mode == Mode::Outline && !changes.is_empty();
        if summary_only || diff.len_estimate() * 10 < full_cost * 7 {
            e.remember(&src);
            let mut t = format!(
                "==> {disp} @{new:016x} (was @{old:016x}): {} hunk{}, +{} -{}, now {} lines\n",
                diff.hunks,
                if diff.hunks == 1 { "" } else { "s" },
                diff.added,
                diff.removed,
                thousands(n)
            );
            t.push_str(&crate::diff::render_changes(&changes, 16));
            if summary_only {
                t.push_str("(hunks omitted for mode=outline; read path#Name for a body)\n");
            } else {
                t.push_str(&diff.render(|g| hunk_label(g, old_o.as_deref(), new_o.as_deref())));
            }
            return vec![Item::text(t, 2)];
        }
        let mut note = format!(
            "changed since @{old:016x}: +{} -{}; full file is smaller than the diff",
            diff.added, diff.removed
        );
        if !changes.is_empty() {
            let names: Vec<&str> = changes.iter().take(12).map(|c| c.name.as_str()).collect();
            note.push_str(&format!("; changed symbols: {}", names.join(", ")));
            if changes.len() > 12 {
                note.push_str(&format!(" (+{} more)", changes.len() - 12));
            }
        }
        self.select(src, disp, sel, Some(note))
    }

    /// Items for a selector on a loaded text file.
    fn select(&self, src: Arc<Source>, disp: String, sel: &Sel, note: Option<String>) -> Vec<Item> {
        let mut items = self.select_inner(src, disp, sel, note.clone());
        if let Some(n) = note {
            for it in &mut items {
                if let Body::Doc(d) = &mut it.body
                    && d.note.is_none()
                {
                    d.note = Some(n.clone());
                }
            }
        }
        items
    }

    fn select_inner(
        &self,
        src: Arc<Source>,
        disp: String,
        sel: &Sel,
        note: Option<String>,
    ) -> Vec<Item> {
        match sel {
            Sel::Whole => vec![self.whole_item(src, disp, note, 0)],
            Sel::Lines(a, b) => vec![self.lines_item(src, disp, *a, *b)],
            Sel::Around(l) => vec![self.around_item(src, disp, *l)],
            Sel::Symbol(name) => {
                let Some(o) = self.e.outline(&src) else {
                    return vec![Item::text(
                        format!(
                            "==> {disp}: no symbol index for this file type; use {disp}:A-B or search\n"
                        ),
                        2,
                    )];
                };
                let hits = o.find(name);
                if hits.is_empty() {
                    let sugg = o.suggest(name, 6);
                    let mut t = format!("==> {disp}: no symbol `{name}`");
                    if !sugg.is_empty() {
                        t.push_str(&format!(". Closest: {}", sugg.join(", ")));
                    }
                    t.push_str(&format!(". Outline: read `{disp}` with mode=outline\n"));
                    return vec![Item::text(t, 2)];
                }
                let mut items: Vec<Item> = hits
                    .iter()
                    .take(8)
                    .map(|&i| self.symbol_item(src.clone(), o.clone(), i, disp.clone(), 1))
                    .collect();
                if hits.len() > 8 {
                    let rest: Vec<String> = hits[8..]
                        .iter()
                        .map(|&i| format!("{}:{}", disp, o.symbols[i].start + 1))
                        .collect();
                    items.push(Item::text(
                        format!("… {} more matches: {}\n", rest.len(), rest.join(", ")),
                        2,
                    ));
                }
                items
            }
        }
    }

    fn big_items(
        &self,
        big: Arc<BigFile>,
        disp: String,
        sel: &Sel,
        since: Option<u64>,
    ) -> Vec<Item> {
        let e = self.e;
        let etag = big.etag();
        if big.binary {
            return vec![Item::text(
                format!("==> {disp} ({}, binary)\n", human_bytes(big.len)),
                2,
            )];
        }
        let lines = big.lines;
        let base = format!(
            "==> {disp} @{etag:016x} ({} lines, {}, streamed)",
            thousands(lines as usize),
            human_bytes(big.len)
        );
        let (a, b, header) = match (since, sel) {
            (Some(old), _) if old == etag => {
                return vec![Item::text(
                    format!(
                        "==> {disp} @{etag:016x} unchanged ({} lines)\n",
                        thousands(lines as usize)
                    ),
                    2,
                )];
            }
            (Some(old), _) => match e.big_snapshot(old).or_else(|| {
                // The previous version may have been small enough to load.
                e.snapshot(old)
                    .filter(|p| !p.bom && !p.utf16)
                    .map(|p| crate::engine::BigSnap {
                        len: p.data.len() as u64,
                        hash: p.hash,
                        lines: p.line_count() as u64,
                        ends_nl: p.ends_with_newline(),
                    })
            }) {
                Some(snap)
                    if big.len > snap.len
                        && big
                            .prefix_hash_matches(snap.len, snap.hash)
                            .unwrap_or(false) =>
                {
                    let first = if snap.ends_nl {
                        snap.lines
                    } else {
                        snap.lines.saturating_sub(1)
                    };
                    (
                        first,
                        lines.saturating_sub(1),
                        format!(
                            "==> {disp} @{etag:016x} (was @{old:016x}): +{} lines appended, now {} lines",
                            thousands((lines - first) as usize),
                            thousands(lines as usize)
                        ),
                    )
                }
                _ => (
                    0,
                    lines.saturating_sub(1),
                    format!("{base} (changed; not append-only)"),
                ),
            },
            (None, Sel::Lines(a, _) | Sel::Around(a)) if *a as u64 > lines => {
                return vec![Item::text(
                    format!(
                        "==> {disp}:{a}: past the end of the file ({} lines)\n",
                        thousands(lines as usize)
                    ),
                    2,
                )];
            }
            (None, Sel::Symbol(name)) => (
                0,
                lines.saturating_sub(1),
                format!(
                    "{base} (symbols are not indexed in streamed files; `#{name}` ignored — use search)"
                ),
            ),
            (None, Sel::Lines(a, b)) => {
                let a = (*a as u64).saturating_sub(1);
                let b = b
                    .map(|b| (b as u64).saturating_sub(1))
                    .unwrap_or(lines.saturating_sub(1))
                    .min(lines.saturating_sub(1))
                    .max(a);
                (
                    a,
                    b,
                    format!(
                        "==> {disp}:{}-{} @{etag:016x} ({} lines, streamed)",
                        a + 1,
                        b + 1,
                        thousands(lines as usize)
                    ),
                )
            }
            (None, Sel::Around(l)) => {
                let l = *l as u64;
                let a = l.saturating_sub(40).max(1) - 1;
                let b = (l + 40).min(lines).saturating_sub(1);
                (
                    a,
                    b,
                    format!(
                        "==> {disp}:{}-{} @{etag:016x} ({} lines, streamed)",
                        a + 1,
                        b + 1,
                        thousands(lines as usize)
                    ),
                )
            }
            (None, _) => (0, lines.saturating_sub(1), base),
        };
        e.remember_big(&big);
        vec![Item {
            body: Body::Big(BigDoc {
                big,
                header,
                disp,
                a,
                b,
            }),
            tier: 2,
            limit: None,
        }]
    }

    fn file_items(
        &self,
        path: &Path,
        disp: String,
        sel: &Sel,
        since: Option<u64>,
        note: Option<String>,
    ) -> Vec<Item> {
        match self.e.load(path) {
            Ok(Loaded::Text(src)) => {
                if src.binary {
                    return vec![Item::text(
                        format!(
                            "==> {disp} ({}, {}) binary, not shown\n",
                            human_bytes(src.stamp.len),
                            describe_binary(&src.data)
                        ),
                        2,
                    )];
                }
                match since {
                    Some(old) => self.since_item(src, disp, old, sel),
                    None => self.select(src, disp, sel, note),
                }
            }
            Ok(Loaded::Big(big)) => self.big_items(big, disp, sel, since),
            Err(err) => vec![Item::text(format!("==> {disp}: {}\n", io_message(&err)), 2)],
        }
    }
}

/// Enclosing symbol of a hunk's first change (git-style function context).
fn hunk_label(
    g: &crate::diff::Group,
    old: Option<&crate::outline::Outline>,
    new: Option<&crate::outline::Outline>,
) -> Option<String> {
    match g.after.iter().find(|r| !r.is_empty()) {
        Some(r) => new.and_then(|o| o.innermost_at(r.start).map(|i| o.qualified_name(i))),
        None => {
            let o = old?;
            let l = g.before.first()?.start;
            o.innermost_at(l).map(|i| o.qualified_name(i))
        }
    }
}

fn io_message(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        Some(libc::EDEADLK) => {
            "iCloud placeholder (not downloaded); open it in Finder to download".to_string()
        }
        Some(libc::EACCES) | Some(libc::EPERM) => {
            "permission denied (macOS privacy protection may require Full Disk Access)".to_string()
        }
        _ => err.to_string(),
    }
}

fn resolve_or_suggest(e: &Engine, raw: &str) -> Result<(PathBuf, Option<String>), String> {
    match e.ws.resolve(raw) {
        Ok(p) => Ok((p, None)),
        Err(PathError::Outside(_)) if e.ws.rootless() => Err(format!(
            "==> {raw}: no workspace root (speedread was started in / without --root); add `--root <project dir>` to its MCP config\n"
        )),
        Err(PathError::Outside(_)) if e.ws.rootless() => Err(format!(
            "==> {raw}: no workspace root (speedread was started in / without --root); add `--root <project dir>` to its MCP config\n"
        )),
        Err(PathError::Outside(p)) => Err(format!(
            "==> {raw}: outside the workspace ({}); start speedread with --root <dir> (or --unrestricted)\n",
            p.display()
        )),
        Err(PathError::NotFound(p)) => {
            let files = e.file_list();
            let wanted = e.ws.display(&p);
            let sugg = workspace::suggest(&wanted, &files, 4);
            let wname = wanted
                .rsplit('/')
                .next()
                .unwrap_or(&wanted)
                .to_ascii_lowercase();
            let same: Vec<&String> = sugg
                .iter()
                .filter(|s| {
                    s.rsplit('/')
                        .next()
                        .unwrap_or(s)
                        .eq_ignore_ascii_case(&wname)
                })
                .collect();
            let unique = same.len() == 1
                && files
                    .iter()
                    .filter(|f| {
                        f.rsplit('/')
                            .next()
                            .unwrap_or(f)
                            .eq_ignore_ascii_case(&wname)
                    })
                    .count()
                    == 1;
            let clean = Workspace::clean(raw);
            let relative =
                !clean.starts_with('/') && !clean.starts_with('~') && !clean.contains("..");
            if unique && relative && wname.contains('.') {
                let p = e.ws.primary().join(same[0]);
                return Ok((p, Some(format!("resolved from {raw}"))));
            }
            let mut t = format!("==> {raw}: not found");
            if !sugg.is_empty() {
                t.push_str(&format!(". Did you mean: {}", sugg.join(", ")));
            }
            t.push('\n');
            Err(t)
        }
    }
}

/// Expand a glob relative to the primary root (or an absolute base).
fn expand_glob(e: &Engine, pat: &str) -> Result<(Vec<PathBuf>, usize), String> {
    let full = e.ws.join(pat);
    let s = full.to_string_lossy().into_owned();
    let mut base = PathBuf::new();
    let mut rest = Vec::new();
    let mut in_glob = false;
    for comp in s.split('/') {
        if !in_glob && !is_glob(comp) {
            base.push(if comp.is_empty() { "/" } else { comp });
        } else {
            in_glob = true;
            rest.push(comp);
        }
    }
    let base = base
        .canonicalize()
        .map_err(|_| format!("==> {pat}: no such directory {}\n", base.display()))?;
    if !e.ws.allowed(&base) {
        return Err(format!("==> {pat}: outside the workspace\n"));
    }
    let glob = format!("/{}", rest.join("/"));
    let opts = WalkOpts {
        globs: vec![glob],
        ..Default::default()
    };
    let (files, _) =
        walk::list_files(&base, &opts, 5000).map_err(|err| format!("==> {pat}: {err}\n"))?;
    let total = files.len();
    Ok((files.into_iter().map(|f| f.path).collect(), total))
}

const MAX_GLOB_FILES: usize = 200;

pub fn read(e: &Engine, req: &ReadRequest) -> String {
    let budget = e.budget(req.budget);
    let numbers = req.numbers;
    let ctx = Ctx { e, mode: req.mode };
    if req.targets.is_empty() {
        return "No targets. Pass e.g. [\"src/main.rs\", \"src/lib.rs#Parser\", \"README.md:1-40\"].\n".to_string();
    }
    let exists = |s: &str| e.ws.join(s).exists();
    let parsed: Vec<Target> = req
        .targets
        .iter()
        .map(|t| parse_target(t, &exists))
        .collect();

    // Resolve paths and warm caches in parallel (read + parse).
    enum Resolved {
        Files(Vec<PathBuf>, Option<String>, usize),
        Dir(PathBuf),
        Symbol(String),
        Error(String),
    }
    let resolved: Vec<Resolved> = parsed
        .par_iter()
        .map(|t| match &t.path {
            None => match &t.sel {
                Sel::Symbol(s) => Resolved::Symbol(s.clone()),
                _ => Resolved::Error("bad target\n".into()),
            },
            Some(p) if is_glob(p) && !e.ws.join(p).exists() => match expand_glob(e, p) {
                Ok((files, total)) if !files.is_empty() => Resolved::Files(files, None, total),
                Ok(_) => Resolved::Error(format!("==> {p}: no files match\n")),
                Err(msg) => Resolved::Error(msg),
            },
            Some(p) => match resolve_or_suggest(e, p) {
                Ok((path, note)) if path.is_dir() => {
                    let _ = note;
                    Resolved::Dir(path)
                }
                Ok((path, note)) => Resolved::Files(vec![path], note, 1),
                Err(msg) => Resolved::Error(msg),
            },
        })
        .collect();
    let per_target = budget / parsed.len().max(1);
    let warm: Vec<(&PathBuf, bool)> = resolved
        .iter()
        .zip(&parsed)
        .filter_map(|(r, t)| match r {
            Resolved::Files(f, _, _) => Some(f.iter().take(MAX_GLOB_FILES).map(move |p| {
                (
                    p,
                    !matches!(t.sel, Sel::Whole | Sel::Lines(..)) || f.len() > 1,
                )
            })),
            _ => None,
        })
        .flatten()
        .collect();
    warm.par_iter().for_each(|(p, force_outline)| {
        if let Ok(Loaded::Text(s)) = e.load(p)
            && (*force_outline || e.tokens(s.data.len()) > per_target)
            && req.mode != Mode::Full
        {
            let _ = e.outline(&s);
        }
    });

    let mut items: Vec<Item> = Vec::new();
    for (t, r) in parsed.iter().zip(resolved) {
        match r {
            Resolved::Error(msg) => items.push(Item::text(msg, 2)),
            Resolved::Dir(path) => {
                let text = crate::map::tree(e, &path, per_target.max(400), false, &[], Some(2));
                items.push(Item::text(text, 2));
            }
            Resolved::Symbol(name) => items.extend(workspace_symbol(&ctx, &name)),
            Resolved::Files(files, note, total) => {
                let many = files.len() > 1;
                for p in files.iter().take(MAX_GLOB_FILES) {
                    let disp = e.ws.display(p);
                    let mut its = ctx.file_items(p, disp, &t.sel, t.since, note.clone());
                    if many {
                        for it in &mut its {
                            it.tier = 0;
                        }
                    }
                    items.extend(its);
                }
                if total > MAX_GLOB_FILES {
                    items.push(Item::text(
                        format!(
                            "… {} more files match; narrow the glob\n",
                            thousands(total - MAX_GLOB_FILES)
                        ),
                        2,
                    ));
                }
            }
        }
    }
    fit(e, &mut items, budget, numbers);
    let mut out = String::new();
    let mut flags = Flags::default();
    let budget_bytes = e.bytes_for(budget);
    let mut omitted: Vec<String> = Vec::new();
    for it in &mut items {
        if it.limit == Some(0) {
            omitted.push(it.label());
            continue;
        }
        render_item(e, it, numbers, budget_bytes, &mut out, &mut flags);
    }
    if !omitted.is_empty() {
        flags.truncated = true;
        let mut names = String::new();
        for (i, n) in omitted.iter().enumerate() {
            if names.len() > 1200 {
                names.push_str(&format!(", … {} more", omitted.len() - i));
                break;
            }
            if i > 0 {
                names.push_str(", ");
            }
            names.push_str(n);
        }
        out.push_str(&format!(
            "⋯ {} omitted to fit the budget (read them in another call): {names}\n",
            crate::util::plural(omitted.len(), "target")
        ));
    }
    if flags.skeleton {
        out.push_str("[skeleton]: bodies collapsed; each `A-B ⋯` line marks elided lines. Expand with path#Name or path:A-B.\n");
    }
    if flags.outline {
        out.push_str("[outline]: `A-B signature` per symbol; (+N members) = nested symbols not listed. Read path#Name or path:A-B.\n");
    }
    if flags.truncated {
        out.push_str(&format!(
            "Output cut to fit budget={budget}; use the continuation targets above, narrower targets, or a larger budget.\n"
        ));
    }
    e.enforce_budget(&mut out, budget);
    out
}

fn workspace_symbol(ctx: &Ctx<'_>, name: &str) -> Vec<Item> {
    let e = ctx.e;
    let (hits, total) = crate::search::find_definitions(e, name, 12);
    if hits.is_empty() {
        return vec![Item::text(
            format!(
                "==> #{name}: no definition found in {} (try search)\n",
                e.ws.display(&e.ws.primary())
            ),
            2,
        )];
    }
    let shown = 5.min(hits.len());
    let mut items: Vec<Item> = hits[..shown]
        .iter()
        .map(|(src, o, i)| ctx.symbol_item(src.clone(), o.clone(), *i, e.ws.display(&src.path), 1))
        .collect();
    if hits.len() > shown || total > hits.len() {
        let rest: Vec<String> = hits[shown..]
            .iter()
            .map(|(src, o, i)| {
                format!(
                    "{}:{} {}",
                    e.ws.display(&src.path),
                    o.symbols[*i].start + 1,
                    o.qualified_name(*i)
                )
            })
            .collect();
        let more = total.saturating_sub(shown);
        items.push(Item::text(
            format!(
                "… {more} more definitions of `{name}`: {}\n",
                rest.join(", ")
            ),
            2,
        ));
    }
    items
}

fn fit(e: &Engine, items: &mut [Item], budget: usize, numbers: bool) {
    for _ in 0..items.len() * 4 + 4 {
        let costs: Vec<usize> = items.iter_mut().map(|it| it.cost(e, numbers)).collect();
        if costs.iter().sum::<usize>() <= budget {
            return;
        }
        let pick = (0..items.len())
            .filter(|&i| items[i].can_degrade())
            .max_by_key(|&i| (Reverse(items[i].tier), costs[i]));
        match pick {
            Some(i) => {
                if let Body::Doc(d) = &mut items[i].body {
                    ensure_outline(e, d);
                    if d.outline.as_ref().is_none_or(|o| o.is_empty()) {
                        d.levels.truncate(1);
                        continue;
                    }
                }
                items[i].degrade(numbers)
            }
            None => break,
        }
    }
    let costs: Vec<usize> = items.iter_mut().map(|it| it.cost(e, numbers)).collect();
    if costs.iter().sum::<usize>() <= budget {
        return;
    }
    // Fair shares, smallest first. An item whose share can't hold a
    // meaningful excerpt is omitted and listed in one summary line instead.
    const MIN_ITEM: usize = 80;
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| costs[i]);
    let mut remaining = budget.saturating_sub(60);
    let mut left = items.len();
    for i in order {
        let share = remaining / left.max(1);
        let alloc = if costs[i] <= share {
            costs[i]
        } else if share >= MIN_ITEM {
            share
        } else {
            0
        };
        if alloc < costs[i] {
            items[i].limit = Some(alloc);
        }
        remaining = remaining.saturating_sub(alloc);
        left -= 1;
    }
}

fn render_item(
    e: &Engine,
    it: &mut Item,
    numbers: bool,
    budget_bytes: usize,
    out: &mut String,
    flags: &mut Flags,
) {
    // Each item converts tokens to bytes at its own density.
    let bpt = match &it.body {
        Body::Doc(d) => e.bpt_for(&d.src, numbers),
        _ => e.bytes_for(1000) as f32 / 1000.0,
    };
    let limit_bytes = it.limit.map(|l| (l as f32 * bpt) as usize);
    let budget_bytes = match &it.body {
        Body::Doc(_) => {
            ((budget_bytes as f32 / (e.bytes_for(1000) as f32 / 1000.0)) * bpt) as usize
        }
        _ => budget_bytes,
    };
    match &mut it.body {
        Body::Text(s) => match limit_bytes {
            Some(max) => {
                let (cut, was_cut) = render::cut_lines(s, max.saturating_sub(40));
                out.push_str(cut);
                if was_cut {
                    out.push_str("⋯ truncated\n");
                    flags.truncated = true;
                }
            }
            None => out.push_str(s),
        },
        Body::Doc(d) => {
            d.render_view(numbers);
            let header = d.header();
            out.push_str(&header);
            out.push('\n');
            e.remember(&d.src);
            let max = limit_bytes.map(|m| m.saturating_sub(header.len() + 90));
            match d.level() {
                Level::Full => render_full(d, numbers, max, out, flags),
                Level::Skeleton | Level::Compact | Level::Outline => {
                    let is_sk = matches!(d.level(), Level::Skeleton | Level::Compact);
                    if is_sk {
                        flags.skeleton = true;
                    } else {
                        flags.outline = true;
                    }
                    let text = match d.level() {
                        Level::Skeleton => d.skeleton.as_deref(),
                        Level::Compact => d.compact.as_deref(),
                        _ => d.outline_text.as_deref(),
                    }
                    .unwrap_or("");
                    let fitted;
                    let text = match (d.level(), max, &d.outline) {
                        (Level::Outline, Some(m), Some(o)) if text.len() > m => {
                            fitted = render::outline_fit(o, d.a, d.b, m);
                            fitted.as_str()
                        }
                        _ => text,
                    };
                    match max {
                        Some(m) => {
                            let (cut, was_cut) = render::cut_lines(text, m);
                            out.push_str(cut);
                            if was_cut {
                                flags.truncated = true;
                                let next = cut.lines().count();
                                let _ = next;
                                out.push_str(&format!(
                                    "⋯ truncated; read narrower ranges such as {}:A-B{}\n",
                                    d.disp,
                                    if is_sk { " or use mode=outline" } else { "" }
                                ));
                            }
                        }
                        None => out.push_str(text),
                    }
                }
            }
        }
        Body::Big(b) => render_big(
            b,
            numbers,
            Some(limit_bytes.unwrap_or(budget_bytes)),
            out,
            flags,
        ),
    }
}

fn render_full(d: &Doc, numbers: bool, max: Option<usize>, out: &mut String, flags: &mut Flags) {
    let src = &d.src;
    let max_line = if d.full_mode || !d.whole {
        MAX_LINE_FULL
    } else {
        MAX_LINE
    };
    let Some(max) = max else {
        push_lines(out, src, d.a, d.b, numbers, max_line, usize::MAX);
        return;
    };
    flags.truncated = true;
    if d.head_tail {
        let head = max * 7 / 10;
        let tail = max - head;
        let h_last = push_lines(out, src, d.a, d.b, numbers, max_line, head).unwrap_or(d.a);
        if h_last >= d.b {
            return;
        }
        let mut t = d.b;
        while t > h_last + 1 && numbered_bytes(src, t - 1, d.b, numbers) <= tail {
            t -= 1;
        }
        if t > h_last + 1 {
            out.push_str(&format!(
                "⋯ lines {}-{} omitted ({}); read {}:{}-{} or search\n",
                h_last + 2,
                t,
                crate::util::plural(t - h_last - 1, "line"),
                d.disp,
                h_last + 2,
                t
            ));
        }
        push_lines(out, src, t, d.b, numbers, max_line, usize::MAX);
    } else {
        let last = push_lines(out, src, d.a, d.b, numbers, max_line, max).unwrap_or(d.a);
        if last < d.b {
            out.push_str(&format!(
                "⋯ truncated at budget; continue with {}:{}-{}\n",
                d.disp,
                last + 2,
                d.b + 1
            ));
        }
    }
}

fn render_big(
    b: &BigDoc,
    numbers: bool,
    limit: Option<usize>,
    out: &mut String,
    flags: &mut Flags,
) {
    out.push_str(&b.header);
    out.push('\n');
    let max = limit
        .unwrap_or(64 << 10)
        .saturating_sub(b.header.len() + 90);
    let push = |out: &mut String, start: u64, lines: &[(Vec<u8>, bool)]| {
        for (k, (l, _)) in lines.iter().enumerate() {
            render::push_line(out, start as usize + k + 1, l, numbers, MAX_LINE);
        }
    };
    let head_bytes = max * 7 / 10;
    let head = b.big.read_lines(b.a, b.b, head_bytes).unwrap_or_default();
    let head_end = b.a + head.len() as u64;
    push(out, b.a, &head);
    if head_end > b.b {
        return;
    }
    flags.truncated = true;
    let tail_bytes = max - head_bytes;
    let tail_start = b.b.saturating_sub(400).max(head_end);
    let tail = b
        .big
        .read_lines(tail_start, b.b, usize::MAX)
        .unwrap_or_default();
    let mut used = 0usize;
    let mut keep = tail.len();
    for (k, (l, _)) in tail.iter().enumerate().rev() {
        used += l.len() + 8;
        if used > tail_bytes {
            break;
        }
        keep = k;
    }
    let t0 = tail_start + keep as u64;
    if t0 > head_end {
        out.push_str(&format!(
            "⋯ lines {}-{} omitted; read {}:{}-{} or search\n",
            head_end + 1,
            t0,
            b.disp,
            head_end + 1,
            t0
        ));
    }
    push(out, t0, &tail[keep..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Target {
        parse_target(s, &|_| false)
    }

    #[test]
    fn target_syntax() {
        assert_eq!(p("src/a.rs").sel, Sel::Whole);
        assert_eq!(p("src/a.rs:10-20").sel, Sel::Lines(10, Some(20)));
        assert_eq!(p("src/a.rs:10-").sel, Sel::Lines(10, None));
        assert_eq!(p("src/a.rs:42").sel, Sel::Around(42));
        let t = p("src/a.rs:42:7");
        assert_eq!(
            (t.path.as_deref(), t.sel),
            (Some("src/a.rs"), Sel::Around(42))
        );
        assert_eq!(p("src/a.rs#L5-L9").sel, Sel::Lines(5, Some(9)));
        assert_eq!(
            p("src/a.rs#Parser.parse").sel,
            Sel::Symbol("Parser.parse".into())
        );
        let t = p("#parse_header");
        assert_eq!((t.path, t.sel), (None, Sel::Symbol("parse_header".into())));
        let t = p("logs/app.log@0a1b2c3d4e5f6071");
        assert_eq!(
            (t.path.as_deref(), t.since),
            (Some("logs/app.log"), Some(0x0a1b2c3d4e5f6071))
        );
        // Old 8-digit tags are not etags (no silent partial matching).
        assert_eq!(p("logs/app.log@0a1b2c3d").since, None);
        let t = p("node_modules/@types/node/index.d.ts");
        assert_eq!(
            (t.path.as_deref(), t.since),
            (Some("node_modules/@types/node/index.d.ts"), None)
        );
        assert_eq!(p("`src/a.rs`").path.as_deref(), Some("src/a.rs"));
    }
}
