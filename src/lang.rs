//! Language detection and tree-sitter grammar registry.

use std::path::Path;

use tree_sitter::Language;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LangId {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
    Bash,
    Swift,
    Kotlin,
    Scala,
    Lua,
    Luau,
    ObjC,
    Markdown,
    Json,
    Yaml,
    Toml,
}

impl LangId {
    pub fn name(self) -> &'static str {
        match self {
            LangId::Rust => "rust",
            LangId::Python => "python",
            LangId::JavaScript => "javascript",
            LangId::TypeScript => "typescript",
            LangId::Tsx => "tsx",
            LangId::Go => "go",
            LangId::Java => "java",
            LangId::C => "c",
            LangId::Cpp => "cpp",
            LangId::CSharp => "csharp",
            LangId::Ruby => "ruby",
            LangId::Php => "php",
            LangId::Bash => "bash",
            LangId::Swift => "swift",
            LangId::Kotlin => "kotlin",
            LangId::Scala => "scala",
            LangId::Lua => "lua",
            LangId::Luau => "luau",
            LangId::ObjC => "objc",
            LangId::Markdown => "markdown",
            LangId::Json => "json",
            LangId::Yaml => "yaml",
            LangId::Toml => "toml",
        }
    }

    /// Tree-sitter grammar, if this language is parsed with tree-sitter.
    pub fn grammar(self) -> Option<Language> {
        Some(match self {
            LangId::Rust => tree_sitter_rust::LANGUAGE.into(),
            LangId::Python => tree_sitter_python::LANGUAGE.into(),
            LangId::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            LangId::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            LangId::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            LangId::Go => tree_sitter_go::LANGUAGE.into(),
            LangId::Java => tree_sitter_java::LANGUAGE.into(),
            LangId::C => tree_sitter_c::LANGUAGE.into(),
            LangId::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            LangId::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            LangId::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            LangId::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            LangId::Bash => tree_sitter_bash::LANGUAGE.into(),
            LangId::Swift => tree_sitter_swift::LANGUAGE.into(),
            LangId::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            LangId::Scala => tree_sitter_scala::LANGUAGE.into(),
            LangId::Lua => tree_sitter_lua::LANGUAGE.into(),
            LangId::Luau => tree_sitter_luau::LANGUAGE.into(),
            LangId::ObjC => tree_sitter_objc::LANGUAGE.into(),
            LangId::Markdown | LangId::Json | LangId::Yaml | LangId::Toml => return None,
        })
    }

    /// Languages whose outline comes from a hand-written structural scanner.
    pub fn is_structured_data(self) -> bool {
        matches!(
            self,
            LangId::Markdown | LangId::Json | LangId::Yaml | LangId::Toml
        )
    }
}

/// Detect the language of a file from its name, extension and (for
/// extensionless scripts) its shebang line.
pub fn detect(path: &Path, head: &[u8]) -> Option<LangId> {
    let name = path.file_name()?.to_str()?;
    match name {
        "Rakefile" | "Gemfile" | "Podfile" | "Fastfile" | "Appfile" | "Matchfile"
        | "Vagrantfile" | "Brewfile" | "Dangerfile" | "Guardfile" | "Berksfile" => {
            return Some(LangId::Ruby);
        }
        ".bashrc" | ".bash_profile" | ".zshrc" | ".zprofile" | ".zshenv" | ".profile"
        | "PKGBUILD" => return Some(LangId::Bash),
        "Cargo.lock" | "Pipfile" | "poetry.lock" | "uv.lock" => return Some(LangId::Toml),
        ".babelrc" | ".eslintrc" | ".prettierrc" | ".swcrc" => return Some(LangId::Json),
        _ => {}
    }
    let ext = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => return detect_shebang(head),
    };
    Some(match ext.as_str() {
        "rs" => LangId::Rust,
        "py" | "pyi" | "pyw" => LangId::Python,
        "js" | "mjs" | "cjs" | "jsx" => LangId::JavaScript,
        "ts" | "mts" | "cts" => LangId::TypeScript,
        "tsx" => LangId::Tsx,
        "go" => LangId::Go,
        "java" => LangId::Java,
        "c" => LangId::C,
        "h" => {
            if looks_like_objc(head) {
                LangId::ObjC
            } else {
                LangId::Cpp
            }
        }
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "tpp" | "inl"
        | "cu" | "cuh" | "metal" => LangId::Cpp,
        "m" | "mm" => LangId::ObjC,
        "cs" => LangId::CSharp,
        "rb" | "rake" | "gemspec" | "ru" | "podspec" => LangId::Ruby,
        "php" | "phtml" => LangId::Php,
        "sh" | "bash" | "zsh" | "ksh" | "command" => LangId::Bash,
        "swift" => LangId::Swift,
        "kt" | "kts" => LangId::Kotlin,
        "scala" | "sc" | "sbt" => LangId::Scala,
        "lua" => {
            if looks_like_luau(head) {
                LangId::Luau
            } else {
                LangId::Lua
            }
        }
        "luau" => LangId::Luau,
        "md" | "markdown" | "mdx" | "mdc" => LangId::Markdown,
        "json" | "jsonc" | "json5" | "geojson" | "webmanifest" | "code-workspace" => LangId::Json,
        "yml" | "yaml" => LangId::Yaml,
        "toml" => LangId::Toml,
        _ => return None,
    })
}

