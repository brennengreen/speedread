//! Shared engine state: workspace, configuration and caches.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::lang::LangId;
use crate::outline::{self, Outline};
use crate::source::{BigFile, MAX_IN_MEMORY, MAX_PARSE, Source, Stamp};
use crate::walk::{self, WalkOpts};
use crate::workspace::Workspace;

pub struct Config {
    pub default_budget: usize,
    pub max_budget: usize,
    /// Conservative bytes-per-token for budget estimates. Claude 4.7+
    /// tokenizers emit ~30% more tokens than older ones; line-numbered code
    /// measures ~3.3 bytes/token on the legacy Claude tokenizer and ~3.9 on
    /// o200k, so 2.6 keeps estimates at or above real counts.
    pub bytes_per_token: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_budget: 8000,
            // ~26 KB: inline in every client (Copilot CLI spills >30 KB to a
            // file; Claude Code warns above 10k tokens).
            max_budget: 10000,
            bytes_per_token: 2.6,
        }
    }
}

/// Minimal LRU keyed map with a cost ceiling.
pub struct Lru<K, V> {
    map: HashMap<K, (V, u64, usize)>,
    order: BTreeMap<u64, K>,
    tick: u64,
    cost: usize,
    cap: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Lru {
            map: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            cost: 0,
            cap,
        }
    }

    pub fn get(&mut self, k: &K) -> Option<V> {
        let (v, t, _) = self.map.get_mut(k)?;
        self.order.remove(t);
        self.tick += 1;
        *t = self.tick;
        self.order.insert(self.tick, k.clone());
        Some(v.clone())
    }

    pub fn insert(&mut self, k: K, v: V, cost: usize) {
        if let Some((_, t, c)) = self.map.remove(&k) {
            self.order.remove(&t);
            self.cost -= c;
        }
        self.tick += 1;
        self.order.insert(self.tick, k.clone());
        self.map.insert(k, (v, self.tick, cost));
        self.cost += cost;
        while self.cost > self.cap && self.map.len() > 1 {
            let Some((&t, _)) = self.order.iter().next() else {
                break;
            };
            let key = self.order.remove(&t).unwrap();
            if let Some((_, _, c)) = self.map.remove(&key) {
                self.cost -= c;
            }
        }
    }
}

#[derive(Clone)]
pub enum Loaded {
    Text(Arc<Source>),
    Big(Arc<BigFile>),
}

/// Snapshot of a streamed file for append detection.
#[derive(Clone, Copy)]
pub struct BigSnap {
    pub len: u64,
    pub hash: u64,
    pub lines: u64,
    pub ends_nl: bool,
}

/// Cached relative file list of the primary root: (taken at, root, paths).
type FileListCache = Option<(Instant, PathBuf, Arc<Vec<String>>)>;

pub struct Engine {
    pub ws: Workspace,
    pub cfg: Config,
    bpt: AtomicU32,
    sources: Mutex<Lru<PathBuf, Arc<Source>>>,
    outlines: Mutex<Lru<(u64, LangId), Arc<Outline>>>,
    snapshots: Mutex<Lru<u32, Arc<Source>>>,
    bigs: Mutex<Lru<PathBuf, Arc<BigFile>>>,
    big_snaps: Mutex<Lru<u32, BigSnap>>,
    files: Mutex<FileListCache>,
}

impl Engine {
    pub fn new(ws: Workspace, cfg: Config) -> Engine {
        Engine {
            ws,
            bpt: AtomicU32::new(cfg.bytes_per_token.to_bits()),
            cfg,
            sources: Mutex::new(Lru::new(512 << 20)),
            outlines: Mutex::new(Lru::new(8192)),
            snapshots: Mutex::new(Lru::new(256 << 20)),
            bigs: Mutex::new(Lru::new(64)),
            big_snaps: Mutex::new(Lru::new(1024)),
            files: Mutex::new(None),
        }
    }

    fn bpt(&self) -> f32 {
        f32::from_bits(self.bpt.load(Ordering::Relaxed))
    }

