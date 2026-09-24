//! Parallel, .gitignore-aware directory walking.
//!
//! On macOS this uses `getattrlistbulk(2)`, which returns name, type, size,
//! mtime, inode and flags for a whole batch of directory entries per syscall
//! instead of readdir + one lstat per entry (2.6–6.4× faster on APFS in
//! published measurements). Gitignore semantics come from the `ignore` crate's
//! matchers, but `.gitignore`/`.ignore` files are only opened when the
//! directory listing shows they exist (the `ignore` walker probes for them in
//! every directory). Dataless iCloud placeholders and firmlinks are skipped.
//! Other platforms fall back to `ignore::WalkParallel`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::overrides::{Override, OverrideBuilder};

/// Directory names that are never walked into unless they are the root.
pub const ALWAYS_SKIP: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    ".DS_Store",
    "node_modules",
    "__pycache__",
    ".venv",
    ".build",
    "DerivedData",
    ".gradle",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".next",
    ".turbo",
    ".parcel-cache",
];

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub depth: usize,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ns: i128,
    /// iCloud placeholder whose content is not on disk.
    pub dataless: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WalkOpts {
    /// ripgrep-style globs: `*.rs` includes, `!tests/**` excludes.
    pub globs: Vec<String>,
    pub max_depth: Option<usize>,
    pub include_dirs: bool,
}

pub fn build_overrides(root: &Path, globs: &[String]) -> anyhow::Result<Option<Override>> {
    if globs.is_empty() {
        return Ok(None);
    }
    let mut b = OverrideBuilder::new(root);
    for g in globs {
        let g = g.trim();
        if !g.is_empty() {
            b.add(g)
                .map_err(|e| anyhow::anyhow!("invalid glob {g:?}: {e}"))?;
        }
    }
    Ok(Some(b.build()?))
}

/// Walk `root` in parallel; `f` is called for every file (and directory when
/// `include_dirs`), from multiple threads. Returning `false` stops the walk.
pub fn walk<F>(root: &Path, opts: &WalkOpts, f: F) -> anyhow::Result<()>
where
    F: Fn(Entry) -> bool + Sync,
{
    walk_batches(root, opts, |batch| batch.into_iter().all(&f))
}

/// Like [`walk`], but `f` receives each directory's accepted entries as one
/// batch (one call — and one lock, for collectors — per directory).
pub fn walk_batches<F>(root: &Path, opts: &WalkOpts, f: F) -> anyhow::Result<()>
where
    F: Fn(Vec<Entry>) -> bool + Sync,
{
    let overrides = build_overrides(root, &opts.globs)?;
    #[cfg(target_os = "macos")]
    if !use_portable() {
        fast::walk(root, opts, overrides, &f);
        return Ok(());
    }
    portable::walk(root, opts, overrides, &|e| f(vec![e]));
    Ok(())
}

/// `SPEEDREAD_WALKER=portable` selects the `ignore`-crate walker (benchmarks,
/// or an escape hatch on unusual filesystems).
#[cfg(target_os = "macos")]
fn use_portable() -> bool {
    static PORTABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PORTABLE.get_or_init(|| std::env::var("SPEEDREAD_WALKER").is_ok_and(|v| v == "portable"))
}

struct IgnoreNode {
    parent: Option<Arc<IgnoreNode>>,
    /// Lowest precedence first.
    matchers: Vec<Gitignore>,
}

fn is_ignored(stack: &Option<Arc<IgnoreNode>>, path: &Path, is_dir: bool) -> bool {
    let mut cur = stack.as_ref();
    while let Some(node) = cur {
        for m in node.matchers.iter().rev() {
            match m.matched(path, is_dir) {
                Match::None => {}
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
            }
        }
        cur = node.parent.as_ref();
    }
    false
}

fn load_matcher(dir: &Path, files: &[&str]) -> Option<Gitignore> {
    let mut b = GitignoreBuilder::new(dir);
    let mut any = false;
    for f in files {
        let p = dir.join(f);
        if p.is_file() && b.add(&p).is_none() {
            any = true;
        }
    }
    if !any {
        return None;
    }
    b.build().ok().filter(|g| !g.is_empty())
}

