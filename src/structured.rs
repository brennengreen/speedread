//! Hand-written structural scanners for documents and data files.
//!
//! These are single-pass, allocation-light, and produce the same [`Outline`]
//! shape as tree-sitter code outlines, so `README.md#Install`,
//! `package.json#scripts` or `Cargo.toml#dependencies` work like code symbols.

use crate::lang::LangId;
use crate::outline::{Kind, Outline, Symbol, clip, squash};

pub fn outline(lang: LangId, src: &[u8], lines: &[u32]) -> Outline {
    let symbols = match lang {
        LangId::Markdown => markdown(src, lines),
        LangId::Json => json(src),
        LangId::Yaml => yaml(src, lines),
        LangId::Toml => toml(src, lines),
        _ => Vec::new(),
    };
    Outline {
        symbols,
        elide: Vec::new(),
    }
}

fn line<'a>(src: &'a [u8], lines: &[u32], i: usize) -> &'a [u8] {
    let s = lines[i] as usize;
    let mut e = lines.get(i + 1).map(|&e| e as usize).unwrap_or(src.len());
    while e > s && matches!(src[e - 1], b'\n' | b'\r') {
        e -= 1;
    }
    &src[s..e]
}

fn is_blank(l: &[u8]) -> bool {
    l.iter().all(|b| b.is_ascii_whitespace())
}

fn indent_of(l: &[u8]) -> usize {
    l.iter().take_while(|&&b| b == b' ' || b == b'\t').count()
}

/// Set each symbol's end to the line before the next symbol at the same or a
/// shallower level (ignoring trailing blank lines), and link parents.
fn close_by_level(syms: &mut [Symbol], levels: &[usize], src: &[u8], lines: &[u32], last: u32) {
    let n = syms.len();
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..n {
        while let Some(&top) = stack.last() {
            if levels[top] >= levels[i] {
                stack.pop();
            } else {
                break;
            }
        }
        syms[i].parent = stack.last().map(|&p| p as u32);
        syms[i].depth = stack.len() as u16;
        stack.push(i);
    }
    for i in 0..n {
        let mut end = last;
        for j in i + 1..n {
            if levels[j] <= levels[i] {
                end = syms[j].start.saturating_sub(1);
                break;
            }
        }
        while end > syms[i].start && is_blank(line(src, lines, end as usize)) {
            end -= 1;
        }
        syms[i].end = end.max(syms[i].start);
    }
}

