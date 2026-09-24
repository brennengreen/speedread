//! Workspace roots, path resolution and display.

use std::path::{Component, Path, PathBuf};

use parking_lot::RwLock;

#[derive(Debug)]
pub enum PathError {
    NotFound(PathBuf),
    Outside(PathBuf),
}

pub struct Workspace {
    /// roots[0] is the primary root (relative paths resolve against it).
    roots: RwLock<Vec<PathBuf>>,
    /// Roots came from `--root` / `$CLAUDE_PROJECT_DIR`, not the cwd default.
    explicit: bool,
    /// The cwd default was `/` (typical for GUI-launched servers): it grants
    /// nothing until the client reports roots or `--root` is given.
    implicit_slash: bool,
    /// Read-only dependency caches (cargo registry, SwiftPM checkouts, SDKs…).
    deps: Vec<PathBuf>,
    unrestricted: bool,
    home: Option<PathBuf>,
}

impl Workspace {
    pub fn new(
        roots: Vec<PathBuf>,
        unrestricted: bool,
        dep_roots: bool,
    ) -> anyhow::Result<Workspace> {
        let explicit = !roots.is_empty();
        let mut canon = Vec::new();
        for r in roots {
            let c = r
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("root {}: {e}", r.display()))?;
            if !canon.contains(&c) {
                canon.push(c);
            }
        }
        if canon.is_empty() {
            canon.push(std::env::current_dir()?.canonicalize()?);
        }
        let implicit_slash = !explicit && canon[0] == Path::new("/");
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut deps = Vec::new();
        if dep_roots {
            let mut cands: Vec<PathBuf> = Vec::new();
            if let Some(h) = &home {
                for rel in [
                    ".cargo/registry/src",
                    ".cargo/git/checkouts",
                    ".rustup/toolchains",
                    "go/pkg/mod",
                    "Library/Developer/Xcode/DerivedData",
                ] {
                    cands.push(h.join(rel));
                }
            }
            for abs in [
                "/Library/Developer/CommandLineTools/SDKs",
                "/Applications/Xcode.app/Contents/Developer/Platforms",
                "/opt/homebrew/opt/go/libexec/src",
                "/usr/local/go/src",
            ] {
                cands.push(PathBuf::from(abs));
            }
            for c in cands {
                if let Ok(c) = c.canonicalize() {
                    deps.push(c);
                }
            }
        }
        Ok(Workspace {
            roots: RwLock::new(canon),
            explicit,
            implicit_slash,
            deps,
            unrestricted,
            home,
        })
    }

    pub fn primary(&self) -> PathBuf {
        self.roots.read()[0].clone()
    }

    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.read().clone()
    }

    /// No usable root yet: started in `/` without `--root` or client roots.
    pub fn rootless(&self) -> bool {
        self.implicit_slash && self.roots.read().iter().all(|r| r == Path::new("/"))
    }

    /// Apply workspace folders reported by the client (MCP `roots/list`):
    /// they replace the cwd default, or extend explicitly configured roots.
    pub fn set_client_roots(&self, client: Vec<PathBuf>) {
        let client: Vec<PathBuf> = client
            .into_iter()
            .filter_map(|p| p.canonicalize().ok())
            .collect();
        if client.is_empty() {
            return;
        }
        let mut roots = self.roots.write();
        if !self.explicit {
            roots.clear();
        }
        for c in client {
            if !roots.contains(&c) {
                roots.push(c);
            }
        }
    }

    /// Normalize what a model typed: quotes, backticks, `file://`, `~`.
    pub fn clean(raw: &str) -> String {
        let mut s = raw
            .trim()
            .trim_matches(|c| c == '`' || c == '"' || c == '\'')
            .trim();
        if let Some(rest) = s.strip_prefix("file://") {
            s = rest;
        }
        s.to_string()
    }

    /// Absolute, lexically normalized path (no filesystem access).
    pub fn join(&self, raw: &str) -> PathBuf {
        let raw = Self::clean(raw);
        let p = if raw == "~" {
            self.home.clone().unwrap_or_else(|| PathBuf::from("/"))
        } else if let Some(rest) = raw.strip_prefix("~/") {
            self.home
                .clone()
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(rest)
        } else {
            let p = PathBuf::from(&raw);
            if p.is_absolute() {
                p
            } else {
                self.primary().join(p)
            }
        };
        normalize(&p)
    }

    pub fn resolve(&self, raw: &str) -> Result<PathBuf, PathError> {
        let lexical = self.join(raw);
        let found = lexical.canonicalize().ok().or_else(|| {
            // A relative path might belong to a secondary root.
            let clean = Self::clean(raw);
            if Path::new(&clean).is_absolute() || clean.starts_with('~') {
                return None;
            }
            let roots = self.roots.read();
            roots[1..]
                .iter()
                .find_map(|r| normalize(&r.join(&clean)).canonicalize().ok())
        });
        match found {
            Some(c) if self.allowed(&c) => Ok(c),
            Some(c) => Err(PathError::Outside(c)),
            None => Err(PathError::NotFound(lexical)),
        }
    }

    pub fn allowed(&self, canon: &Path) -> bool {
        self.unrestricted
            || self
                .roots
                .read()
                .iter()
                .any(|r| !(self.implicit_slash && r == Path::new("/")) && canon.starts_with(r))
            || self.deps.iter().any(|r| canon.starts_with(r))
    }

    /// Shortest unambiguous display form: root-relative, `~/…`, or absolute.
    pub fn display(&self, p: &Path) -> String {
        if let Ok(rel) = p.strip_prefix(self.primary()) {
            let s = rel.to_string_lossy();
            return if s.is_empty() {
                ".".to_string()
            } else {
                s.into_owned()
            };
        }
        if let Some(h) = &self.home
            && let Ok(rel) = p.strip_prefix(h)
        {
            return format!("~/{}", rel.to_string_lossy());
        }
        p.to_string_lossy().into_owned()
    }
}

pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `file:///Users/me/My%20App` → `/Users/me/My App`.
pub fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

/// Rank candidate files for a path that does not exist.
pub fn suggest(wanted: &str, files: &[String], limit: usize) -> Vec<String> {
    let wanted = wanted.trim_start_matches("./").to_ascii_lowercase();
    let wname = wanted.rsplit('/').next().unwrap_or(&wanted).to_string();
    let wstem = wname.split('.').next().unwrap_or(&wname).to_string();
    let wdirs: Vec<&str> = wanted.split('/').collect();
    let mut scored: Vec<(i64, &String)> = Vec::new();
    for f in files {
        let fl = f.to_ascii_lowercase();
        let name = fl.rsplit('/').next().unwrap_or(&fl);
        let stem = name.split('.').next().unwrap_or(name);
        let mut score: i64 = if name == wname {
            1000
        } else if stem == wstem && !wstem.is_empty() {
            600
        } else if wname.len() >= 4 {
            let d = crate::util::levenshtein(name, &wname);
            if d <= (wname.len() / 4).max(1) {
                400 - 50 * d as i64
            } else if name.contains(&wstem) && wstem.len() >= 4 {
                200
            } else {
                continue;
            }
        } else {
            continue;
        };
        // Reward shared directory components, penalize depth.
        let fdirs: Vec<&str> = fl.split('/').collect();
        score += 20 * wdirs.iter().filter(|d| fdirs.contains(d)).count() as i64;
        score -= fdirs.len() as i64;
        scored.push((score, f));
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, f)| f.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_prefer_same_name() {
        let files: Vec<String> = [
            "src/lib/utils.ts",
            "src/utils/index.ts",
            "src/util.ts",
            "test/utils.test.ts",
            "README.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let s = suggest("src/utils.ts", &files, 3);
        assert_eq!(s[0], "src/lib/utils.ts");
        assert!(s.contains(&"src/util.ts".to_string()));
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(
            file_uri_to_path("file:///Users/me/My%20App"),
            Some(PathBuf::from("/Users/me/My App"))
        );
        assert_eq!(file_uri_to_path("/nope"), None);
    }
}
