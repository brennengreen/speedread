//! File loading.
//!
//! Files up to [`MAX_IN_MEMORY`] are read with a single `read(2)` into an owned
//! buffer (no mmap: a cached mapping of a file that another process truncates
//! in place — which editors and agents do constantly — would SIGBUS), indexed
//! with a NEON-accelerated `memchr` newline scan, hashed with xxh3 and cached
//! keyed by (size, mtime ns, inode, device). Larger files are streamed with a
//! sparse line index ([`BigFile`]) and never loaded whole.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::lang::{self, LangId};

/// Files larger than this are streamed instead of loaded.
pub const MAX_IN_MEMORY: u64 = 64 << 20;
/// Files larger than this are not parsed for outlines.
pub const MAX_PARSE: usize = 8 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub mtime_ns: i128,
    pub ino: u64,
    pub dev: u64,
}

impl Stamp {
    pub fn of(m: &std::fs::Metadata) -> Stamp {
        Stamp {
            len: m.len(),
            mtime_ns: m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
            ino: m.ino(),
            dev: m.dev(),
        }
    }
}

pub struct Source {
    pub path: PathBuf,
    /// Decoded content (UTF-8 BOM stripped, UTF-16 transcoded).
    pub data: Vec<u8>,
    /// Byte offset of the start of every line.
    pub lines: Vec<u32>,
    pub hash: u64,
    pub stamp: Stamp,
    pub binary: bool,
    pub bom: bool,
    pub utf16: bool,
    pub crlf: bool,
    /// Content is not valid UTF-8 (rendered lossily).
    pub lossy: bool,
    pub lang: Option<LangId>,
}

impl Source {
    pub fn load(path: &Path) -> io::Result<Source> {
        let f = File::open(path)?;
        let meta = f.metadata()?;
        let mut data = Vec::with_capacity(meta.len().min(MAX_IN_MEMORY) as usize + 1);
        // Bounded even if the file grows while it is being read.
        f.take(MAX_IN_MEMORY + 1).read_to_end(&mut data)?;
        Ok(Source::from_bytes(
            path.to_path_buf(),
            data,
            Stamp::of(&meta),
        ))
    }

    pub fn from_bytes(path: PathBuf, mut data: Vec<u8>, stamp: Stamp) -> Source {
        let mut bom = false;
        let mut utf16 = false;
        if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
            data.drain(..3);
            bom = true;
        } else if data.len() >= 2 && (data[..2] == [0xFF, 0xFE] || data[..2] == [0xFE, 0xFF]) {
            let le = data[0] == 0xFF;
            let units = data[2..].chunks_exact(2).map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            });
            let s: String = char::decode_utf16(units)
                .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            data = s.into_bytes();
            utf16 = true;
        }
        let head = &data[..data.len().min(8192)];
        let binary = memchr::memchr(0, head).is_some();
        let lossy = !binary && simdutf8::basic::from_utf8(&data).is_err();
        let crlf =
            !binary && memchr::memmem::find(&data[..data.len().min(65536)], b"\r\n").is_some();
        let lines = if binary {
            Vec::new()
        } else {
            line_starts(&data)
        };
        let hash = xxh3_64(&data);
        let lang = if binary {
            None
        } else {
            lang::detect(&path, head)
        };
        Source {
            path,
            data,
            lines,
            hash,
            stamp,
            binary,
            bom,
            utf16,
            crlf,
            lossy,
            lang,
        }
    }

    pub fn etag(&self) -> u32 {
        self.hash as u32
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Raw bytes of line `i` without its terminator.
    pub fn line(&self, i: usize) -> &[u8] {
        let s = self.lines[i] as usize;
        let mut e = self
            .lines
            .get(i + 1)
            .map(|&e| e as usize)
            .unwrap_or(self.data.len());
        if e > s && self.data[e - 1] == b'\n' {
            e -= 1;
        }
        if e > s && self.data[e - 1] == b'\r' {
            e -= 1;
        }
        &self.data[s..e]
    }

    pub fn line_str(&self, i: usize) -> Cow<'_, str> {
        String::from_utf8_lossy(self.line(i))
    }

    /// Byte length of lines `a..=b` including terminators.
    pub fn span_bytes(&self, a: usize, b: usize) -> usize {
        if self.lines.is_empty() || a > b {
            return 0;
        }
        let s = self.lines[a] as usize;
        let e = self
            .lines
            .get(b + 1)
            .map(|&e| e as usize)
            .unwrap_or(self.data.len());
        e - s
    }

    /// First byte offset of line `i` (or the end of the data).
    pub fn offset_of_line(&self, i: usize) -> usize {
        self.lines
            .get(i)
            .map(|&o| o as usize)
            .unwrap_or(self.data.len())
    }

    pub fn ends_with_newline(&self) -> bool {
        self.data.last() == Some(&b'\n')
    }

    pub fn is_minified_line(&self, i: usize) -> bool {
        self.line(i).len() > 2000
    }
}