/// Ignore matchers that apply above the walk root: the global excludes file,
/// then every `.gitignore` from the enclosing repository root down to (but
/// excluding) `root` itself.
/// The global excludes file (`core.excludesFile`), the base of every stack.
fn global_node() -> Option<Arc<IgnoreNode>> {
    let (global, _) = Gitignore::global();
    (!global.is_empty()).then(|| {
        Arc::new(IgnoreNode {
            parent: None,
            matchers: vec![global],
        })
    })
}

/// A repository root is an ignore boundary: rules from an enclosing
/// repository (a dotfiles repo in `~`, a monorepo around a nested checkout)
/// never apply inside it, exactly as in git.
fn initial_stack(root: &Path, global: Option<Arc<IgnoreNode>>) -> Option<Arc<IgnoreNode>> {
    let mut node = global;
    if root.join(".git").exists() {
        return node;
    }
    let mut ancestors: Vec<&Path> = Vec::new();
    let mut cur = root.parent();
    while let Some(p) = cur {
        ancestors.push(p);
        if p.join(".git").exists() {
            break;
        }
        cur = p.parent();
    }
    // Only use ancestors if we actually found an enclosing repository.
    if ancestors.last().is_some_and(|p| p.join(".git").exists()) {
        for dir in ancestors.into_iter().rev() {
            let mut matchers = Vec::new();
            if dir.join(".git").exists()
                && let Some(m) = load_matcher(dir, &[".git/info/exclude"])
            {
                matchers.push(m);
            }
            if let Some(m) = load_matcher(dir, &[".gitignore"]) {
                matchers.push(m);
            }
            if let Some(m) = load_matcher(dir, &[".ignore"]) {
                matchers.push(m);
            }
            if !matchers.is_empty() {
                node = Some(Arc::new(IgnoreNode {
                    parent: node,
                    matchers,
                }));
            }
        }
    }
    node
}

#[cfg(target_os = "macos")]
mod fast {
    use std::cell::RefCell;
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const SF_FIRMLINK: u32 = 0x0080_0000;
    const SF_DATALESS: u32 = 0x4000_0000;
    const BUF_SIZE: usize = 256 * 1024;
    /// Bytes of always-present fields before the optional file group.
    const FIXED: usize = 68;

    struct Raw {
        name: Vec<u8>,
        objtype: u32,
        flags: u32,
        mtime_ns: i128,
        size: u64,
    }

    thread_local! {
        static BUF: RefCell<Vec<u8>> = RefCell::new(vec![0u8; BUF_SIZE]);
    }

    fn rd_u32(b: &[u8], o: usize) -> u32 {
        u32::from_ne_bytes(b[o..o + 4].try_into().unwrap())
    }
    fn rd_i32(b: &[u8], o: usize) -> i32 {
        i32::from_ne_bytes(b[o..o + 4].try_into().unwrap())
    }
    fn rd_i64(b: &[u8], o: usize) -> i64 {
        i64::from_ne_bytes(b[o..o + 8].try_into().unwrap())
    }

