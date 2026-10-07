//! Enclosing blocks without a syntax tree.
//!
//! `path:LINE` reads the symbol around a line. When no symbol encloses it (a
//! language without a grammar, or top-level code such as a callback passed
//! to a call), the block is found from indentation and closing lines
//! (`end`, `}`, `fi`) instead: the innermost enclosing block that looks like
//! a definition, else the outermost statement around the line.

use crate::outline::{clip, squash};

/// Blocks longer than this are not returned (the caller shows a window).
pub const MAX_BLOCK: usize = 400;

/// Only the start of a line is examined (minified code has huge lines).
const SCAN: usize = 512;

/// A block's lines, 0-based and inclusive. `header` is the definition's first
/// line; `start` also covers the comments and decorators directly above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub start: usize,
    pub header: usize,
    pub end: usize,
    /// The header line, whitespace-squashed, for display.
    pub title: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Blank,
    Comment,
    /// `}`, `end`, `fi`, `});`, or the tail of a multi-line header (`) {`).
    Closer,
    /// `else`, `elif x:`, `} catch (e) {`: continues a compound statement.
    Clause,
    Code,
}

const CLOSE_WORDS: &[&str] = &[
    "end",
    "fi",
    "done",
    "esac",
    "until",
    "wend",
    "endif",
    "endfor",
    "endforeach",
    "endwhile",
    "endfunction",
    "endsub",
    "endmodule",
    "endclass",
    "endcase",
    "endswitch",
    "endtask",
    "endpackage",
    "endmacro",
    "endprogram",
    "endinterface",
    "endprocedure",
    "endselect",
    "enddo",
    "endblock",
    "endtype",
    "endstruct",
    "endenum",
    "endrecord",
    "endgenerate",
    "endwith",
    "endtry",
    "endregion",
    "endproperty",
    "enddeclare",
];

const CLAUSE_WORDS: &[&str] = &[
    "else", "elif", "elsif", "elseif", "except", "catch", "finally", "rescue", "ensure",
];

/// Statements that are never the "definition" a line belongs to.
const CONTROL_WORDS: &[&str] = &[
    "if", "else", "elif", "elsif", "elseif", "unless", "for", "foreach", "while", "until", "do",
    "loop", "switch", "case", "when", "match", "try", "catch", "except", "finally", "rescue",
    "ensure", "with", "select", "repeat", "guard", "defer", "return", "yield", "await", "begin",
    "then", "where", "let", "var", "val", "const", "local",
];

const DEF_WORDS: &[&str] = &[
    "fn",
    "func",
    "function",
    "def",
    "defp",
    "defmacro",
    "defmacrop",
    "defmodule",
    "defprotocol",
    "defimpl",
    "defn",
    "defn-",
    "defun",
    "defmethod",
    "defclass",
    "defrecord",
    "deftype",
    "defstruct",
    "define",
    "cdef",
    "cpdef",
    "class",
    "struct",
    "enum",
    "union",
    "trait",
    "impl",
    "interface",
    "module",
    "namespace",
    "object",
    "record",
    "type",
    "sub",
    "proc",
    "procedure",
    "method",
    "macro",
    "subroutine",
    "program",
    "contract",
    "library",
    "modifier",
    "message",
    "service",
    "actor",
    "extension",
    "protocol",
    "fun",
    "instance",
    "newtype",
    "constructor",
    "init",
    "deinit",
    "test",
    "describe",
    "it",
    "context",
    "task",
    "feature",
    "scenario",
];

/// Words that may precede a definition keyword.
const MODIFIERS: &[&str] = &[
    "pub",
    "export",
    "default",
    "async",
    "static",
    "public",
    "private",
    "protected",
    "internal",
    "override",
    "abstract",
    "final",
    "open",
    "sealed",
    "inline",
    "local",
    "extern",
    "unsafe",
    "virtual",
    "partial",
    "suspend",
    "operator",
    "infix",
    "tailrec",
    "external",
    "const",
    "constexpr",
    "global",
    "friend",
    "synchronized",
    "native",
    "pure",
    "elemental",
    "recursive",
    "mutating",
    "nonmutating",
    "fileprivate",
    "required",
    "convenience",
    "indirect",
    "lazy",
    "dynamic",
];

