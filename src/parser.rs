//! Parses logical lines into directives, before booking.

use rust_decimal::Decimal;

use crate::date::Date;
use crate::lexer::{Line, Tok, Token, lex};
use crate::model::{Amount, Booking, CustomType, CustomValue, ErrorKind, Meta, MetaValue};
use crate::number;

/// A cost as written: `{...}` or `{{...}}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CostSpec {
    pub per_unit: Option<Decimal>,
    pub total: Option<Decimal>,
    pub currency: Option<String>,
    pub date: Option<Date>,
    pub label: Option<String>,
}

/// A price as written: `@ ...` or `@@ ...`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PriceSpec {
    pub number: Option<Decimal>,
    pub currency: Option<String>,
    pub total: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawPosting {
    pub account: String,
    pub number: Option<Decimal>,
    pub currency: Option<String>,
    pub cost: Option<CostSpec>,
    pub price: Option<PriceSpec>,
    pub flag: Option<char>,
    pub meta: Meta,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawTransaction {
    pub flag: char,
    pub payee: Option<String>,
    pub narration: String,
    pub tags: Vec<String>,
    pub links: Vec<String>,
    pub postings: Vec<RawPosting>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawKind {
    Open {
        account: String,
        currencies: Option<Vec<String>>,
        booking: Option<Booking>,
    },
    Close {
        account: String,
    },
    Commodity {
        currency: String,
    },
    Balance {
        account: String,
        amount: Amount,
        tolerance: Option<Decimal>,
    },
    Pad {
        account: String,
        source_account: String,
    },
    Price {
        currency: String,
        amount: Amount,
    },
    Custom {
        type_name: String,
        values: Vec<CustomValue>,
    },
    Transaction(RawTransaction),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawEntry {
    pub date: Date,
    pub line: usize,
    pub meta: Meta,
    pub kind: RawKind,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Parsed {
    Entry(RawEntry),
    Option {
        name: String,
        value: String,
        line: usize,
    },
    Include {
        path: String,
        line: usize,
    },
    Plugin {
        name: String,
        config: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParseError {
    pub kind: ErrorKind,
    pub line: usize,
    pub message: String,
}

/// A parsed file: its directives and errors, plus the text split into
/// pieces for lossless writing.
#[derive(Debug, Clone, Default)]
pub struct Document {
    pub(crate) items: Vec<String>,
    pub(crate) parsed: Vec<Parsed>,
    pub(crate) errors: Vec<ParseError>,
    /// Every account written, with its line, so roots can be checked once
    /// the options are known.
    pub(crate) accounts: Vec<(String, usize)>,
}

/// Parses ledger text. Errors are kept in the document rather than returned.
#[must_use]
pub fn parse(text: &str) -> Document {
    parse_with_replacements(text, &[])
}

/// Parses text decoded from bytes that weren't all valid UTF-8; `replaced`
/// holds the sorted offsets of the replacement characters.
pub(crate) fn parse_with_replacements(text: &str, replaced: &[usize]) -> Document {
    let lines = lex(text, replaced);
    let mut document = Document::default();
    split_items(text, &lines, &mut document.items);
    for block in blocks(&lines, &mut document.errors) {
        parse_block(&block, &mut document);
    }
    document
}

/// Writes a document back to text.
#[must_use]
pub fn write(document: &Document) -> String {
    document.items.concat()
}

/// Splits the text at the start of every logical line, so writing the pieces
/// back reproduces it exactly.
fn split_items(text: &str, lines: &[Line], items: &mut Vec<String>) {
    let mut last = 0;
    for line in lines {
        if line.start > last {
            items.push(text[last..line.start].to_owned());
        }
        items.push(text[line.start..line.end].to_owned());
        last = line.end;
    }
    if last < text.len() {
        items.push(text[last..].to_owned());
    }
}

/// Groups lines into blocks: a line at the margin and the indented lines
/// after it, up to a blank line.
fn blocks<'a>(lines: &'a [Line], errors: &mut Vec<ParseError>) -> Vec<Block<'a>> {
    let mut blocks: Vec<Block<'a>> = Vec::new();
    let mut open = false;
    for line in lines {
        if line.blank {
            open = false;
            continue;
        }
        // The parser needs the next token to finish a directive, so an
        // invalid token straight after one discards it.
        if line.starts_invalid
            && open
            && !line.indented
            && let Some(block) = blocks.last_mut()
        {
            block.discarded = true;
        }
        if line.indented && open {
            if let Some(block) = blocks.last_mut() {
                block.lines.push(line);
            }
            continue;
        }
        if line.indented {
            errors.push(syntax_error(line.line, "unexpected indentation"));
            let starts_with_date = matches!(
                line.tokens.first(),
                Some(Token {
                    tok: Tok::Date(_),
                    ..
                })
            );
            if !starts_with_date {
                // Swallow this and any following indented lines
                blocks.push(Block {
                    lines: vec![line],
                    discarded: true,
                });
                open = true;
                continue;
            }
        }
        blocks.push(Block {
            lines: vec![line],
            discarded: false,
        });
        open = true;
    }
    blocks
}

/// A directive's lines.
struct Block<'a> {
    lines: Vec<&'a Line>,
    /// Dropped because of an error after it; only its lexer errors count.
    discarded: bool,
}

fn syntax_error(line: usize, message: &str) -> ParseError {
    ParseError {
        kind: ErrorKind::ParserSyntaxError,
        line,
        message: message.to_owned(),
    }
}

fn parse_block(block: &Block<'_>, document: &mut Document) {
    let discarded = block.discarded;
    let block = block.lines.as_slice();
    // Invalid tokens drop the whole directive
    let mut lexer_errors = false;
    for line in block {
        for &at in &line.errors {
            document.errors.push(ParseError {
                kind: ErrorKind::LexerError,
                line: at,
                message: "invalid token".into(),
            });
            lexer_errors = true;
        }
        if let Some(at) = line.unterminated {
            document.errors.push(ParseError {
                kind: ErrorKind::LexerError,
                line: at,
                message: "unterminated string".into(),
            });
            lexer_errors = true;
        }
    }
    if lexer_errors || discarded {
        return;
    }
    let mut parser = BlockParser {
        errors: Vec::new(),
        accounts: Vec::new(),
    };
    let result = parser.block(block);
    document.errors.append(&mut parser.errors);
    if result.is_ok() {
        document.accounts.append(&mut parser.accounts);
    }
    match result {
        Ok(Some(parsed)) => document.parsed.push(parsed),
        Ok(None) => {}
        Err(error) => document.errors.push(error),
    }
}

struct BlockParser {
    /// Errors that don't drop the directive.
    errors: Vec<ParseError>,
    accounts: Vec<(String, usize)>,
}

/// A cursor over one logical line's tokens.
struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
    end_line: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|t| &t.tok)
    }

    fn next(&mut self) -> Option<&Tok> {
        let tok = self.tokens.get(self.pos).map(|t| &t.tok);
        self.pos += 1;
        tok
    }

    fn done(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    /// The line to report an error at: the current token's, or the line
    /// after the end if there are no more tokens.
    fn here(&self) -> usize {
        self.tokens.get(self.pos).map_or(self.end_line, |t| t.line)
    }

    fn error(&self, message: &str) -> ParseError {
        syntax_error(self.here(), message)
    }

    fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn end(&self) -> Result<(), ParseError> {
        if self.done() {
            Ok(())
        } else {
            Err(self.error("unexpected token"))
        }
    }

    fn string(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::String(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err(self.error("expected a string")),
        }
    }

    fn currency(&mut self) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::Currency(s)) => {
                let s = s.clone();
                self.pos += 1;
                Ok(s)
            }
            _ => Err(self.error("expected a currency")),
        }
    }

    fn starts_expression(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Number(_) | Tok::LParen | Tok::Minus | Tok::Plus)
        )
    }

    /// expr := term (('+' | '-') term)*
    fn expression(&mut self) -> Result<Decimal, ParseError> {
        let mut value = self.term()?;
        loop {
            if self.eat(&Tok::Plus) {
                let rhs = self.term()?;
                value = number::add(value, rhs).ok_or_else(|| self.error("overflow"))?;
            } else if self.eat(&Tok::Minus) {
                let rhs = self.term()?;
                value = number::sub(value, rhs).ok_or_else(|| self.error("overflow"))?;
            } else {
                return Ok(value);
            }
        }
    }

    /// term := factor (('*' | '/') factor)*
    fn term(&mut self) -> Result<Decimal, ParseError> {
        let mut value = self.factor()?;
        loop {
            if self.peek() == Some(&Tok::Star) {
                self.pos += 1;
                let rhs = self.factor()?;
                value = number::mul(value, rhs).ok_or_else(|| self.error("overflow"))?;
            } else if self.peek() == Some(&Tok::Slash) {
                self.pos += 1;
                let line = self.here();
                let rhs = self.factor()?;
                value = number::div(value, rhs).ok_or_else(|| ParseError {
                    kind: ErrorKind::ParserError,
                    line,
                    message: "division by zero".into(),
                })?;
            } else {
                return Ok(value);
            }
        }
    }

    /// factor := NUMBER | '(' expr ')' | '-' factor | '+' factor
    fn factor(&mut self) -> Result<Decimal, ParseError> {
        match self.peek() {
            Some(Tok::Number(n)) => {
                let n = *n;
                self.pos += 1;
                Ok(n)
            }
            Some(Tok::LParen) => {
                self.pos += 1;
                let value = self.expression()?;
                if self.eat(&Tok::RParen) {
                    Ok(value)
                } else {
                    Err(self.error("expected ')'"))
                }
            }
            Some(Tok::Minus) => {
                self.pos += 1;
                Ok(-self.factor()?)
            }
            Some(Tok::Plus) => {
                self.pos += 1;
                self.factor()
            }
            _ => Err(self.error("expected a number")),
        }
    }

    fn amount(&mut self) -> Result<Amount, ParseError> {
        let number = self.expression()?;
        let currency = self.currency()?;
        Ok(Amount { number, currency })
    }
}