    /// List a directory with getattrlistbulk. Record layout with
    /// FSOPT_PACK_INVAL_ATTRS (common attributes always present, 4-byte aligned):
    /// len u32 | returned attribute_set 20B | error u32 | name attrreference 8B |
    /// objtype u32 | modtime timespec 16B | flags u32 | fileid u64 | [datalength i64]
    /// The file group (datalength) is only packed for non-directories, so it is
    /// read only when the returned attribute set says it is present.
    fn list_dir(dir: &Path) -> std::io::Result<Vec<Raw>> {
        match list_dir_bulk(dir) {
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::ENOTSUP) | Some(libc::EINVAL) | Some(libc::ENOSYS)
                ) =>
            {
                list_dir_readdir(dir)
            }
            other => other,
        }
    }

    /// Fallback for filesystems without getattrlistbulk support.
    fn list_dir_readdir(dir: &Path) -> std::io::Result<Vec<Raw>> {
        use std::os::unix::fs::MetadataExt;
        let mut out = Vec::new();
        for ent in std::fs::read_dir(dir)? {
            let Ok(ent) = ent else { continue };
            let Ok(md) = ent.metadata() else { continue };
            let ft = md.file_type();
            out.push(Raw {
                name: ent.file_name().as_bytes().to_vec(),
                objtype: if ft.is_dir() {
                    VDIR
                } else if ft.is_file() {
                    VREG
                } else {
                    0
                },
                flags: 0,
                mtime_ns: md.mtime() as i128 * 1_000_000_000 + md.mtime_nsec() as i128,
                size: md.len(),
            });
        }
        Ok(out)
    }

    fn list_dir_bulk(dir: &Path) -> std::io::Result<Vec<Raw>> {
        let c = CString::new(dir.as_os_str().as_bytes())?;
        // SAFETY: valid NUL-terminated path.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: attrlist is a plain C struct; zero is a valid initial state.
        let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
        al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        al.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_MODTIME
            | libc::ATTR_CMN_FLAGS
            | libc::ATTR_CMN_FILEID;
        al.fileattr = libc::ATTR_FILE_DATALENGTH;
        let mut out = Vec::new();
        let res = BUF.with(|cell| {
            let mut buf = cell.borrow_mut();
            loop {
                // SAFETY: fd is an open directory, buffers are valid for their lengths.
                let n = unsafe {
                    libc::getattrlistbulk(
                        fd,
                        &mut al as *mut libc::attrlist as *mut libc::c_void,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                        libc::FSOPT_PACK_INVAL_ATTRS as u64,
                    )
                };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(e);
                }
                if n == 0 {
                    return Ok(());
                }
                let mut off = 0usize;
                for _ in 0..n {
                    if off + FIXED > buf.len() {
                        break;
                    }
                    let b = &buf[off..];
                    let len = rd_u32(b, 0) as usize;
                    if len < FIXED || off + len > buf.len() {
                        break;
                    }
                    let rec = &b[..len];
                    let err = rd_u32(rec, 24);
                    let name_off = rd_i32(rec, 28);
                    let name_len = rd_u32(rec, 32) as usize;
                    let name_start = (28i64 + name_off as i64) as usize;
                    off += len;
                    if err != 0 || name_len == 0 || name_start + name_len > len {
                        continue;
                    }
                    let name = &rec[name_start..name_start + name_len - 1];
                    if name == b"." || name == b".." {
                        continue;
                    }
                    let objtype = rd_u32(rec, 36);
                    let secs = rd_i64(rec, 40);
                    let nsecs = rd_i64(rec, 48);
                    let flags = rd_u32(rec, 56);
                    let returned_file = rd_u32(rec, 16);
                    let size =
                        if returned_file & libc::ATTR_FILE_DATALENGTH != 0 && len >= FIXED + 8 {
                            rd_i64(rec, FIXED).max(0) as u64
                        } else {
                            0
                        };
                    out.push(Raw {
                        name: name.to_vec(),
                        objtype,
                        flags,
                        mtime_ns: secs as i128 * 1_000_000_000 + nsecs as i128,
                        size,
                    });
                }
            }
        });
        // SAFETY: closing the fd we opened.
        unsafe { libc::close(fd) };
        res.map(|_| out)
    }

    struct Ctx<'a, F> {
        f: &'a F,
        overrides: Option<Override>,
        global: Option<Arc<IgnoreNode>>,
        stop: AtomicBool,
        max_depth: Option<usize>,
        include_dirs: bool,
    }

    pub(super) fn walk<F>(root: &Path, opts: &WalkOpts, overrides: Option<Override>, f: &F)
    where
        F: Fn(Vec<Entry>) -> bool + Sync,
    {
        let ctx = Ctx {
            f,
            overrides,
            global: global_node(),
            stop: AtomicBool::new(false),
            max_depth: opts.max_depth,
            include_dirs: opts.include_dirs,
        };
        let stack = initial_stack(root, ctx.global.clone());
        let ctx = &ctx;
        rayon::in_place_scope(|s| {
            s.spawn(move |s| process(s, ctx, root.to_path_buf(), 1, stack));
        });
    }

    fn process<'s, F>(
        s: &rayon::Scope<'s>,
        ctx: &'s Ctx<'s, F>,
        dir: PathBuf,
        depth: usize,
        parent: Option<Arc<IgnoreNode>>,
    ) where
        F: Fn(Vec<Entry>) -> bool + Sync,
    {
        if ctx.stop.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut entries) = list_dir(&dir) else {
            return;
        };
        let mut batch: Vec<Entry> = Vec::with_capacity(entries.len());
        // Deterministic order within a directory (APFS returns hash order).
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let has = |n: &[u8]| entries.iter().any(|e| e.name == n);
        let (git, gi, ig) = (has(b".git"), has(b".gitignore"), has(b".ignore"));
        // A nested repository starts a fresh stack (global excludes only).
        let parent = if git { ctx.global.clone() } else { parent };
        let stack = if git || gi || ig {
            let mut matchers = Vec::new();
            if git && let Some(m) = load_matcher(&dir, &[".git/info/exclude"]) {
                matchers.push(m);
            }
            if gi && let Some(m) = load_matcher(&dir, &[".gitignore"]) {
                matchers.push(m);
            }
            if ig && let Some(m) = load_matcher(&dir, &[".ignore"]) {
                matchers.push(m);
            }
            if matchers.is_empty() {
                parent
            } else {
                Some(Arc::new(IgnoreNode { parent, matchers }))
            }
        } else {
            parent
        };
        for e in entries {
            if ctx.stop.load(Ordering::Relaxed) {
                return;
            }
            let is_dir = e.objtype == VDIR;
            if !is_dir && e.objtype != VREG {
                continue;
            }
            let name = OsStr::from_bytes(&e.name);
            if ALWAYS_SKIP.iter().any(|s| name == OsStr::new(s)) {
                continue;
            }
            let path = dir.join(name);
            let decided = match &ctx.overrides {
                Some(ov) => match ov.matched(&path, is_dir) {
                    Match::Ignore(_) => continue,
                    Match::Whitelist(_) => true,
                    Match::None => false,
                },
                None => false,
            };
            if !decided && is_ignored(&stack, &path, is_dir) {
                continue;
            }
            let entry = Entry {
                path,
                depth,
                is_dir,
                size: e.size,
                mtime_ns: e.mtime_ns,
                dataless: e.flags & SF_DATALESS != 0,
            };
            if is_dir {
                if e.flags & SF_FIRMLINK != 0 {
                    continue;
                }
                if ctx.max_depth.is_none_or(|m| depth < m) {
                    let st = stack.clone();
                    let p = entry.path.clone();
                    s.spawn(move |s| process(s, ctx, p, depth + 1, st));
                }
                if ctx.include_dirs {
                    batch.push(entry);
                }
            } else {
                batch.push(entry);
            }
        }
        if !batch.is_empty() && !(ctx.f)(batch) {
            ctx.stop.store(true, Ordering::Relaxed);
        }
    }
}

