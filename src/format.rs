//! Writes directives as beancount text, and lines up the amounts in a file.
//!
//! [`entry`] turns an [`Entry`] into text that parses back to the same
//! directive. [`align`] then lines up the numbers in a whole file, so every
//! amount ends in the same column, which is how `bean-format` lays out a file.

use std::fmt::Write as _;

use crate::model::{Amount, Cost, CustomType, Entry, EntryKind, Meta, MetaValue, Posting};

/// Formats a directive as text, ending in a newline. Its source and origin are
/// ignored. Amounts are separated from what comes before by two spaces; run
/// [`align`] over the file to line them up.
///
/// Metadata strings are always quoted, as [`MetaValue`] doesn't record
/// whether a value was written as an account, currency or tag.
#[must_use]
pub fn entry(entry: &Entry) -> String {
    let mut out = format!("{} ", entry.date);
    match &entry.kind {
        EntryKind::Open {
            account,
            currencies,
            booking,
        } => {
            write!(out, "open {account}").unwrap();
            if let Some(currencies) = currencies {
                write!(out, " {}", currencies.join(",")).unwrap();
            }
            if let Some(booking) = booking {
                write!(out, " {}", quote(booking.name())).unwrap();
            }
        }
        EntryKind::Close { account } => write!(out, "close {account}").unwrap(),
        EntryKind::Commodity { currency } => write!(out, "commodity {currency}").unwrap(),
        EntryKind::Balance {
            account,
            amount,
            tolerance,
        } => {
            write!(out, "balance {account}  {}", amount.number).unwrap();
            if let Some(tolerance) = tolerance {
                write!(out, " ~ {tolerance}").unwrap();
            }
            write!(out, " {}", amount.currency).unwrap();
        }
        EntryKind::Pad {
            account,
            source_account,
        } => write!(out, "pad {account} {source_account}").unwrap(),
        EntryKind::Price { currency, amount } => {
            write!(out, "price {currency}  {}", format_amount(amount)).unwrap();
        }
        EntryKind::Custom { type_name, values } => {
            write!(out, "custom {}", quote(type_name)).unwrap();
            for value in values {
                out.push(' ');
                out.push_str(&custom_value(value.dtype, &value.value));
            }
        }
        EntryKind::Transaction(txn) => {
            out.push(txn.flag);
            if let Some(payee) = &txn.payee {
                write!(out, " {}", quote(payee)).unwrap();
            }
            write!(out, " {}", quote(&txn.narration)).unwrap();
            for tag in &txn.tags {
                write!(out, " #{tag}").unwrap();
            }
            for link in &txn.links {
                write!(out, " ^{link}").unwrap();
            }
            out.push('\n');
            write_meta(&mut out, &entry.meta, 2);
            for posting in &txn.postings {
                write_posting(&mut out, posting);
            }
            return out;
        }
    }
    out.push('\n');
    write_meta(&mut out, &entry.meta, 2);
    out
}

fn write_posting(out: &mut String, posting: &Posting) {
    out.push_str("  ");
    if let Some(flag) = posting.flag {
        write!(out, "{flag} ").unwrap();
    }
    out.push_str(&posting.account);
    if let Some(units) = &posting.units {
        write!(out, "  {}", format_amount(units)).unwrap();
    }
    if let Some(cost) = &posting.cost {
        write!(out, " {}", format_cost(cost)).unwrap();
    }
    if let Some(price) = &posting.price {
        write!(out, " @ {}", format_amount(price)).unwrap();
    }
    out.push('\n');
    write_meta(out, &posting.meta, 4);
}

fn write_meta(out: &mut String, meta: &Meta, indent: usize) {
    for (key, value) in meta {
        write!(out, "{:indent$}{key}:", "").unwrap();
        if *value != MetaValue::Null {
            write!(out, " {}", meta_value(value)).unwrap();
        }
        out.push('\n');
    }
}

fn format_amount(amount: &Amount) -> String {
    format!("{} {}", amount.number, amount.currency)
}

fn format_cost(cost: &Cost) -> String {
    let mut out = format!("{{{} {}, {}", cost.number, cost.currency, cost.date);
    if let Some(label) = &cost.label {
        write!(out, ", {}", quote(label)).unwrap();
    }
    out.push('}');
    out
}

fn meta_value(value: &MetaValue) -> String {
    match value {
        MetaValue::String(s) => quote(s),
        MetaValue::Number(n) => n.to_string(),
        MetaValue::Date(d) => d.to_string(),
        MetaValue::Bool(b) => bool_text(*b).to_owned(),
        MetaValue::Amount(a) => format_amount(a),
        MetaValue::Null => String::new(),
    }
}

fn custom_value(dtype: CustomType, value: &MetaValue) -> String {
    match (dtype, value) {
        (CustomType::Account, MetaValue::String(account)) => account.clone(),
        _ => meta_value(value),
    }
}

fn bool_text(value: bool) -> &'static str {
    if value { "TRUE" } else { "FALSE" }
}

/// A string in double quotes, with quotes, backslashes and control
/// characters escaped.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Lines up the amounts in a file: every posting, `balance` and `price` line
/// with a plain number gets padded so the numbers are right-aligned in one
/// column, as wide as the widest number and two spaces after the longest
/// account or currency. Everything else is left
/// as it is, including line endings.
#[must_use]
pub fn align(text: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let parts: Vec<Option<Parts<'_>>> = lines.iter().map(|line| split_amount(line)).collect();
    let found = || parts.iter().flatten();
    let (Some(prefix), Some(number)) = (
        found().map(|p| p.prefix.chars().count()).max(),
        found().map(|p| p.number.len()).max(),
    ) else {
        return text.to_owned();
    };
    // Numbers are right-aligned in a column as wide as the widest, two spaces
    // after the longest account or currency.
    let end = prefix + 2 + number;
    let mut out = String::with_capacity(text.len());
    for (line, parts) in lines.iter().zip(parts) {
        match parts {
            Some(p) => {
                let width = end - p.number.len();
                write!(out, "{:<width$}{} {}", p.prefix, p.number, p.rest).unwrap();
            }
            None => out.push_str(line),
        }
    }
    out
}