struct Win<'a> {
    lo: usize,
    lines: Vec<&'a [u8]>,
}

impl Win<'_> {
    fn len(&self) -> usize {
        self.lines.len()
    }

    fn indent(&self, i: usize) -> usize {
        let mut w = 0;
        for &b in self.lines[i].iter().take(SCAN) {
            match b {
                b' ' => w += 1,
                b'\t' => w = (w / 4 + 1) * 4,
                _ => break,
            }
        }
        w
    }

    fn text(&self, i: usize) -> &str {
        let l = self.lines[i];
        let l = &l[..l.len().min(SCAN)];
        let s = match std::str::from_utf8(l) {
            Ok(s) => s,
            Err(e) => std::str::from_utf8(&l[..e.valid_up_to()]).unwrap_or(""),
        };
        s.trim()
    }

    fn shape(&self, i: usize) -> Shape {
        shape_of(self.text(i))
    }

    fn is_code(&self, i: usize) -> bool {
        !matches!(self.shape(i), Shape::Blank | Shape::Comment)
    }

    fn next_code(&self, i: usize) -> Option<usize> {
        (i + 1..self.len()).find(|&j| self.is_code(j))
    }

    /// The first line of the statement `i` belongs to: a clause (`else`), a
    /// closer or header tail (`}`, `) {`) or a lone `{` resolves upward to
    /// the line at the same indentation that opened it.
    fn statement_start(&self, mut i: usize) -> usize {
        loop {
            let t = self.text(i);
            if !(matches!(self.shape(i), Shape::Clause | Shape::Closer) || t.starts_with('{')) {
                return i;
            }
            let ind = self.indent(i);
            match (0..i)
                .rev()
                .find(|&j| self.is_code(j) && self.indent(j) <= ind)
            {
                Some(p) if self.indent(p) == ind => i = p,
                _ => return i,
            }
        }
    }

    /// Last line of the block opened at `h`, including its closer.
    fn block_end(&self, h: usize) -> usize {
        let ind = self.indent(h);
        let mut last = h;
        let mut j = h + 1;
        while j < self.len() {
            if !self.is_code(j) {
                j += 1;
                continue;
            }
            let d = self.indent(j);
            if d > ind {
                last = j;
                j += 1;
                continue;
            }
            if d == ind
                && (matches!(self.shape(j), Shape::Clause | Shape::Closer)
                    || self.text(j).starts_with('{'))
            {
                last = j;
                // `} else {`, `) -> T {`, `else`, an Allman `{`: the statement goes on.
                if self.next_code(j).is_some_and(|k| self.indent(k) > ind) {
                    j += 1;
                    continue;
                }
            }
            break;
        }
        last
    }

    /// Extend a header upward over adjacent comments and decorators.
    fn doc_start(&self, h: usize) -> usize {
        let ind = self.indent(h);
        let mut s = h;
        while s > 0 {
            let p = s - 1;
            let t = self.text(p);
            let doc = match self.shape(p) {
                Shape::Comment => true,
                Shape::Code => t.starts_with('@'),
                _ => false,
            };
            if !doc || self.indent(p) != ind {
                break;
            }
            s = p;
        }
        s
    }

    fn is_definition(&self, h: usize) -> bool {
        // A signature split over lines is joined up to its closing paren.
        let mut text = self.text(h).to_string();
        let mut last = h;
        let mut depth = paren_balance(&text);
        while depth > 0 && last + 1 < self.len() && last - h < 12 {
            last += 1;
            let t = self.text(last);
            depth += paren_balance(t);
            text.push(' ');
            text.push_str(t);
        }
        let allman = self
            .next_code(last)
            .is_some_and(|k| self.text(k).starts_with('{'));
        is_definition(&text, allman)
    }
}

fn paren_balance(t: &str) -> i32 {
    t.bytes().fold(0, |d, b| match b {
        b'(' => d + 1,
        b')' => d - 1,
        _ => d,
    })
}

