//! Symbol extraction ("outlines").
//!
//! Code is parsed with tree-sitter and walked iteratively (no recursion, so
//! pathological nesting cannot overflow the stack). Every definition becomes a
//! [`Symbol`] carrying its full line range (including leading doc comments,
//! attributes and decorators), a one-line signature label, and — for
//! function-like symbols — the interior line range that skeleton views may
//! collapse. Markdown/JSON/YAML/TOML use hand-written scanners (see
//! [`crate::structured`]) that produce the same structure.

use std::cell::RefCell;

use tree_sitter::{Node, Parser};

use crate::lang::LangId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Method,
    Constructor,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Impl,
    Module,
    Type,
    Const,
    Macro,
    Property,
    Test,
    Heading,
    Key,
    Section,
}

impl Kind {
    /// Symbols whose bodies are collapsed in skeleton views.
    pub fn collapsible(self) -> bool {
        matches!(
            self,
            Kind::Function
                | Kind::Method
                | Kind::Constructor
                | Kind::Property
                | Kind::Test
                | Kind::Key
                | Kind::Section
        )
    }

    pub fn is_type_like(self) -> bool {
        matches!(
            self,
            Kind::Class
                | Kind::Struct
                | Kind::Enum
                | Kind::Interface
                | Kind::Trait
                | Kind::Impl
                | Kind::Module
        )
    }
}

#[derive(Clone, Debug)]
pub struct Symbol {
    pub kind: Kind,
    /// Name used for lookups (`path#name`).
    pub name: String,
    /// Extra qualifier segments that are not ancestors (Go receivers, C++
    /// `Foo::bar` definitions, TOML dotted tables).
    pub scope: Option<String>,
    /// One-line signature for outlines.
    pub label: String,
    /// First line including leading docs/attributes (0-based).
    pub start: u32,
    /// First line of the definition itself.
    pub def: u32,
    /// Last line (inclusive).
    pub end: u32,
    /// Interior lines a skeleton view may elide (inclusive).
    pub collapse: Option<(u32, u32)>,
    pub parent: Option<u32>,
    pub depth: u16,
}

#[derive(Debug, Default, Clone)]
pub struct Outline {
    pub symbols: Vec<Symbol>,
    /// Extra line ranges skeletons may elide (e.g. long Python class and
    /// module docstrings), inclusive.
    pub elide: Vec<(u32, u32)>,
}

impl Outline {
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Innermost symbol containing `line` (0-based).
    pub fn innermost_at(&self, line: u32) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, s) in self.symbols.iter().enumerate() {
            if s.start <= line && line <= s.end {
                match best {
                    Some(b) if self.symbols[b].depth >= s.depth => {}
                    _ => best = Some(i),
                }
            }
        }
        best
    }

    /// Chain of names from the outermost ancestor to the symbol itself,
    /// including scope segments.
    pub fn qualified(&self, i: usize) -> Vec<&str> {
        let mut chain: Vec<&str> = Vec::new();
        let mut cur = Some(i as u32);
        while let Some(c) = cur {
            let s = &self.symbols[c as usize];
            let mut segs: Vec<&str> = split_segments(&s.name);
            if let Some(scope) = &s.scope {
                let mut sc = split_segments(scope);
                sc.append(&mut segs);
                segs = sc;
            }
            for seg in segs.into_iter().rev() {
                chain.push(seg);
            }
            cur = s.parent;
        }
        chain.reverse();
        chain
    }

    pub fn qualified_name(&self, i: usize) -> String {
        self.qualified(i).join(".")
    }

    /// Find symbols matching a (possibly qualified) name such as `parse`,
    /// `Parser.parse`, `Parser::parse` or `tool.poetry`.
    pub fn find(&self, query: &str) -> Vec<usize> {
        let q: Vec<&str> = split_segments(query.trim().trim_end_matches("()"));
        if q.is_empty() {
            return Vec::new();
        }
        for case_insensitive in [false, true] {
            let eq = |a: &str, b: &str| {
                if case_insensitive {
                    a.eq_ignore_ascii_case(b)
                } else {
                    a == b
                }
            };
            let hits: Vec<usize> = (0..self.symbols.len())
                .filter(|&i| {
                    let chain = self.qualified(i);
                    if chain.len() < q.len() {
                        return false;
                    }
                    let tail = &chain[chain.len() - q.len()..];
                    tail.iter()
                        .zip(&q)
                        .all(|(a, b)| eq(a, b) || eq(strip_selector(a), b))
                })
                .collect();
            if !hits.is_empty() {
                return hits;
            }
        }
        Vec::new()
    }

    /// Symbol names closest to `query`, for "did you mean" hints.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<String> {
        let q = split_segments(query)
            .last()
            .copied()
            .unwrap_or(query)
            .to_ascii_lowercase();
        let mut scored: Vec<(usize, String)> = self
            .symbols
            .iter()
            .enumerate()
            .filter(|(_, s)| !matches!(s.kind, Kind::Heading))
            .map(|(i, s)| {
                let n = s.name.to_ascii_lowercase();
                let d = if n.contains(&q) || q.contains(&n) {
                    0
                } else {
                    crate::util::levenshtein(&n, &q)
                };
                (d, self.qualified_name(i))
            })
            .filter(|(d, _)| *d <= (q.len() / 3).max(2))
            .collect();
        scored.sort();
        scored.dedup_by(|a, b| a.1 == b.1);
        scored.into_iter().take(limit).map(|(_, n)| n).collect()
    }

    /// Top-level symbol names (used by repo maps).
    pub fn top_level_names(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().filter(|s| s.parent.is_none())
    }
}

pub fn split_segments(s: &str) -> Vec<&str> {
    s.split("::")
        .flat_map(|p| p.split(['.', '#', '/']))
        .filter(|p| !p.is_empty())
        .collect()
}

/// `initWithFrame:style:` → `initWithFrame` (Objective-C selectors).
fn strip_selector(s: &str) -> &str {
    s.split(':').next().unwrap_or(s)
}

