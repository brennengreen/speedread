//! The `map` tool: a budgeted, .gitignore-aware directory overview.
//!
//! Directories are expanded breadth-first until the token budget is spent,
//! so the output is always a complete picture at some depth instead of a
//! listing that stops halfway down. Single-child directory chains collapse to
//! one line; with `symbols`, each file lists its top-level definitions.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

use rayon::prelude::*;

use crate::engine::Engine;
use crate::outline::Kind;
use crate::util::{human_bytes, plural, thousands};
use crate::walk::{self, WalkOpts};
use crate::workspace::PathError;

pub struct MapRequest {
    pub path: Option<String>,
    pub depth: Option<usize>,
    pub symbols: bool,
    pub globs: Vec<String>,
    pub budget: Option<usize>,
}

struct Dir {
    name: String,
    parent: usize,
    depth: usize,
    dirs: Vec<usize>,
    files: Vec<(String, u64)>,
    total_files: usize,
    total_bytes: u64,
}

/// Files listed per expanded directory before summarizing the rest.
const MAX_FILES: usize = 14;
/// Files listed at the root before summarizing: the root is always shown in
/// full for orientation (up to this sanity cap).
const MAX_ROOT_FILES: usize = 80;

fn file_cap(d: usize) -> usize {
    if d == 0 { MAX_ROOT_FILES } else { MAX_FILES }
}
/// Subdirectories listed per expanded directory (largest first).
const MAX_DIRS: usize = 24;

/// Orientation should be cheap: `map` defaults to a smaller budget.
pub const DEFAULT_MAP_BUDGET: usize = 3000;

pub fn map(e: &Engine, req: &MapRequest) -> String {
    let budget = e.budget(Some(
        req.budget
            .unwrap_or(DEFAULT_MAP_BUDGET.min(e.cfg.default_budget)),
    ));
    let path = match &req.path {
        None => e.ws.primary().to_path_buf(),
        Some(p) => match e.ws.resolve(p) {
            Ok(p) => p,
            Err(PathError::NotFound(_)) => {
                let sugg = crate::workspace::suggest(p, &e.file_list(), 3);
                return if sugg.is_empty() {
                    format!("Not found: {p}\n")
                } else {
                    format!("Not found: {p}. Did you mean: {}\n", sugg.join(", "))
                };
            }
            Err(PathError::Outside(p)) => {
                return format!("Outside the workspace: {}\n", p.display());
            }
        },
    };
    if path.is_file() {
        let r = crate::read::ReadRequest {
            targets: vec![path.to_string_lossy().into_owned()],
            mode: crate::read::Mode::Outline,
            budget: Some(budget),
            numbers: true,
        };
        return crate::read::read(e, &r);
    }
    tree(e, &path, budget, req.symbols, &req.globs, req.depth)
}

