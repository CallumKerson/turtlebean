//! Splits ledger text into tokens, grouped into logical lines.

use rust_decimal::Decimal;

use crate::date::Date;
use crate::number;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    Date(Date),
    String(String),
    Account(String),
    Currency(String),
    Number(Decimal),
    Tag(String),
    Link(String),
    Key(String),
    Bool(bool),
    Null,
    Keyword(String),
    Flag(char),
    Star,
    LParen,
    RParen,
    Plus,
    Minus,
    Slash,
    Comma,
    LCurl,
    RCurl,
    LCurlCurl,
    RCurlCurl,
    At,
    AtAt,
    Tilde,
    Hash,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub tok: Tok,
    pub line: usize,
}

/// One logical line: a physical line, or several if a string spans lines.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::struct_field_names,
    reason = "line numbers of a Line read naturally as line and end_line"
)]
pub(crate) struct Line {
    /// The first physical line, 1-based.
    pub line: usize,
    /// The physical line after this logical line ends.
    pub end_line: usize,
    /// Byte offsets of the whole logical line, including its newline.
    pub start: usize,
    pub end: usize,
    pub indented: bool,
    pub blank: bool,
    pub tokens: Vec<Token>,
    /// Lines of invalid tokens.
    pub errors: Vec<usize>,
    /// Lines of unterminated strings.
    pub unterminated: Option<usize>,
    /// The first thing on the line is an invalid token.
    pub starts_invalid: bool,
}

const KEYWORDS: &[&str] = &[
    "txn",
    "balance",
    "open",
    "close",
    "commodity",
    "pad",
    "event",
    "price",
    "note",
    "document",
    "query",
    "custom",
    "pushtag",
    "poptag",
    "pushmeta",
    "popmeta",
    "option",
    "include",
    "plugin",
];

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: usize,
    /// Sorted offsets of characters that replaced invalid UTF-8.
    replaced: &'a [usize],
    /// Set when a string contains a replaced character.
    hit_replaced: bool,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn at_line_end(&self) -> bool {
        matches!(self.peek(), None | Some(b'\n'))
            || (self.peek() == Some(b'\r') && matches!(self.peek_at(1), None | Some(b'\n')))
    }

    /// Moves past the rest of the physical line, including its newline.
    fn skip_line(&mut self) {
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'\n' {
                self.line += 1;
                return;
            }
        }
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.pos += 1;
        }
    }

    fn take_while(&mut self, keep: impl Fn(u8) -> bool) -> &str {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if !keep(b) {
                break;
            }
            self.pos += 1;
        }
        &self.text[start..self.pos]
    }

    fn current_char(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    /// An uppercase letter, which may start an account or a currency.
    fn starts_upper_word(&self) -> bool {
        self.current_char().is_some_and(char::is_uppercase)
    }

    /// Reads a word that may be an account or currency name.
    fn take_word(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.current_char() {
            if !(c.is_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | '\'')) {
                break;
            }
            self.pos += c.len_utf8();
        }
        self.text[start..self.pos].to_owned()
    }

    /// Skips a run of characters that aren't whitespace, for an invalid token.
    fn skip_invalid(&mut self) {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start && self.peek().is_some_and(|b| !b.is_ascii_whitespace()) {
            self.pos += 1;
        }
        // Don't stop part-way through a multi-byte character
        while !self.text.is_char_boundary(self.pos) {
            self.pos += 1;
        }
    }
}

fn is_tag_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'/' | b'.')
}

pub(crate) fn valid_account(text: &str) -> bool {
    let mut parts = text.split(':');
    let Some(root) = parts.next() else {
        return false;
    };
    let valid_part = |part: &str, first: fn(char) -> bool| {
        let mut chars = part.chars();
        chars.next().is_some_and(first) && chars.all(|c| c.is_alphanumeric() || c == '-')
    };
    let mut count = 0;
    let rest_valid = parts.all(|p| {
        count += 1;
        valid_part(p, |c| c.is_uppercase() || c.is_ascii_digit())
    });
    valid_part(root, char::is_uppercase) && count > 0 && rest_valid
}

fn valid_currency(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 24 || !bytes[0].is_ascii_uppercase() {
        return false;
    }
    let last = bytes[bytes.len() - 1];
    if bytes.len() > 1 && !(last.is_ascii_uppercase() || last.is_ascii_digit()) {
        return false;
    }
    bytes.iter().all(|b| {
        b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'\'' | b'.' | b'_' | b'-')
    })
}

