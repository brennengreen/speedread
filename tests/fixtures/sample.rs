//! Module docs.
use std::io;

/// A parser.
#[derive(Debug, Clone)]
pub struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

pub enum Token {
    Ident(String),
    Num(i64),
}

pub trait Visitor {
    fn visit(&mut self, t: &Token);
    fn done(&self) -> bool {
        true
    }
}

impl<'a> Parser<'a> {
    /// Create a parser.
    pub fn new(input: &'a [u8]) -> Self {
        Parser { input, pos: 0 }
    }

    pub fn parse_header(&mut self) -> io::Result<u32> {
        let a = self.input[0] as u32;
        let b = self.input[1] as u32;
        self.pos += 2;
        Ok(a << 8 | b)
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Ident(s) => write!(f, "{s}"),
            Token::Num(n) => write!(f, "{n}"),
        }
    }
}

pub const MAX: usize = 10;
type Result<T> = std::result::Result<T, io::Error>;

macro_rules! square {
    ($x:expr) => {
        $x * $x
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let mut p = Parser::new(&[1, 2]);
        assert_eq!(p.parse_header().unwrap(), 258);
        assert!(true);
    }
}