pub fn tree(
    e: &Engine,
    root: &Path,
    budget: usize,
    symbols: bool,
    globs: &[String],
    max_depth: Option<usize>,
) -> String {
    let opts = WalkOpts {
        globs: globs.to_vec(),
        include_symlinks: true,
        ..Default::default()
    };
    let (files, truncated) = match walk::list_files(root, &opts, 400_000) {
        Ok(v) => v,
        Err(err) => return format!("Cannot walk {}: {err}\n", e.ws.display(root)),
    };
    let mut dirs: Vec<Dir> = vec![Dir {
        name: String::new(),
        parent: 0,
        depth: 0,
        dirs: Vec::new(),
        files: Vec::new(),
        total_files: 0,
        total_bytes: 0,
    }];
    let mut index: HashMap<(usize, String), usize> = HashMap::new();
    let mut dataless = 0usize;
    for f in &files {
        let Ok(rel) = f.path.strip_prefix(root) else {
            continue;
        };
        if f.dataless {
            dataless += 1;
        }
        let comps: Vec<String> = rel
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect();
        let Some((name, parents)) = comps.split_last() else {
            continue;
        };
        let mut cur = 0usize;
        for p in parents {
            let key = (cur, p.clone());
            cur = match index.get(&key) {
                Some(&i) => i,
                None => {
                    let i = dirs.len();
                    let depth = dirs[cur].depth + 1;
                    dirs.push(Dir {
                        name: p.clone(),
                        parent: cur,
                        depth,
                        dirs: Vec::new(),
                        files: Vec::new(),
                        total_files: 0,
                        total_bytes: 0,
                    });
                    dirs[cur].dirs.push(i);
                    index.insert(key, i);
                    i
                }
            };
        }
        dirs[cur].files.push((name.clone(), f.size));
    }
    // Totals bottom-up (children always have larger indices than parents).
    for i in (0..dirs.len()).rev() {
        let (nf, nb) = (
            dirs[i].files.len(),
            dirs[i].files.iter().map(|f| f.1).sum::<u64>(),
        );
        let (cf, cb) = dirs[i].dirs.iter().fold((0usize, 0u64), |acc, &c| {
            (acc.0 + dirs[c].total_files, acc.1 + dirs[c].total_bytes)
        });
        dirs[i].total_files = nf + cf;
        dirs[i].total_bytes = nb + cb;
    }
    for i in 0..dirs.len() {
        let mut ds = std::mem::take(&mut dirs[i].dirs);
        ds.sort_by(|a, b| dirs[*a].name.cmp(&dirs[*b].name));
        dirs[i].dirs = ds;
        dirs[i].files.sort();
    }

    let mut max_bytes = e.bytes_for(budget);
    let root_disp = e.ws.display(root);
    let mut header = format!(
        "==> {}/ ({}, {}; .gitignore respected{})\n",
        root_disp.trim_end_matches('/'),
        plural(dirs[0].total_files, "file"),
        human_bytes(dirs[0].total_bytes),
        if truncated { "; listing capped" } else { "" }
    );
    if dataless > 0 {
        header.push_str(&format!(
            "({} iCloud placeholders not downloaded)\n",
            thousands(dataless)
        ));
    }
    if dirs[0].total_files == 0 {
        header.push_str("(no files)\n");
        return header;
    }

    // Per-file annotations (line counts, optional symbols) computed lazily.
    let mut notes: HashMap<(usize, usize), String> = HashMap::new();
    let annotate = |d: usize, dirs: &[Dir], notes: &mut HashMap<(usize, usize), String>| {
        let dir_path = dir_path(dirs, d, root);
        let items: Vec<(usize, String)> = dirs[d]
            .files
            .par_iter()
            .enumerate()
            .take(file_cap(d))
            .map(|(k, (name, size))| (k, file_note(e, &dir_path.join(name), *size, symbols)))
            .collect();
        for (k, n) in items {
            notes.insert((d, k), n);
        }
    };
    let listing_cost = |d: usize, dirs: &[Dir], notes: &HashMap<(usize, usize), String>| -> usize {
        let indent = 2 * dirs[d].depth;
        let mut c = 0;
        for &cd in dirs[d].dirs.iter().take(MAX_DIRS) {
            c += indent + dirs[cd].name.len() + 40;
        }
        if dirs[d].dirs.len() > MAX_DIRS {
            c += indent + 50;
        }
        for (k, (name, _)) in dirs[d].files.iter().enumerate().take(file_cap(d)) {
            c += indent + name.len() + 2 + notes.get(&(d, k)).map_or(6, |n| n.len());
        }
        if dirs[d].files.len() > file_cap(d) {
            c += indent + 60;
        }
        c
    };
    annotate(0, &dirs, &mut notes);
    // Importance-weighted expansion: a directory's value is its file count,
    // discounted 4× per level of depth and 8× per penalty step (hidden, test,
    // vendored…); top-level source directories come first.
    let score = |dirs: &[Dir], d: usize| -> u64 {
        let files = dirs[d].total_files as f64;
        let depth = dirs[d].depth.max(1) as i32;
        let pen = penalty(&dirs[d].name) as i32;
        let mut v = files / 4f64.powi(depth - 1) / 8f64.powi(pen);
        if depth == 1 && pen == 0 {
            v *= 1000.0;
        }
        (v * 1000.0) as u64
    };
    // Byte-sized expansion, then a content-aware check: listings are dense
    // (numbers, short names), so shrink the allowance and retry if needed.
    let mut out = String::new();
    for _attempt in 0..4 {
        let mut expanded = vec![false; dirs.len()];
        expanded[0] = true;
        let mut used = header.len() + listing_cost(0, &dirs, &notes);
        let mut heap: BinaryHeap<(u64, Reverse<usize>)> = BinaryHeap::new();
        for c in visible_dirs(&dirs, 0) {
            heap.push((score(&dirs, c), Reverse(c)));
        }
        while let Some((_, Reverse(d))) = heap.pop() {
            if max_depth.is_some_and(|m| dirs[d].depth >= m) {
                continue;
            }
            let d = chain_end(&dirs, d);
            annotate(d, &dirs, &mut notes);
            let c = listing_cost(d, &dirs, &notes);
            if used + c > max_bytes {
                continue;
            }
            used += c;
            expanded[d] = true;
            for c in visible_dirs(&dirs, d) {
                heap.push((score(&dirs, c), Reverse(c)));
            }
        }
        out = header.clone();
        render_dir(&dirs, 0, &expanded, &notes, &mut out, 0);
        let est = e.estimate(out.as_bytes());
        if est <= budget || max_bytes < 1024 {
            break;
        }
        max_bytes = (max_bytes as f64 * budget as f64 / est as f64 * 0.97) as usize;
    }
    e.enforce_budget(&mut out, budget);
    out
}

