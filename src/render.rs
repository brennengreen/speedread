//! Text rendering primitives.
//!
//! Line format is `N<TAB>text`: unpadded numbers plus a tab measured cheapest
//! among unambiguous formats (1.8 tokens/line on o200k vs 4.6 for the
//! `%6d→` format common in agent harnesses), and the tab keeps content
//! whitespace unambiguous for exact-match edits.

use crate::lang::LangId;
use crate::outline::{Outline, Symbol};
use crate::source::Source;

/// Lines longer than this are cut (minified/generated content).
pub const MAX_LINE: usize = 2000;
pub const MAX_LINE_FULL: usize = 8000;

pub fn push_num(out: &mut String, n: usize) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    let mut v = n;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    // SAFETY: ASCII digits.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&buf[i..]) });
}

pub fn digits(n: usize) -> usize {
    let mut d = 1;
    let mut v = n;
    while v >= 10 {
        v /= 10;
        d += 1;
    }
    d
}

/// Push `text` (lossy UTF-8), cutting at `max` bytes on a char boundary.
pub fn push_text(out: &mut String, text: &[u8], max: usize) {
    if text.len() <= max {
        out.push_str(&String::from_utf8_lossy(text));
        return;
    }
    let s = String::from_utf8_lossy(&text[..max]);
    let mut cut = s.len();
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    out.push_str(&s[..cut]);
    out.push_str(" … [+");
    push_num(out, text.len() - max);
    out.push_str(" chars]");
}

/// One numbered line (`lineno` is 1-based).
pub fn push_line(out: &mut String, lineno: usize, text: &[u8], numbers: bool, max: usize) {
    if numbers {
        push_num(out, lineno);
        out.push('\t');
    }
    push_text(out, text, max);
    out.push('\n');
}

/// Estimated bytes of `a..=b` rendered with numbers.
pub fn numbered_bytes(src: &Source, a: usize, b: usize, numbers: bool) -> usize {
    if src.lines.is_empty() || a > b {
        return 0;
    }
    let n = b - a + 1;
    let mut bytes = src.span_bytes(a, b);
    if numbers {
        bytes += n * (digits(b + 1) + 1);
    }
    bytes
}

/// Push lines `a..=b` (0-based), stopping before `max_bytes`. Returns the
/// last line rendered (or `None` if none fit).
pub fn push_lines(
    out: &mut String,
    src: &Source,
    a: usize,
    b: usize,
    numbers: bool,
    max_line: usize,
    max_bytes: usize,
) -> Option<usize> {
    let start_len = out.len();
    let mut last = None;
    for i in a..=b.min(src.line_count().saturating_sub(1)) {
        let before = out.len();
        push_line(out, i + 1, src.line(i), numbers, max_line);
        if out.len() - start_len > max_bytes && last.is_some() {
            out.truncate(before);
            break;
        }
        last = Some(i);
    }
    last
}

fn leading_ws(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|&&c| c == b' ' || c == b'\t').count();
    &b[..n]
}

/// Collapse ranges that fall inside `a..=b`, outermost only.
pub fn collapse_ranges(outline: &Outline, a: usize, b: usize) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = outline
        .symbols
        .iter()
        .filter_map(|s| s.collapse)
        .chain(outline.elide.iter().copied())
        .map(|(x, y)| (x as usize, y as usize))
        .filter(|&(x, y)| x >= a && y <= b && y >= x)
        .collect();
    v.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(v.len());
    for r in v {
        if let Some(last) = out.last()
            && r.0 <= last.1
        {
            continue;
        }
        out.push(r);
    }
    out
}

/// Skeleton: every line of `a..=b` except collapsed bodies, which become a
/// single `A-B<TAB><indent>⋯` marker naming the elided range.
pub fn skeleton(src: &Source, outline: &Outline, a: usize, b: usize, numbers: bool) -> String {
    let ranges = collapse_ranges(outline, a, b);
    render_elided(src, a, b, numbers, &ranges)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Blank,
    Comment,
    Import,
    Other,
}

