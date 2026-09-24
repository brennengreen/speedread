//! Compact unified diffs (imara-diff histogram algorithm).

use imara_diff::{Algorithm, Diff, InternedInput};

use crate::render::push_text;

pub struct DiffText {
    pub text: String,
    pub added: u32,
    pub removed: u32,
    pub hunks: u32,
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
    let mut out = String::new();
    let mut groups = 0;
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
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            bstart + 1,
            bend - bstart,
            astart + 1,
            aend - astart
        ));
        let mut pos = bstart;
        for h in &hunks[i..=j] {
            for k in pos..h.before.start {
                out.push(' ');
                push_text(&mut out, line(input.before[k as usize]), 500);
                out.push('\n');
            }
            for k in h.before.clone() {
                out.push('-');
                push_text(&mut out, line(input.before[k as usize]), 500);
                out.push('\n');
            }
            for k in h.after.clone() {
                out.push('+');
                push_text(&mut out, line(input.after[k as usize]), 500);
                out.push('\n');
            }
            pos = h.before.end;
        }
        for k in pos..bend {
            out.push(' ');
            push_text(&mut out, line(input.before[k as usize]), 500);
            out.push('\n');
        }
        groups += 1;
        i = j + 1;
    }
    DiffText {
        text: out,
        added: diff.count_additions(),
        removed: diff.count_removals(),
        hunks: groups,
    }
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
        assert!(d.text.starts_with("@@ -2,3 +2,3 @@\n b\n-c\n+C\n d\n"));
        assert!(d.text.contains("+h\n"));
    }
}
