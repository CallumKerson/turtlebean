//! Loads a ledger from disk: follows includes, applies options, sorts
//! entries, then books and checks them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rust_decimal::Decimal;

use crate::booking::{self, Context, Inventory, Outcome};
use crate::checks;
use crate::model::{Booking, Entry, EntryKind, Error, ErrorKind, Ledger, Options, Source};
use crate::parser::{Parsed, RawEntry, RawKind, RawTransaction, parse_with_replacements};

/// Options beancount accepts that turtlebean doesn't act on.
const IGNORED_OPTIONS: &[&str] = &[
    "account_previous_balances",
    "account_previous_earnings",
    "account_previous_conversions",
    "account_current_earnings",
    "account_current_conversions",
    "account_unrealized_gains",
    "account_rounding",
    "conversion_currency",
    "documents",
    "render_commas",
    "plugin_processing_mode",
    "long_string_maxlines",
    "insert_pythonpath",
];

struct Loaded {
    entries: Vec<(RawEntry, PathBuf)>,
    errors: Vec<Error>,
    options: Options,
    /// Account root names, which `name_*` options can change.
    roots: [String; 5],
    accounts: Vec<(String, Source)>,
}

/// Loads a ledger file and everything it includes.
#[must_use]
pub fn load_file(path: &Path) -> Ledger {
    let mut loaded = Loaded {
        entries: Vec::new(),
        errors: Vec::new(),
        options: Options {
            tolerance_multiplier: Decimal::new(5, 1),
            ..Options::default()
        },
        roots: ["Assets", "Liabilities", "Equity", "Income", "Expenses"].map(String::from),
        accounts: Vec::new(),
    };
    let mut visited = HashSet::new();
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    load_recursive(&path, &mut loaded, &mut visited);
    finish(loaded)
}

fn load_recursive(path: &Path, loaded: &mut Loaded, visited: &mut HashSet<PathBuf>) {
    if !visited.insert(path.to_path_buf()) {
        return;
    }
    let (text, replaced) = match std::fs::read(path) {
        Ok(bytes) => decode(&bytes),
        Err(e) => {
            loaded.errors.push(Error {
                kind: ErrorKind::LoadError,
                source: None,
                message: format!("can't read {}: {e}", path.display()),
            });
            return;
        }
    };
    let document = parse_with_replacements(&text, &replaced);
    let source = |line| Source {
        file: path.to_path_buf(),
        line,
    };
    loaded.accounts.extend(
        document
            .accounts
            .into_iter()
            .map(|(a, line)| (a, source(line))),
    );
    for e in document.errors {
        loaded.errors.push(Error {
            kind: e.kind,
            source: Some(source(e.line)),
            message: e.message,
        });
    }
    let mut includes = Vec::new();
    for parsed in document.parsed {
        match parsed {
            Parsed::Entry(entry) => loaded.entries.push((entry, path.to_path_buf())),
            Parsed::Option { name, value, line } => {
                let source = source(line);
                match root_option(&name) {
                    Some(index) => loaded.roots[index] = value,
                    None => apply_option(
                        &name,
                        &value,
                        &source,
                        &mut loaded.options,
                        &mut loaded.errors,
                    ),
                }
            }
            Parsed::Include { path: include, .. } => includes.push(include),
            Parsed::Plugin { name, config } => loaded.options.plugins.push((name, config)),
        }
    }
    let base = path.parent().unwrap_or(Path::new("."));
    for include in includes {
        let target = base.join(&include);
        let target = target.canonicalize().unwrap_or(target);
        load_recursive(&target, loaded, visited);
    }
}

/// Decodes UTF-8, replacing invalid bytes rather than refusing the file, as
/// beancount does. Returns the text and the offsets of the replacements.
fn decode(bytes: &[u8]) -> (String, Vec<usize>) {
    let mut text = String::with_capacity(bytes.len());
    let mut replaced = Vec::new();
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            replaced.push(text.len());
            text.push(char::REPLACEMENT_CHARACTER);
        }
    }
    (text, replaced)
}

fn root_option(name: &str) -> Option<usize> {
    [
        "name_assets",
        "name_liabilities",
        "name_equity",
        "name_income",
        "name_expenses",
    ]
    .iter()
    .position(|n| *n == name)
}