fn line_kind(lang: Option<LangId>, line: &[u8]) -> LineKind {
    let t = trim_start(line);
    if t.is_empty() {
        return LineKind::Blank;
    }
    let hash_comments = matches!(
        lang,
        Some(LangId::Python | LangId::Ruby | LangId::Bash | LangId::Yaml | LangId::Toml)
    );
    let starts = |p: &[u8]| t.starts_with(p);
    if starts(b"//") || starts(b"/*") || starts(b"*") || starts(b"--") && lang == Some(LangId::Lua)
    {
        return LineKind::Comment;
    }
    if hash_comments && starts(b"#") && !starts(b"#!") {
        return LineKind::Comment;
    }
    if starts(b"use ")
        || starts(b"pub use ")
        || starts(b"import ")
        || starts(b"from ")
        || starts(b"#include")
        || starts(b"#import")
        || starts(b"@import")
        || starts(b"require ")
        || starts(b"require_relative ")
        || starts(b"using ")
        || (starts(b"const ") && memchr::memmem::find(t, b"require(").is_some())
    {
        return LineKind::Import;
    }
    LineKind::Other
}

fn trim_start(b: &[u8]) -> &[u8] {
    let n = b.iter().take_while(|c| c.is_ascii_whitespace()).count();
    &b[n..]
}