thread_local! {
    static PARSERS: RefCell<Vec<(LangId, Parser)>> = const { RefCell::new(Vec::new()) };
}

/// Apple SDK macros that tree-sitter's C-family grammars cannot parse
/// (`NS_ASSUME_NONNULL_BEGIN` alone derails error recovery for the rest of a
/// header). They are blanked with spaces — newlines and byte offsets are
/// preserved — before parsing. Type-defining macros like `NS_ENUM` are kept.
const APPLE_MACROS: &[&str] = &[
    "NS_ASSUME_NONNULL_BEGIN",
    "NS_ASSUME_NONNULL_END",
    "CF_ASSUME_NONNULL_BEGIN",
    "CF_ASSUME_NONNULL_END",
    "NS_HEADER_AUDIT_BEGIN",
    "NS_HEADER_AUDIT_END",
    "CF_EXTERN_C_BEGIN",
    "CF_EXTERN_C_END",
    "CF_IMPLICIT_BRIDGING_ENABLED",
    "CF_IMPLICIT_BRIDGING_DISABLED",
    "__BEGIN_DECLS",
    "__END_DECLS",
    "API_AVAILABLE",
    "API_UNAVAILABLE",
    "API_DEPRECATED",
    "API_DEPRECATED_WITH_REPLACEMENT",
    "API_AVAILABLE_BEGIN",
    "API_AVAILABLE_END",
    "NS_AVAILABLE",
    "NS_AVAILABLE_IOS",
    "NS_AVAILABLE_MAC",
    "NS_DEPRECATED",
    "NS_DEPRECATED_IOS",
    "NS_DEPRECATED_MAC",
    "NS_SWIFT_NAME",
    "NS_SWIFT_UNAVAILABLE",
    "NS_SWIFT_UI_ACTOR",
    "NS_SWIFT_SENDABLE",
    "NS_SWIFT_NONSENDABLE",
    "NS_SWIFT_ASYNC",
    "NS_SWIFT_ASYNC_NAME",
    "NS_REFINED_FOR_SWIFT",
    "NS_DESIGNATED_INITIALIZER",
    "NS_UNAVAILABLE",
    "NS_REQUIRES_SUPER",
    "NS_FORMAT_FUNCTION",
    "NS_RETURNS_RETAINED",
    "NS_RETURNS_NOT_RETAINED",
    "NS_NOESCAPE",
    "NS_ROOT_CLASS",
    "NS_EXTENSION_UNAVAILABLE",
    "NS_EXTENSION_UNAVAILABLE_IOS",
    "CF_RETURNS_RETAINED",
    "CF_RETURNS_NOT_RETAINED",
    "UI_APPEARANCE_SELECTOR",
    "IB_DESIGNABLE",
    "IBInspectable",
];

fn sanitize_apple_macros(src: &[u8]) -> Option<Vec<u8>> {
    if memchr::memmem::find(src, b"NS_").is_none()
        && memchr::memmem::find(src, b"API_").is_none()
        && memchr::memmem::find(src, b"CF_").is_none()
        && memchr::memmem::find(src, b"__BEGIN_DECLS").is_none()
    {
        return None;
    }
    let mut out: Option<Vec<u8>> = None;
    let n = src.len();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while i < n {
        if !(src[i].is_ascii_alphabetic() || src[i] == b'_') || (i > 0 && is_ident(src[i - 1])) {
            i += 1;
            continue;
        }
        let s = i;
        while i < n && is_ident(src[i]) {
            i += 1;
        }
        let word = &src[s..i];
        if !APPLE_MACROS.iter().any(|m| m.as_bytes() == word) {
            continue;
        }
        let mut end = i;
        let mut j = i;
        while j < n && (src[j] == b' ' || src[j] == b'\t') {
            j += 1;
        }
        if j < n && src[j] == b'(' {
            let mut depth = 0usize;
            while j < n {
                match src[j] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            end = j;
        }
        let buf = out.get_or_insert_with(|| src.to_vec());
        for b in &mut buf[s..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        i = end;
    }
    out
}

/// Parse `src` and extract its outline. Returns `None` if the language has no
/// grammar or parsing failed.
/// Parse `src` into a syntax tree (code languages only). Byte offsets match
/// `src` exactly (Apple SDK macros are blanked in place, not removed).
pub fn parse_tree(lang: LangId, src: &[u8]) -> Option<tree_sitter::Tree> {
    if lang.is_structured_data() {
        return None;
    }
    let grammar = lang.grammar()?;
    let cleaned = if matches!(lang, LangId::ObjC | LangId::C | LangId::Cpp) {
        sanitize_apple_macros(src)
    } else {
        None
    };
    let src: &[u8] = cleaned.as_deref().unwrap_or(src);
    PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        let idx = match parsers.iter().position(|(l, _)| *l == lang) {
            Some(i) => i,
            None => {
                let mut p = Parser::new();
                p.set_language(&grammar).ok()?;
                parsers.push((lang, p));
                parsers.len() - 1
            }
        };
        parsers[idx].1.parse(src, None)
    })
}

pub fn parse_outline(lang: LangId, src: &[u8], lines: &[u32]) -> Option<Outline> {
    if lang.is_structured_data() {
        return Some(crate::structured::outline(lang, src, lines));
    }
    let grammar = lang.grammar()?;
    let cleaned = if matches!(lang, LangId::ObjC | LangId::C | LangId::Cpp) {
        sanitize_apple_macros(src)
    } else {
        None
    };
    let src: &[u8] = cleaned.as_deref().unwrap_or(src);
    let tree = PARSERS.with(|cell| {
        let mut parsers = cell.borrow_mut();
        let idx = match parsers.iter().position(|(l, _)| *l == lang) {
            Some(i) => i,
            None => {
                let mut p = Parser::new();
                p.set_language(&grammar).ok()?;
                parsers.push((lang, p));
                parsers.len() - 1
            }
        };
        parsers[idx].1.parse(src, None)
    })?;
    let mut ex = Extractor {
        lang,
        src,
        lines,
        syms: Vec::new(),
        elide: Vec::new(),
    };
    let root = tree.root_node();
    ex.run(root);
    if lang == LangId::Python {
        ex.elide_docstring(root);
    }
    Some(Outline {
        symbols: ex.syms,
        elide: ex.elide,
    })
}