mod portable {
    use std::os::unix::fs::MetadataExt;

    use ignore::{WalkBuilder, WalkState};

    use super::*;

    pub(super) fn walk<F>(root: &Path, opts: &WalkOpts, overrides: Option<Override>, f: &F)
    where
        F: Fn(Entry) -> bool + Sync,
    {
        let mut b = WalkBuilder::new(root);
        b.hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .ignore(true)
            // A root that is itself a repository ignores enclosing repos' rules.
            .parents(!root.join(".git").exists())
            .require_git(false)
            .follow_links(false)
            .max_depth(opts.max_depth)
            .filter_entry(|e| {
                !ALWAYS_SKIP
                    .iter()
                    .any(|s| e.file_name() == std::ffi::OsStr::new(s))
            });
        if let Some(ov) = overrides {
            b.overrides(ov);
        }
        let stop = AtomicBool::new(false);
        let include_dirs = opts.include_dirs;
        b.build_parallel().run(|| {
            let stop = &stop;
            Box::new(move |res| {
                if stop.load(Ordering::Relaxed) {
                    return ignore::WalkState::Quit;
                }
                let Ok(e) = res else {
                    return WalkState::Continue;
                };
                if e.depth() == 0 {
                    return WalkState::Continue;
                }
                let Some(ft) = e.file_type() else {
                    return WalkState::Continue;
                };
                let is_dir = ft.is_dir();
                if !is_dir && !ft.is_file() {
                    return WalkState::Continue;
                }
                if is_dir && !include_dirs {
                    return WalkState::Continue;
                }
                let md = e.metadata().ok();
                let entry = Entry {
                    path: e.path().to_path_buf(),
                    depth: e.depth(),
                    is_dir,
                    size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                    mtime_ns: md
                        .as_ref()
                        .map(|m| m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128)
                        .unwrap_or(0),
                    dataless: false,
                };
                if f(entry) {
                    WalkState::Continue
                } else {
                    stop.store(true, Ordering::Relaxed);
                    WalkState::Quit
                }
            })
        });
    }
}