/// A line split around its amount's number.
struct Parts<'a> {
    /// Up to the end of the account (or the priced currency).
    prefix: &'a str,
    number: &'a str,
    /// From the currency on, including the line ending.
    rest: &'a str,
}

fn split_amount(line: &str) -> Option<Parts<'_>> {
    let (prefix_end, after) = if line.starts_with([' ', '\t']) {
        // A posting: indent, optional flag, then an account.
        let mut body = line.trim_start();
        if let Some((flag, rest)) = body.split_once(' ')
            && flag.chars().count() == 1
            && !flag.starts_with(char::is_alphanumeric)
        {
            body = rest.trim_start();
        }
        let account = word(body);
        if !account.starts_with(char::is_uppercase) || !account.contains(':') {
            return None;
        }
        let start = line.len() - body.len();
        (start + account.len(), &body[account.len()..])
    } else {
        // `DATE balance ACCOUNT` or `DATE price CURRENCY`.
        let date = word(line);
        if date.len() != 10 || !date.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        let rest = line[date.len()..].trim_start();
        let keyword = word(rest);
        if keyword != "balance" && keyword != "price" {
            return None;
        }
        let rest = rest[keyword.len()..].trim_start();
        let name = word(rest);
        let start = line.len() - rest.len();
        (start + name.len(), &rest[name.len()..])
    };
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let number_text = after.trim_start();
    let number = word(number_text);
    if !is_plain_number(number) {
        return None;
    }
    let rest = number_text[number.len()..].trim_start_matches([' ', '\t']);
    if !rest.starts_with(|c: char| c.is_uppercase() || c == '~') {
        return None;
    }
    Some(Parts {
        prefix: &line[..prefix_end],
        number,
        rest,
    })
}

fn word(text: &str) -> &str {
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    &text[..end]
}

/// A number with an optional sign, digits, commas and one decimal point; not
/// an expression such as `(1 + 2)`.
fn is_plain_number(text: &str) -> bool {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    !digits.is_empty()
        && digits.starts_with(|c: char| c.is_ascii_digit())
        && digits
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',')
        && digits.matches('.').count() <= 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Origin, Source, Transaction};

    #[test]
    fn aligns_numbers_in_one_right_aligned_column() {
        let text = "\
2026-10-01 * \"Shop\"
  Liabilities:Joint:Card  -15.29 GBP
  ! Expenses:Food  15.29 GBP

2026-10-02 balance Assets:Bank  270.00 GBP
2026-10-02 price VWRL  98.1 GBP
";
        // The longest prefix is the balance line (30 characters) and the
        // widest number has 6, so numbers end at column 38.
        let expected = "\
2026-10-01 * \"Shop\"
  Liabilities:Joint:Card        -15.29 GBP
  ! Expenses:Food                15.29 GBP

2026-10-02 balance Assets:Bank  270.00 GBP
2026-10-02 price VWRL             98.1 GBP
";
        assert_eq!(align(text), expected);
        assert_eq!(align(expected), expected);
    }

    #[test]
    fn leaves_other_lines_alone() {
        let text = "\
; Expenses:Food  1 GBP
2026-10-01 * \"Shop\" #tag
  note: \"x\"
  Assets:Bank
  Assets:Cash  (1 + 2) GBP\r
2026-10-01 open Assets:Bank  GBP
";
        assert_eq!(align(text), text);
    }

    #[test]
    fn keeps_cost_price_and_tolerance_after_the_number() {
        let text = concat!(
            "  Assets:Holdings  118.92 VWRL {254.28 GBP, 2026-01-26}\n",
            "  Assets:Cash  -30000.00 GBP @ 1.2 USD\n",
            "2026-01-01 balance Assets:Cash  1.00 ~ 0.01 GBP\n",
        );
        let expected = concat!(
            "  Assets:Holdings                  118.92 VWRL {254.28 GBP, 2026-01-26}\n",
            "  Assets:Cash                   -30000.00 GBP @ 1.2 USD\n",
            "2026-01-01 balance Assets:Cash       1.00 ~ 0.01 GBP\n",
        );
        assert_eq!(align(text), expected);
    }

    #[test]
    fn quotes_and_escapes_strings() {
        let entry = Entry {
            date: crate::date::Date::new(2026, 1, 2).unwrap(),
            source: Source {
                file: std::path::PathBuf::new(),
                line: 0,
            },
            meta: vec![(
                "note".to_owned(),
                MetaValue::String("say \"hi\"\\".to_owned()),
            )],
            kind: EntryKind::Transaction(Transaction {
                flag: '!',
                payee: None,
                narration: "line\nbreak".to_owned(),
                tags: vec!["joint".to_owned()],
                links: vec!["tx-1".to_owned()],
                postings: Vec::new(),
            }),
            origin: Origin::Written,
        };
        assert_eq!(
            super::entry(&entry),
            "2026-01-02 ! \"line\\nbreak\" #joint ^tx-1\n  note: \"say \\\"hi\\\"\\\\\"\n"
        );
    }
}