fn apply_option(
    name: &str,
    value: &str,
    source: &Source,
    options: &mut Options,
    errors: &mut Vec<Error>,
) {
    let invalid = |message: String| Error {
        kind: ErrorKind::ParserError,
        source: Some(source.clone()),
        message,
    };
    match name {
        "title" => options.title = Some(value.to_owned()),
        "operating_currency" => options.operating_currencies.push(value.to_owned()),
        "booking_method" => match Booking::from_name(value) {
            Some(b) => options.booking = b,
            None => errors.push(invalid(format!("invalid booking method {value}"))),
        },
        "inferred_tolerance_default" => {
            match value
                .split_once(':')
                .and_then(|(c, n)| Some((c.to_owned(), n.parse::<Decimal>().ok()?)))
            {
                Some(pair) => options.tolerance_defaults.push(pair),
                None => errors.push(invalid(format!("invalid tolerance {value}"))),
            }
        }
        "inferred_tolerance_multiplier" => {
            errors.push(Error {
                kind: ErrorKind::DeprecatedError,
                source: Some(source.clone()),
                message: "inferred_tolerance_multiplier is deprecated".into(),
            });
            match value.parse() {
                Ok(m) => options.tolerance_multiplier = m,
                Err(_) => errors.push(invalid(format!("invalid multiplier {value}"))),
            }
        }
        "infer_tolerance_from_cost" if value.eq_ignore_ascii_case("true") => errors.push(Error {
            kind: ErrorKind::Unsupported,
            source: Some(source.clone()),
            message: "turtlebean doesn't support infer_tolerance_from_cost yet".into(),
        }),
        "infer_tolerance_from_cost" => {}
        _ if IGNORED_OPTIONS.contains(&name) => {}
        _ => errors.push(invalid(format!("unknown option {name}"))),
    }
}

/// Sort order within a day: opens first, then balances, then the rest, then
/// closes.
fn type_order(kind: &RawKind) -> i8 {
    match kind {
        RawKind::Open { .. } => -2,
        RawKind::Balance { .. } => -1,
        RawKind::Close { .. } => 2,
        _ => 0,
    }
}

/// Reports accounts whose root isn't one of the five root names.
fn check_roots(accounts: &[(String, Source)], roots: &[String; 5], errors: &mut Vec<Error>) {
    for (account, source) in accounts {
        let root = account.split(':').next().unwrap_or_default();
        if !roots.iter().any(|r| r == root) {
            errors.push(Error {
                kind: ErrorKind::ParserError,
                source: Some(source.clone()),
                message: format!("invalid account root in {account}"),
            });
        }
    }
}

/// Books a transaction, reporting any problem. Returns `None` if booking
/// dropped it.
fn book_transaction(
    txn: RawTransaction,
    ctx: &Context<'_>,
    inventory: &mut Inventory,
    source: &Source,
    errors: &mut Vec<Error>,
) -> Option<EntryKind> {
    let mut report = |e: booking::BookingError| {
        let line = e.line.unwrap_or(source.line);
        errors.push(Error {
            kind: e.kind,
            source: Some(Source {
                file: source.file.clone(),
                line,
            }),
            message: e.message,
        });
    };
    match booking::book(txn, ctx, inventory) {
        Outcome::Booked(txn, problem) => {
            if let Some(e) = problem {
                report(e);
            }
            Some(EntryKind::Transaction(txn))
        }
        Outcome::Failed(txn, e) => {
            report(e);
            Some(EntryKind::Transaction(txn))
        }
        Outcome::Dropped(e) => {
            report(e);
            None
        }
    }
}

fn finish(loaded: Loaded) -> Ledger {
    let Loaded {
        mut entries,
        mut errors,
        options,
        roots,
        accounts,
    } = loaded;
    check_roots(&accounts, &roots, &mut errors);
    entries.sort_by_key(|(e, _)| (e.date, type_order(&e.kind), e.line));

    let methods: HashMap<String, Booking> = entries
        .iter()
        .filter_map(|(e, _)| match &e.kind {
            RawKind::Open {
                account,
                booking: Some(b),
                ..
            } => Some((account.clone(), *b)),
            _ => None,
        })
        .collect();

    let mut inventory = Inventory::default();
    let mut booked = Vec::with_capacity(entries.len());
    for (raw, file) in entries {
        let source = Source {
            file,
            line: raw.line,
        };
        let kind = match raw.kind {
            RawKind::Transaction(txn) => {
                let ctx = Context {
                    date: raw.date,
                    options: &options,
                    methods: &methods,
                };
                match book_transaction(txn, &ctx, &mut inventory, &source, &mut errors) {
                    Some(kind) => kind,
                    None => continue,
                }
            }
            RawKind::Open {
                account,
                currencies,
                booking,
            } => EntryKind::Open {
                account,
                currencies,
                booking,
            },
            RawKind::Close { account } => EntryKind::Close { account },
            RawKind::Commodity { currency } => EntryKind::Commodity { currency },
            RawKind::Balance {
                account,
                amount,
                tolerance,
            } => EntryKind::Balance {
                account,
                amount,
                tolerance,
            },
            RawKind::Pad {
                account,
                source_account,
            } => EntryKind::Pad {
                account,
                source_account,
            },
            RawKind::Price { currency, amount } => EntryKind::Price { currency, amount },
            RawKind::Custom { type_name, values } => EntryKind::Custom { type_name, values },
        };
        booked.push(Entry {
            date: raw.date,
            source,
            meta: raw.meta,
            kind,
        });
    }

    let entries = checks::pad_and_balance(booked, &options, &mut errors);
    checks::validate(&entries, &mut errors);
    Ledger {
        entries,
        errors,
        options,
    }
}