    pub fn set_bytes_per_token(&self, v: f32) {
        self.bpt.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn tokens(&self, bytes: usize) -> usize {
        (bytes as f32 / self.bpt()).ceil() as usize
    }

    pub fn bytes_for(&self, tokens: usize) -> usize {
        (tokens as f32 * self.bpt()) as usize
    }

    pub fn budget(&self, requested: Option<usize>) -> usize {
        requested
            .unwrap_or(self.cfg.default_budget)
            .clamp(200, self.cfg.max_budget.max(200))
    }

    /// Load a file through the stamp-validated cache.
    pub fn load(&self, path: &Path) -> io::Result<Loaded> {
        let meta = std::fs::metadata(path)?;
        if meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "is a directory",
            ));
        }
        if !meta.is_file() {
            // FIFOs, sockets and devices would block or never end.
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file (pipe, socket or device)",
            ));
        }
        let stamp = Stamp::of(&meta);
        if meta.len() > MAX_IN_MEMORY {
            if let Some(b) = self.bigs.lock().get(&path.to_path_buf())
                && b.stamp == stamp
            {
                return Ok(Loaded::Big(b));
            }
            let b = Arc::new(BigFile::scan(path)?);
            self.bigs.lock().insert(path.to_path_buf(), b.clone(), 1);
            return Ok(Loaded::Big(b));
        }
        if let Some(s) = self.sources.lock().get(&path.to_path_buf())
            && s.stamp == stamp
        {
            return Ok(Loaded::Text(s));
        }
        let s = Arc::new(Source::load(path)?);
        let cost = s.data.len() + s.lines.len() * 4 + 256;
        self.sources
            .lock()
            .insert(path.to_path_buf(), s.clone(), cost);
        Ok(Loaded::Text(s))
    }

    pub fn load_text(&self, path: &Path) -> io::Result<Arc<Source>> {
        match self.load(path)? {
            Loaded::Text(s) => Ok(s),
            Loaded::Big(_) => Err(io::Error::other("file too large to load")),
        }
    }

    /// Outline for a source (cached by content hash).
    pub fn outline(&self, src: &Source) -> Option<Arc<Outline>> {
        let lang = src.lang?;
        if src.binary || src.data.len() > MAX_PARSE {
            return None;
        }
        let key = (src.hash, lang);
        if let Some(o) = self.outlines.lock().get(&key) {
            return Some(o);
        }
        // A parser bug on one odd file must not fail a whole batch.
        let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            outline::parse_outline(lang, &src.data, &src.lines)
        }))
        .ok()
        .flatten()?;
        let o = Arc::new(parsed);
        self.outlines.lock().insert(key, o.clone(), 1);
        Some(o)
    }

    /// Remember content returned to the agent so `path@etag` can diff later.
    pub fn remember(&self, src: &Arc<Source>) {
        let cost = src.data.len() + 64;
        self.snapshots.lock().insert(src.etag(), src.clone(), cost);
    }

    pub fn snapshot(&self, etag: u32) -> Option<Arc<Source>> {
        self.snapshots.lock().get(&etag)
    }

    pub fn remember_big(&self, b: &BigFile) {
        let snap = BigSnap {
            len: b.len,
            hash: b.hash,
            lines: b.lines,
            ends_nl: b.ends_nl,
        };
        self.big_snaps.lock().insert(b.etag(), snap, 1);
    }

    pub fn big_snapshot(&self, etag: u32) -> Option<BigSnap> {
        self.big_snaps.lock().get(&etag)
    }

    /// Relative paths of all files in the primary root (cached briefly).
    pub fn file_list(&self) -> Arc<Vec<String>> {
        let root = self.ws.primary().to_path_buf();
        {
            let g = self.files.lock();
            if let Some((t, r, v)) = g.as_ref()
                && *r == root
                && t.elapsed() < Duration::from_secs(3)
            {
                return v.clone();
            }
        }
        let list = walk::list_files(&root, &WalkOpts::default(), 250_000)
            .map(|(v, _)| {
                v.into_iter()
                    .filter_map(|e| {
                        e.path
                            .strip_prefix(&root)
                            .ok()
                            .map(|p| p.to_string_lossy().into_owned())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let list = Arc::new(list);
        *self.files.lock() = Some((Instant::now(), root, list.clone()));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_oldest_by_cost() {
        let mut l: Lru<u32, u32> = Lru::new(10);
        l.insert(1, 1, 4);
        l.insert(2, 2, 4);
        assert_eq!(l.get(&1), Some(1));
        l.insert(3, 3, 4);
        assert_eq!(l.get(&2), None);
        assert_eq!(l.get(&1), Some(1));
        assert_eq!(l.get(&3), Some(3));
    }
}