fn markdown(src: &[u8], lines: &[u32]) -> Vec<Symbol> {
    let mut syms = Vec::new();
    let mut levels = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut i = 0;
    let n = lines.len();
    // YAML front matter.
    if n > 0 && line(src, lines, 0) == b"---" {
        i = 1;
        while i < n && line(src, lines, i) != b"---" {
            i += 1;
        }
        i += 1;
    }
    while i < n {
        let l = line(src, lines, i);
        let ind = indent_of(l);
        let t = &l[ind.min(l.len())..];
        if ind <= 3 && (t.starts_with(b"```") || t.starts_with(b"~~~")) {
            let ch = t[0];
            let len = t.iter().take_while(|&&b| b == ch).count();
            match fence {
                None => fence = Some((ch, len)),
                Some((c, fl)) if c == ch && len >= fl && is_blank(&t[len..]) => fence = None,
                _ => {}
            }
            i += 1;
            continue;
        }
        if fence.is_some() {
            i += 1;
            continue;
        }
        if ind <= 3 && t.first() == Some(&b'#') {
            let level = t.iter().take_while(|&&b| b == b'#').count();
            if level <= 6 && (t.len() == level || t[level] == b' ' || t[level] == b'\t') {
                let text = String::from_utf8_lossy(&t[level..]);
                let text = text.trim().trim_end_matches('#').trim();
                push_heading(&mut syms, &mut levels, i, level, text);
                i += 1;
                continue;
            }
        }
        // Setext headings.
        if i + 1 < n && !is_blank(l) && ind <= 3 {
            let next = line(src, lines, i + 1);
            let nt = String::from_utf8_lossy(next);
            let nt = nt.trim();
            if nt.len() >= 2
                && (nt.bytes().all(|b| b == b'=') || nt.bytes().all(|b| b == b'-'))
                && !t.starts_with(b"-")
                && !t.starts_with(b"*")
                && !t.starts_with(b"|")
            {
                let level = if nt.starts_with('=') { 1 } else { 2 };
                let text = String::from_utf8_lossy(t);
                push_heading(&mut syms, &mut levels, i, level, text.trim());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    let last = n.saturating_sub(1) as u32;
    close_by_level(&mut syms, &levels, src, lines, last);
    // Sections collapse to their heading line in skeleton views.
    for s in &mut syms {
        if s.end > s.start + 2 {
            s.collapse = Some((s.start + 1, s.end));
        }
    }
    // Only leaf-level sections collapse; parents keep their headings visible.
    let has_child: Vec<bool> = {
        let mut v = vec![false; syms.len()];
        for s in &syms {
            if let Some(p) = s.parent {
                v[p as usize] = true;
            }
        }
        v
    };
    for (i, s) in syms.iter_mut().enumerate() {
        if has_child[i] {
            s.collapse = None;
        }
    }
    syms
}

fn push_heading(
    syms: &mut Vec<Symbol>,
    levels: &mut Vec<usize>,
    row: usize,
    level: usize,
    text: &str,
) {
    let text = clip(&squash(text), 120);
    syms.push(Symbol {
        kind: Kind::Heading,
        name: text.clone(),
        scope: None,
        label: format!("{} {}", "#".repeat(level), text),
        start: row as u32,
        def: row as u32,
        end: row as u32,
        collapse: None,
        parent: None,
        depth: 0,
    });
    levels.push(level);
}

/// JSON / JSONC: object members up to depth 3 with exact line ranges.
fn json(src: &[u8]) -> Vec<Symbol> {
    const MAX_DEPTH: usize = 3;
    struct Frame {
        object: bool,
        expecting_key: bool,
        member: Option<usize>,
        member_value_line: u32,
    }
    let mut syms: Vec<Symbol> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut line: u32 = 0;
    let mut last_sig: u32 = 0;
    let mut pending_key: Option<(String, u32)> = None;
    let mut i = 0;
    let n = src.len();
    let close_member = |syms: &mut Vec<Symbol>, f: &mut Frame, end: u32| {
        if let Some(m) = f.member.take() {
            syms[m].end = end.max(syms[m].start);
            let s = &syms[m];
            let multi = s.end > f.member_value_line;
            if multi && s.end >= s.start + 3 {
                let cs = f.member_value_line + 1;
                let ce = s.end - 1;
                if ce > cs {
                    syms[m].collapse = Some((cs, ce));
                }
            }
        }
    };
    while i < n {
        let c = src[i];
        match c {
            b'\n' => line += 1,
            b' ' | b'\t' | b'\r' => {}
            b'/' if i + 1 < n && src[i + 1] == b'/' => {
                while i < n && src[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if i + 1 < n && src[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(src[i] == b'*' && src[i + 1] == b'/') {
                    if src[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'"' | b'\'' => {
                let q = c;
                let start_line = line;
                let s = i + 1;
                i += 1;
                while i < n && src[i] != q {
                    if src[i] == b'\\' {
                        i += 1;
                    } else if src[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
                let key_text = String::from_utf8_lossy(&src[s..i.min(n)]).into_owned();
                if let Some(f) = stack.last_mut()
                    && f.object
                    && f.expecting_key
                {
                    pending_key = Some((key_text, start_line));
                    f.expecting_key = false;
                }
                last_sig = line;
            }
            b':' => {
                let in_object = stack.last().is_some_and(|f| f.object);
                if let Some((key, kline)) = pending_key.take()
                    && in_object
                {
                    let depth = stack.len();
                    if depth <= MAX_DEPTH {
                        let parent = if depth >= 2 {
                            stack[depth - 2].member
                        } else {
                            None
                        };
                        syms.push(Symbol {
                            kind: Kind::Key,
                            label: format!("\"{}\"", clip(&key, 100)),
                            name: key,
                            scope: None,
                            start: kline,
                            def: kline,
                            end: kline,
                            collapse: None,
                            parent: parent.map(|p| p as u32),
                            depth: (depth - 1) as u16,
                        });
                        if let Some(f) = stack.last_mut() {
                            f.member = Some(syms.len() - 1);
                            f.member_value_line = line;
                        }
                    }
                }
                last_sig = line;
            }
            b'{' | b'[' => {
                stack.push(Frame {
                    object: c == b'{',
                    expecting_key: c == b'{',
                    member: None,
                    member_value_line: line,
                });
                last_sig = line;
            }
            b'}' | b']' => {
                pending_key = None;
                if let Some(mut f) = stack.pop() {
                    close_member(&mut syms, &mut f, last_sig);
                }
                last_sig = line;
            }
            b',' => {
                if let Some(f) = stack.last_mut()
                    && f.object
                {
                    close_member(&mut syms, f, last_sig);
                    f.expecting_key = true;
                }
                last_sig = line;
            }
            _ => last_sig = line,
        }
        i += 1;
    }
    // Keep top-level members with children visible in skeletons.
    let mut has_child = vec![false; syms.len()];
    for s in &syms {
        if let Some(p) = s.parent {
            has_child[p as usize] = true;
        }
    }
    for (i, s) in syms.iter_mut().enumerate() {
        if has_child[i] && s.depth == 0 {
            s.collapse = None;
        }
        if s.depth >= 2 {
            s.collapse = None;
        }
    }
    // Depth-2 members only matter when their parent is expanded; drop them from
    // collapsible consideration but keep them for lookups.
    syms
}

/// YAML: mapping keys at the first two nesting levels.
fn yaml(src: &[u8], lines: &[u32]) -> Vec<Symbol> {
    let mut syms: Vec<Symbol> = Vec::new();
    let mut levels: Vec<usize> = Vec::new();
    let mut indents: Vec<usize> = Vec::new();
    let mut block_scalar_indent: Option<usize> = None;
    let n = lines.len();
    for i in 0..n {
        let l = line(src, lines, i);
        if is_blank(l) {
            continue;
        }
        let ind = indent_of(l);
        if let Some(bi) = block_scalar_indent {
            if ind > bi {
                continue;
            }
            block_scalar_indent = None;
        }
        let t = &l[ind..];
        if t.starts_with(b"#") || t.starts_with(b"---") || t.starts_with(b"...") {
            continue;
        }
        let (t, ind) = if let Some(rest) = t.strip_prefix(b"- ") {
            (rest, ind + 2)
        } else {
            (t, ind)
        };
        let Some(colon) = find_yaml_colon(t) else {
            continue;
        };
        let key = String::from_utf8_lossy(&t[..colon]);
        let key = key.trim().trim_matches(|c| c == '"' || c == '\'');
        if key.is_empty() || key.contains(' ') && key.len() > 60 {
            continue;
        }
        let rest = String::from_utf8_lossy(&t[colon + 1..]);
        let rest = rest.trim();
        if rest.starts_with('|') || rest.starts_with('>') {
            block_scalar_indent = Some(ind);
        }
        // Nesting level by indentation.
        while let Some(&top) = indents.last() {
            if top >= ind {
                indents.pop();
            } else {
                break;
            }
        }
        let level = indents.len();
        indents.push(ind);
        if level > 1 {
            continue;
        }
        syms.push(Symbol {
            kind: Kind::Key,
            name: key.to_string(),
            scope: None,
            label: clip(
                &format!("{key}:{}{}", if rest.is_empty() { "" } else { " " }, rest),
                120,
            ),
            start: i as u32,
            def: i as u32,
            end: i as u32,
            collapse: None,
            parent: None,
            depth: 0,
        });
        levels.push(level);
    }
    let last = n.saturating_sub(1) as u32;
    close_by_level(&mut syms, &levels, src, lines, last);
    let mut has_child = vec![false; syms.len()];
    for s in &syms {
        if let Some(p) = s.parent {
            has_child[p as usize] = true;
        }
    }
    for (i, s) in syms.iter_mut().enumerate() {
        if !has_child[i] && s.end >= s.start + 3 {
            s.collapse = Some((s.start + 1, s.end));
        }
    }
    syms
}

fn find_yaml_colon(t: &[u8]) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, &b) in t.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None => match b {
                b'"' | b'\'' if i == 0 => quote = Some(b),
                b'#' if i > 0 && t[i - 1] == b' ' => return None,
                b':' if i + 1 == t.len() || t[i + 1] == b' ' || t[i + 1] == b'\t' => {
                    return if i > 0 { Some(i) } else { None };
                }
                b'{' | b'[' if i == 0 => return None,
                _ => {}
            },
        }
    }
    None
}

/// TOML: `[table]` and `[[array]]` headers.
fn toml(src: &[u8], lines: &[u32]) -> Vec<Symbol> {
    let mut syms: Vec<Symbol> = Vec::new();
    let mut levels = Vec::new();
    let n = lines.len();
    let mut in_multiline: Option<&[u8]> = None;
    for i in 0..n {
        let l = line(src, lines, i);
        let t = &l[indent_of(l)..];
        if let Some(delim) = in_multiline {
            if memchr::memmem::find(t, delim).is_some() {
                in_multiline = None;
            }
            continue;
        }
        for d in [&b"\"\"\""[..], &b"'''"[..]] {
            if let Some(p) = memchr::memmem::find(t, d)
                && memchr::memmem::find(&t[p + 3..], d).is_none()
            {
                in_multiline = Some(d);
            }
        }
        if !t.starts_with(b"[") {
            continue;
        }
        let array = t.starts_with(b"[[");
        let inner_start = if array { 2 } else { 1 };
        let Some(close) = memchr::memchr(b']', &t[inner_start..]) else {
            continue;
        };
        let header = String::from_utf8_lossy(&t[inner_start..inner_start + close]);
        let header = header.trim();
        if header.is_empty() {
            continue;
        }
        let (scope, name) = match header.rsplit_once('.') {
            Some((s, n)) if !header.contains('"') => (Some(s.to_string()), n.to_string()),
            _ => (None, header.trim_matches('"').to_string()),
        };
        syms.push(Symbol {
            kind: Kind::Section,
            name,
            scope,
            label: if array {
                format!("[[{header}]]")
            } else {
                format!("[{header}]")
            },
            start: i as u32,
            def: i as u32,
            end: i as u32,
            collapse: None,
            parent: None,
            depth: 0,
        });
        levels.push(0);
    }
    let last = n.saturating_sub(1) as u32;
    close_by_level(&mut syms, &levels, src, lines, last);
    for s in &mut syms {
        s.parent = None;
        s.depth = 0;
        if s.end >= s.start + 3 {
            s.collapse = Some((s.start + 1, s.end));
        }
    }
    syms
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::line_starts;

    fn outline_of(lang: LangId, s: &str) -> Vec<(String, u32, u32, u16)> {
        let ls = line_starts(s.as_bytes());
        outline(lang, s.as_bytes(), &ls)
            .symbols
            .into_iter()
            .map(|s| (s.name, s.start + 1, s.end + 1, s.depth))
            .collect()
    }

    #[test]
    fn markdown_headings_and_fences() {
        let md = "# Title\nintro\n\n## Install\nrun it\n```\n# not a heading\n```\n\n## Usage\ntext\nSetext\n------\nmore\n";
        let o = outline_of(LangId::Markdown, md);
        assert_eq!(
            o,
            vec![
                ("Title".into(), 1, 14, 0),
                ("Install".into(), 4, 8, 1),
                ("Usage".into(), 10, 11, 1),
                ("Setext".into(), 12, 14, 1),
            ]
        );
    }

    #[test]
    fn json_members_with_ranges() {
        let j = "{\n  \"name\": \"x\",\n  \"scripts\": {\n    \"build\": \"tsc\",\n    \"test\": \"vitest\"\n  },\n  // comment\n  \"deps\": [1,\n    2]\n}\n";
        let o = outline_of(LangId::Json, j);
        assert_eq!(
            o,
            vec![
                ("name".into(), 2, 2, 0),
                ("scripts".into(), 3, 6, 0),
                ("build".into(), 4, 4, 1),
                ("test".into(), 5, 5, 1),
                ("deps".into(), 8, 9, 0),
            ]
        );
    }

    #[test]
    fn json_malformed_does_not_panic() {
        for j in [
            "{\"a\"}:1",
            "[{\"k\"}]: x",
            ":::",
            "{\"a\": {\"b\"}}: 2",
            "\"x\":",
        ] {
            let _ = outline_of(LangId::Json, j);
        }
    }

    #[test]
    fn yaml_two_levels() {
        let y = "name: CI\non:\n  push:\n    branches: [main]\njobs:\n  build:\n    runs-on: macos-latest\n    steps:\n      - run: make\n  test:\n    script: |\n      a: b\n";
        let o = outline_of(LangId::Yaml, y);
        assert_eq!(
            o,
            vec![
                ("name".into(), 1, 1, 0),
                ("on".into(), 2, 4, 0),
                ("push".into(), 3, 4, 1),
                ("jobs".into(), 5, 12, 0),
                ("build".into(), 6, 9, 1),
                ("test".into(), 10, 12, 1),
            ]
        );
    }

    #[test]
    fn toml_tables() {
        let t = "[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\n\n[profile.release]\nlto = true\n";
        let o = outline_of(LangId::Toml, t);
        assert_eq!(
            o,
            vec![
                ("package".into(), 1, 2, 0),
                ("dependencies".into(), 4, 5, 0),
                ("release".into(), 7, 8, 0),
            ]
        );
    }
}