pub fn line_starts(data: &[u8]) -> Vec<u32> {
    let mut v = Vec::with_capacity(data.len() / 40 + 1);
    if data.is_empty() {
        return v;
    }
    v.push(0);
    let n = data.len();
    for i in memchr::memchr_iter(b'\n', data) {
        if i + 1 < n {
            v.push((i + 1) as u32);
        }
    }
    v
}

/// Streaming access to files too large to load: a sparse line index built in
/// one pass, then seek + buffered reads for any line window.
pub struct BigFile {
    pub path: PathBuf,
    pub stamp: Stamp,
    pub len: u64,
    pub lines: u64,
    pub hash: u64,
    pub binary: bool,
    pub ends_nl: bool,
    index: Vec<u64>,
}

const STRIDE: u64 = 1024;
const MAX_LINE: usize = 64 * 1024;

impl BigFile {
    pub fn scan(path: &Path) -> io::Result<BigFile> {
        let mut f = File::open(path)?;
        let meta = f.metadata()?;
        let mut buf = vec![0u8; 1 << 20];
        let mut hasher = Xxh3::new();
        let mut offset = 0u64;
        let mut newlines = 0u64;
        let mut index = vec![0u64];
        let mut last = b'\n';
        let mut binary = false;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            if offset == 0 {
                binary = memchr::memchr(0, &chunk[..n.min(8192)]).is_some();
            }
            hasher.update(chunk);
            for i in memchr::memchr_iter(b'\n', chunk) {
                newlines += 1;
                if newlines.is_multiple_of(STRIDE) {
                    index.push(offset + i as u64 + 1);
                }
            }
            last = chunk[n - 1];
            offset += n as u64;
        }
        let lines = if offset == 0 {
            0
        } else if last == b'\n' {
            newlines
        } else {
            newlines + 1
        };
        Ok(BigFile {
            path: path.to_path_buf(),
            stamp: Stamp::of(&meta),
            len: offset,
            lines,
            hash: hasher.digest(),
            binary,
            ends_nl: last == b'\n',
            index,
        })
    }

    pub fn etag(&self) -> u32 {
        self.hash as u32
    }

    /// Lines `a..=b` (0-based), stopping once `max_bytes` have been collected.
    /// Lines longer than 64 KiB are cut (the flag is set).
    pub fn read_lines(&self, a: u64, b: u64, max_bytes: usize) -> io::Result<Vec<(Vec<u8>, bool)>> {
        let mut out = Vec::new();
        if a >= self.lines {
            return Ok(out);
        }
        let k = ((a / STRIDE) as usize).min(self.index.len() - 1);
        let mut cur = k as u64 * STRIDE;
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.index[k]))?;
        let mut r = BufReader::with_capacity(1 << 16, f);
        let mut total = 0usize;
        let mut line = Vec::new();
        loop {
            line.clear();
            let (consumed, cut) =
                read_line_capped(&mut r, &mut line, if cur >= a { MAX_LINE } else { 0 })?;
            if consumed == 0 {
                break;
            }
            if cur >= a {
                while matches!(line.last(), Some(b'\n' | b'\r')) {
                    line.pop();
                }
                total += line.len() + 1;
                out.push((line.clone(), cut));
                if cur >= b || total >= max_bytes {
                    break;
                }
            }
            cur += 1;
        }
        Ok(out)
    }

    /// Whether the first `len` bytes hash to `hash` (append-only detection).
    pub fn prefix_hash_matches(&self, len: u64, hash: u64) -> io::Result<bool> {
        if len > self.len {
            return Ok(false);
        }
        let mut f = File::open(&self.path)?;
        let mut hasher = Xxh3::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut left = len;
        while left > 0 {
            let want = (buf.len() as u64).min(left) as usize;
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                return Ok(false);
            }
            hasher.update(&buf[..n]);
            left -= n as u64;
        }
        Ok(hasher.digest() == hash)
    }
}