impl Line {
    fn invalid(&mut self, line: usize) {
        if self.tokens.is_empty() && self.errors.is_empty() {
            self.starts_invalid = true;
        }
        self.errors.push(line);
    }
}

/// Splits text into logical lines of tokens. `replaced` holds the sorted
/// offsets of characters that stand in for invalid UTF-8: they're fine in
/// comments but make any token containing them invalid.
pub(crate) fn lex(text: &str, replaced: &[usize]) -> Vec<Line> {
    let mut lexer = Lexer {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        line: 1,
        replaced,
        hit_replaced: false,
    };
    let mut lines = Vec::new();
    while lexer.pos < text.len() {
        let start = lexer.pos;
        let first_line = lexer.line;
        let mut current = Line {
            line: first_line,
            start,
            ..Line::default()
        };
        match lexer.peek() {
            // Comments, and org-mode headings, at the start of a line
            Some(b';' | b'*' | b'#' | b'%') => {
                lexer.skip_line();
                continue;
            }
            Some(b' ' | b'\t') => {
                lexer.skip_spaces();
                if lexer.at_line_end() {
                    current.blank = true;
                } else if lexer.peek() == Some(b';') {
                    lexer.skip_line();
                    continue;
                } else {
                    current.indented = true;
                }
            }
            Some(b'\n' | b'\r') => current.blank = true,
            _ => {}
        }
        lex_line(&mut lexer, &mut current);
        current.end = lexer.pos;
        current.end_line = lexer.line;
        lines.push(current);
    }
    lines
}

/// Reads tokens up to and including the end of the logical line.
fn lex_line(lexer: &mut Lexer<'_>, current: &mut Line) {
    loop {
        lexer.skip_spaces();
        let line = lexer.line;
        let Some(b) = lexer.peek() else { return };
        let tok = match b {
            b'\n' => {
                lexer.pos += 1;
                lexer.line += 1;
                return;
            }
            b'\r' if lexer.peek_at(1) == Some(b'\n') => {
                lexer.pos += 2;
                lexer.line += 1;
                return;
            }
            b';' => {
                while !lexer.at_line_end() {
                    lexer.pos += 1;
                }
                continue;
            }
            b'"' => match lex_string(lexer) {
                Some(_) if std::mem::take(&mut lexer.hit_replaced) => {
                    current.invalid(line);
                    continue;
                }
                Some(s) => Tok::String(s),
                None => {
                    current.unterminated = Some(line);
                    return;
                }
            },
            b'0'..=b'9' => {
                if let Some(tok) = lex_number_or_date(lexer) {
                    tok
                } else {
                    current.invalid(line);
                    continue;
                }
            }
            _ if lexer.starts_upper_word() => {
                let Some(tok) = lex_upper_word(lexer) else {
                    current.invalid(line);
                    continue;
                };
                tok
            }
            b'a'..=b'z' => {
                let Some(tok) = lex_lower_word(lexer) else {
                    current.invalid(line);
                    continue;
                };
                tok
            }
            b'#' if lexer.peek_at(1).is_some_and(is_tag_byte) => {
                lexer.pos += 1;
                Tok::Tag(lexer.take_while(is_tag_byte).to_owned())
            }
            b'^' if lexer.peek_at(1).is_some_and(is_tag_byte) => {
                lexer.pos += 1;
                Tok::Link(lexer.take_while(is_tag_byte).to_owned())
            }
            _ => {
                let Some(tok) = lex_punctuation(lexer, b) else {
                    current.invalid(line);
                    continue;
                };
                tok
            }
        };
        current.tokens.push(Token { tok, line });
    }
}

/// Reads an account, currency or constant. Skips the word and returns `None`
/// if it's none of them.
fn lex_upper_word(lexer: &mut Lexer<'_>) -> Option<Tok> {
    let word = lexer.take_word();
    let tok = if word.contains(':') {
        valid_account(&word).then_some(Tok::Account(word))
    } else {
        match word.as_str() {
            "TRUE" => Some(Tok::Bool(true)),
            "FALSE" => Some(Tok::Bool(false)),
            "NULL" => Some(Tok::Null),
            _ => valid_currency(&word).then_some(Tok::Currency(word)),
        }
    };
    if tok.is_none() {
        lexer.skip_invalid();
    }
    tok
}