fn looks_like_objc(head: &[u8]) -> bool {
    let n = head.len().min(16 * 1024);
    let h = &head[..n];
    memchr::memmem::find(h, b"@interface").is_some()
        || memchr::memmem::find(h, b"#import").is_some()
        || memchr::memmem::find(h, b"@protocol").is_some()
        || memchr::memmem::find(h, b"NS_ASSUME_NONNULL").is_some()
}

/// Luau saved as `.lua` (common in Roblox projects): a `--!strict`-style
/// directive, a type alias, or a type annotation that plain Lua cannot parse.
fn looks_like_luau(head: &[u8]) -> bool {
    fn ident_len(b: &[u8]) -> usize {
        b.iter()
            .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
            .count()
    }
    fn skip_ws(b: &[u8]) -> &[u8] {
        let n = b.iter().take_while(|c| **c == b' ' || **c == b'\t').count();
        &b[n..]
    }
    let h = &head[..head.len().min(16 * 1024)];
    h.split(|&b| b == b'\n').any(|line| {
        let t = skip_ws(line);
        if let Some(d) = t.strip_prefix(b"--!") {
            return ["strict", "nonstrict", "nocheck", "native", "optimize"]
                .iter()
                .any(|w| d.starts_with(w.as_bytes()));
        }
        let t = t.strip_prefix(b"export ").map_or(t, skip_ws);
        // `type Name =` / `type Name<T> =`
        if let Some(r) = t.strip_prefix(b"type ") {
            let r = skip_ws(r);
            let n = ident_len(r);
            return n > 0 && matches!(skip_ws(&r[n..]).first(), Some(b'=' | b'<'));
        }
        // `local name: Type`
        if let Some(r) = t.strip_prefix(b"local ") {
            let r = skip_ws(r);
            let n = ident_len(r);
            if n > 0 && &r[..n] != b"function" {
                let rest = skip_ws(&r[n..]);
                return rest.first() == Some(&b':') && rest.get(1) != Some(&b':');
            }
        }
        // `function name(a: Type)`
        let t = t.strip_prefix(b"local ").map_or(t, skip_ws);
        if t.starts_with(b"function ")
            && let Some(open) = memchr::memchr(b'(', t)
        {
            let params = &t[open + 1..];
            let close = memchr::memchr(b')', params).unwrap_or(params.len());
            return memchr::memchr(b':', &params[..close]).is_some();
        }
        false
    })
}

fn detect_shebang(head: &[u8]) -> Option<LangId> {
    if !head.starts_with(b"#!") {
        return None;
    }
    let end = memchr::memchr(b'\n', head).unwrap_or(head.len().min(256));
    let line = std::str::from_utf8(&head[..end]).ok()?;
    let interp = line
        .split_whitespace()
        .find(|w| !w.starts_with("#!") && !w.ends_with("/env") && !w.starts_with('-'))
        .or_else(|| line.trim_start_matches("#!").split_whitespace().next())?;
    let interp = interp.rsplit('/').next().unwrap_or(interp);
    let base = interp.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match base {
        "bash" | "sh" | "zsh" | "ksh" | "dash" => LangId::Bash,
        "python" => LangId::Python,
        "node" | "deno" | "bun" => LangId::JavaScript,
        "ruby" => LangId::Ruby,
        "php" => LangId::Php,
        "lua" | "luajit" => LangId::Lua,
        "luau" | "lune" => LangId::Luau,
        "swift" => LangId::Swift,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_extension_and_shebang() {
        assert_eq!(detect(Path::new("a/b.rs"), b""), Some(LangId::Rust));
        assert_eq!(detect(Path::new("x.TSX"), b""), Some(LangId::Tsx));
        assert_eq!(detect(Path::new("Podfile"), b""), Some(LangId::Ruby));
        assert_eq!(
            detect(Path::new("View.h"), b"#import <UIKit/UIKit.h>"),
            Some(LangId::ObjC)
        );
        assert_eq!(
            detect(Path::new("vec.h"), b"#pragma once"),
            Some(LangId::Cpp)
        );
        assert_eq!(
            detect(Path::new("run"), b"#!/usr/bin/env python3\n"),
            Some(LangId::Python)
        );
        assert_eq!(
            detect(Path::new("deploy"), b"#!/bin/zsh -e\n"),
            Some(LangId::Bash)
        );
        assert_eq!(detect(Path::new("init.luau"), b""), Some(LangId::Luau));
        assert_eq!(
            detect(Path::new("init.lua"), b"local M = {}\n"),
            Some(LangId::Lua)
        );
        assert_eq!(
            detect(Path::new("init.lua"), b"--!strict\nlocal M = {}\n"),
            Some(LangId::Luau)
        );
        assert_eq!(
            detect(Path::new("Types.lua"), b"export type Id = number\n"),
            Some(LangId::Luau)
        );
        assert_eq!(
            detect(Path::new("a.lua"), b"local function f(x: number)\nend\n"),
            Some(LangId::Luau)
        );
        assert_eq!(
            detect(Path::new("a.lua"), b"local n: number = 1\n"),
            Some(LangId::Luau)
        );
        // Plain Lua: method definitions, `type(...)` calls, a variable named `type`.
        assert_eq!(
            detect(
                Path::new("a.lua"),
                b"function A:b(c)\nend\nlocal t = type(x)\ntype = 1\nlocal s = a:b()\n"
            ),
            Some(LangId::Lua)
        );
        assert_eq!(
            detect(Path::new("run"), b"#!/usr/bin/env lune\n"),
            Some(LangId::Luau)
        );
        assert_eq!(detect(Path::new(".gitignore"), b""), None);
        assert_eq!(detect(Path::new("notes.txt"), b""), None);
    }
}