struct Def<'t> {
    kind: Kind,
    name: String,
    scope: Option<String>,
    /// Node whose start (plus preceding docs) begins the symbol's range.
    outer: Node<'t>,
    /// Node the range ends with (usually the definition itself).
    last: Node<'t>,
    /// Byte offset where the label begins.
    label_from: usize,
    /// Body node: label ends here and skeletons collapse its interior.
    body: Option<Node<'t>>,
    /// For containers: the node whose children are scanned for members.
    scan: Option<Node<'t>>,
}

struct Extractor<'a> {
    lang: LangId,
    src: &'a [u8],
    lines: &'a [u32],
    syms: Vec<Symbol>,
    elide: Vec<(u32, u32)>,
}

const MAX_NAME: usize = 120;
const MAX_LABEL: usize = 180;

impl<'a> Extractor<'a> {
    fn run(&mut self, root: Node<'_>) {
        let mut stack: Vec<(Node<'_>, Option<u32>, u16)> = vec![(root, None, 0)];
        let mut kids: Vec<Node<'_>> = Vec::new();
        while let Some((node, parent, depth)) = stack.pop() {
            if let Some(def) = self.classify(node, parent) {
                let idx = self.emit(&def, parent, depth);
                if let Some(scan) = def.scan {
                    kids.clear();
                    let mut c = scan.walk();
                    kids.extend(scan.named_children(&mut c));
                    for k in kids.iter().rev() {
                        stack.push((*k, Some(idx), depth + 1));
                    }
                }
                continue;
            }
            if self.opaque(node.kind()) {
                continue;
            }
            kids.clear();
            let mut c = node.walk();
            kids.extend(node.named_children(&mut c));
            for k in kids.iter().rev() {
                stack.push((*k, parent, depth));
            }
        }
    }

    fn opaque(&self, kind: &str) -> bool {
        matches!(
            kind,
            "comment"
                | "line_comment"
                | "block_comment"
                | "string"
                | "string_literal"
                | "raw_string_literal"
                | "template_string"
                | "regex"
                | "interpreted_string_literal"
                | "heredoc_body"
                | "jsx_element"
                | "jsx_self_closing_element"
        )
    }

    fn text(&self, n: Node<'_>) -> &'a str {
        std::str::from_utf8(&self.src[n.start_byte()..n.end_byte()]).unwrap_or("")
    }

    fn field_text(&self, n: Node<'_>, field: &str) -> Option<String> {
        n.child_by_field_name(field)
            .map(|c| clip(&squash(self.text(c)), MAX_NAME))
    }

    fn parent_kind(&self, parent: Option<u32>) -> Option<Kind> {
        parent.map(|p| self.syms[p as usize].kind)
    }

    fn in_type(&self, parent: Option<u32>) -> bool {
        self.parent_kind(parent)
            .is_some_and(|k| k.is_type_like() && k != Kind::Module)
    }

    fn comment_prefix(&self) -> &'static str {
        match self.lang {
            LangId::Python | LangId::Ruby | LangId::Bash => "#",
            LangId::Lua => "--",
            _ => "//",
        }
    }

    fn fn_kind(&self, parent: Option<u32>) -> Kind {
        if self.in_type(parent) {
            Kind::Method
        } else {
            Kind::Function
        }
    }

    /// Standard definition: named via the `name` field, body via `body`.
    fn simple<'t>(&self, n: Node<'t>, kind: Kind, container: bool) -> Option<Def<'t>> {
        let name = self.field_text(n, "name")?;
        Some(self.with_name(n, kind, container, name))
    }

    fn with_name<'t>(&self, n: Node<'t>, kind: Kind, container: bool, name: String) -> Def<'t> {
        let body = n.child_by_field_name("body");
        let outer = self.climb(n);
        let label_from = if self.label_includes_wrapper(outer) {
            outer.start_byte()
        } else {
            n.start_byte()
        };
        Def {
            kind,
            name,
            scope: None,
            outer,
            last: n,
            label_from,
            body,
            scan: if container {
                Some(body.unwrap_or(n))
            } else {
                None
            },
        }
    }

    /// Climb through wrapper nodes (export statements, decorators, templates).
    fn climb<'t>(&self, mut n: Node<'t>) -> Node<'t> {
        while let Some(p) = n.parent() {
            let wrap = match self.lang {
                LangId::JavaScript | LangId::TypeScript | LangId::Tsx => {
                    matches!(p.kind(), "export_statement" | "ambient_declaration")
                }
                LangId::Python => p.kind() == "decorated_definition",
                LangId::Cpp => p.kind() == "template_declaration",
                _ => false,
            };
            if !wrap {
                break;
            }
            n = p;
        }
        n
    }

    fn label_includes_wrapper(&self, outer: Node<'_>) -> bool {
        matches!(
            outer.kind(),
            "export_statement"
                | "ambient_declaration"
                | "template_declaration"
                | "lexical_declaration"
                | "variable_declaration"
        )
    }

    fn classify<'t>(&self, n: Node<'t>, parent: Option<u32>) -> Option<Def<'t>> {
        let k = n.kind();
        match self.lang {
            LangId::Rust => self.rust(n, k, parent),
            LangId::Python => self.python(n, k, parent),
            LangId::JavaScript | LangId::TypeScript | LangId::Tsx => self.js(n, k, parent),
            LangId::Go => self.go(n, k),
            LangId::Java => self.java(n, k),
            LangId::C | LangId::Cpp => self.c_like(n, k, parent),
            LangId::ObjC => self.objc(n, k, parent),
            LangId::CSharp => self.csharp(n, k),
            LangId::Ruby => self.ruby(n, k, parent),
            LangId::Php => self.php(n, k, parent),
            LangId::Bash => match k {
                "function_definition" => self.simple(n, Kind::Function, false),
                _ => None,
            },
            LangId::Swift => self.swift(n, k, parent),
            LangId::Kotlin => self.kotlin(n, k, parent),
            LangId::Scala => self.scala(n, k, parent),
            LangId::Lua => self.lua(n, k),
            LangId::Markdown | LangId::Json | LangId::Yaml | LangId::Toml => None,
        }
    }

    fn rust<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "function_item" => self.simple(n, self.fn_kind(parent), false),
            "function_signature_item" => self.simple(n, Kind::Method, false),
            "struct_item" | "union_item" => self.simple(n, Kind::Struct, false),
            "enum_item" => self.simple(n, Kind::Enum, false),
            "trait_item" => self.simple(n, Kind::Trait, true),
            "impl_item" => {
                let ty = n.child_by_field_name("type")?;
                let name = strip_generics(self.text(ty)).to_string();
                Some(self.with_name(n, Kind::Impl, true, name))
            }
            "mod_item" => {
                let has_body = n.child_by_field_name("body").is_some();
                self.simple(n, Kind::Module, has_body)
            }
            "const_item" | "static_item" => self.simple(n, Kind::Const, false),
            "type_item" => self.simple(n, Kind::Type, false),
            "macro_definition" => self.simple(n, Kind::Macro, false),
            _ => None,
        }
    }

    fn python<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "function_definition" => self.simple(n, self.fn_kind(parent), false),
            "class_definition" => self.simple(n, Kind::Class, true),
            _ => None,
        }
    }

    fn js<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                self.simple(n, Kind::Function, false)
            }
            "class_declaration" | "abstract_class_declaration" => self.simple(n, Kind::Class, true),
            "class" => self.simple(n, Kind::Class, true),
            "method_definition" | "method_signature" | "abstract_method_signature" => {
                let name = self.field_text(n, "name")?;
                let kind = if name == "constructor" {
                    Kind::Constructor
                } else {
                    Kind::Method
                };
                Some(self.with_name(n, kind, false, name))
            }
            "interface_declaration" => self.simple(n, Kind::Interface, true),
            "type_alias_declaration" => self.simple(n, Kind::Type, false),
            "enum_declaration" => self.simple(n, Kind::Enum, false),
            "internal_module" | "module" => {
                let name = self.field_text(n, "name")?;
                let name = name.trim_matches(|c| c == '"' || c == '\'').to_string();
                Some(self.with_name(n, Kind::Module, true, name))
            }
            "variable_declarator" => self.js_declarator(n, parent),
            "public_field_definition" | "field_definition" => {
                let value = n.child_by_field_name("value")?;
                let body = js_function_body(value)?;
                let name = self
                    .field_text(n, "name")
                    .or_else(|| self.field_text(n, "property"))?;
                let mut d = self.with_name(n, Kind::Method, false, name);
                d.body = Some(body);
                Some(d)
            }
            "pair" => {
                let value = n.child_by_field_name("value")?;
                if !matches!(
                    value.kind(),
                    "arrow_function" | "function_expression" | "function"
                ) {
                    return None;
                }
                let key = self.field_text(n, "key")?;
                let key = key.trim_matches(|c| c == '"' || c == '\'').to_string();
                let mut d = self.with_name(n, Kind::Method, false, key);
                d.body = value.child_by_field_name("body");
                Some(d)
            }
            "call_expression" => self.js_test_call(n),
            _ => None,
        }
    }

    fn js_declarator<'t>(&self, n: Node<'t>, parent: Option<u32>) -> Option<Def<'t>> {
        let value = n.child_by_field_name("value")?;
        let (kind, body, container) = match value.kind() {
            "class" => (Kind::Class, value.child_by_field_name("body"), true),
            _ => (self.fn_kind(parent), Some(js_function_body(value)?), false),
        };
        let name = self.field_text(n, "name")?;
        // Range covers `export const x = ...` when the declaration has a single declarator.
        let mut outer = n;
        if let Some(p) = n.parent()
            && matches!(p.kind(), "lexical_declaration" | "variable_declaration")
            && p.named_child_count() == 1
        {
            outer = self.climb(p);
        }
        Some(Def {
            kind,
            name,
            scope: None,
            outer,
            last: n,
            label_from: outer.start_byte(),
            body,
            scan: if container { body } else { None },
        })
    }

    /// `describe("...", () => {...})`, `it(...)`, `test(...)` blocks.
    fn js_test_call<'t>(&self, n: Node<'t>) -> Option<Def<'t>> {
        let func = n.child_by_field_name("function")?;
        let base = match func.kind() {
            "identifier" => self.text(func),
            "member_expression" => {
                let obj = func.child_by_field_name("object")?;
                if obj.kind() != "identifier" {
                    return None;
                }
                self.text(obj)
            }
            _ => return None,
        };
        let container = match base {
            "describe" | "suite" | "context" => true,
            "it" | "test" | "specify" | "bench" => false,
            _ => return None,
        };
        let args = n.child_by_field_name("arguments")?;
        let mut c = args.walk();
        let list: Vec<Node<'t>> = args.named_children(&mut c).collect();
        let first = list.first()?;
        if !matches!(first.kind(), "string" | "template_string") {
            return None;
        }
        let title = clip(&squash(self.text(*first)), MAX_NAME);
        let callback = list.iter().rev().find(|a| {
            matches!(
                a.kind(),
                "arrow_function" | "function_expression" | "function"
            )
        })?;
        let body = callback.child_by_field_name("body");
        let outer = match n.parent() {
            Some(p) if p.kind() == "expression_statement" => p,
            _ => n,
        };
        let name = title
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .to_string();
        Some(Def {
            kind: Kind::Test,
            name,
            scope: None,
            outer,
            last: n,
            label_from: n.start_byte(),
            body,
            scan: if container { body } else { None },
        })
    }

    fn go<'t>(&self, n: Node<'t>, k: &str) -> Option<Def<'t>> {
        match k {
            "function_declaration" => self.simple(n, Kind::Function, false),
            "method_declaration" => {
                let mut d = self.simple(n, Kind::Method, false)?;
                if let Some(recv) = n.child_by_field_name("receiver") {
                    let t = self.text(recv);
                    let ty = t
                        .trim_matches(|c| c == '(' || c == ')')
                        .split_whitespace()
                        .last()
                        .unwrap_or("")
                        .trim_start_matches('*');
                    let ty = strip_generics(ty);
                    if !ty.is_empty() {
                        d.scope = Some(ty.to_string());
                    }
                }
                Some(d)
            }
            "type_spec" | "type_alias" => {
                let ty = n.child_by_field_name("type");
                let kind = match ty.map(|t| t.kind()) {
                    Some("struct_type") => Kind::Struct,
                    Some("interface_type") => Kind::Interface,
                    _ => Kind::Type,
                };
                let name = self.field_text(n, "name")?;
                let mut d = self.with_name(n, kind, false, name);
                if let Some(p) = n.parent()
                    && p.kind() == "type_declaration"
                    && p.named_child_count() == 1
                {
                    d.outer = p;
                    d.label_from = p.start_byte();
                }
                if let Some(t) = ty {
                    if matches!(t.kind(), "struct_type" | "interface_type") {
                        d.body = Some(t);
                    }
                    if t.kind() == "interface_type" {
                        d.scan = Some(t);
                    }
                }
                Some(d)
            }
            "method_elem" | "method_spec" => self.simple(n, Kind::Method, false),
            _ => None,
        }
    }

    fn java<'t>(&self, n: Node<'t>, k: &str) -> Option<Def<'t>> {
        match k {
            "class_declaration" | "record_declaration" => self.simple(n, Kind::Class, true),
            "interface_declaration" | "annotation_type_declaration" => {
                self.simple(n, Kind::Interface, true)
            }
            "enum_declaration" => self.simple(n, Kind::Enum, true),
            "method_declaration" => self.simple(n, Kind::Method, false),
            "constructor_declaration" | "compact_constructor_declaration" => {
                self.simple(n, Kind::Constructor, false)
            }
            _ => None,
        }
    }

    fn c_like<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        let cpp = self.lang == LangId::Cpp;
        match k {
            "function_definition" => {
                let decl = n.child_by_field_name("declarator")?;
                let name = self.declarator_name(decl)?;
                let kind = if self.in_type(parent) || name.contains("::") {
                    Kind::Method
                } else {
                    Kind::Function
                };
                let mut d = self.with_name(n, kind, false, name);
                split_cpp_scope(&mut d);
                Some(d)
            }
            "declaration" | "field_declaration" => {
                let decl = n.child_by_field_name("declarator")?;
                if !has_function_declarator(decl) {
                    return None;
                }
                let name = self.declarator_name(decl)?;
                let kind = if k == "field_declaration" || self.in_type(parent) {
                    Kind::Method
                } else {
                    Kind::Function
                };
                let mut d = self.with_name(n, kind, false, name);
                split_cpp_scope(&mut d);
                Some(d)
            }
            "struct_specifier" | "union_specifier" | "class_specifier" | "enum_specifier" => {
                n.child_by_field_name("body")?;
                let kind = match k {
                    "class_specifier" => Kind::Class,
                    "enum_specifier" => Kind::Enum,
                    _ => Kind::Struct,
                };
                let container = cpp && kind != Kind::Enum;
                self.simple(n, kind, container)
            }
            "type_definition" => {
                let decl = n.child_by_field_name("declarator")?;
                let name = clip(&squash(self.text(decl)), MAX_NAME);
                let ty = n.child_by_field_name("type");
                let kind = match ty.map(|t| t.kind()) {
                    Some("struct_specifier" | "union_specifier") => Kind::Struct,
                    Some("enum_specifier") => Kind::Enum,
                    _ => Kind::Type,
                };
                let mut d = self.with_name(n, kind, false, name);
                d.body = ty.and_then(|t| t.child_by_field_name("body"));
                Some(d)
            }
            "namespace_definition" => {
                let name = self
                    .field_text(n, "name")
                    .unwrap_or_else(|| "(anonymous)".to_string());
                Some(self.with_name(n, Kind::Module, true, name))
            }
            "preproc_function_def" => self.simple(n, Kind::Macro, false),
            _ => None,
        }
    }

    fn declarator_name(&self, mut d: Node<'_>) -> Option<String> {
        for _ in 0..16 {
            match d.kind() {
                "identifier"
                | "field_identifier"
                | "qualified_identifier"
                | "destructor_name"
                | "operator_name"
                | "type_identifier"
                | "template_function" => {
                    return Some(clip(&squash(self.text(d)), MAX_NAME));
                }
                _ => d = d.child_by_field_name("declarator")?,
            }
        }
        None
    }

    fn objc<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "class_interface" | "class_implementation" | "protocol_declaration" => {
                let mut c = n.walk();
                let ident = n
                    .named_children(&mut c)
                    .find(|ch| matches!(ch.kind(), "identifier" | "type_identifier"))?;
                let mut name = self.text(ident).to_string();
                if let Some(cat) = n.child_by_field_name("category") {
                    name = format!("{name}({})", self.text(cat));
                }
                let kind = match k {
                    "protocol_declaration" => Kind::Interface,
                    "class_implementation" => Kind::Impl,
                    _ => Kind::Class,
                };
                let mut d = self.with_name(n, kind, true, name);
                d.scan = Some(n);
                Some(d)
            }
            "method_declaration" | "method_definition" => {
                let name = self.objc_selector(n)?;
                let mut d = self.with_name(n, Kind::Method, false, name);
                let mut c = n.walk();
                d.body = n
                    .named_children(&mut c)
                    .find(|ch| ch.kind() == "compound_statement");
                Some(d)
            }
            _ => self.c_like(n, k, parent),
        }
    }

    fn objc_selector(&self, n: Node<'_>) -> Option<String> {
        let mut c = n.walk();
        let mut parts = String::new();
        let mut keyword = false;
        for ch in n.named_children(&mut c) {
            match ch.kind() {
                "identifier" if parts.is_empty() => parts.push_str(self.text(ch)),
                "method_parameter" => {
                    keyword = true;
                    parts.push(':');
                }
                "keyword_declarator" => {
                    keyword = true;
                    if let Some(kw) = ch.child_by_field_name("keyword") {
                        parts.push_str(self.text(kw));
                    }
                    parts.push(':');
                }
                "identifier" if keyword => {
                    parts.push_str(self.text(ch));
                }
                _ => {}
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(clip(&parts, MAX_NAME))
        }
    }

    fn csharp<'t>(&self, n: Node<'t>, k: &str) -> Option<Def<'t>> {
        match k {
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                let mut d = self.simple(n, Kind::Module, true)?;
                if d.scan.is_none() {
                    d.scan = Some(n);
                }
                Some(d)
            }
            "class_declaration" | "record_declaration" => self.simple(n, Kind::Class, true),
            "struct_declaration" | "record_struct_declaration" => {
                self.simple(n, Kind::Struct, true)
            }
            "interface_declaration" => self.simple(n, Kind::Interface, true),
            "enum_declaration" => self.simple(n, Kind::Enum, false),
            "method_declaration" | "operator_declaration" | "conversion_operator_declaration" => {
                let name = self
                    .field_text(n, "name")
                    .or_else(|| {
                        self.field_text(n, "operator")
                            .map(|o| format!("operator {o}"))
                    })
                    .unwrap_or_else(|| "operator".to_string());
                Some(self.with_name(n, Kind::Method, false, name))
            }
            "constructor_declaration" | "destructor_declaration" => {
                self.simple(n, Kind::Constructor, false)
            }
            "property_declaration" => {
                let mut d = self.simple(n, Kind::Property, false)?;
                d.body = n.child_by_field_name("accessors");
                Some(d)
            }
            "indexer_declaration" => {
                let mut d = self.with_name(n, Kind::Property, false, "this[]".to_string());
                d.body = n.child_by_field_name("accessors");
                Some(d)
            }
            "delegate_declaration" => self.simple(n, Kind::Type, false),
            _ => None,
        }
    }

    fn ruby<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "class" => self.simple(n, Kind::Class, true),
            "module" => self.simple(n, Kind::Module, true),
            "singleton_class" => Some(self.with_name(n, Kind::Class, true, "<< self".to_string())),
            "method" => self.simple(n, self.fn_kind(parent), false),
            "singleton_method" => {
                let name = self.field_text(n, "name")?;
                Some(self.with_name(n, Kind::Method, false, name))
            }
            "call" => {
                let method = n.child_by_field_name("method")?;
                let m = self.text(method);
                let container = match m {
                    "describe" | "context" | "feature" | "shared_examples" => true,
                    "it" | "specify" | "scenario" => false,
                    _ => return None,
                };
                let block = n.child_by_field_name("block")?;
                let args = n.child_by_field_name("arguments");
                let title = args
                    .and_then(|a| a.named_child(0))
                    .map(|a| squash(self.text(a)))
                    .unwrap_or_default();
                let name = clip(title.trim_matches(|c| c == '"' || c == '\''), MAX_NAME);
                let body = block.child_by_field_name("body").or(Some(block));
                Some(Def {
                    kind: Kind::Test,
                    name,
                    scope: None,
                    outer: n,
                    last: n,
                    label_from: n.start_byte(),
                    body,
                    scan: if container { body } else { None },
                })
            }
            _ => None,
        }
    }

    fn php<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "namespace_definition" => {
                let has_body = n.child_by_field_name("body").is_some();
                self.simple(n, Kind::Module, has_body)
            }
            "class_declaration" | "trait_declaration" => self.simple(n, Kind::Class, true),
            "interface_declaration" => self.simple(n, Kind::Interface, true),
            "enum_declaration" => self.simple(n, Kind::Enum, true),
            "function_definition" => self.simple(n, self.fn_kind(parent), false),
            "method_declaration" => {
                let name = self.field_text(n, "name")?;
                let kind = if name == "__construct" {
                    Kind::Constructor
                } else {
                    Kind::Method
                };
                Some(self.with_name(n, kind, false, name))
            }
            _ => None,
        }
    }

    fn swift<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "class_declaration" => {
                let dk = n
                    .child_by_field_name("declaration_kind")
                    .map(|c| self.text(c))
                    .unwrap_or("class");
                let kind = match dk {
                    "struct" => Kind::Struct,
                    "enum" => Kind::Enum,
                    "extension" => Kind::Impl,
                    _ => Kind::Class,
                };
                let name = strip_generics(&self.field_text(n, "name")?).to_string();
                Some(self.with_name(n, kind, true, name))
            }
            "protocol_declaration" => self.simple(n, Kind::Interface, true),
            "function_declaration" => self.simple(n, self.fn_kind(parent), false),
            "protocol_function_declaration" => self.simple(n, Kind::Method, false),
            "init_declaration" => {
                Some(self.with_name(n, Kind::Constructor, false, "init".to_string()))
            }
            "deinit_declaration" => {
                Some(self.with_name(n, Kind::Method, false, "deinit".to_string()))
            }
            "subscript_declaration" => {
                Some(self.with_name(n, Kind::Method, false, "subscript".to_string()))
            }
            "typealias_declaration" => self.simple(n, Kind::Type, false),
            "property_declaration" => {
                // Computed properties (`var body: some View { ... }`) are worth outlining.
                let mut c = n.walk();
                let computed = n
                    .named_children(&mut c)
                    .find(|ch| ch.kind() == "computed_property")?;
                let name = self.field_text(n, "name")?;
                let mut d = self.with_name(n, Kind::Property, false, name);
                d.body = Some(computed);
                Some(d)
            }
            _ => None,
        }
    }

    fn kotlin<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        let child_of_kind = |kinds: &[&str]| {
            let mut c = n.walk();
            n.named_children(&mut c)
                .find(|ch| kinds.contains(&ch.kind()))
        };
        match k {
            "class_declaration" | "object_declaration" | "companion_object" => {
                let name = self
                    .field_text(n, "name")
                    .unwrap_or_else(|| "companion object".to_string());
                let is_interface = {
                    let mut c = n.walk();
                    n.children(&mut c).any(|ch| ch.kind() == "interface")
                };
                let kind = if is_interface {
                    Kind::Interface
                } else {
                    Kind::Class
                };
                let mut d = self.with_name(n, kind, true, name);
                let body = child_of_kind(&["class_body", "enum_class_body"]);
                d.body = body;
                d.scan = Some(body.unwrap_or(n));
                Some(d)
            }
            "function_declaration" => {
                let name = self.field_text(n, "name")?;
                let mut d = self.with_name(n, self.fn_kind(parent), false, name);
                d.body = child_of_kind(&["function_body"]);
                Some(d)
            }
            "secondary_constructor" => {
                let mut d = self.with_name(n, Kind::Constructor, false, "constructor".to_string());
                d.body = child_of_kind(&["block"]);
                Some(d)
            }
            "type_alias" => {
                let name = child_of_kind(&["identifier", "type_identifier"])
                    .map(|c| self.text(c).to_string())
                    .or_else(|| self.field_text(n, "type"))?;
                Some(self.with_name(n, Kind::Type, false, name))
            }
            _ => None,
        }
    }

    fn scala<'t>(&self, n: Node<'t>, k: &str, parent: Option<u32>) -> Option<Def<'t>> {
        match k {
            "class_definition" | "object_definition" | "enum_definition" => {
                self.simple(n, Kind::Class, true)
            }
            "trait_definition" => self.simple(n, Kind::Trait, true),
            "function_definition" => self.simple(n, self.fn_kind(parent), false),
            "function_declaration" => self.simple(n, Kind::Method, false),
            "type_definition" => self.simple(n, Kind::Type, false),
            _ => None,
        }
    }

    fn lua<'t>(&self, n: Node<'t>, k: &str) -> Option<Def<'t>> {
        match k {
            "function_declaration" => self.simple(n, Kind::Function, false),
            "assignment_statement" | "variable_declaration" => {
                // `M.foo = function(...) ... end`
                let mut c = n.walk();
                let kids: Vec<Node<'t>> = n.named_children(&mut c).collect();
                let (vars, vals) = if n.kind() == "variable_declaration" {
                    let inner = kids.first()?;
                    if inner.kind() != "assignment_statement" {
                        return None;
                    }
                    let mut c2 = inner.walk();
                    let ik: Vec<Node<'t>> = inner.named_children(&mut c2).collect();
                    (ik.first().copied()?, ik.get(1).copied()?)
                } else {
                    (kids.first().copied()?, kids.get(1).copied()?)
                };
                let func = if vals.kind() == "function_definition" {
                    vals
                } else {
                    vals.named_child(0)
                        .filter(|f| f.kind() == "function_definition")?
                };
                let name = clip(&squash(self.text(vars)), MAX_NAME);
                let mut d = self.with_name(n, Kind::Function, false, name);
                d.body = func.child_by_field_name("body");
                d.last = n;
                Some(d)
            }
            _ => None,
        }
    }

    /// Long docstring as the first statement of `block`: keep its first line.
    fn elide_docstring(&mut self, block: Node<'_>) {
        if let Some(first) = block.named_child(0)
            && first.kind() == "expression_statement"
            && first.named_child(0).is_some_and(|s| s.kind() == "string")
        {
            let (a, b) = (first.start_position().row, end_row(first));
            if b >= a + 4 {
                self.elide.push((a as u32 + 1, b as u32));
            }
        }
    }

    fn emit(&mut self, d: &Def<'_>, parent: Option<u32>, depth: u16) -> u32 {
        if self.lang == LangId::Python
            && d.kind == Kind::Class
            && let Some(body) = d.body
        {
            self.elide_docstring(body);
        }
        let def_row = d.outer.start_position().row as u32;
        let start = self.doc_start(d.outer) as u32;
        let end = end_row(d.last).max(end_row(d.outer)) as u32;
        let label_to = match d.body {
            Some(b) if b.start_byte() > d.label_from => b.start_byte(),
            _ => d.last.end_byte().min(d.label_from + 400),
        };
        let mut raw = &self.src[d.label_from.min(label_to)..label_to];
        if d.body.is_none() && d.scan.is_some() {
            // Body-less containers (ObjC @interface): keep the first line only.
            let nl = memchr::memchr(b'\n', raw).unwrap_or(raw.len());
            raw = &raw[..nl];
        }
        let label = make_label(raw, self.comment_prefix());
        let collapse = if d.kind.collapsible() && d.scan.is_none() {
            d.body.and_then(|b| self.collapse_range(d.last, b))
        } else {
            None
        };
        let name = if d.name.is_empty() {
            "_".to_string()
        } else {
            d.name.clone()
        };
        self.syms.push(Symbol {
            kind: d.kind,
            name,
            scope: d.scope.clone(),
            label,
            start,
            def: def_row,
            end,
            collapse,
            parent,
            depth,
        });
        (self.syms.len() - 1) as u32
    }

    /// Extend a definition upward over adjacent doc comments / attributes.
    fn doc_start(&self, outer: Node<'_>) -> usize {
        let mut row = outer.start_position().row;
        let col = outer.start_position().column;
        let mut cur = outer;
        while let Some(prev) = cur.prev_named_sibling() {
            let is_doc = matches!(
                prev.kind(),
                "comment"
                    | "line_comment"
                    | "block_comment"
                    | "multiline_comment"
                    | "attribute_item"
                    | "decorator"
                    | "attribute_list"
                    | "annotation"
                    | "marker_annotation"
            );
            if !is_doc || end_row(prev) + 1 < row || prev.start_position().column > col {
                break;
            }
            // A trailing comment on a line that also holds code is not a doc comment.
            let line_start = self.lines[prev.start_position().row] as usize;
            if self.src[line_start..prev.start_byte()]
                .iter()
                .any(|b| !b.is_ascii_whitespace())
            {
                break;
            }
            row = prev.start_position().row;
            cur = prev;
        }
        row
    }

    /// Interior rows of a body that a skeleton may elide.
    fn collapse_range(&self, node: Node<'_>, body: Node<'_>) -> Option<(u32, u32)> {
        let header_row = node.start_position().row.max(self.label_row(node));
        let bs = body.start_position().row;
        let first = self.src.get(body.start_byte()).copied().unwrap_or(b' ');
        let opens = matches!(first, b'{' | b'(' | b'[');
        let mut cs = if bs <= header_row || opens {
            bs + 1
        } else {
            bs
        };
        // Keep Python docstrings' first line(s) visible.
        if self.lang == LangId::Python
            && let Some(first_stmt) = body.named_child(0)
            && first_stmt.kind() == "expression_statement"
            && first_stmt
                .named_child(0)
                .is_some_and(|s| s.kind() == "string")
        {
            let ds = first_stmt.start_position().row;
            let de = end_row(first_stmt);
            cs = cs.max(if de - ds <= 2 { de + 1 } else { ds + 1 });
        }
        let nend = end_row(node).max(end_row(body));
        let mut ce = nend;
        if self.is_closing_line(nend) {
            ce = nend.saturating_sub(1);
        }
        if ce >= cs + 2 {
            Some((cs as u32, ce as u32))
        } else {
            None
        }
    }

    fn label_row(&self, node: Node<'_>) -> usize {
        node.start_position().row
    }

    fn line_text(&self, row: usize) -> &[u8] {
        let s = self.lines[row] as usize;
        let e = self
            .lines
            .get(row + 1)
            .map(|&e| e as usize)
            .unwrap_or(self.src.len());
        &self.src[s..e]
    }

    fn is_closing_line(&self, row: usize) -> bool {
        if row >= self.lines.len() {
            return false;
        }
        let t = trim_ascii(self.line_text(row));
        if t.is_empty() {
            return false;
        }
        let rest: &[u8] = {
            let mut i = 0;
            while i < t.len() && matches!(t[i], b'}' | b')' | b']') {
                i += 1;
            }
            &t[i..]
        };
        if rest.len() < t.len()
            && rest
                .iter()
                .all(|b| matches!(b, b';' | b',' | b')' | b']' | b'}' | b' '))
        {
            return true;
        }
        matches!(
            t,
            b"end" | b"end;" | b"end)" | b"end," | b"@end" | b"fi" | b"done" | b"esac"
        )
    }
}