/// Compact skeleton: a skeleton that additionally collapses comment blocks of
/// 3+ lines and import runs of 4+ lines to their first line — license
/// headers and long doc comments are the bulk of many "skeletons".
pub fn compact_skeleton(
    src: &Source,
    outline: &Outline,
    a: usize,
    b: usize,
    numbers: bool,
) -> String {
    let bodies = collapse_ranges(outline, a, b);
    let last = b.min(src.line_count().saturating_sub(1));
    let mut extra: Vec<(usize, usize)> = Vec::new();
    let mut i = a;
    let mut bi = 0;
    while i <= last {
        while bi < bodies.len() && bodies[bi].1 < i {
            bi += 1;
        }
        if bi < bodies.len() && bodies[bi].0 <= i {
            i = bodies[bi].1 + 1;
            continue;
        }
        let kind = line_kind(src.lang, src.line(i));
        if matches!(kind, LineKind::Comment | LineKind::Import) {
            let mut j = i;
            while j < last {
                let next = j + 1;
                if bi < bodies.len() && bodies[bi].0 <= next {
                    break;
                }
                let k = line_kind(src.lang, src.line(next));
                if k == kind || (kind == LineKind::Import && k == LineKind::Blank) {
                    j = next;
                } else {
                    break;
                }
            }
            while j > i && line_kind(src.lang, src.line(j)) == LineKind::Blank {
                j -= 1;
            }
            let min = if kind == LineKind::Comment { 3 } else { 4 };
            if j + 1 - i >= min {
                extra.push((i + 1, j));
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    let mut ranges = bodies;
    ranges.extend(extra);
    ranges.sort_unstable();
    render_elided(src, a, b, numbers, &ranges)
}

fn render_elided(
    src: &Source,
    a: usize,
    b: usize,
    numbers: bool,
    ranges: &[(usize, usize)],
) -> String {
    let mut out = String::with_capacity(src.span_bytes(a, b) / 3 + 64);
    let mut i = a;
    let mut ri = 0;
    let last = b.min(src.line_count().saturating_sub(1));
    while i <= last {
        if ri < ranges.len() && ranges[ri].0 == i {
            let (x, y) = ranges[ri];
            if numbers {
                push_num(&mut out, x + 1);
                out.push('-');
                push_num(&mut out, y + 1);
                out.push('\t');
            }
            out.push_str(&String::from_utf8_lossy(leading_ws(src.line(x))));
            out.push_str("⋯\n");
            i = y + 1;
            ri += 1;
            continue;
        }
        push_line(&mut out, i + 1, src.line(i), numbers, MAX_LINE);
        i += 1;
    }
    out
}

/// Outline listing: `A-B<TAB><indent>signature` for symbols inside `a..=b`.
/// With `max_depth`, deeper symbols are summarized as `(+N members)` on
/// their shown ancestor, so the listing still covers the whole range.
pub fn outline_text(outline: &Outline, a: usize, b: usize, max_depth: Option<u16>) -> String {
    let idx: Vec<usize> = (0..outline.symbols.len())
        .filter(|&i| {
            let s = &outline.symbols[i];
            s.start as usize >= a && s.end as usize <= b
        })
        .collect();
    let base = idx
        .iter()
        .map(|&i| outline.symbols[i].depth)
        .min()
        .unwrap_or(0);
    let limit = max_depth.map(|d| base + d);
    let mut hidden = vec![0usize; outline.symbols.len()];
    if let Some(l) = limit {
        for &i in &idx {
            if outline.symbols[i].depth > l {
                // Attribute to the nearest shown ancestor.
                let mut p = outline.symbols[i].parent;
                while let Some(pi) = p {
                    if outline.symbols[pi as usize].depth <= l {
                        hidden[pi as usize] += 1;
                        break;
                    }
                    p = outline.symbols[pi as usize].parent;
                }
            }
        }
    }
    let mut out = String::with_capacity(idx.len() * 48);
    for &i in &idx {
        let s: &Symbol = &outline.symbols[i];
        if limit.is_some_and(|l| s.depth > l) {
            continue;
        }
        push_num(&mut out, s.start as usize + 1);
        out.push('-');
        push_num(&mut out, s.end as usize + 1);
        out.push('\t');
        for _ in base..s.depth {
            out.push_str("  ");
        }
        out.push_str(&s.label);
        if hidden[i] > 0 {
            out.push_str(" (+");
            push_num(&mut out, hidden[i]);
            out.push_str(if hidden[i] == 1 {
                " member)"
            } else {
                " members)"
            });
        }
        out.push('\n');
    }
    out
}

/// Outline fitted to `max_bytes`: every top-level symbol, then nested
/// members breadth-first while they fit; hidden members are counted on their
/// nearest shown ancestor as `(+N members)`. Always covers the whole range.
pub fn outline_fit(outline: &Outline, a: usize, b: usize, max_bytes: usize) -> String {
    let idx: Vec<usize> = (0..outline.symbols.len())
        .filter(|&i| {
            let s = &outline.symbols[i];
            s.start as usize >= a && s.end as usize <= b
        })
        .collect();
    let base = idx
        .iter()
        .map(|&i| outline.symbols[i].depth)
        .min()
        .unwrap_or(0);
    let max_depth = idx
        .iter()
        .map(|&i| outline.symbols[i].depth)
        .max()
        .unwrap_or(0);
    let line_cost = |s: &Symbol| s.label.len() + 16 + 2 * (s.depth - base) as usize;
    let mut shown = vec![false; outline.symbols.len()];
    let mut used = 0usize;
    'levels: for d in base..=max_depth {
        for &i in &idx {
            let s = &outline.symbols[i];
            if s.depth != d {
                continue;
            }
            let c = line_cost(s);
            if d > base && used + c > max_bytes {
                break 'levels;
            }
            shown[i] = true;
            used += c;
        }
    }
    let mut hidden = vec![0usize; outline.symbols.len()];
    for &i in &idx {
        if shown[i] {
            continue;
        }
        let mut p = outline.symbols[i].parent;
        while let Some(pi) = p {
            if shown[pi as usize] {
                hidden[pi as usize] += 1;
                break;
            }
            p = outline.symbols[pi as usize].parent;
        }
    }
    let mut out = String::with_capacity(used + 64);
    for &i in &idx {
        if !shown[i] {
            continue;
        }
        let s = &outline.symbols[i];
        push_num(&mut out, s.start as usize + 1);
        out.push('-');
        push_num(&mut out, s.end as usize + 1);
        out.push('\t');
        for _ in base..s.depth {
            out.push_str("  ");
        }
        out.push_str(&s.label);
        if hidden[i] > 0 {
            out.push_str(" (+");
            push_num(&mut out, hidden[i]);
            out.push_str(if hidden[i] == 1 {
                " member)"
            } else {
                " members)"
            });
        }
        out.push('\n');
    }
    out
}

/// Cut `s` to at most `max_bytes`, at a line boundary.
pub fn cut_lines(s: &str, max_bytes: usize) -> (&str, bool) {
    if s.len() <= max_bytes {
        return (s, false);
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    match s[..cut].rfind('\n') {
        Some(nl) => (&s[..nl + 1], true),
        // Never emit a partial line.
        None => ("", true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_cuts() {
        let mut s = String::new();
        push_num(&mut s, 0);
        s.push(' ');
        push_num(&mut s, 12345);
        assert_eq!(s, "0 12345");
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        let mut t = String::new();
        push_text(&mut t, "héllo world".as_bytes(), 2);
        assert!(t.starts_with('h') && t.contains("[+"));
        assert_eq!(cut_lines("a\nbb\nccc\n", 6), ("a\nbb\n", true));
    }
}