impl BlockParser {
    fn account(&mut self, cursor: &mut Cursor<'_>) -> Result<String, ParseError> {
        let line = cursor.here();
        match cursor.peek() {
            Some(Tok::Account(a)) => {
                let a = a.clone();
                cursor.pos += 1;
                self.accounts.push((a.clone(), line));
                Ok(a)
            }
            _ => Err(cursor.error("expected an account")),
        }
    }

    fn block(&mut self, block: &[&Line]) -> Result<Option<Parsed>, ParseError> {
        let header = block[0];
        let mut cursor = Cursor {
            tokens: &header.tokens,
            pos: 0,
            end_line: header.end_line,
        };
        let line = header.line;
        match cursor.next().cloned() {
            Some(Tok::Date(date)) => self.dated(date, line, &mut cursor, &block[1..]),
            Some(Tok::Keyword(k)) if k == "option" => {
                let name = cursor.string()?;
                let value = cursor.string()?;
                cursor.end()?;
                no_continuation(&block[1..])?;
                Ok(Some(Parsed::Option { name, value, line }))
            }
            Some(Tok::Keyword(k)) if k == "include" => {
                let path = cursor.string()?;
                cursor.end()?;
                no_continuation(&block[1..])?;
                Ok(Some(Parsed::Include { path, line }))
            }
            Some(Tok::Keyword(k)) if k == "plugin" => {
                let name = cursor.string()?;
                let config = if cursor.done() {
                    None
                } else {
                    Some(cursor.string()?)
                };
                cursor.end()?;
                no_continuation(&block[1..])?;
                Ok(Some(Parsed::Plugin { name, config }))
            }
            Some(Tok::Keyword(k)) if k.starts_with("push") || k.starts_with("pop") => {
                Err(unsupported(line, &k))
            }
            _ => Err(syntax_error(line, "expected a date or a keyword")),
        }
    }

