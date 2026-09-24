//! JSON Lines output for scripts and code-execution agents.
//!
//! Anthropic's "Code execution with MCP" recommends letting agents filter
//! and aggregate tool results in code, so only the final answer reaches the
//! model. These emitters expose speedread's structure (symbols, grouped
//! search hits, file sizes) as one JSON object per line for `jq`, Python or
//! TypeScript to consume.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde_json::{Value, json};

use crate::engine::Engine;
use crate::outline::Outline;
use crate::search::{self, Output, SearchRequest};
use crate::walk::{self, WalkOpts};
use crate::workspace::PathError;

const MAX_TEXT: usize = 1000;

fn write_line(w: &mut impl Write, v: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *w, v)?;
    w.write_all(b"\n")
}

fn kind_name(k: crate::outline::Kind) -> String {
    format!("{k:?}").to_lowercase()
}

fn symbol_fields(o: &Outline, i: usize) -> serde_json::Map<String, Value> {
    let s = &o.symbols[i];
    let mut m = serde_json::Map::new();
    m.insert("name".into(), json!(s.name));
    m.insert("qualified".into(), json!(o.qualified_name(i)));
    m.insert("kind".into(), json!(kind_name(s.kind)));
    m.insert("start".into(), json!(s.start + 1));
    m.insert("end".into(), json!(s.end + 1));
    m.insert("def".into(), json!(s.def + 1));
    m.insert("depth".into(), json!(s.depth));
    m.insert("signature".into(), json!(s.label));
    m
}

fn resolve(e: &Engine, p: &str) -> Result<PathBuf, String> {
    match e.ws.resolve(p) {
        Ok(r) => Ok(r),
        Err(PathError::NotFound(_)) => Err(format!("not found: {p}")),
        Err(PathError::Outside(r)) => Err(format!("outside the workspace: {}", r.display())),
    }
}

fn is_glob(p: &str) -> bool {
    p.contains(['*', '?', '[', '{'])
}

/// Files named by `targets` (files, directories or globs), in path order.
fn expand(e: &Engine, targets: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let targets: Vec<String> = if targets.is_empty() {
        vec![".".into()]
    } else {
        targets.to_vec()
    };
    for t in &targets {
        let (root, globs) = match resolve(e, t) {
            Ok(p) => (p, Vec::new()),
            Err(err) if is_glob(t) => {
                let _ = err;
                (e.ws.primary().to_path_buf(), vec![t.clone()])
            }
            Err(err) => return Err(err),
        };
        if root.is_file() {
            out.push(root);
            continue;
        }
        let opts = WalkOpts {
            globs,
            ..Default::default()
        };
        let (files, _) = walk::list_files(&root, &opts, 400_000).map_err(|x| x.to_string())?;
        out.extend(files.into_iter().map(|f| f.path));
    }
    Ok(out)
}

/// `{path, name, qualified, kind, start, end, def, depth, signature}` for
/// every symbol in the targets. Lines are 1-based and inclusive; `start`
/// includes leading docs/attributes, `def` is the definition line.
pub fn symbols(e: &Engine, targets: &[String], w: &mut impl Write) -> Result<usize, String> {
    let records = symbol_records(e, targets)?;
    for v in &records {
        write_line(w, v).map_err(|x| x.to_string())?;
    }
    Ok(records.len())
}

/// The records `symbols` prints, in path then line order.
pub fn symbol_records(e: &Engine, targets: &[String]) -> Result<Vec<Value>, String> {
    let files = expand(e, targets)?;
    let per_file: Vec<Vec<Value>> = files
        .par_iter()
        .map(|path| {
            let Ok(src) = e.load_text(path) else {
                return Vec::new();
            };
            let Some(o) = e.outline(&src) else {
                return Vec::new();
            };
            let shown = e.ws.display(path);
            (0..o.symbols.len())
                .map(|i| {
                    let mut m = serde_json::Map::new();
                    m.insert("path".into(), json!(shown));
                    m.extend(symbol_fields(&o, i));
                    Value::Object(m)
                })
                .collect()
        })
        .collect();
    Ok(per_file.into_iter().flatten().collect())
}