/// Collect all files under `root` (sorted bytewise), up to `limit`.
pub fn list_files(
    root: &Path,
    opts: &WalkOpts,
    limit: usize,
) -> anyhow::Result<(Vec<Entry>, bool)> {
    let out = parking_lot::Mutex::new(Vec::new());
    let truncated = AtomicBool::new(false);
    walk_batches(root, opts, |batch| {
        let mut v = out.lock();
        let room = limit.saturating_sub(v.len());
        if batch.len() > room {
            v.extend(batch.into_iter().take(room));
            truncated.store(true, Ordering::Relaxed);
            return false;
        }
        v.extend(batch);
        true
    })?;
    let mut v = out.into_inner();
    sort_by_path(&mut v);
    Ok((v, truncated.load(Ordering::Relaxed)))
}

/// Bytewise path order: deterministic and far cheaper than `Path::cmp`,
/// which re-parses components on every comparison.
pub fn sort_by_path(v: &mut [Entry]) {
    use std::os::unix::ffi::OsStrExt;
    v.sort_unstable_by(|a, b| {
        a.path
            .as_os_str()
            .as_bytes()
            .cmp(b.path.as_os_str().as_bytes())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walks_respecting_gitignore_and_globs() {
        let dir = std::env::temp_dir().join(format!("speedread-walk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::create_dir_all(dir.join("target/debug")).unwrap();
        fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join(".gitignore"), "target/\n*.log\n!keep.log\n").unwrap();
        fs::write(dir.join("src/nested/.gitignore"), "secret.txt\n").unwrap();
        for f in [
            "src/a.rs",
            "src/nested/b.rs",
            "src/nested/secret.txt",
            "target/debug/out.rs",
            "node_modules/x/i.js",
            "debug.log",
            "keep.log",
            "README.md",
            ".github-like",
        ] {
            fs::write(dir.join(f), "x\n").unwrap();
        }
        let root = dir.canonicalize().unwrap();
        let (files, _) = list_files(&root, &WalkOpts::default(), 1000).unwrap();
        let rel: Vec<String> = files
            .iter()
            .map(|e| {
                e.path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            rel,
            vec![
                ".github-like",
                ".gitignore",
                "README.md",
                "keep.log",
                "src/a.rs",
                "src/nested/.gitignore",
                "src/nested/b.rs"
            ]
        );
        // A nested repository is not subject to the outer repo's rules.
        fs::create_dir_all(dir.join("inner/.git")).unwrap();
        fs::write(dir.join("inner/.gitignore"), "").unwrap();
        fs::write(dir.join("inner/app.log"), "x\n").unwrap();
        #[cfg(target_os = "macos")]
        if !use_portable() {
            let (files, _) = list_files(&root, &WalkOpts::default(), 1000).unwrap();
            assert!(files.iter().any(|e| e.path.ends_with("inner/app.log")));
        }
        let (files, _) = list_files(&root.join("inner"), &WalkOpts::default(), 1000).unwrap();
        assert!(files.iter().any(|e| e.path.ends_with("inner/app.log")));
        fs::remove_dir_all(dir.join("inner")).unwrap();
        let opts = WalkOpts {
            globs: vec!["*.rs".into()],
            ..Default::default()
        };
        let (files, _) = list_files(&root, &opts, 1000).unwrap();
        assert_eq!(files.len(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }
}