    fn dated(
        &mut self,
        date: Date,
        line: usize,
        cursor: &mut Cursor<'_>,
        rest: &[&Line],
    ) -> Result<Option<Parsed>, ParseError> {
        let kind = match cursor.peek().cloned() {
            Some(Tok::Star | Tok::Flag(_) | Tok::Keyword(_)) if is_transaction(cursor) => {
                let txn = self.transaction(cursor, rest)?;
                return Ok(Some(Parsed::Entry(RawEntry {
                    date,
                    line,
                    meta: txn.1,
                    kind: RawKind::Transaction(txn.0),
                })));
            }
            Some(Tok::Keyword(k)) => {
                cursor.pos += 1;
                match k.as_str() {
                    "open" => self.open(cursor)?,
                    "close" => RawKind::Close {
                        account: self.account(cursor)?,
                    },
                    "commodity" => RawKind::Commodity {
                        currency: cursor.currency()?,
                    },
                    "balance" => self.balance(cursor)?,
                    "pad" => RawKind::Pad {
                        account: self.account(cursor)?,
                        source_account: self.account(cursor)?,
                    },
                    "price" => RawKind::Price {
                        currency: cursor.currency()?,
                        amount: cursor.amount()?,
                    },
                    "custom" => self.custom(cursor)?,
                    other => return Err(unsupported(line, other)),
                }
            }
            _ => return Err(cursor.error("expected a directive")),
        };
        cursor.end()?;
        let meta = self.meta_lines(rest)?;
        Ok(Some(Parsed::Entry(RawEntry {
            date,
            line,
            meta,
            kind,
        })))
    }

