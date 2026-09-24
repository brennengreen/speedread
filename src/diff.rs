//! Compact unified diffs (imara-diff histogram algorithm).

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};

use crate::outline::Outline;
use crate::render::push_text;

pub struct DiffText {
    pub groups: Vec<Group>,
    pub added: u32,
    pub removed: u32,
    pub hunks: u32,
}

/// One `@@` hunk (nearby changes merged, with context).
pub struct Group {
    /// `@@ -a,b +c,d @@` without a trailing newline.
    pub header: String,
    /// Context/removed/added lines, each ending in `\n`.
    pub body: String,
    /// Changed line ranges (0-based, half-open) in the old and new text.
    pub before: Vec<Range<u32>>,
    pub after: Vec<Range<u32>>,
}

impl DiffText {
    /// The diff with an optional label after each `@@ … @@` header (git's
    /// "function context").
    pub fn render(&self, label: impl Fn(&Group) -> Option<String>) -> String {
        let mut out = String::new();
        for g in &self.groups {
            out.push_str(&g.header);
            if let Some(l) = label(g) {
                out.push(' ');
                out.push_str(&l);
            }
            out.push('\n');
            out.push_str(&g.body);
        }
        out
    }

    pub fn text(&self) -> String {
        self.render(|_| None)
    }

    pub fn len_estimate(&self) -> usize {
        self.groups
            .iter()
            .map(|g| g.header.len() + g.body.len() + 40)
            .sum()
    }
}

fn strip_nl(b: &[u8]) -> &[u8] {
    let mut e = b.len();
    if e > 0 && b[e - 1] == b'\n' {
        e -= 1;
    }
    if e > 0 && b[e - 1] == b'\r' {
        e -= 1;
    }
    &b[..e]
}

/// Unified diff of `old` → `new` with `ctx` lines of context.
pub fn unified(old: &[u8], new: &[u8], ctx: u32) -> DiffText {
    let input = InternedInput::new(old, new);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let hunks: Vec<_> = diff.hunks().collect();
    let nb = input.before.len() as u32;
    let line = |t: imara_diff::Token| -> &[u8] { strip_nl(input.interner[t]) };
    let mut groups = Vec::new();
    let mut i = 0;
    while i < hunks.len() {
        let mut j = i;
        while j + 1 < hunks.len() && hunks[j + 1].before.start <= hunks[j].before.end + 2 * ctx {
            j += 1;
        }
        let first = &hunks[i];
        let last = &hunks[j];
        let bstart = first.before.start.saturating_sub(ctx);
        let astart = first.after.start - (first.before.start - bstart);
        let bend = (last.before.end + ctx).min(nb);
        let aend = last.after.end + (bend - last.before.end);
        let header = format!(
            "@@ -{},{} +{},{} @@",
            bstart + 1,
            bend - bstart,
            astart + 1,
            aend - astart
        );
        let mut body = String::new();
        let mut pos = bstart;
        let mut before = Vec::new();
        let mut after = Vec::new();
        for h in &hunks[i..=j] {
            for k in pos..h.before.start {
                body.push(' ');
                push_text(&mut body, line(input.before[k as usize]), 500);
                body.push('\n');
            }
            for k in h.before.clone() {
                body.push('-');
                push_text(&mut body, line(input.before[k as usize]), 500);
                body.push('\n');
            }
            for k in h.after.clone() {
                body.push('+');
                push_text(&mut body, line(input.after[k as usize]), 500);
                body.push('\n');
            }
            before.push(h.before.clone());
            after.push(h.after.clone());
            pos = h.before.end;
        }
        for k in pos..bend {
            body.push(' ');
            push_text(&mut body, line(input.before[k as usize]), 500);
            body.push('\n');
        }
        groups.push(Group {
            header,
            body,
            before,
            after,
        });
        i = j + 1;
    }
    DiffText {
        hunks: groups.len() as u32,
        groups,
        added: diff.count_additions(),
        removed: diff.count_removals(),
    }
}

/// How a symbol changed between two versions of a file.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    /// Lines inside the symbol changed; its signature line did not.
    Body,
    /// The one-line signature changed (holds the old signature).
    Signature(String),
    Added,
    Removed,
}