fn js_function_body<'t>(value: Node<'t>) -> Option<Node<'t>> {
    match value.kind() {
        "arrow_function" | "function_expression" | "function" | "generator_function" => {
            value.child_by_field_name("body")
        }
        // `export const Button = forwardRef((props, ref) => {...})`
        "call_expression" => {
            let args = value.child_by_field_name("arguments")?;
            let mut c = args.walk();
            let f = args
                .named_children(&mut c)
                .filter(|a| {
                    matches!(
                        a.kind(),
                        "arrow_function" | "function_expression" | "function"
                    )
                })
                .last()?;
            f.child_by_field_name("body")
        }
        _ => None,
    }
}

fn has_function_declarator(mut d: Node<'_>) -> bool {
    for _ in 0..16 {
        if d.kind() == "function_declarator" {
            return true;
        }
        match d.child_by_field_name("declarator") {
            Some(n) => d = n,
            None => return false,
        }
    }
    false
}

/// `Foo::bar` → name `bar`, scope `Foo`.
fn split_cpp_scope(d: &mut Def<'_>) {
    if let Some((scope, name)) = d.name.rsplit_once("::")
        && !name.is_empty()
        && !scope.is_empty()
    {
        let scope: Vec<&str> = scope.split("::").map(strip_generics).collect();
        d.scope = Some(scope.join("::"));
        d.name = name.to_string();
    }
}