    fn open(&mut self, cursor: &mut Cursor<'_>) -> Result<RawKind, ParseError> {
        let account = self.account(cursor)?;
        let mut currencies = Vec::new();
        if matches!(cursor.peek(), Some(Tok::Currency(_))) {
            currencies.push(cursor.currency()?);
            while cursor.eat(&Tok::Comma) {
                currencies.push(cursor.currency()?);
            }
        }
        let booking = match cursor.peek() {
            Some(Tok::String(_)) => {
                let line = cursor.here();
                let name = cursor.string()?;
                let booking = Booking::from_name(&name);
                if booking.is_none() {
                    self.errors.push(ParseError {
                        kind: ErrorKind::ParserError,
                        line,
                        message: format!("invalid booking method {name}"),
                    });
                }
                booking
            }
            _ => None,
        };
        Ok(RawKind::Open {
            account,
            currencies: if currencies.is_empty() {
                None
            } else {
                Some(currencies)
            },
            booking,
        })
    }

    fn balance(&mut self, cursor: &mut Cursor<'_>) -> Result<RawKind, ParseError> {
        let account = self.account(cursor)?;
        let number = cursor.expression()?;
        let tolerance = if cursor.eat(&Tok::Tilde) {
            Some(cursor.expression()?)
        } else {
            None
        };
        let currency = cursor.currency()?;
        Ok(RawKind::Balance {
            account,
            amount: Amount { number, currency },
            tolerance,
        })
    }

    fn custom(&mut self, cursor: &mut Cursor<'_>) -> Result<RawKind, ParseError> {
        let type_name = cursor.string()?;
        let mut values = Vec::new();
        while let Some(tok) = cursor.peek().cloned() {
            let value = match tok {
                Tok::String(s) => {
                    cursor.pos += 1;
                    CustomValue {
                        dtype: CustomType::String,
                        value: MetaValue::String(s),
                    }
                }
                Tok::Account(_) => {
                    let a = self.account(cursor)?;
                    CustomValue {
                        dtype: CustomType::Account,
                        value: MetaValue::String(a),
                    }
                }
                Tok::Date(d) => {
                    cursor.pos += 1;
                    CustomValue {
                        dtype: CustomType::Date,
                        value: MetaValue::Date(d),
                    }
                }
                Tok::Bool(b) => {
                    cursor.pos += 1;
                    CustomValue {
                        dtype: CustomType::Bool,
                        value: MetaValue::Bool(b),
                    }
                }
                _ if cursor.starts_expression() => {
                    let number = cursor.expression()?;
                    if let Some(Tok::Currency(c)) = cursor.peek().cloned() {
                        cursor.pos += 1;
                        CustomValue {
                            dtype: CustomType::Amount,
                            value: MetaValue::Amount(Amount {
                                number,
                                currency: c,
                            }),
                        }
                    } else {
                        CustomValue {
                            dtype: CustomType::Number,
                            value: MetaValue::Number(number),
                        }
                    }
                }
                _ => return Err(cursor.error("unexpected custom value")),
            };
            values.push(value);
        }
        Ok(RawKind::Custom { type_name, values })
    }