fn first_word(t: &str) -> &str {
    let end = t
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(t.len());
    &t[..end]
}

fn shape_of(t: &str) -> Shape {
    if t.is_empty() {
        return Shape::Blank;
    }
    const COMMENTS: &[&str] = &[
        "//", "#", "--", ";", "%", "/*", "*/", "<!--", "{-", "-}", "(*", "*)", "! ",
    ];
    if (COMMENTS.iter().any(|p| t.starts_with(p)) && !t.starts_with(";(") && !t.starts_with(";["))
        || t == "*"
        || t.starts_with("* ")
        || t.starts_with("*\t")
    {
        return Shape::Comment;
    }
    if t.starts_with(['}', ')', ']']) {
        let rest = t.trim_start_matches(['}', ')', ']', ' ', '\t', ';', ',']);
        return if rest.starts_with(|c: char| c.is_alphabetic()) {
            Shape::Clause
        } else {
            Shape::Closer
        };
    }
    let w = first_word(t).to_ascii_lowercase();
    let after = t[first_word(t).len()..].chars().next();
    // `end`, `end)`, `end if`, but not `endpoint = 1` or `end_time`.
    let standalone = !matches!(after, Some(c) if c.is_alphanumeric() || c == '_' || c == '=');
    if standalone && CLOSE_WORDS.contains(&w.as_str()) {
        return Shape::Closer;
    }
    if standalone && CLAUSE_WORDS.contains(&w.as_str()) {
        return Shape::Clause;
    }
    Shape::Code
}

/// Whether a block header looks like a definition: a definition keyword, a
/// function literal, or a C-style signature `name(...) {`.
fn is_definition(t: &str, allman: bool) -> bool {
    let t = t.trim_start_matches('(');
    if t.starts_with('|') {
        return false; // pattern-match arms
    }
    let mut rest = t;
    let mut word;
    let mut n = 0;
    loop {
        rest = rest.trim_start();
        word = first_word(rest);
        // `pub(crate)`, `@Override`
        let skip = if rest.starts_with('@') {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        } else if MODIFIERS.contains(&word) {
            let mut k = word.len();
            if rest[k..].starts_with('(') {
                k += rest[k..].find(')').map_or(0, |p| p + 1);
            }
            k
        } else {
            0
        };
        if skip == 0 || n == 6 {
            break;
        }
        rest = &rest[skip..];
        n += 1;
    }
    if CONTROL_WORDS.contains(&word) {
        return false;
    }
    let after = &rest[word.len()..];
    if DEF_WORDS.contains(&word)
        && after.starts_with([' ', '\t'])
        && !after.trim_start().starts_with('=')
    {
        return true;
    }
    has_function_literal(t) || c_signature(rest, allman)
}