/// Subdirectories shown in a listing: the largest `MAX_DIRS`, alphabetical.
fn visible_dirs(dirs: &[Dir], d: usize) -> Vec<usize> {
    let mut v = dirs[d].dirs.clone();
    if v.len() > MAX_DIRS {
        v.sort_by(|a, b| {
            dirs[*b]
                .total_files
                .cmp(&dirs[*a].total_files)
                .then(dirs[*a].name.cmp(&dirs[*b].name))
        });
        v.truncate(MAX_DIRS);
        v.sort_by(|a, b| dirs[*a].name.cmp(&dirs[*b].name));
    }
    v
}

/// Expansion penalty: 0 for ordinary source directories.
fn penalty(name: &str) -> u8 {
    let n = name.to_ascii_lowercase();
    if n.starts_with('.') {
        return 3;
    }
    match n.as_str() {
        "vendor" | "third_party" | "third-party" | "external" | "deps" | "pods" | "carthage"
        | "coverage" => 3,
        "test" | "tests" | "__tests__" | "spec" | "specs" | "fixtures" | "testdata"
        | "test-data" | "e2e" | "benches" | "examples" | "samples" | "docs" | "doc" => 2,
        _ => 0,
    }
}

/// `.ts 35, .json 3` for a list of file names.
fn ext_summary<'a>(names: impl Iterator<Item = &'a str>, top: usize) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in names {
        let ext = match n.rsplit_once('.') {
            Some((stem, e)) if !stem.is_empty() => e,
            _ => "",
        };
        *counts.entry(ext).or_insert(0) += 1;
    }
    let mut v: Vec<(&str, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.into_iter()
        .take(top)
        .map(|(e, c)| {
            if e.is_empty() {
                format!("(no ext) {c}")
            } else {
                format!(".{e} {c}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Follow single-child chains with no files (`src/main/java/com/acme`).
fn chain_end(dirs: &[Dir], mut d: usize) -> usize {
    while dirs[d].files.is_empty() && dirs[d].dirs.len() == 1 {
        d = dirs[d].dirs[0];
    }
    d
}

fn dir_path(dirs: &[Dir], d: usize, root: &Path) -> std::path::PathBuf {
    let mut chain = Vec::new();
    let mut cur = d;
    while cur != 0 {
        chain.push(dirs[cur].name.as_str());
        cur = dirs[cur].parent;
    }
    let mut p = root.to_path_buf();
    for c in chain.iter().rev() {
        p.push(c);
    }
    p
}

fn file_note(e: &Engine, path: &Path, size: u64, symbols: bool) -> String {
    if let Ok(md) = std::fs::symlink_metadata(path)
        && md.file_type().is_symlink()
    {
        return match std::fs::read_link(path) {
            Ok(t) => format!("→ {}", t.display()),
            Err(_) => "→ ?".to_string(),
        };
    }
    if size > crate::source::MAX_PARSE as u64 {
        return human_bytes(size);
    }
    if symbols && let Ok(src) = e.load_text(path) {
        if src.binary {
            return human_bytes(size);
        }
        let mut note = format!("{}L", src.line_count());
        if let Some(o) = e.outline(&src) {
            let names: Vec<&str> = o
                .top_level_names()
                .filter(|s| !matches!(s.kind, Kind::Key | Kind::Heading) || o.symbols.len() < 40)
                .map(|s| s.name.as_str())
                .collect();
            if !names.is_empty() {
                let mut list = String::new();
                for (i, n) in names.iter().enumerate() {
                    if list.len() + n.len() > 160 {
                        list.push_str(&format!(", +{}", names.len() - i));
                        break;
                    }
                    if i > 0 {
                        list.push_str(", ");
                    }
                    list.push_str(n);
                }
                note.push_str(": ");
                note.push_str(&list);
            }
        }
        return note;
    }
    match std::fs::read(path) {
        Ok(data) => {
            if memchr::memchr(0, &data[..data.len().min(8192)]).is_some() {
                human_bytes(size)
            } else {
                let n = memchr::memchr_iter(b'\n', &data).count()
                    + usize::from(!data.is_empty() && !data.ends_with(b"\n"));
                format!("{n}L")
            }
        }
        Err(_) => human_bytes(size),
    }
}

fn render_dir(
    dirs: &[Dir],
    d: usize,
    expanded: &[bool],
    notes: &HashMap<(usize, usize), String>,
    out: &mut String,
    indent: usize,
) {
    let pad = "  ".repeat(indent);
    let shown_dirs = visible_dirs(dirs, d);
    let hidden_dirs: Vec<usize> = dirs[d]
        .dirs
        .iter()
        .copied()
        .filter(|c| !shown_dirs.contains(c))
        .collect();
    for &cd in &shown_dirs {
        // Collapse single-child chains into one path segment.
        let mut name = dirs[cd].name.clone();
        let mut end = cd;
        while dirs[end].files.is_empty() && dirs[end].dirs.len() == 1 {
            end = dirs[end].dirs[0];
            name.push('/');
            name.push_str(&dirs[end].name);
        }
        if expanded[end] {
            out.push_str(&format!("{pad}{name}/\n"));
            render_dir(dirs, end, expanded, notes, out, indent + 1);
        } else {
            let mut line = format!(
                "{pad}{name}/ ({}, {}",
                plural(dirs[end].total_files, "file"),
                human_bytes(dirs[end].total_bytes)
            );
            if dirs[end].total_files >= 8 {
                let mut names = Vec::new();
                collect_names(dirs, end, &mut names, 5000);
                line.push_str(": ");
                line.push_str(&ext_summary(names.into_iter(), 3));
            }
            line.push_str(")\n");
            out.push_str(&line);
        }
    }
    if !hidden_dirs.is_empty() {
        let nfiles: usize = hidden_dirs.iter().map(|&h| dirs[h].total_files).sum();
        let mut names: Vec<&str> = hidden_dirs
            .iter()
            .take(6)
            .map(|&h| dirs[h].name.as_str())
            .collect();
        if hidden_dirs.len() > 6 {
            names.push("…");
        }
        out.push_str(&format!(
            "{pad}⋯ {} more directories, {} ({})\n",
            thousands(hidden_dirs.len()),
            plural(nfiles, "file"),
            names.join(", ")
        ));
    }
    let files = &dirs[d].files;
    let shown = if files.len() > file_cap(d) + 2 {
        file_cap(d)
    } else {
        files.len()
    };
    for (k, (name, size)) in files.iter().enumerate().take(shown) {
        let note = notes
            .get(&(d, k))
            .cloned()
            .unwrap_or_else(|| human_bytes(*size));
        out.push_str(&format!("{pad}{name} {note}\n"));
    }
    if files.len() > shown {
        let rest = files[shown..].iter().map(|(n, _)| n.as_str());
        out.push_str(&format!(
            "{pad}⋯ {} more ({})\n",
            plural(files.len() - shown, "file"),
            ext_summary(rest, 4)
        ));
    }
}

fn collect_names<'a>(dirs: &'a [Dir], d: usize, out: &mut Vec<&'a str>, cap: usize) {
    for (n, _) in &dirs[d].files {
        if out.len() >= cap {
            return;
        }
        out.push(n);
    }
    for &c in &dirs[d].dirs {
        collect_names(dirs, c, out, cap);
    }
}