    fn meta_value(&mut self, cursor: &mut Cursor<'_>) -> Result<MetaValue, ParseError> {
        let value = match cursor.peek().cloned() {
            None => return Ok(MetaValue::Null),
            Some(Tok::String(s)) => {
                cursor.pos += 1;
                MetaValue::String(s)
            }
            Some(Tok::Account(_)) => MetaValue::String(self.account(cursor)?),
            Some(Tok::Currency(c)) => {
                cursor.pos += 1;
                MetaValue::String(c)
            }
            Some(Tok::Tag(t)) => {
                cursor.pos += 1;
                MetaValue::String(t)
            }
            Some(Tok::Date(d)) => {
                cursor.pos += 1;
                MetaValue::Date(d)
            }
            Some(Tok::Bool(b)) => {
                cursor.pos += 1;
                MetaValue::Bool(b)
            }
            Some(Tok::Null) => {
                cursor.pos += 1;
                MetaValue::Null
            }
            Some(_) if cursor.starts_expression() => {
                let number = cursor.expression()?;
                if let Some(Tok::Currency(c)) = cursor.peek().cloned() {
                    cursor.pos += 1;
                    MetaValue::Amount(Amount {
                        number,
                        currency: c,
                    })
                } else {
                    MetaValue::Number(number)
                }
            }
            Some(_) => return Err(cursor.error("unexpected metadata value")),
        };
        Ok(value)
    }

    fn meta_line(&mut self, line: &Line) -> Result<(String, MetaValue), ParseError> {
        let mut cursor = Cursor {
            tokens: &line.tokens,
            pos: 0,
            end_line: line.end_line,
        };
        let Some(Tok::Key(key)) = cursor.next().cloned() else {
            return Err(syntax_error(line.line, "expected metadata"));
        };
        let value = self.meta_value(&mut cursor)?;
        cursor.end()?;
        Ok((key, value))
    }

    fn meta_lines(&mut self, lines: &[&Line]) -> Result<Meta, ParseError> {
        lines.iter().map(|line| self.meta_line(line)).collect()
    }

    fn transaction(
        &mut self,
        cursor: &mut Cursor<'_>,
        rest: &[&Line],
    ) -> Result<(RawTransaction, Meta), ParseError> {
        let flag = match cursor.next() {
            Some(Tok::Flag(c)) => *c,
            _ => '*', // `*` or `txn`
        };
        let mut strings = Vec::new();
        while let Some(Tok::String(s)) = cursor.peek() {
            strings.push(s.clone());
            cursor.pos += 1;
        }
        if strings.len() > 2 {
            return Err(syntax_error(cursor.here(), "too many strings"));
        }
        let narration = strings.pop().unwrap_or_default();
        let payee = strings.pop();
        let mut txn = RawTransaction {
            flag,
            payee,
            narration,
            tags: Vec::new(),
            links: Vec::new(),
            postings: Vec::new(),
        };
        tags_and_links(cursor, &mut txn)?;
        cursor.end()?;

        let mut meta = Meta::new();
        for line in rest {
            let mut cursor = Cursor {
                tokens: &line.tokens,
                pos: 0,
                end_line: line.end_line,
            };
            match cursor.peek() {
                Some(Tok::Tag(_) | Tok::Link(_)) if txn.postings.is_empty() => {
                    tags_and_links(&mut cursor, &mut txn)?;
                    cursor.end()?;
                }
                Some(Tok::Key(_)) => {
                    let pair = self.meta_line(line)?;
                    match txn.postings.last_mut() {
                        Some(posting) => posting.meta.push(pair),
                        None => meta.push(pair),
                    }
                }
                _ => {
                    let posting = self.posting(&mut cursor, line.line)?;
                    txn.postings.push(posting);
                }
            }
        }
        txn.tags.sort();
        txn.tags.dedup();
        txn.links.sort();
        txn.links.dedup();
        Ok((txn, meta))
    }