/// Reads a metadata key or a keyword. Skips the word and returns `None` if
/// it's neither.
fn lex_lower_word(lexer: &mut Lexer<'_>) -> Option<Tok> {
    let word = lexer
        .take_while(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        .to_owned();
    if lexer.peek() == Some(b':') {
        lexer.pos += 1;
        return Some(Tok::Key(word));
    }
    if KEYWORDS.contains(&word.as_str()) && lexer.peek().is_none_or(|b| b.is_ascii_whitespace()) {
        return Some(Tok::Keyword(word));
    }
    lexer.skip_invalid();
    None
}

/// Reads punctuation or a flag. Skips the character and returns `None` if
/// it's neither.
fn lex_punctuation(lexer: &mut Lexer<'_>, b: u8) -> Option<Tok> {
    let (tok, len) = match (b, lexer.peek_at(1)) {
        (b'{', Some(b'{')) => (Tok::LCurlCurl, 2),
        (b'}', Some(b'}')) => (Tok::RCurlCurl, 2),
        (b'@', Some(b'@')) => (Tok::AtAt, 2),
        (b'{', _) => (Tok::LCurl, 1),
        (b'}', _) => (Tok::RCurl, 1),
        (b'@', _) => (Tok::At, 1),
        (b'(', _) => (Tok::LParen, 1),
        (b')', _) => (Tok::RParen, 1),
        (b'+', _) => (Tok::Plus, 1),
        (b'-', _) => (Tok::Minus, 1),
        (b'*', _) => (Tok::Star, 1),
        (b'/', _) => (Tok::Slash, 1),
        (b',', _) => (Tok::Comma, 1),
        (b'~', _) => (Tok::Tilde, 1),
        (b'#', _) => (Tok::Hash, 1),
        (b'!' | b'&' | b'?' | b'%', _) => (Tok::Flag(char::from(b)), 1),
        _ => {
            lexer.skip_invalid();
            return None;
        }
    };
    lexer.pos += len;
    Some(tok)
}

/// Reads a quoted string, or returns `None` and moves to the end of the text
/// if it's never closed.
fn lex_string(lexer: &mut Lexer<'_>) -> Option<String> {
    lexer.pos += 1;
    let mut value = String::new();
    let mut chars = lexer.text[lexer.pos..].char_indices();
    while let Some((offset, c)) = chars.next() {
        match c {
            '"' => {
                lexer.pos += offset + 1;
                return Some(value);
            }
            '\\' => match chars.next() {
                Some((_, 'n')) => value.push('\n'),
                Some((_, 't')) => value.push('\t'),
                Some((_, 'r')) => value.push('\r'),
                Some((_, 'f')) => value.push('\x0c'),
                Some((_, 'b')) => value.push('\x08'),
                Some((_, other)) => {
                    if other == '\n' {
                        lexer.line += 1;
                    }
                    value.push(other);
                }
                None => break,
            },
            '\n' => {
                lexer.line += 1;
                value.push('\n');
            }
            other => {
                if lexer.replaced.binary_search(&(lexer.pos + offset)).is_ok() {
                    lexer.hit_replaced = true;
                }
                value.push(other);
            }
        }
    }
    lexer.pos = lexer.text.len();
    None
}

fn lex_number_or_date(lexer: &mut Lexer<'_>) -> Option<Tok> {
    let rest = &lexer.bytes[lexer.pos..];
    let looks_like_date = rest.len() >= 10
        && rest[..4].iter().all(u8::is_ascii_digit)
        && matches!(rest[4], b'-' | b'/')
        && rest[5..7].iter().all(u8::is_ascii_digit)
        && rest[7] == rest[4]
        && rest[8..10].iter().all(u8::is_ascii_digit)
        && !rest.get(10).is_some_and(u8::is_ascii_alphanumeric);
    if looks_like_date {
        let text = &lexer.text[lexer.pos..lexer.pos + 10];
        lexer.pos += 10;
        return Date::parse(text).map(Tok::Date);
    }
    let start = lexer.pos;
    lexer.take_while(|b| b.is_ascii_digit());
    while lexer.peek() == Some(b',') && lexer.peek_at(1).is_some_and(|b| b.is_ascii_digit()) {
        lexer.pos += 1;
        lexer.take_while(|b| b.is_ascii_digit());
    }
    if lexer.peek() == Some(b'.') {
        lexer.pos += 1;
        lexer.take_while(|b| b.is_ascii_digit());
    }
    number::parse_literal(&lexer.text[start..lexer.pos]).map(Tok::Number)
}