/// Read one line, keeping at most `cap` bytes (the rest of the line is
/// consumed and dropped). Returns (bytes consumed, was_cut).
fn read_line_capped<R: BufRead>(
    r: &mut R,
    out: &mut Vec<u8>,
    cap: usize,
) -> io::Result<(usize, bool)> {
    let mut consumed = 0;
    let mut cut = false;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok((consumed, cut));
        }
        let (take, done) = match memchr::memchr(b'\n', buf) {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        let room = cap.saturating_sub(out.len());
        if room >= take {
            out.extend_from_slice(&buf[..take]);
        } else {
            out.extend_from_slice(&buf[..room]);
            cut = cut || cap > 0;
        }
        r.consume(take);
        consumed += take;
        if done {
            return Ok((consumed, cut));
        }
    }
}

/// Identify common binary formats by magic number.
pub fn describe_binary(data: &[u8]) -> &'static str {
    let starts = |m: &[u8]| data.starts_with(m);
    if starts(b"\x89PNG\r\n\x1a\n") {
        "PNG image"
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        "JPEG image"
    } else if starts(b"GIF8") {
        "GIF image"
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "WebP image"
    } else if starts(b"%PDF") {
        "PDF document"
    } else if starts(b"PK\x03\x04") {
        "ZIP archive (or jar/docx/xlsx)"
    } else if starts(&[0x1F, 0x8B]) {
        "gzip data"
    } else if starts(b"SQLite format 3\0") {
        "SQLite database"
    } else if starts(&[0xCF, 0xFA, 0xED, 0xFE]) || starts(&[0xCE, 0xFA, 0xED, 0xFE]) {
        "Mach-O binary"
    } else if starts(&[0xCA, 0xFE, 0xBA, 0xBE]) {
        "Mach-O universal binary (or Java class)"
    } else if starts(b"\x7fELF") {
        "ELF binary"
    } else if starts(b"bplist00") {
        "binary property list"
    } else if starts(b"\0asm") {
        "WebAssembly module"
    } else if data.len() > 8 && &data[4..8] == b"ftyp" {
        "MP4/QuickTime media"
    } else if starts(b"wOFF") || starts(b"wOF2") {
        "web font"
    } else if starts(b"\x00\x01\x00\x00") || starts(b"OTTO") {
        "font file"
    } else {
        "binary data"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_and_lines() {
        let s = Source::from_bytes(
            "x.rs".into(),
            b"a\r\nbb\n\nccc".to_vec(),
            Stamp {
                len: 0,
                mtime_ns: 0,
                ino: 0,
                dev: 0,
            },
        );
        assert_eq!(s.line_count(), 4);
        assert_eq!(s.line(0), b"a");
        assert_eq!(s.line(1), b"bb");
        assert_eq!(s.line(2), b"");
        assert_eq!(s.line(3), b"ccc");
        assert!(s.crlf);
        assert_eq!(s.span_bytes(1, 2), 4);
        assert_eq!(line_starts(b"x\n"), vec![0]);
        assert_eq!(line_starts(b""), Vec::<u32>::new());
    }

    #[test]
    fn bom_and_utf16_and_binary() {
        let st = Stamp {
            len: 0,
            mtime_ns: 0,
            ino: 0,
            dev: 0,
        };
        let s = Source::from_bytes("a.txt".into(), b"\xEF\xBB\xBFhi\n".to_vec(), st);
        assert!(s.bom);
        assert_eq!(s.line(0), b"hi");
        let mut u = vec![0xFF, 0xFE];
        for c in "hé\n".encode_utf16() {
            u.extend_from_slice(&c.to_le_bytes());
        }
        let s = Source::from_bytes("a.txt".into(), u, st);
        assert!(s.utf16 && !s.binary);
        assert_eq!(s.line_str(0), "hé");
        let s = Source::from_bytes("a.bin".into(), b"\x89PNG\r\n\x1a\n\0\0".to_vec(), st);
        assert!(s.binary);
        assert_eq!(describe_binary(&s.data), "PNG image");
    }

    #[test]
    fn big_file_streaming() {
        let dir = std::env::temp_dir().join(format!("speedread-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("log.txt");
        let mut s = String::new();
        for i in 0..5000 {
            s.push_str(&format!("line {i}\n"));
        }
        std::fs::write(&p, &s).unwrap();
        let b = BigFile::scan(&p).unwrap();
        assert_eq!(b.lines, 5000);
        let got = b.read_lines(2047, 2049, usize::MAX).unwrap();
        let got: Vec<String> = got
            .into_iter()
            .map(|(l, _)| String::from_utf8(l).unwrap())
            .collect();
        assert_eq!(got, vec!["line 2047", "line 2048", "line 2049"]);
        let prefix = "line 0\nline 1\n";
        assert!(
            b.prefix_hash_matches(prefix.len() as u64, xxh3_64(prefix.as_bytes()))
                .unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