    fn posting(&mut self, cursor: &mut Cursor<'_>, line: usize) -> Result<RawPosting, ParseError> {
        let flag = match cursor.peek() {
            Some(Tok::Star) => {
                cursor.pos += 1;
                Some('*')
            }
            Some(Tok::Flag(c)) => {
                let c = *c;
                cursor.pos += 1;
                Some(c)
            }
            _ => None,
        };
        let account = self.account(cursor)?;
        let number = if cursor.starts_expression() {
            Some(cursor.expression()?)
        } else {
            None
        };
        let currency = match cursor.peek() {
            Some(Tok::Currency(c)) => {
                let c = c.clone();
                cursor.pos += 1;
                Some(c)
            }
            _ => None,
        };
        let cost = match cursor.peek() {
            Some(Tok::LCurl) => {
                cursor.pos += 1;
                Some(cost_spec(cursor, false)?)
            }
            Some(Tok::LCurlCurl) => {
                cursor.pos += 1;
                Some(cost_spec(cursor, true)?)
            }
            _ => None,
        };
        let price = match cursor.peek() {
            Some(Tok::At | Tok::AtAt) => {
                let total = cursor.peek() == Some(&Tok::AtAt);
                cursor.pos += 1;
                let number = if cursor.starts_expression() {
                    Some(cursor.expression()?)
                } else {
                    None
                };
                let currency = match cursor.peek() {
                    Some(Tok::Currency(c)) => {
                        let c = c.clone();
                        cursor.pos += 1;
                        Some(c)
                    }
                    _ => None,
                };
                Some(PriceSpec {
                    number,
                    currency,
                    total,
                })
            }
            _ => None,
        };
        cursor.end()?;
        Ok(RawPosting {
            account,
            number,
            currency,
            cost,
            price,
            flag,
            meta: Meta::new(),
            line,
        })
    }
}

fn is_transaction(cursor: &Cursor<'_>) -> bool {
    match cursor.peek() {
        Some(Tok::Star | Tok::Flag(_)) => true,
        Some(Tok::Keyword(k)) => k == "txn",
        _ => false,
    }
}

fn tags_and_links(cursor: &mut Cursor<'_>, txn: &mut RawTransaction) -> Result<(), ParseError> {
    loop {
        match cursor.peek() {
            Some(Tok::Tag(t)) => txn.tags.push(t.clone()),
            Some(Tok::Link(l)) => txn.links.push(l.clone()),
            _ => return Ok(()),
        }
        cursor.pos += 1;
    }
}

fn cost_spec(cursor: &mut Cursor<'_>, total: bool) -> Result<CostSpec, ParseError> {
    let close = if total { Tok::RCurlCurl } else { Tok::RCurl };
    let mut spec = CostSpec::default();
    if cursor.eat(&close) {
        return Ok(spec);
    }
    loop {
        match cursor.peek().cloned() {
            Some(Tok::Date(d)) => {
                cursor.pos += 1;
                spec.date = Some(d);
            }
            Some(Tok::String(s)) => {
                cursor.pos += 1;
                spec.label = Some(s);
            }
            Some(Tok::Currency(c)) => {
                cursor.pos += 1;
                spec.currency = Some(c);
            }
            Some(Tok::Hash | Tok::Star) => {
                return Err(unsupported(cursor.here(), "compound or merged cost"));
            }
            _ if cursor.starts_expression() => {
                let number = cursor.expression()?;
                if cursor.peek() == Some(&Tok::Hash) {
                    return Err(unsupported(cursor.here(), "compound cost"));
                }
                if total {
                    spec.total = Some(number);
                } else {
                    spec.per_unit = Some(number);
                }
                spec.currency = Some(cursor.currency()?);
            }
            _ => return Err(cursor.error("unexpected token in cost")),
        }
        if cursor.eat(&close) {
            return Ok(spec);
        }
        if !cursor.eat(&Tok::Comma) {
            return Err(cursor.error("expected ',' in cost"));
        }
    }
}

fn no_continuation(rest: &[&Line]) -> Result<(), ParseError> {
    match rest.first() {
        Some(line) => Err(syntax_error(line.line, "unexpected indented line")),
        None => Ok(()),
    }
}

fn unsupported(line: usize, what: &str) -> ParseError {
    ParseError {
        kind: ErrorKind::Unsupported,
        line,
        message: format!("turtlebean doesn't support `{what}` yet"),
    }
}