pub fn end_row(n: Node<'_>) -> usize {
    let e = n.end_position();
    if e.column == 0 && e.row > n.start_position().row {
        e.row - 1
    } else {
        e.row
    }
}

fn strip_generics(s: &str) -> &str {
    let s = s.trim();
    match s.find('<') {
        Some(i) if i > 0 => s[..i].trim(),
        _ => s,
    }
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let mut s = 0;
    let mut e = b.len();
    while s < e && b[s].is_ascii_whitespace() {
        s += 1;
    }
    while e > s && b[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    &b[s..e]
}

/// Drop a trailing line comment (outside string literals).
fn strip_line_comment<'s>(line: &'s str, prefix: &str) -> &'s str {
    let b = line.as_bytes();
    let p = prefix.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match quote {
            Some(q) => {
                if c == b'\\' {
                    i += 2;
                    continue;
                }
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == b'"' || c == b'`' {
                    quote = Some(c);
                } else if b[i..].starts_with(p) && (i == 0 || b[i - 1].is_ascii_whitespace()) {
                    return &line[..i];
                }
            }
        }
        i += 1;
    }
    line
}

/// Collapse runs of whitespace into single spaces.
pub fn squash(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(256));
    let mut ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            ws = true;
        } else {
            if ws && !out.is_empty() {
                out.push(' ');
            }
            ws = false;
            out.push(c);
        }
    }
    out
}

pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn make_label(raw: &[u8], comment: &str) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut joined = String::with_capacity(s.len());
    for line in s.lines() {
        joined.push_str(strip_line_comment(line, comment));
        joined.push('\n');
    }
    let mut l = squash(&joined);
    loop {
        let t = l.trim_end();
        let t2 = t
            .strip_suffix('{')
            .or_else(|| t.strip_suffix(':'))
            .or_else(|| t.strip_suffix(" do"))
            .or_else(|| t.strip_suffix(" ="));
        match t2 {
            Some(x) => l = x.trim_end().to_string(),
            None => {
                l = t.to_string();
                break;
            }
        }
    }
    let l = l
        .replace("( ", "(")
        .replace(" )", ")")
        .replace(",)", ")")
        .replace("[ ", "[")
        .replace(" ]", "]");
    clip(&l, MAX_LABEL)
}