/// `function(`, `fn x ->`, `lambda`, `(a) => {`, `{ x ->`.
fn has_function_literal(t: &str) -> bool {
    let b = t.as_bytes();
    let word_at = |i: usize, w: &str| {
        t[i..].starts_with(w)
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_'))
            && !t[i + w.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
    };
    for (i, _) in t.char_indices() {
        if ["function", "fn", "lambda", "proc"]
            .iter()
            .any(|w| word_at(i, w))
        {
            return true;
        }
        if t[i..].starts_with("=>") {
            return true;
        }
        if t[i..].starts_with("->") && matches!(b.get(i + 2), None | Some(b' ' | b'\t' | b'{')) {
            return true;
        }
    }
    false
}

/// `int area(Shape s) {`, `Widget build(BuildContext c) {`, `void f()` + `{`.
fn c_signature(t: &str, allman: bool) -> bool {
    let Some(open) = t.find('(') else {
        return false;
    };
    let before = t[..open].trim_end();
    let name_start = before
        .char_indices()
        .rev()
        .find(|&(_, c)| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |(p, c)| p + c.len_utf8());
    let name = &before[name_start..];
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    let prefix = &before[..name_start];
    if prefix.contains(['=', '(', ')', ',', '.', '"', '\''])
        || CONTROL_WORDS.contains(&name)
        || CONTROL_WORDS.contains(&first_word(prefix.trim_start()))
    {
        return false;
    }
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in t[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return false;
    };
    let tail = t[close + 1..].trim();
    if tail.contains(['=', ';']) {
        return false;
    }
    tail.ends_with('{') || (allman && !tail.contains('{'))
}

/// The block enclosing `target` (0-based), given a way to read line `i`
/// of `n`. `None` when the line is not inside a block, or the block is
/// longer than [`MAX_BLOCK`] lines.
pub fn enclosing<'a>(line: impl Fn(usize) -> &'a [u8], n: usize, target: usize) -> Option<Block> {
    if target >= n {
        return None;
    }
    let lo = target.saturating_sub(MAX_BLOCK);
    let hi = (target + MAX_BLOCK + 1).min(n);
    let w = Win {
        lo,
        lines: (lo..hi).map(line).collect(),
    };
    let t = target - lo;
    // Blank lines and comments belong to the code after them, and so do
    // decorators (`@spec`, `@native`) unless that block misses the line.
    let anchor = if w.is_code(t) { t } else { w.next_code(t)? };
    let mut decorated = anchor;
    while w.text(decorated).starts_with('@')
        && let Some(k) = w.next_code(decorated)
        && w.indent(k) == w.indent(decorated)
    {
        decorated = k;
    }
    let (start, end, pick) = [decorated, anchor]
        .into_iter()
        .filter_map(|a| block_from(&w, a))
        .find(|&(s, e, _)| s <= t && t <= e)?;
    if end - start >= MAX_BLOCK {
        return None;
    }
    Some(Block {
        start: start + w.lo,
        header: pick + w.lo,
        end: end + w.lo,
        title: clip(&squash(w.text(pick)), 100),
    })
}