fn clip(text: &[u8]) -> (String, bool) {
    if text.len() <= MAX_TEXT {
        return (String::from_utf8_lossy(text).into_owned(), false);
    }
    let mut end = MAX_TEXT;
    while end > 0 && (text[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    (String::from_utf8_lossy(&text[..end]).into_owned(), true)
}

/// Search results as JSON Lines.
/// - `matches`: `{path, line, text, context?, symbol?}` per matching line
///   (context lines carry `"context": true`).
/// - `symbols`: `{path, symbol, matches, lines}` per enclosing symbol.
/// - `files`: `{path, matches}` per file.
pub fn search(e: &Engine, req: &SearchRequest, w: &mut impl Write) -> Result<usize, String> {
    let matcher = search::build_matcher(req)?;
    let roots = search::resolve_roots(e, &req.paths).map_err(|m| m.trim_end().to_string())?;
    let context = if req.output == Output::Files {
        0
    } else {
        req.context.min(10)
    };
    let sc = search::scan(
        e,
        &roots,
        &req.globs,
        &matcher,
        context,
        req.max_matches.max(1),
    );
    let need_outline = req.output != Output::Files;
    let outlines: Vec<Option<std::sync::Arc<Outline>>> = sc
        .files
        .par_iter()
        .map(|f| {
            if !need_outline {
                return None;
            }
            let src = e.load_text(&f.path).ok()?;
            if src.data.len() > search::MAX_GROUP_BYTES {
                return None;
            }
            e.outline(&src)
        })
        .collect();
    let mut n = 0;
    let err = |x: io::Error| x.to_string();
    for (f, o) in sc.files.iter().zip(&outlines) {
        let shown = e.ws.display(&f.path);
        let owner = |line: u32| -> Option<usize> {
            o.as_ref()
                .and_then(|o| o.innermost_at(line.saturating_sub(1)))
        };
        match req.output {
            Output::Files => {
                write_line(w, &json!({"path": shown, "matches": f.matches})).map_err(err)?;
                n += 1;
            }
            Output::Symbols => {
                let mut groups: Vec<(Option<usize>, Vec<u32>)> = Vec::new();
                for h in f.hits.iter().filter(|h| h.is_match) {
                    let s = owner(h.line);
                    match groups.iter_mut().find(|g| g.0 == s) {
                        Some(g) => g.1.push(h.line),
                        None => groups.push((s, vec![h.line])),
                    }
                }
                for (s, lines) in groups {
                    let mut m = serde_json::Map::new();
                    m.insert("path".into(), json!(shown));
                    m.insert(
                        "symbol".into(),
                        match (s, o) {
                            (Some(i), Some(o)) => Value::Object(symbol_fields(o, i)),
                            _ => Value::Null,
                        },
                    );
                    m.insert("matches".into(), json!(lines.len()));
                    m.insert("lines".into(), json!(lines));
                    write_line(w, &Value::Object(m)).map_err(err)?;
                    n += 1;
                }
            }
            Output::Matches => {
                for h in &f.hits {
                    let (text, clipped) = clip(&h.text);
                    let mut m = serde_json::Map::new();
                    m.insert("path".into(), json!(shown));
                    m.insert("line".into(), json!(h.line));
                    m.insert("text".into(), json!(text));
                    if clipped {
                        m.insert("clipped".into(), json!(true));
                    }
                    if !h.is_match {
                        m.insert("context".into(), json!(true));
                    }
                    if let (Some(i), Some(o)) = (owner(h.line), o) {
                        m.insert("symbol".into(), json!(o.qualified_name(i)));
                        let s = &o.symbols[i];
                        m.insert("symbol_lines".into(), json!([s.start + 1, s.end + 1]));
                    }
                    write_line(w, &Value::Object(m)).map_err(err)?;
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}

/// Every file under `path`: `{path, bytes, lines}` (`lines` is null for
/// binary or very large files) and `{path, symlink}` for links.
pub fn files(
    e: &Engine,
    path: Option<&str>,
    globs: &[String],
    max_depth: Option<usize>,
    w: &mut impl Write,
) -> Result<usize, String> {
    let root = match path {
        None => e.ws.primary().to_path_buf(),
        Some(p) => resolve(e, p)?,
    };
    let opts = WalkOpts {
        globs: globs.to_vec(),
        max_depth,
        include_symlinks: true,
        ..Default::default()
    };
    let (list, _) = if root.is_file() {
        let md = std::fs::metadata(&root).map_err(|x| x.to_string())?;
        (
            vec![walk::Entry {
                path: root.clone(),
                size: md.len(),
                ..Default::default()
            }],
            false,
        )
    } else {
        walk::list_files(&root, &opts, 400_000).map_err(|x| x.to_string())?
    };
    let records: Vec<Value> = list
        .par_iter()
        .map(|f| {
            let shown = e.ws.display(&f.path);
            if f.symlink {
                let target = std::fs::read_link(&f.path)
                    .map(|t| t.to_string_lossy().into_owned())
                    .unwrap_or_default();
                return json!({"path": shown, "symlink": target});
            }
            json!({"path": shown, "bytes": f.size, "lines": line_count(&f.path, f.size, f.dataless)})
        })
        .collect();
    for v in &records {
        write_line(w, v).map_err(|x| x.to_string())?;
    }
    Ok(records.len())
}

fn line_count(path: &Path, size: u64, dataless: bool) -> Option<usize> {
    if dataless || size > crate::source::MAX_PARSE as u64 {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    if memchr::memchr(0, &data[..data.len().min(8192)]).is_some() {
        return None;
    }
    let nl = memchr::memchr_iter(b'\n', &data).count();
    Some(nl + usize::from(!data.is_empty() && !data.ends_with(b"\n")))
}