#[derive(Debug)]
pub struct SymbolChange {
    /// Qualified name (`Parser.parse`).
    pub name: String,
    pub change: Change,
    /// 1-based inclusive lines in the new version (`None` when removed).
    pub lines: Option<(u32, u32)>,
    /// Signature in the new version (old version when removed).
    pub label: String,
}

/// `(qualified name, occurrence)` keys so overloads pair up in order.
fn keys(o: &Outline) -> Vec<(String, u32)> {
    let mut seen: HashMap<String, u32> = HashMap::new();
    (0..o.symbols.len())
        .map(|i| {
            let q = o.qualified_name(i);
            let n = seen.entry(q.clone()).or_default();
            *n += 1;
            (q, *n)
        })
        .collect()
}

/// Innermost symbols touched by the diff, classified by matching qualified
/// names between the old and new outlines. Ordered by position in the new
/// file, removed symbols last.
pub fn symbol_changes(old: &Outline, new: &Outline, d: &DiffText) -> Vec<SymbolChange> {
    let (ok, nk) = (keys(old), keys(new));
    let old_idx: HashMap<&(String, u32), usize> =
        ok.iter().enumerate().map(|(i, k)| (k, i)).collect();
    let new_idx: HashMap<&(String, u32), usize> =
        nk.iter().enumerate().map(|(i, k)| (k, i)).collect();
    let mut touched_new = BTreeSet::new();
    let mut touched_old = BTreeSet::new();
    for g in &d.groups {
        for r in &g.after {
            touched_new.extend(r.clone().filter_map(|l| new.innermost_at(l)));
        }
        for r in &g.before {
            touched_old.extend(r.clone().filter_map(|l| old.innermost_at(l)));
        }
    }
    for &i in &touched_old {
        if let Some(&j) = new_idx.get(&ok[i]) {
            touched_new.insert(j);
        }
    }
    let mut out = Vec::new();
    for &j in &touched_new {
        let s = &new.symbols[j];
        let change = match old_idx.get(&nk[j]) {
            None => Change::Added,
            Some(&i) if old.symbols[i].label != s.label => {
                Change::Signature(old.symbols[i].label.clone())
            }
            Some(_) => Change::Body,
        };
        out.push(SymbolChange {
            name: nk[j].0.clone(),
            change,
            lines: Some((s.start + 1, s.end + 1)),
            label: s.label.clone(),
        });
    }
    for &i in &touched_old {
        if !new_idx.contains_key(&ok[i]) {
            out.push(SymbolChange {
                name: ok[i].0.clone(),
                change: Change::Removed,
                lines: None,
                label: old.symbols[i].label.clone(),
            });
        }
    }
    out
}

/// One line per changed symbol (at most `max`), e.g.
/// `  Parser.parse [40-72]: body changed, signature unchanged`.
pub fn render_changes(changes: &[SymbolChange], max: usize) -> String {
    if changes.is_empty() {
        return String::new();
    }
    let mut t = String::from("symbols:\n");
    for c in changes.iter().take(max) {
        t.push_str("  ");
        t.push_str(&c.name);
        if let Some((a, b)) = c.lines {
            t.push_str(&format!(" [{a}-{b}]"));
        }
        match &c.change {
            Change::Body => t.push_str(": body changed, signature unchanged"),
            Change::Signature(old) => {
                t.push_str(&format!(": signature changed: `{old}` → `{}`", c.label))
            }
            Change::Added => t.push_str(&format!(": added `{}`", c.label)),
            Change::Removed => t.push_str(": removed"),
        }
        t.push('\n');
    }
    if changes.len() > max {
        t.push_str(&format!("  … {} more\n", changes.len() - max));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_diff() {
        let old = b"a\nb\nc\nd\ne\nf\ng\n";
        let new = b"a\nb\nC\nd\ne\nf\ng\nh\n";
        let d = unified(old, new, 1);
        assert_eq!(d.added, 2);
        assert_eq!(d.removed, 1);
        assert_eq!(d.hunks, 2);
        let t = d.text();
        assert!(t.starts_with("@@ -2,3 +2,3 @@\n b\n-c\n+C\n d\n"));
        assert!(t.contains("+h\n"));
        assert_eq!(d.groups[0].after, vec![2..3]);
        let labeled = d.render(|_| Some("f".into()));
        assert!(labeled.starts_with("@@ -2,3 +2,3 @@ f\n"));
    }
}