/// (start, end, header) of the block around the code line `anchor`.
fn block_from(w: &Win<'_>, anchor: usize) -> Option<(usize, usize, usize)> {
    let mut headers: Vec<usize> = Vec::new();
    let mut level = w.indent(anchor);
    match w.shape(anchor) {
        Shape::Closer | Shape::Clause => level += 1,
        _ => {
            if w.next_code(anchor).is_some_and(|k| w.indent(k) > level) {
                headers.push(w.statement_start(anchor));
            }
        }
    }
    let mut i = headers.last().copied().unwrap_or(anchor);
    while level > 0 {
        let Some(h) = (0..i).rev().find(|&j| w.is_code(j) && w.indent(j) < level) else {
            break;
        };
        let h = w.statement_start(h);
        headers.push(h);
        level = w.indent(h);
        i = h;
    }
    let pick = headers
        .iter()
        .copied()
        .find(|&h| w.is_definition(h))
        .or_else(|| headers.last().copied())?;
    Some((w.doc_start(pick), w.block_end(pick), pick))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Block around 1-based line `l` of `src`, as 1-based (start, end, title).
    fn around(src: &str, l: usize) -> Option<(usize, usize, String)> {
        let lines: Vec<&[u8]> = src.lines().map(str::as_bytes).collect();
        enclosing(|i| lines[i], lines.len(), l - 1).map(|b| (b.start + 1, b.end + 1, b.title))
    }

    #[test]
    fn lua_callbacks_and_closers() {
        let src = "\
local Players = game:GetService(\"Players\")

Players.PlayerAdded:Connect(function(player)
\tlocal data = load(player)
\tplayer.CharacterAdded:Connect(function(char)
\t\tsetup(char, data)
\tend)
\tif data then
\t\tprint(data)
\telse
\t\twarn(\"none\")
\tend
end)

print(\"ready\")
";
        let outer = Some((3, 13, "Players.PlayerAdded:Connect(function(player)".into()));
        assert_eq!(around(src, 6).map(|b| (b.0, b.1)), Some((5, 7)));
        // `if`/`else` is control flow: the enclosing function is returned.
        assert_eq!(around(src, 11), outer);
        assert_eq!(around(src, 4), outer);
        // The header and the closer belong to their block.
        assert_eq!(around(src, 3), outer);
        assert_eq!(around(src, 13), outer);
        // Top-level lines outside any block: no block.
        assert_eq!(around(src, 1), None);
        assert_eq!(around(src, 15), None);
    }

    #[test]
    fn keyword_definitions_with_docs() {
        let src = "\
defmodule Greeter do
  @moduledoc false

  # Says hello.
  @spec hello(String.t()) :: String.t()
  def hello(name) do
    if name == \"\" do
      \"hi\"
    else
      \"hello \" <> name
    end
  end
end
";
        assert_eq!(around(src, 10), Some((4, 12, "def hello(name) do".into())));
        assert_eq!(around(src, 2).map(|b| (b.0, b.1)), Some((1, 13)));
        // A comment belongs to the definition below it.
        assert_eq!(around(src, 4).map(|b| (b.0, b.1)), Some((4, 12)));
    }

    #[test]
    fn indentation_only_and_clauses() {
        let src = "\
func _ready():
\tvar x = 1
\tif x > 0:
\t\tprint(x)
\telif x < 0:
\t\tpass

if __name__ == \"__main__\":
    main()
else:
    sys.exit(1)
";
        assert_eq!(around(src, 6), Some((1, 6, "func _ready():".into())));
        // No definition: the outermost statement, with all its clauses.
        assert_eq!(around(src, 11).map(|b| (b.0, b.1)), Some((8, 11)));
    }

    #[test]
    fn brace_styles_and_signatures() {
        let src = "\
class Shape {
  double area(int scale) {
    return compute(
      scale,
      2
    );
  }

  void draw()
  {
    paint(this);
  }

  Widget build(
    BuildContext context,
  ) {
    return items.map((i) {
      return Text(i);
    });
  }
}
";
        // Continuation lines (an open call) are not blocks of their own.
        assert_eq!(
            around(src, 4),
            Some((2, 7, "double area(int scale) {".into()))
        );
        assert_eq!(around(src, 11), Some((9, 12, "void draw()".into())));
        // A multi-line signature is recognized and resolves to its first
        // line; `return ...((i) {` is not a definition of its own.
        let build = Some((14, 20, "Widget build(".into()));
        assert_eq!(around(src, 18), build);
        assert_eq!(around(src, 16), build);
    }

    #[test]
    fn rejects_huge_blocks_and_unrelated_lines() {
        let mut src = String::from("def big():\n");
        for i in 0..500 {
            src.push_str(&format!("    x{i} = {i}\n"));
        }
        assert_eq!(around(&src, 250), None);
        // A blank line between definitions is in neither.
        let src = "def a():\n    pass\n\n\ndef b():\n    pass\n";
        assert_eq!(around(src, 3), None);
        assert_eq!(around(src, 6).map(|b| (b.0, b.1)), Some((5, 6)));
    }

    #[test]
    fn shapes() {
        assert_eq!(shape_of("end)"), Shape::Closer);
        assert_eq!(shape_of("End Sub"), Shape::Closer);
        assert_eq!(shape_of("});"), Shape::Closer);
        assert_eq!(shape_of(") -> u32 {"), Shape::Closer);
        assert_eq!(shape_of("} else {"), Shape::Clause);
        assert_eq!(shape_of("elif x:"), Shape::Clause);
        assert_eq!(shape_of("endpoint = 1"), Shape::Code);
        assert_eq!(shape_of("end_time()"), Shape::Code);
        assert_eq!(shape_of("-- note"), Shape::Comment);
        assert_eq!(shape_of("* @param x"), Shape::Comment);
        assert!(is_definition("pub(crate) async fn run(&self) {", false));
        assert!(is_definition("@Override public void run() {", false));
        assert!(is_definition("(defn handler [req]", false));
        assert!(is_definition("items.forEach((x) => {", false));
        assert!(is_definition("float4 main(PSInput i) : SV_Target {", false));
        assert!(!is_definition("if (x > 0) {", false));
        assert!(!is_definition("foo({", false));
        assert!(!is_definition("type = 3", false));
        assert!(!is_definition("local t = {", false));
        assert!(!is_definition("x = compute(a, b) {", false));
    }
}
