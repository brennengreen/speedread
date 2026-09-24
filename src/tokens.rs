//! Content-aware token estimate.
//!
//! A fixed bytes-per-token ratio is fine for ordinary code but undercounts
//! dense content badly: SVG path data, JSON, base64, hex or numeric tables
//! run 1.4–1.9× over (evals/budget_eval.py). Instead, tokens are modelled as
//! a linear function of character classes — words, camelCase breaks, digit
//! runs, punctuation, whitespace runs, newlines, non-ASCII characters and long
//! mixed alphanumeric runs (hashes, base64) — fitted by
//! evals/fit_estimator.py to the larger of the o200k_base and Claude
//! (legacy) token counts, then scaled so 99.5% of samples are not
//! undercounted. On the fitting corpus the undercount rate drops from 22% to
//! 0.5% (worst case 1.85× → 1.07×) at the same average headroom.

const W_DIGITS: f32 = 0.2309;
const W_PUNCT: f32 = 0.5142;
const W_WS_RUNS: f32 = 0.1415;
const W_NEWLINES: f32 = 3.1389;
const W_NON_ASCII: f32 = 1.8830;
const W_WORDS: f32 = 1.1830;
const W_DIGIT_RUNS: f32 = 1.3637;
const W_CASE_BREAKS: f32 = 1.3540;
const W_DENSE_RUNS: f32 = 0.1716;

/// Streaming counter: feeding chunks in order equals feeding the whole text.
#[derive(Default, Clone)]
pub struct Counter {
    digits: u32,
    punct: u32,
    ws_runs: u32,
    newlines: u32,
    non_ascii: u32,
    words: u32,
    digit_runs: u32,
    case_breaks: u32,
    dense: u32,
    prev: u8,
    run_len: u32,
    run_alpha: bool,
    run_digit: bool,
}

const OTHER: u8 = 0;
const LOWER: u8 = 1;
const UPPER: u8 = 2;
const DIGIT: u8 = 3;
const SPACE: u8 = 4;

impl Counter {
    pub fn feed(&mut self, b: &[u8]) {
        for &c in b {
            let lower = c.is_ascii_lowercase();
            let upper = c.is_ascii_uppercase();
            let digit = c.is_ascii_digit();
            if lower || upper || digit || matches!(c, b'+' | b'/' | b'=' | b'_' | b'-') {
                self.run_len += 1;
                self.run_alpha |= lower || upper;
                self.run_digit |= digit;
            } else {
                self.end_run();
            }
            if lower || upper {
                if self.prev != LOWER && self.prev != UPPER {
                    self.words += 1;
                } else if upper && self.prev == LOWER {
                    self.case_breaks += 1;
                }
                self.prev = if lower { LOWER } else { UPPER };
            } else if digit {
                self.digits += 1;
                if self.prev != DIGIT {
                    self.digit_runs += 1;
                }
                self.prev = DIGIT;
            } else if c == b'\n' {
                self.newlines += 1;
                self.prev = OTHER;
            } else if matches!(c, b' ' | b'\t' | b'\r') {
                if self.prev != SPACE {
                    self.ws_runs += 1;
                }
                self.prev = SPACE;
            } else if c >= 0xC0 {
                self.non_ascii += 1;
                self.prev = OTHER;
            } else {
                if c < 0x80 {
                    self.punct += 1;
                }
                self.prev = OTHER;
            }
        }
    }

    fn end_run(&mut self) {
        if self.run_len >= 16 && self.run_alpha && self.run_digit {
            self.dense += self.run_len;
        }
        self.run_len = 0;
        self.run_alpha = false;
        self.run_digit = false;
    }

    pub fn tokens(&self) -> f32 {
        let mut c = self.clone();
        c.end_run();
        c.digits as f32 * W_DIGITS
            + c.punct as f32 * W_PUNCT
            + c.ws_runs as f32 * W_WS_RUNS
            + c.newlines as f32 * W_NEWLINES
            + c.non_ascii as f32 * W_NON_ASCII
            + c.words as f32 * W_WORDS
            + c.digit_runs as f32 * W_DIGIT_RUNS
            + c.case_breaks as f32 * W_CASE_BREAKS
            + c.dense as f32 * W_DENSE_RUNS
    }
}

/// Estimated tokens of `text`.
pub fn estimate(text: &[u8]) -> f32 {
    let mut c = Counter::default();
    c.feed(text);
    c.tokens()
}

/// Tokens a `N<TAB>` line-number prefix adds to a line of a file with
/// `digits`-wide line numbers.
pub fn line_number_tokens(digits: f32) -> f32 {
    digits * W_DIGITS + W_DIGIT_RUNS + W_WS_RUNS * 0.5
}

/// Cut `out` at a line boundary so its estimate stays within `limit` tokens
/// (`scale` converts estimates to the client's tokenizer). Returns whether
/// anything was cut.
pub fn fit_text(out: &mut String, limit: usize, scale: f32) -> bool {
    if estimate(out.as_bytes()) * scale <= limit as f32 {
        return false;
    }
    let reserve = 40.0;
    let mut c = Counter::default();
    let mut cut = 0;
    let bytes = out.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        let end = memchr::memchr(b'\n', &bytes[start..]).map_or(bytes.len(), |i| start + i + 1);
        c.feed(&bytes[start..end]);
        if c.tokens() * scale > limit as f32 - reserve {
            break;
        }
        cut = end;
        start = end;
    }
    out.truncate(cut);
    out.push_str(
        "⋯ truncated: dense content reached the token budget; narrow the target or raise budget.\n",
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_equals_whole() {
        let text =
            b"fn parseHeader(x: u32) -> Result<()> {\n    let h = \"a9f3c2e1b7d4f6a8c0e2\";\n}\n";
        let mut c = Counter::default();
        for chunk in text.chunks(7) {
            c.feed(chunk);
        }
        assert!((c.tokens() - estimate(text)).abs() < 1e-3);
    }

    #[test]
    fn dense_content_costs_more_per_byte() {
        let code = "    let value = compute_total(items, options);\n".repeat(50);
        let b64 = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0\n"
            .repeat(50);
        let per_byte = |s: &str| estimate(s.as_bytes()) / s.len() as f32;
        assert!(per_byte(&b64) > per_byte(&code) * 1.3);
        let svg = "<path d=\"M12.5 3.25L4.75 11.5 12.5 19.75 13.56 18.69 6.87 12h13.38v-1.5H6.87l6.69-6.69z\"/>\n".repeat(40);
        assert!(per_byte(&svg) > 1.0 / 2.6);
    }

    #[test]
    fn fit_text_cuts_at_lines() {
        let mut s = "0123456789 abcdef 42 99 1.5e3\n".repeat(400);
        assert!(fit_text(&mut s, 500, 1.0));
        assert!(estimate(s.as_bytes()) <= 520.0);
        assert!(s.ends_with("raise budget.\n"));
        let mut small = String::from("hello world\n");
        assert!(!fit_text(&mut small, 500, 1.0));
    }
}
